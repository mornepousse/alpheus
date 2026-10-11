//! NetworkManager's Wi-Fi for the quick panel (quick Wi-Fi spec § 5), read only and only while the
//! panel is open: the radio, the first Wi-Fi device and the access point it is on
//! (`WifiScope::Row`). The loop asks through a `WifiTap`; the D-Bus thread reads here, through
//! NetworkManager's unique name as every reader does. Secrets are never asked for.

use super::props::Props;
use super::{Bus, NM, network, optional};
use crate::log::write as log;
use crate::system::{AccessPoint, Security, WifiDevice, WifiScope, WifiState};
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

/// What the loop asks the D-Bus thread (quick Wi-Fi decision 1): the scope the open panel shows,
/// and a socket pair whose first end the thread polls beside the bus's own socket; the loop writes
/// one byte to the second when the scope changes, never otherwise (no wake-up at rest). Without the
/// pair (`default()`, the tests' tap) the scope is still kept, and nothing wakes: a session reads it
/// when it starts and on each `Signal::poke`.
#[derive(Debug, Default)]
pub struct WifiTap {
    scope: Mutex<WifiScope>,
    wake: Option<(UnixStream, UnixStream)>,
}

impl WifiTap {
    /// A tap with its socket pair, both ends non-blocking and close-on-exec.
    pub fn new() -> io::Result<WifiTap> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(WifiTap { scope: Mutex::new(WifiScope::Closed), wake: Some((read, write)) })
    }

    pub fn scope(&self) -> WifiScope {
        *self.scope.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The loop asks for `scope`: kept, and the thread woken when it changed (a byte already waiting
    /// is enough: a full socket is not an error). True when it changed.
    pub fn set(&self, scope: WifiScope) -> bool {
        let changed = {
            let mut s = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = *s != scope;
            *s = scope;
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
    /// One failed read is logged per open.
    logged: bool,
}

impl WifiReader {
    pub fn scope(&self) -> WifiScope {
        self.scope
    }

    /// The loop asked for `scope` (a poke): Closed forgets everything and reads nothing; otherwise
    /// the state is read for it. Err only when the bus itself went away (`UnexpectedEof`).
    pub fn rescope(&mut self, bus: &mut dyn Bus, scope: WifiScope) -> io::Result<Option<WifiState>> {
        if scope == WifiScope::Closed {
            *self = WifiReader::default();
            return Ok(None);
        }
        self.scope = scope;
        self.reread(bus)
    }

    /// The state again (a network signal, the machine resuming): None when closed. A failed read
    /// hides the row (logged once per open); the bus going away is an error.
    pub fn reread(&mut self, bus: &mut dyn Bus) -> io::Result<Option<WifiState>> {
        if self.scope == WifiScope::Closed {
            return Ok(None);
        }
        let r = self.read(bus);
        self.soft(r).map(Some)
    }

    fn soft(&mut self, r: io::Result<WifiState>) -> io::Result<WifiState> {
        match r {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(e),
            Err(e) => {
                if !std::mem::replace(&mut self.logged, true) {
                    log(&format!("D-Bus Wi-Fi: {e}"));
                }
                Ok(WifiState::default())
            }
        }
    }

    fn forget(&mut self) {
        self.owner = None;
        self.device = None;
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
        let state = device.int("State").and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
        Ok(WifiState { enabled, device: Some(WifiDevice { state, active, active_uuid, access_points: None, saved: None }) })
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

    /// One `D-Bus Wi-Fi:` line per open: a second failure in the same open is quiet, a new open
    /// may log again.
    #[test]
    fn a_failed_read_is_logged_once_per_open() {
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
}
