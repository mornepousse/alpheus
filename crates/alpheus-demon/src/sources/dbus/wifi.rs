//! NetworkManager's Wi-Fi for the quick panel (quick Wi-Fi spec § 5), read only and only while the
//! panel is open: the radio, the first Wi-Fi device and the access point it is on
//! (`WifiScope::Row`); with the detail open, every access point and the saved Wi-Fi profiles too,
//! their signals followed through five match rules added and removed with it (`WifiScope::Detail`).
//! The loop asks through a `WifiTap`; the D-Bus thread reads here, through NetworkManager's unique
//! name as every reader does. Secrets are never asked for.

use super::dbusrs_bus::WIFI_RULES;
use super::props::Props;
use super::{Bus, NM, is_service_gone, network, optional};
use crate::log::write as log;
use crate::system::{AccessPoint, SavedWifi, Security, WifiDevice, WifiScope, WifiState};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, PoisonError};

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

/// What the loop asks the D-Bus thread (quick Wi-Fi decision 1): the scope the open panel shows,
/// and a socket pair whose first end the thread polls beside the bus's own socket; the loop writes
/// one byte to the second when the scope changes, never otherwise (no wake-up at rest). Without the
/// pair (`default()`, the tests' tap) the scope is still kept, and nothing wakes: a session reads it
/// when it starts and on each `Signal::poke`. The tap also counts the panel's opens (Closed to any
/// other scope): a close and a reopen the thread reads as one poke are still a new open, and a
/// state read for an earlier open is told apart from the current one's.
#[derive(Debug, Default)]
pub struct WifiTap {
    /// The scope asked, and the opens so far.
    scope: Mutex<(WifiScope, u64)>,
    wake: Option<(UnixStream, UnixStream)>,
}

impl WifiTap {
    /// A tap with its socket pair, both ends non-blocking and close-on-exec.
    pub fn new() -> io::Result<WifiTap> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(WifiTap { scope: Mutex::new((WifiScope::Closed, 0)), wake: Some((read, write)) })
    }

    pub fn scope(&self) -> WifiScope {
        self.snapshot().0
    }

    /// The scope asked and the opens so far, read together.
    pub fn snapshot(&self) -> (WifiScope, u64) {
        *self.scope.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The loop asks for `scope`: kept, and the thread woken when it changed (a byte already waiting
    /// is enough: a full socket is not an error); from Closed to another scope, one more open. True
    /// when it changed.
    pub fn set(&self, scope: WifiScope) -> bool {
        let changed = {
            let mut s = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = s.0 != scope;
            if s.0 == WifiScope::Closed && scope != WifiScope::Closed {
                s.1 = s.1.wrapping_add(1);
            }
            s.0 = scope;
            changed
        };
        if changed && let Some((_, w)) = &self.wake {
            let _ = (&*w).write(&[1]);
        }
        changed
    }

    /// The end the D-Bus thread polls; None for the tests' tap.
    pub fn wake_fd(&self) -> Option<RawFd> {
        self.wake.as_ref().map(|(r, _)| r.as_raw_fd())
    }

    /// Reads every byte waiting, so the poll sleeps again.
    pub fn drain(&self) {
        if let Some((r, _)) = &self.wake {
            let mut buf = [0u8; 64];
            while matches!((&*r).read(&mut buf), Ok(n) if n > 0) {}
        }
    }
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
    /// (`UnexpectedEof`), or a rule could not be removed (final review A-M4: never tried again
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

    /// A failed read (a timeout): the last state again, unchanged (final review A-M3: no device
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
    /// it, the rest of the detail kept (ruling P4); the bus going away is still an error.
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
            log(&format!("D-Bus Wi-Fi: {e}"));
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
        let Some((path, device)) = self.device(bus, &nm)? else { return Ok(WifiState { enabled, device: None }) };
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
            // attempt to its follower (quick_app::WifiFollow): a reset would read a profile skipped
            // before as new.
            let whole = !self.saved_stale && self.saved_whole;
            (Some(self.listed()), whole.then(|| self.saved.values().flatten().cloned().collect()))
        } else {
            (None, None)
        };
        let state = device.int("State").and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        Ok(WifiState { enabled, device: Some(WifiDevice { state, active, active_uuid, access_points, saved }) })
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
    /// gone midway, is not a whole list either (final review A-M1): an empty one would let a
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
                        self.saved.insert(p.clone(), s);
                    }
                    Err(e) if is_service_gone(&e) => return Ok(false),
                    // Gone since the listing, or another user's (final review A-M2): not ours.
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
mod tests {
    use super::*;
    use crate::sources::dbus::fake::{AP, FakeBus, NM_OWNER, WIFI_DEVICE, WIRED_DEVICE};
    use crate::sources::dbus::props::Prop;

