//! NetworkManager's Wi-Fi for the quick panel (quick Wi-Fi spec § 5), read only and only while the
//! panel is open: the radio, the first Wi-Fi device and the access point it is on
//! (`WifiScope::Row`); with the detail open, every access point and the saved Wi-Fi profiles too,
//! their signals followed through five match rules added and removed with it (`WifiScope::Detail`).
//! The loop asks through a `WifiTap`; the D-Bus thread reads here, through NetworkManager's unique
//! name as every reader does. Secrets are never asked for.
//!
//! Never:
//! - read while the panel is closed, or keep its match rules: both follow the scope, so a closed
//!   panel costs nothing (`at_rest_no_wakeups_after_the_wifi_detail`);
//! - name an SSID in a log line, or pass one to `nmcli` that it would misread
//!   (docs/decisions.md, "An SSID that nmcli would misread is never passed to it");
//! - panic on what NetworkManager answers (`the_panel_readers_have_no_panicking_path`);
//! - act: the connect, the rescan and the radio go through `nmcli`, from the panel.
//!
//! See: docs/features/quick-settings.md ("Wi-Fi"), docs/decisions.md ("A failed Wi-Fi attempt
//! deletes its new profile only when sure").

use super::dbusrs_bus::WIFI_RULES;
use super::props::Props;
pub use super::tap::WifiTap;
use super::{Bus, NM, is_service_gone, network, optional};
use crate::log::write as log;
use crate::system::{AccessPoint, Saved, SavedWifi, Security, WifiDevice, WifiScope, WifiState};
use std::collections::BTreeMap;
use std::io;

pub const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
pub const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
pub const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
/// NMDeviceType: Wi-Fi.
pub const DEVICE_TYPE_WIFI: i64 = 2;
/// GetDevices' answer is searched through this many devices at most.
pub const MAX_DEVICES: usize = 16;
/// An SSID's longest, in bytes (802.11).
pub const SSID_MAX: usize = 32;
const UUID_MAX: usize = 64;
/// The detail reads at most this many access points and saved connections (quick Wi-Fi spec § 6).
pub const MAX_APS: usize = 64;
pub const MAX_SAVED: usize = 64;
pub const AP_ROOT: &str = "/org/freedesktop/NetworkManager/AccessPoint";
pub const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
pub const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
pub const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";

/// What a detail signal asks to read again (`Signal::wifi_change`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiChange {
    /// One access point's properties.
    AccessPoint(String),
    /// The device's list of access points (AccessPointAdded, AccessPointRemoved).
    AccessPoints,
    /// The saved connections (NewConnection, ConnectionRemoved).
    Saved,
}

