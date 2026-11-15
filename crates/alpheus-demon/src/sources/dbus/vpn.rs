//! VPN profiles as the quick panel reads them from NetworkManager's `GetSettings`: name, uuid,
//! kind, last use and whether a secret is asked at connection time. No secret is ever decoded.

use super::props::Props;
pub use super::tap::VpnTap;
use super::wifi::{MAX_SAVED, valid_uuid};
use super::{Bus, NM, is_service_gone, network, optional};
use crate::log::write as log;
use crate::system::{ActiveVpn, Saved, VpnKind, VpnProfile, VpnScope, VpnState};
use crate::text;
use std::collections::BTreeMap;
use std::io;

/// The longest profile name kept, in bytes (NetworkManager's own limit).
pub const NAME_MAX: usize = 255;
/// The most active VPN connections kept.
pub const MAX_ACTIVE: usize = 16;

/// NMSettingSecretFlags: a secret is asked at connection time when it is agent-owned (1) or
/// not saved (2) and not marked not-required (4).
pub fn secret_asked(flags: u32) -> bool {
    flags & 4 == 0 && flags & 3 != 0
}

/// A saved connection's `connection` setting as a VPN profile; None for any other kind, or a uuid
/// nmcli would read as an option. The name is cleaned and cut on a character boundary.
pub fn saved_vpn(connection: Option<&Props>, asks: bool) -> Option<VpnProfile> {
    let c = connection?;
    let kind = match c.str("type")? {
        "vpn" => VpnKind::Plugin,
        "wireguard" => VpnKind::WireGuard,
        _ => return None,
    };
    let uuid = c.str("uuid").filter(|u| valid_uuid(u))?.to_string();
    let mut name = text::clean(c.str("id").unwrap_or(""));
    if name.len() > NAME_MAX {
        let mut end = NAME_MAX;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    if name.is_empty() {
        name.push('?');
    }
    let last_used = c.int("timestamp").and_then(|t| u64::try_from(t).ok()).unwrap_or(0);
    Some(VpnProfile { uuid, name, kind, last_used, asks })
}

/// An active connection of the root's list, as a VPN: a plugin's (`Vpn` true) or a WireGuard
/// tunnel's (`Type` `wireguard`, `Vpn` false), with a uuid the panel may pass to nmcli.
fn active_vpn(a: &Props) -> Option<ActiveVpn> {
    let vpn = a.bool("Vpn") == Some(true) || matches!(a.str("Type"), Some("vpn" | "wireguard"));
    let uuid = a.str("Uuid").filter(|u| valid_uuid(u))?;
    vpn.then(|| ActiveVpn { uuid: uuid.to_string(), state: a.int("State").and_then(|n| u32::try_from(n).ok()).unwrap_or(0) })
}

/// The D-Bus thread's VPN reading for one session: profiles and active VPN connections, read only
/// while the quick panel is open, through NetworkManager's unique name. Closed, it is back to its
/// default: nothing kept. Secrets are never asked for.
#[derive(Debug, Default)]
pub struct VpnReader {
    scope: VpnScope,
    /// NetworkManager's unique name when the profiles below were read: another owner, other objects.
    owner: Option<String>,
    /// The profiles by connection path (at most MAX_SAVED); None for any other kind, or a
    /// connection that is gone or not this user's.
    profiles: BTreeMap<String, Option<VpnProfile>>,
    /// The connections must be listed again: a new open, a new owner, or a listing that stopped.
    stale: bool,
    /// The last state read.
    last: Option<VpnState>,
    /// One failed read is logged per open.
    logged: bool,
}

impl VpnReader {
    pub fn scope(&self) -> VpnScope {
        self.scope
    }

    /// The loop asked for `scope` (a poke). The same scope: read again. Another open one: the list
    /// is marked stale, and read. Closed: forgotten, nothing read. Err when the bus went away.
    pub fn rescope(&mut self, bus: &mut dyn Bus, scope: VpnScope) -> io::Result<Option<VpnState>> {
        if scope == self.scope && scope != VpnScope::Closed {
            return self.reread(bus);
        }
        if scope == VpnScope::Closed {
            *self = VpnReader::default();
            return Ok(None);
        }
        self.stale = true;
        self.scope = scope;
        self.reread(bus)
    }

    /// The state again: None when closed. A failed read keeps the last state, or an empty one
    /// before the first (logged once per open); the bus going away is an error.
    pub fn reread(&mut self, bus: &mut dyn Bus) -> io::Result<Option<VpnState>> {
        if self.scope == VpnScope::Closed {
            return Ok(None);
        }
        let s = match self.read(bus) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
            Err(e) => {
                self.note(&e);
                self.last.clone().unwrap_or_default()
            }
        };
        self.last = Some(s.clone());
        Ok(Some(s))
    }

    /// The error's kind only: never a name, a path or a message.
    fn note(&mut self, e: &io::Error) {
        if !std::mem::replace(&mut self.logged, true) {
            log(&format!("D-Bus VPN: read failed ({:?})", e.kind()));
        }
    }

    fn forget(&mut self) {
        self.owner = None;
        self.profiles.clear();
        self.stale = true;
    }

    fn read(&mut self, bus: &mut dyn Bus) -> io::Result<VpnState> {
        let Some(nm) = bus.owner(NM)? else {
            self.forget();
            return Ok(VpnState::default());
        };
        if self.owner.as_deref() != Some(nm.as_str()) {
            self.forget();
            self.owner = Some(nm.clone());
        }
        if self.stale || self.scope == VpnScope::Detail {
            // A hung listing is an error here: the read ends on it (the last state kept), so a hung
            // NetworkManager costs one deadline per read, not a second one on the actives.
            self.stale = true;
            self.stale = !self.list(bus, &nm)?;
        }
        let root = bus.get_all(&nm, network::PATH, network::INTERFACE)?;
        let mut active = Vec::new();
        for path in root.paths("ActiveConnections").iter().take(MAX_ACTIVE) {
            if let Some(a) = optional(bus.get_all(&nm, path, network::ACTIVE))?.as_ref().and_then(active_vpn) {
                active.push(a);
            }
        }
        Ok(VpnState { profiles: self.profiles.values().flatten().cloned().collect(), active })
    }

    /// The saved connections listed (the first MAX_SAVED): the ones gone dropped, the new ones read
    /// through GetSettings (never their secrets). True when the list is whole: no listing or a
    /// service gone midway leaves it stale; a call that failed otherwise (a timeout) is an error,
    /// the listing stopping there (that profile not cached).
    fn list(&mut self, bus: &mut dyn Bus, nm: &str) -> io::Result<bool> {
        let all = match bus.list_connections(nm) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
            Err(e) if e.kind() == io::ErrorKind::NotFound || is_service_gone(&e) => return Ok(false),
            Err(e) => return Err(e),
        };
        let listed = &all[..all.len().min(MAX_SAVED)];
        self.profiles.retain(|p, _| listed.contains(p));
        for p in listed {
            if self.profiles.contains_key(p) {
                continue;
            }
            match bus.get_settings(nm, p) {
                Ok(s) => {
                    self.profiles.insert(p.clone(), s.and_then(|s| if let Saved::Vpn(v) = s { Some(v) } else { None }));
                }
                Err(e) if is_service_gone(&e) => return Ok(false),
                Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied) => {
                    self.profiles.insert(p.clone(), None);
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
                // A call that failed otherwise (a timeout: NetworkManager hung): the listing stops,
                // so a hung service costs one deadline per read, not one per profile. The profile is
                // not cached and the list stays stale.
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
pub(super) mod tests;