    /// The recorded Wi-Fi connection's uuid (dbus-nm-active-9.json).
    const RECORDED_UUID: &str = "00000000-0000-0000-0000-000000000009";

    fn wifi_bus() -> FakeBus {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        bus
    }

    fn row(bus: &mut FakeBus) -> WifiState {
        WifiReader::default().rescope(bus, WifiScope::Row).unwrap().expect("open: a state")
    }

    #[test]
    fn the_row_reads_the_radio_the_device_and_its_access_point() {
        let mut bus = wifi_bus();
        let home = AccessPoint { ssid: b"MON_WIFI_1".to_vec(), strength: 70, security: Security::Psk };
        let device =
            WifiDevice { state: 100, active: Some(home), active_uuid: Some(RECORDED_UUID.into()), access_points: None, saved: None };
        assert_eq!(row(&mut bus), WifiState { enabled: true, device: Some(device) });
        let get_all: Vec<&String> = bus.calls.iter().filter(|c| c.starts_with("GetAll ")).collect();
        assert!(get_all.iter().any(|c| c.contains(WIRED_DEVICE)), "the wired device read, then passed over: {get_all:?}");
        assert!(get_all.iter().any(|c| c.ends_with(&format!("{AP}/10 {ACCESS_POINT}"))), "{get_all:?}");
        assert!(
            !get_all.iter().any(|c| c.contains(&format!("{AP}/11")) || c.contains(&format!("{AP}/12"))),
            "the row reads its own access point only: {get_all:?}"
        );
        for c in &bus.calls {
            assert!(c.starts_with("GetNameOwner ") || c.split(' ').nth(1) == Some(NM_OWNER), "{c}: only NetworkManager's unique name");
        }
    }