/// NetworkManager's uuid as the panel will pass it to nmcli, one argument: hexadecimal digits and
/// dashes only, never a leading dash, at most 64 bytes.
pub fn valid_uuid(s: &str) -> bool {
    (1..=UUID_MAX).contains(&s.len()) && !s.starts_with('-') && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// An access point's properties as the panel uses them; None: hidden (no SSID), or an SSID longer
/// than 802.11 allows. The strength is capped at 100.
pub fn access_point(p: &Props) -> Option<AccessPoint> {
    let ssid = p.bytes("Ssid")?;
    if ssid.is_empty() || ssid.len() > SSID_MAX {
        return None;
    }
    let flag = |k: &str| p.int(k).and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
    let strength = p.int("Strength").unwrap_or(0).clamp(0, 100) as u8;
    Some(AccessPoint { ssid: ssid.to_vec(), strength, security: Security::from_flags(flag("Flags"), flag("WpaFlags"), flag("RsnFlags")) })
}

/// A saved connection's `connection` and `802-11-wireless` settings, as a Wi-Fi profile: its uuid
/// (checked) and SSID (1 to 32 bytes); None for any other kind of connection.
pub fn saved_wifi(connection: Option<&Props>, wireless: Option<&Props>) -> Option<SavedWifi> {
    let c = connection?;
    if c.str("type") != Some("802-11-wireless") {
        return None;
    }
    let uuid = c.str("uuid").filter(|u| valid_uuid(u))?;
    let ssid = wireless?.bytes("ssid")?;
    (!ssid.is_empty() && ssid.len() <= SSID_MAX).then(|| SavedWifi { uuid: uuid.to_string(), ssid: ssid.to_vec() })
}

/// The D-Bus thread's Wi-Fi reading for one session: what it reads for the scope asked, and what it
/// remembers between two reads while the panel is open (NetworkManager's owner, the device's path).
/// Closed, it is back to its default: nothing kept.
#[derive(Debug, Default)]
pub struct WifiReader {
    scope: WifiScope,
    /// NetworkManager's unique name when the paths below were read: another owner, other objects.
    owner: Option<String>,
    /// The Wi-Fi device found this open.
    device: Option<String>,
    /// The access point the device is on, as last read (its change patches the row's `active`).
    active_ap: Option<String>,
    /// Detail only: the device's access points by path (at most MAX_APS), None for a hidden or
    /// unreadable one (read again only on its own signal).
    aps: BTreeMap<String, Option<AccessPoint>>,
    /// Detail only: the saved connections by path (at most MAX_SAVED), None for any other kind
    /// (or an unreadable one).
    saved: BTreeMap<String, Option<SavedWifi>>,
    /// The saved connections must be listed again: the detail opened, or one was added or removed.
    saved_stale: bool,
    /// The last listing held every saved connection (at most MAX_SAVED of them).
    saved_whole: bool,
    /// WIFI_RULES are on this connection.
    watching: bool,
    /// The last state read: one access point's change patches it.
    last: Option<WifiState>,
    /// One failed read is logged per open.
    logged: bool,
}

impl WifiReader {
    pub fn scope(&self) -> WifiScope {
        self.scope
    }

    /// The loop asked for `scope` (a poke). The same scope: read again, nothing added. Leaving the
    /// detail removes its rules and drops its lists; entering it adds the rules and reads both lists
    /// whole; Closed forgets everything and reads nothing. Err when the bus itself went away
    /// (`UnexpectedEof`), or a rule could not be removed (never tried again
    /// here, it would wake the thread for nothing; the session ends, and its reconnection drops
    /// every rule).
    pub fn rescope(&mut self, bus: &mut dyn Bus, scope: WifiScope) -> io::Result<Option<WifiState>> {
        if scope == self.scope && scope != WifiScope::Closed {
            return self.reread(bus);
        }
        let detail = scope == WifiScope::Detail;
        if self.watching && !detail {
            self.watching = false;
            bus.match_rules(&WIFI_RULES, false)?;
        }
        if scope == WifiScope::Closed {
            *self = WifiReader::default();
            return Ok(None);
        }
        if detail && !self.watching {
            let r = bus.match_rules(&WIFI_RULES, true);
            self.watching = r.is_ok();
            self.soft_unit(r)?;
        }
        self.aps.clear();
        self.saved.clear();
        self.saved_stale = detail;
        self.scope = scope;
        self.reread(bus)
    }

    /// The state again (a network signal, a list's signal, the machine resuming): None when closed.
    /// A failed read keeps the last state, or hides the row before the first (logged once per open);
    /// the bus going away is an error.
    pub fn reread(&mut self, bus: &mut dyn Bus) -> io::Result<Option<WifiState>> {
        if self.scope == WifiScope::Closed {
            return Ok(None);
        }
        let r = self.read(bus);
        let s = self.soft(r)?;
        self.last = Some(s.clone());
        Ok(Some(s))
    }

    /// A detail signal (quick Wi-Fi spec § 5): one access point read again (only one the device
    /// lists), the list of access points reconciled, or the saved connections listed again.
    /// Outside the detail: nothing read, the last state as it was.
    pub fn changed(&mut self, bus: &mut dyn Bus, c: &WifiChange) -> io::Result<Option<WifiState>> {
        if self.scope != WifiScope::Detail {
            return Ok(self.last.clone());
        }
        match c {
            WifiChange::AccessPoint(path) => {
                let Some(nm) = self.owner.clone().filter(|_| self.aps.contains_key(path)) else { return Ok(self.last.clone()) };
                let r = bus.get_all(&nm, path, ACCESS_POINT).map(Some);
                let ap = self.soft_item(r)?.as_ref().and_then(access_point);
                let active = self.active_ap.as_deref() == Some(path.as_str());
                self.aps.insert(path.clone(), ap.clone());
                let listed = self.listed();
                if let Some(d) = self.last.as_mut().and_then(|s| s.device.as_mut()) {
                    d.access_points = Some(listed);
                    // The device's own: the row's strength and glyph follow (kept when unreadable).
                    if active && ap.is_some() {
                        d.active = ap;
                    }
                }
                Ok(self.last.clone())
            }
            WifiChange::AccessPoints => self.reread(bus),
            WifiChange::Saved => {
                self.saved_stale = true;
                self.reread(bus)
            }
        }
    }

    /// A failed read (a timeout): the last state again, unchanged (no device
    /// reads as NetworkManager gone, and would end a followed attempt for nothing); none before the
    /// first. NetworkManager really gone reads as none without an error (no owner, no device).
    fn soft(&mut self, r: io::Result<WifiState>) -> io::Result<WifiState> {
        match r {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(e),
            Err(e) => {
                self.note(&e);
                Ok(self.last.clone().unwrap_or_default())
            }
        }
    }

    fn soft_unit(&mut self, r: io::Result<()>) -> io::Result<()> {
        match r {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(e),
            Err(e) => {
                self.note(&e);
                Ok(())
            }
        }
    }

    /// One item of a list (an access point, a saved connection) that could not be read: None for
    /// it, the rest of the detail kept; the bus going away is still an error.
    fn soft_item<T>(&mut self, r: io::Result<Option<T>>) -> io::Result<Option<T>> {
        match r {
            Ok(v) => Ok(v),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(e),
            Err(e) => {
                self.note(&e);
                Ok(None)
            }
        }
    }

    fn note(&mut self, e: &io::Error) {
        if !std::mem::replace(&mut self.logged, true) {
            log(&format!("D-Bus Wi-Fi: {}", crate::log::kind_word(e.kind())));
        }
    }

    /// Another NetworkManager (or none): every path read from the last one is someone else's now.
    fn forget(&mut self) {
        self.owner = None;
        self.device = None;
        self.active_ap = None;
        self.aps.clear();
        self.saved.clear();
        self.saved_stale = self.scope == WifiScope::Detail;
    }

    fn listed(&self) -> Vec<AccessPoint> {
        self.aps.values().flatten().cloned().collect()
    }

    fn read(&mut self, bus: &mut dyn Bus) -> io::Result<WifiState> {
        let Some(nm) = bus.owner(NM)? else {
            self.forget();
            return Ok(WifiState::default());
        };
        if self.owner.as_deref() != Some(nm.as_str()) {
            self.forget();
            self.owner = Some(nm.clone());
        }
        let root = bus.get_all(&nm, network::PATH, network::INTERFACE)?;
        let enabled = root.bool("WirelessEnabled").unwrap_or(false);
        let Some((path, device)) = self.device(bus, &nm)? else { return Ok(WifiState { enabled, device: None, stale: false }) };
        let wireless = optional(bus.get_all(&nm, &path, WIRELESS))?.unwrap_or_default();
        self.active_ap = wireless.path("ActiveAccessPoint").map(String::from);
        let active = match wireless.path("ActiveAccessPoint") {
            Some(ap) => optional(bus.get_all(&nm, ap, ACCESS_POINT))?.as_ref().and_then(access_point),
            None => None,
        };
        let active_uuid = match device.path("ActiveConnection") {
            Some(ac) => {
                optional(bus.get_all(&nm, ac, network::ACTIVE))?.and_then(|a| a.str("Uuid").filter(|u| valid_uuid(u)).map(String::from))
            }
            None => None,
        };
        let (access_points, saved) = if self.scope == WifiScope::Detail {
            self.reconcile_aps(bus, &nm, wireless.paths("AccessPoints"))?;
            if self.saved_stale {
                // Listing failing (not absent) keeps the row and the access points; the saved list
                // is listed again at the next read.
                match self.reconcile_saved(bus, &nm) {
                    Ok(whole) => self.saved_stale = !whole,
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
                    Err(e) => self.note(&e),
                }
            }
            // Only a whole, current list is sent: the panel takes the profiles it holds when an
            // attempt begins as the ones never to delete, so a partial one (not listed yet, a failed
            // listing, a profile that could not be read, more than MAX_SAVED) is None, and then
            // nothing is ever deleted. The reader must keep its lists when the panel hands an
            // attempt to its follower (`WifiFollow` in `quick/app.rs`): a reset would read a profile skipped
            // before as new.
            let whole = !self.saved_stale && self.saved_whole;
            (Some(self.listed()), whole.then(|| self.saved.values().flatten().cloned().collect()))
        } else {
            (None, None)
        };
        let state = device.int("State").and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        Ok(WifiState { enabled, device: Some(WifiDevice { state, active, active_uuid, access_points, saved }), stale: false })
    }

    /// The Wi-Fi device and its `Device` properties: the one found this open while it still is
    /// one, else the first of GetDevices' answer (at most MAX_DEVICES) whose DeviceType is Wi-Fi.
    fn device(&mut self, bus: &mut dyn Bus, nm: &str) -> io::Result<Option<(String, Props)>> {
        if let Some(path) = self.device.clone() {
            if let Some(d) = optional(bus.get_all(nm, &path, DEVICE))?
                && d.int("DeviceType") == Some(DEVICE_TYPE_WIFI)
            {
                return Ok(Some((path, d)));
            }
            self.device = None;
        }
        let paths = match bus.get_devices(nm) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        for path in paths.into_iter().take(MAX_DEVICES) {
            if let Some(d) = optional(bus.get_all(nm, &path, DEVICE))?
                && d.int("DeviceType") == Some(DEVICE_TYPE_WIFI)
            {
                self.device = Some(path.clone());
                return Ok(Some((path, d)));
            }
        }
        Ok(None)
    }

    /// The access points the device lists now (the first MAX_APS): the ones gone dropped, the new ones
    /// read; the others kept as their own signals left them.
    fn reconcile_aps(&mut self, bus: &mut dyn Bus, nm: &str, listed: &[String]) -> io::Result<()> {
        let listed = &listed[..listed.len().min(MAX_APS)];
        self.aps.retain(|p, _| listed.contains(p));
        for p in listed {
            if !self.aps.contains_key(p) {
                let r = bus.get_all(nm, p, ACCESS_POINT).map(Some);
                let ap = self.soft_item(r)?;
                self.aps.insert(p.clone(), ap.as_ref().and_then(access_point));
            }
        }
        Ok(())
    }

    /// The saved connections listed (the first MAX_SAVED): the ones gone dropped, the new ones read
    /// through GetSettings (never their secrets). True when every one was read: one that failed
    /// (not gone, not another kind, not refused to this user) is left out of the map, so the next
    /// listing reads it again, and the list stays unknown until then. No listing, or NetworkManager
    /// gone midway, is not a whole list either: an empty one would let a
    /// profile saved before be taken for an attempt's.
    fn reconcile_saved(&mut self, bus: &mut dyn Bus, nm: &str) -> io::Result<bool> {
        let all = match bus.list_connections(nm) {
            Ok(p) => p,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        self.saved_whole = all.len() <= MAX_SAVED;
        let listed = &all[..all.len().min(MAX_SAVED)];
        self.saved.retain(|p, _| listed.contains(p));
        let mut whole = true;
        for p in listed {
            if !self.saved.contains_key(p) {
                match bus.get_settings(nm, p) {
                    Ok(s) => {
                        self.saved.insert(p.clone(), s.and_then(|s| if let Saved::Wifi(w) = s { Some(w) } else { None }));
                    }
                    Err(e) if is_service_gone(&e) => return Ok(false),
                    // Gone since the listing, or another user's: not ours.
                    Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied) => {
                        self.saved.insert(p.clone(), None);
                    }
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(e),
                    Err(e) => {
                        self.note(&e);
                        whole = false;
                    }
                }
            }
        }
        Ok(whole)
    }
}

#[cfg(test)]
mod tests;