    #[test]
    fn the_device_is_found_once_per_open_then_kept() {
        let mut bus = wifi_bus();
        let mut r = WifiReader::default();
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        r.reread(&mut bus).unwrap();
        r.reread(&mut bus).unwrap();
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "{:?}", bus.calls);
        assert_eq!(bus.calls.iter().filter(|c| c.contains(WIRED_DEVICE)).count(), 1, "the wired one passed over once");
        r.rescope(&mut bus, WifiScope::Closed).unwrap();
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 2, "a new open finds it again");
    }

    #[test]
    fn closed_reads_nothing_and_keeps_nothing() {
        let mut bus = wifi_bus();
        let mut r = WifiReader::default();
        assert_eq!(r.reread(&mut bus).unwrap(), None);
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        let calls = bus.calls.len();
        assert_eq!(r.rescope(&mut bus, WifiScope::Closed).unwrap(), None);
        assert_eq!(r.reread(&mut bus).unwrap(), None);
        assert_eq!(bus.calls.len(), calls, "closed: not one call");
        assert_eq!(r.scope(), WifiScope::Closed);
    }

    #[test]
    fn no_networkmanager_no_wifi_device_or_the_radio_off() {
        let mut bus = FakeBus::empty();
        assert_eq!(row(&mut bus), WifiState::default(), "no NetworkManager: the row hidden");
        assert_eq!(bus.calls, ["GetNameOwner org.freedesktop.NetworkManager"], "never a call to a name nobody owns");
        let mut bus = FakeBus::workstation();
        assert_eq!(row(&mut bus), WifiState { enabled: true, device: None }, "GetDevices unknown (the recording): no device");
        let mut bus = wifi_bus();
        bus.props_mut(WIFI_DEVICE, DEVICE).0.insert("DeviceType".into(), Prop::Int(1));
        assert_eq!(row(&mut bus).device, None, "a wired device only: no Wi-Fi row");
        let mut bus = wifi_bus();
        bus.props_mut(super::super::network::PATH, super::super::network::INTERFACE).0.insert("WirelessEnabled".into(), Prop::Bool(false));
        assert!(!row(&mut bus).enabled, "the radio off");
    }

    #[test]
    fn a_failed_read_hides_the_row_and_a_lost_bus_is_an_error() {
        let mut bus = wifi_bus();
        bus.failing.insert(WIFI_DEVICE.into());
        assert_eq!(row(&mut bus), WifiState::default(), "a timeout: the row hidden, not an outage of the network module");
        let mut bus = wifi_bus();
        bus.disconnecting.insert(WIFI_DEVICE.into());
        let e = WifiReader::default().rescope(&mut bus, WifiScope::Row).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "the bus itself gone: the session's to handle");
    }

    #[test]
    fn an_access_point_is_read_bounded() {
        let p = |ssid: &[u8], strength: i64, rsn: i64| {
            Props(
                [
                    ("Ssid".to_string(), Prop::Bytes(ssid.to_vec())),
                    ("Strength".to_string(), Prop::Int(strength)),
                    ("Flags".to_string(), Prop::Int(1)),
                    ("RsnFlags".to_string(), Prop::Int(rsn)),
                ]
                .into(),
            )
        };
        assert_eq!(
            access_point(&p(b"Cafe-Libre", 55, 0)).map(|a| (a.strength, a.security)),
            Some((55, Security::Other)),
            "privacy, no key: WEP"
        );
        assert_eq!(
            access_point(&p(&[b'x'; 32], 250, 0x188)).map(|a| (a.ssid.len(), a.strength)),
            Some((32, 100)),
            "32 bytes kept; strength capped"
        );
        assert_eq!(access_point(&p(&[b'x'; 33], 50, 0)), None, "longer than 802.11 allows");
        assert_eq!(access_point(&p(b"", 50, 0)), None, "hidden");
        assert_eq!(access_point(&Props::default()), None);
    }

    #[test]
    fn security_from_the_flags() {
        assert_eq!(Security::from_flags(0, 0, 0), Security::Open);
        assert_eq!(Security::from_flags(1, 0, 0x188), Security::Psk, "WPA2 personal");
        assert_eq!(Security::from_flags(1, 0x148, 0), Security::Psk, "WPA personal");
        assert_eq!(Security::from_flags(1, 0, 0x400 | 0x8), Security::Psk, "WPA3 SAE");
        assert_eq!(Security::from_flags(1, 0, 0x200 | 0x8), Security::Enterprise, "802.1X");
        assert_eq!(Security::from_flags(1, 0, 0x300), Security::Enterprise, "802.1X wins over PSK: a password field would not do");
        assert_eq!(Security::from_flags(1, 0, 0x2000), Security::Enterprise, "Suite B 192");
        assert_eq!(Security::from_flags(0, 0, 0x800), Security::Open, "OWE: no password");
        assert_eq!(Security::from_flags(1, 0, 0), Security::Other, "WEP");
        assert!(!Security::Open.secured() && Security::Psk.secured() && Security::Enterprise.secured() && Security::Other.secured());
    }

    #[test]
    fn uuids_are_checked_before_any_use() {
        assert!(valid_uuid("11111111-2222-4333-8444-555555555555"));
        assert!(valid_uuid(RECORDED_UUID));
        for bad in ["", "-1111", "1111 2222", "1111;rm", "g111", &"a".repeat(65)] {
            assert!(!valid_uuid(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_tap_wakes_only_on_a_change_and_drains() {
        let tap = WifiTap::new().unwrap();
        let fd = tap.wake_fd().expect("a socket pair");
        let readable = || {
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: poll(2) on one pollfd owned here, no wait.
            unsafe { libc::poll(&mut p, 1, 0) == 1 }
        };
        assert_eq!(tap.scope(), WifiScope::Closed);
        assert!(!readable());
        assert!(tap.set(WifiScope::Row));
        assert!(readable(), "a change wakes the thread");
        tap.drain();
        assert!(!readable(), "drained");
        assert!(!tap.set(WifiScope::Row), "the same scope: no word");
        assert!(!readable());
        assert_eq!(tap.scope(), WifiScope::Row);
        // SAFETY: fcntl(2) F_GETFD only reads the descriptor's flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "a child would inherit the tap");
        let idle = WifiTap::default();
        assert!(idle.set(WifiScope::Detail) && idle.wake_fd().is_none(), "the tests' tap: a scope, no socket");
    }

    /// Review Focus 1, the row's part: NetworkManager restarting under a new unique name while the
    /// panel is open: the device's path, an object of the old instance, is dropped and found again.
    #[test]
    fn a_new_networkmanager_owner_finds_the_device_again() {
        let mut bus = wifi_bus();
        let mut r = WifiReader::default();
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        r.reread(&mut bus).unwrap();
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "{:?}", bus.calls);
        bus.owners.insert(NM, ":1.20");
        bus.calls.clear();
        assert!(r.reread(&mut bus).unwrap().is_some_and(|w| w.device.is_some()));
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "the new owner asked again: {:?}", bus.calls);
        for c in &bus.calls {
            assert!(c.starts_with("GetNameOwner ") || c.split(' ').nth(1) == Some(":1.20"), "{c}: only the new owner");
        }
    }

    /// Final review A-M3: a read that fails (a timeout) once the device is known keeps the last
    /// state, unchanged, so a followed attempt never takes it for NetworkManager gone; NetworkManager
    /// really gone (no owner) or no Wi-Fi device any more still reads as no device.
    #[test]
    fn a_failed_reread_keeps_the_last_state_a_real_absence_does_not() {
        let mut bus = wifi_bus();
        let mut r = WifiReader::default();
        let first = r.rescope(&mut bus, WifiScope::Row).unwrap();
        assert!(first.as_ref().is_some_and(|s| s.device.is_some()));
        bus.failing.insert(WIFI_DEVICE.into());
        assert_eq!(r.reread(&mut bus).unwrap(), first, "a timeout: the last state, no change");
        bus.failing.clear();
        bus.owners.remove(NM);
        assert_eq!(r.reread(&mut bus).unwrap(), Some(WifiState::default()), "no owner: gone");
        let mut bus = wifi_bus();
        let mut r = WifiReader::default();
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        bus.props_mut(WIFI_DEVICE, DEVICE).0.insert("DeviceType".into(), Prop::Int(1));
        assert_eq!(r.reread(&mut bus).unwrap().map(|s| s.device), Some(None), "no Wi-Fi device any more: none");
    }

    /// The flag that keeps the `D-Bus Wi-Fi:` line to one per open: raised by a failure, kept for
    /// the rest of the open, lowered on close (the log call itself is not captured here).
    #[test]
    fn the_failure_flag_is_raised_once_and_lowered_on_close() {
        let mut bus = wifi_bus();
        bus.failing.insert(WIFI_DEVICE.into());
        let mut r = WifiReader::default();
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        assert!(r.logged, "the first failure logged");
        bus.failing.clear();
        r.reread(&mut bus).unwrap();
        assert!(r.logged, "still this open: no second line");
        r.rescope(&mut bus, WifiScope::Closed).unwrap();
        assert!(!r.logged, "closed: the next open may log");
        bus.failing.insert(WIFI_DEVICE.into());
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        assert!(r.logged);
    }

    fn detail(bus: &mut FakeBus) -> (WifiReader, WifiDevice) {
        let mut r = WifiReader::default();
        let s = r.rescope(bus, WifiScope::Detail).unwrap().expect("open: a state");
        (r, s.device.expect("the Wi-Fi device"))
    }

    fn ssids(d: &WifiDevice) -> Vec<String> {
        d.access_points.iter().flatten().map(|a| String::from_utf8_lossy(&a.ssid).into_owned()).collect()
    }

    #[test]
    fn the_detail_adds_its_rules_and_reads_both_lists() {
        let mut bus = wifi_bus();
        let (_, d) = detail(&mut bus);
        assert_eq!(ssids(&d), ["MON_WIFI_1", "Voisin-Test", "Cafe-Libre"], "the hidden one left out");
        assert_eq!(
            d.saved,
            Some(vec![SavedWifi { uuid: RECORDED_UUID.into(), ssid: b"MON_WIFI_1".to_vec() }]),
            "the wired profile left out"
        );
        let added: Vec<&String> = bus.calls.iter().filter(|c| c.starts_with("AddMatch ")).collect();
        assert_eq!(added.len(), WIFI_RULES.len(), "{added:?}");
        for n in 11..=13 {
            assert_eq!(bus.calls.iter().filter(|c| c.contains(&format!("{AP}/{n} "))).count(), 1, "AccessPoint/{n} read once");
        }
        assert_eq!(
            bus.calls.iter().filter(|c| c.contains(&format!("{AP}/10 "))).count(),
            2,
            "the device's own: as the row's, and in the list"
        );
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetSettings ")).count(), 2);
        assert!(bus.calls.iter().all(|c| !c.contains("Secret")), "no secret asked: {:?}", bus.calls);
    }

    #[test]
    fn leaving_the_detail_removes_its_rules_and_drops_the_lists() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        let row = r.rescope(&mut bus, WifiScope::Row).unwrap().unwrap().device.unwrap();
        assert_eq!((row.access_points, row.saved), (None, None));
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("RemoveMatch ")).count(), WIFI_RULES.len());
        r.rescope(&mut bus, WifiScope::Detail).unwrap();
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("AddMatch ")).count(), 2 * WIFI_RULES.len(), "added again");
        assert_eq!(bus.calls.iter().filter(|c| c.contains(&format!("{AP}/11 "))).count(), 2, "read again: nothing kept between");
        r.rescope(&mut bus, WifiScope::Closed).unwrap();
        assert_eq!(
            bus.calls.iter().filter(|c| c.starts_with("RemoveMatch ")).count(),
            2 * WIFI_RULES.len(),
            "closing from the detail removes them too"
        );
        let calls = bus.calls.len();
        r.rescope(&mut bus, WifiScope::Detail).unwrap();
        let again = bus.calls.len();
        r.rescope(&mut bus, WifiScope::Detail).unwrap();
        assert_eq!(
            bus.calls[calls..].iter().filter(|c| c.starts_with("AddMatch ")).count(),
            WIFI_RULES.len(),
            "a poke for the same scope adds nothing"
        );
        let kept = |c: &&String| c.starts_with("ListConnections ") || c.starts_with("GetSettings ") || c.contains(&format!("{AP}/11 "));
        assert_eq!(bus.calls[again..].iter().filter(kept).count(), 0, "nor reads the lists again: {:?}", &bus.calls[again..]);
    }

    #[test]
    fn one_access_points_change_reads_that_access_point_only() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.props_mut(&format!("{AP}/11"), ACCESS_POINT).0.insert("Strength".into(), Prop::Int(90));
        let calls = bus.calls.len();
        let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/11"))).unwrap().unwrap().device.unwrap();
        assert_eq!(bus.calls[calls..], [format!("GetAll {NM_OWNER} {AP}/11 {ACCESS_POINT}")]);
        assert_eq!(d.access_points.unwrap().iter().find(|a| a.ssid == b"Voisin-Test").map(|a| a.strength), Some(90));
        let calls = bus.calls.len();
        r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/99"))).unwrap();
        assert_eq!(bus.calls.len(), calls, "an access point the device does not list: nothing read");
    }

    /// Outside the detail its signals read nothing (their rules are off; one still in flight when
    /// the detail closed is ignored).
    #[test]
    fn detail_changes_outside_the_detail_read_nothing() {
        let mut bus = wifi_bus();
        let mut r = WifiReader::default();
        for scope in [WifiScope::Closed, WifiScope::Row] {
            r.rescope(&mut bus, scope).unwrap();
            let calls = bus.calls.len();
            for c in [WifiChange::AccessPoint(format!("{AP}/11")), WifiChange::AccessPoints, WifiChange::Saved] {
                r.changed(&mut bus, &c).unwrap();
            }
            assert_eq!(bus.calls[calls..], [] as [String; 0], "{scope:?}: nothing read");
        }
    }

    #[test]
    fn added_and_removed_access_points_are_reconciled() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.props.insert(
            (format!("{AP}/14"), ACCESS_POINT.into()),
            Props([("Ssid".to_string(), Prop::Bytes(b"Labo-Test".to_vec())), ("Strength".to_string(), Prop::Int(40))].into()),
        );
        let listed: Vec<String> = [10, 11, 14].iter().map(|n| format!("{AP}/{n}")).collect();
        bus.props_mut(WIFI_DEVICE, WIRELESS).0.insert("AccessPoints".into(), Prop::Paths(listed));
        let calls = bus.calls.len();
        let d = r.changed(&mut bus, &WifiChange::AccessPoints).unwrap().unwrap().device.unwrap();
        assert_eq!(ssids(&d), ["MON_WIFI_1", "Voisin-Test", "Labo-Test"], "12 gone, 14 come");
        let aps: Vec<&String> = bus.calls[calls..].iter().filter(|c| c.ends_with(ACCESS_POINT)).collect();
        assert_eq!(aps.len(), 2, "the new one, and the device's own access point (the row's): {aps:?}");
    }

    #[test]
    fn saved_profiles_are_read_on_their_signal_only() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        r.reread(&mut bus).unwrap();
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("ListConnections ")).count(), 1, "a network re-read does not list them again");
        bus.connections.as_mut().unwrap().push("/org/freedesktop/NetworkManager/Settings/5".into());
        bus.settings.insert(
            "/org/freedesktop/NetworkManager/Settings/5".into(),
            Some(SavedWifi { uuid: "11111111-2222-4333-8444-555555555555".into(), ssid: b"Voisin-Test".to_vec() }),
        );
        let calls = bus.calls.len();
        let d = r.changed(&mut bus, &WifiChange::Saved).unwrap().unwrap().device.unwrap();
        assert_eq!(d.saved.map(|s| s.len()), Some(2));
        let settings: Vec<&String> = bus.calls[calls..].iter().filter(|c| c.starts_with("GetSettings ")).collect();
        assert_eq!(settings.len(), 1, "the new one only: {settings:?}");
    }

    #[test]
    fn the_lists_are_capped() {
        let mut bus = wifi_bus();
        let mut listed = Vec::new();
        for n in 100..170 {
            let path = format!("{AP}/{n}");
            let ssid = format!("Reseau-Test-{n}");
            bus.props.insert((path.clone(), ACCESS_POINT.into()), Props([("Ssid".to_string(), Prop::Bytes(ssid.into_bytes()))].into()));
            listed.push(path);
        }
        bus.props_mut(WIFI_DEVICE, WIRELESS).0.insert("AccessPoints".into(), Prop::Paths(listed));
        let connections: Vec<String> = (100..170).map(|n| format!("/org/freedesktop/NetworkManager/Settings/{n}")).collect();
        for c in &connections {
            bus.settings.insert(c.clone(), None);
        }
        bus.connections = Some(connections);
        let (_, d) = detail(&mut bus);
        assert_eq!(d.access_points.map(|a| a.len()), Some(MAX_APS));
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetSettings ")).count(), MAX_SAVED);
        assert_eq!(d.saved, None, "more than MAX_SAVED: not the whole list, so unknown");
    }

    /// Ruling P4: one access point or one profile that fails to read (a timeout) is left out; the
    /// rest of the detail stays, and the failure is logged once per open.
    #[test]
    fn one_unreadable_item_is_left_out_never_the_whole_detail() {
        let mut bus = wifi_bus();
        bus.failing.insert(format!("{AP}/11"));
        let voisin = "/org/freedesktop/NetworkManager/Settings/5";
        bus.connections.as_mut().unwrap().push(voisin.into());
        bus.settings
            .insert(voisin.into(), Some(SavedWifi { uuid: "11111111-2222-4333-8444-555555555555".into(), ssid: b"Voisin-Test".to_vec() }));
        bus.failing.insert("/org/freedesktop/NetworkManager/Settings/3".into());
        let (mut r, d) = detail(&mut bus);
        assert_eq!(ssids(&d), ["MON_WIFI_1", "Cafe-Libre"], "Voisin-Test left out, the others kept");
        assert_eq!(d.saved, None, "a profile unread: the list unknown (fix round 2), so nothing is ever deleted");
        assert!(d.active.is_some(), "the row's part kept");
        assert!(r.logged, "the failure noted");
        bus.failing.clear();
        let d = r.reread(&mut bus).unwrap().unwrap().device.unwrap();
        let saved: Vec<Vec<u8>> = d.saved.expect("read whole at last").into_iter().map(|s| s.ssid).collect();
        assert!(saved.contains(&b"Voisin-Test".to_vec()) && saved.len() == 2, "the unread one read again: {saved:?}");
        let mut bus = wifi_bus();
        bus.disconnecting.insert(format!("{AP}/11"));
        let e = WifiReader::default().rescope(&mut bus, WifiScope::Detail).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "the bus itself gone: still the session's");
    }

    /// Final review A-M1: NetworkManager gone while the saved profiles are listed or read
    /// (ServiceUnknown), or ListConnections not there: the list is unknown, never empty, so a
    /// profile saved before is never taken for an attempt's. One profile gone (UnknownObject) is
    /// skipped, the list still whole.
    #[test]
    fn networkmanager_gone_while_listing_leaves_the_saved_list_unknown() {
        let mut bus = wifi_bus();
        bus.gone.insert(SETTINGS_PATH.into());
        assert_eq!(detail(&mut bus).1.saved, None, "ListConnections: ServiceUnknown");
        let mut bus = wifi_bus();
        bus.connections = None;
        assert_eq!(detail(&mut bus).1.saved, None, "ListConnections: UnknownMethod");
        let mut bus = wifi_bus();
        bus.gone.insert("/org/freedesktop/NetworkManager/Settings/3".into());
        let (mut r, d) = detail(&mut bus);
        assert_eq!(d.saved, None, "GetSettings: ServiceUnknown");
        bus.gone.clear();
        assert_eq!(r.reread(&mut bus).unwrap().unwrap().device.unwrap().saved.map(|s| s.len()), Some(1), "listed again");
        let mut bus = wifi_bus();
        bus.settings.remove("/org/freedesktop/NetworkManager/Settings/4");
        let saved = detail(&mut bus).1.saved.expect("whole");
        assert_eq!(saved.len(), 1, "the wired one gone (UnknownObject): skipped, the list whole");
    }

    /// Final review A-M2: a profile NetworkManager will not show this user (PermissionDenied) is
    /// not ours to delete: skipped, the list still whole, nothing logged.
    #[test]
    fn a_profile_refused_to_this_user_is_skipped() {
        let mut bus = wifi_bus();
        bus.denied.insert("/org/freedesktop/NetworkManager/Settings/4".into());
        let (r, d) = detail(&mut bus);
        assert_eq!(d.saved.map(|s| s.len()), Some(1), "whole, the refused one left out");
        assert!(!r.logged, "not a failure");
    }

    /// Per rule, how many times the bus holds it for this session.
    fn rules_left(bus: &FakeBus) -> Vec<usize> {
        WIFI_RULES.iter().map(|r| bus.rules.iter().filter(|h| h == r).count()).collect()
    }

    /// Fix round 1: one rule refused (a timeout, LimitsExceeded) takes back the ones already added:
    /// none left on the bus to wake the thread once the panel closes, none doubled at the next open.
    #[test]
    fn a_partly_added_set_of_rules_is_taken_back() {
        let mut bus = wifi_bus();
        bus.failing_rules.insert((true, WIFI_RULES[3].to_string()));
        let (mut r, d) = detail(&mut bus);
        assert_eq!(rules_left(&bus), [0; 5], "{:?}", bus.calls);
        assert!(d.access_points.is_some_and(|a| !a.is_empty()), "the detail still read once");
        assert!(r.logged, "the refusal noted");
        r.rescope(&mut bus, WifiScope::Row).unwrap();
        assert_eq!(rules_left(&bus), [0; 5], "nothing removed twice");
        bus.failing_rules.clear();
        r.rescope(&mut bus, WifiScope::Detail).unwrap();
        assert_eq!(rules_left(&bus), [1; 5], "added whole at the next open");
        r.rescope(&mut bus, WifiScope::Closed).unwrap();
        assert_eq!(rules_left(&bus), [0; 5]);
    }

    /// Fix round 1: one RemoveMatch refused does not keep the others on the bus; final review A-M4:
    /// the refused one is never tried again here, so the error ends the session (a reconnection
    /// drops every rule).
    #[test]
    fn every_rule_is_removed_even_after_one_fails() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.failing_rules.insert((false, WIFI_RULES[0].to_string()));
        let e = r.rescope(&mut bus, WifiScope::Row).unwrap_err();
        assert_ne!(e.kind(), io::ErrorKind::NotFound, "{e}");
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("RemoveMatch ")).count(), WIFI_RULES.len(), "every rule tried");
        assert_eq!(rules_left(&bus), [1, 0, 0, 0, 0], "only the refused one left");
    }

    /// Final review A-M4: an AddMatch whose reply was lost may still have been applied, so the
    /// rollback removes it too; a rule the bus never took answers MatchRuleNotFound, ignored. A
    /// rule already gone when the detail closes (MatchRuleNotFound) is not a failure either.
    #[test]
    fn a_timed_out_rule_is_rolled_back_too() {
        let mut bus = wifi_bus();
        bus.late_rules.insert(WIFI_RULES[2].to_string());
        let (mut r, _) = detail(&mut bus);
        assert_eq!(rules_left(&bus), [0; 5], "{:?}", bus.calls);
        r.rescope(&mut bus, WifiScope::Closed).unwrap();
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.rules.retain(|h| h != WIFI_RULES[1]);
        assert!(r.rescope(&mut bus, WifiScope::Row).is_ok(), "already gone: what removing asks");
        assert_eq!(rules_left(&bus), [0; 5]);
    }

    /// Fix round 1 (P4): an access point whose re-read fails is left out, like at the first read.
    #[test]
    fn an_access_points_failed_reread_leaves_it_out() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.failing.insert(format!("{AP}/11"));
        let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/11"))).unwrap().unwrap().device.unwrap();
        assert_eq!(ssids(&d), ["MON_WIFI_1", "Cafe-Libre"], "Voisin-Test left out, never its old strength");
        assert!(d.active.is_some(), "the rest kept");
    }

    /// Fix round 1: the device's own access point changing reaches the row too (its strength, its
    /// glyph), not only the list.
    #[test]
    fn the_active_access_points_change_reaches_the_row() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.props_mut(&format!("{AP}/10"), ACCESS_POINT).0.insert("Strength".into(), Prop::Int(20));
        let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/10"))).unwrap().unwrap().device.unwrap();
        assert_eq!(d.active.map(|a| a.strength), Some(20));
        assert_eq!(d.access_points.unwrap()[0].strength, 20);
        bus.props_mut(&format!("{AP}/11"), ACCESS_POINT).0.insert("Strength".into(), Prop::Int(99));
        let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/11"))).unwrap().unwrap().device.unwrap();
        assert_eq!(d.active.map(|a| a.strength), Some(20), "another access point: the row's stays");
    }

    /// Fix round 1: ListConnections failing (not absent) keeps the row and the access points, the
    /// saved list empty until a later read lists it.
    #[test]
    fn a_failed_listing_keeps_the_row_and_is_tried_again() {
        let mut bus = wifi_bus();
        bus.failing.insert(SETTINGS_PATH.into());
        let (mut r, d) = detail(&mut bus);
        assert!(d.active.is_some() && d.access_points.is_some_and(|a| a.len() == 3), "the row and the list kept");
        assert_eq!(d.saved, None, "not listed: unknown, never empty");
        assert!(r.logged);
        bus.failing.clear();
        let d = r.reread(&mut bus).unwrap().unwrap().device.unwrap();
        assert_eq!(d.saved.map(|s| s.len()), Some(1), "listed at the next read");
    }

    /// Review Focus 1: NetworkManager restarted with the detail open: another unique name, the same
    /// paths now other objects. Every cached path is dropped and read again from the new owner.
    #[test]
    fn a_new_networkmanager_owner_drops_every_cached_path() {
        let mut bus = wifi_bus();
        let (mut r, _) = detail(&mut bus);
        bus.owners.insert(NM, ":1.99");
        bus.props_mut(&format!("{AP}/11"), ACCESS_POINT).0.insert("Ssid".into(), Prop::Bytes(b"Autre-Test".to_vec()));
        let calls = bus.calls.len();
        let d = r.reread(&mut bus).unwrap().unwrap().device.unwrap();
        assert!(ssids(&d).contains(&"Autre-Test".to_string()), "{:?}", ssids(&d));
        let new: Vec<&String> = bus.calls[calls..].iter().collect();
        assert!(new.iter().any(|c| c.starts_with("GetDevices :1.99")), "the device found again: {new:?}");
        assert!(new.iter().any(|c| *c == &format!("GetAll :1.99 {AP}/12 {ACCESS_POINT}")), "every access point read again");
        assert!(new.iter().any(|c| c.starts_with("ListConnections :1.99")), "the saved profiles listed again");
    }

    #[test]
    fn a_saved_profile_keeps_its_uuid_and_ssid_only() {
        let p = |pairs: &[(&str, Prop)]| Props(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
        let connection =
            p(&[("type", Prop::Str("802-11-wireless".into())), ("uuid", Prop::Str("11111111-2222-4333-8444-555555555555".into()))]);
        let wireless = p(&[("ssid", Prop::Bytes(b"Maison-Test".to_vec()))]);
        assert_eq!(
            saved_wifi(Some(&connection), Some(&wireless)),
            Some(SavedWifi { uuid: "11111111-2222-4333-8444-555555555555".into(), ssid: b"Maison-Test".to_vec() })
        );
        let ethernet = p(&[("type", Prop::Str("802-3-ethernet".into())), ("uuid", Prop::Str("aaaa".into()))]);
        assert_eq!(saved_wifi(Some(&ethernet), None), None);
        let odd = p(&[("type", Prop::Str("802-11-wireless".into())), ("uuid", Prop::Str("-rf".into()))]);
        assert_eq!(saved_wifi(Some(&odd), Some(&wireless)), None, "a uuid nmcli would read as an option");
        let long = p(&[("ssid", Prop::Bytes(vec![b'x'; 33]))]);
        assert_eq!(saved_wifi(Some(&connection), Some(&long)), None);
        assert_eq!(saved_wifi(Some(&connection), None), None, "no 802-11-wireless setting");
    }
}
