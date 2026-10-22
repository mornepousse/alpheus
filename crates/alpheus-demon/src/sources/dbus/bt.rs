//! BlueZ's Bluetooth for the quick panel (quick settings delivery 3), read only and only while the
//! panel is open: the first adapter (`BtScope::Row`); with the detail open, the paired devices and
//! the ones scanning has seen nearby too (`BtScope::Detail`), followed through the bar's own BlueZ
//! signals (`Signal::bt_change`). The loop asks through a `BtTap`; the D-Bus thread reads here,
//! through BlueZ's unique name as every reader does. A device's address is never read; its object
//! path embeds it, so neither is ever logged.

use super::bluez::{ADAPTER, DEVICE};
use super::props::Props;
use super::{BLUEZ, Bus, under};
use crate::log::write as log;
use crate::pipewire::drawable;
use crate::system::{BtAdapter, BtEntry, BtKind, BtScope, BtView};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, PoisonError};

/// The detail reads at most this many devices of the adapter, in path order.
pub const MAX_DEVICES: usize = 64;

/// What a Bluetooth panel signal asks to read again (`Signal::bt_change`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtChange {
    /// The adapter's `Powered` or `PowerState`.
    Adapter(String),
    /// A device's properties; `rssi_only`: nothing but its signal strength moved.
    Device { path: String, rssi_only: bool },
    /// A device object appeared (scanning found it, or it was paired).
    Added(String),
    /// A device object went.
    Removed(String),
}

/// What the loop asks the D-Bus thread: the scope the open panel shows, and a socket pair whose
/// first end the thread polls beside the bus's own socket; the loop writes one byte to the second
/// when the scope changes, never otherwise (no wake-up at rest). Without the pair (`default()`, the
/// tests' tap) the scope is still kept, and nothing wakes. The tap also counts the panel's opens
/// (Closed to any other scope), as `WifiTap` does.
#[derive(Debug, Default)]
pub struct BtTap {
    /// The scope asked, and the opens so far.
    scope: Mutex<(BtScope, u64)>,
    wake: Option<(UnixStream, UnixStream)>,
}

impl BtTap {
    /// A tap with its socket pair, both ends non-blocking and close-on-exec.
    pub fn new() -> io::Result<BtTap> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(BtTap { scope: Mutex::new((BtScope::Closed, 0)), wake: Some((read, write)) })
    }

    pub fn scope(&self) -> BtScope {
        self.snapshot().0
    }

    /// The scope asked and the opens so far, read together.
    pub fn snapshot(&self) -> (BtScope, u64) {
        *self.scope.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The loop asks for `scope`: kept, and the thread woken when it changed (a byte already waiting
    /// is enough: a full socket is not an error); from Closed to another scope, one more open. True
    /// when it changed.
    pub fn set(&self, scope: BtScope) -> bool {
        let changed = {
            let mut s = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = s.0 != scope;
            if s.0 == BtScope::Closed && scope != BtScope::Closed {
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

/// A device's `Icon` as the panel's glyph picks it; anything else (or none) is `Other`.
pub fn kind_of_icon(icon: Option<&str>) -> BtKind {
    match icon {
        Some("audio-headset" | "audio-headphones") => BtKind::Headset,
        Some("input-keyboard") => BtKind::Keyboard,
        Some("input-mouse") => BtKind::Mouse,
        Some("audio-card" | "audio-speakers") => BtKind::Speaker,
        Some("phone") => BtKind::Phone,
        _ => BtKind::Other,
    }
}

/// One device the detail keeps.
#[derive(Debug)]
struct Known {
    entry: BtEntry,
    /// BlueZ gave it a `Name` (an address-only device is noise from scanning).
    named: bool,
    /// The last RSSI seen during this detail.
    rssi: Option<i64>,
    /// The RSSI it had when it was listed in `nearby` (its place never moves after).
    listed_rssi: Option<i64>,
}

impl Known {
    /// The device from its `Device1` properties; `before`: what the detail knew of it.
    fn new(path: &str, p: &Props, before: Option<&Known>) -> Known {
        let alias = drawable(p.str("Alias").or(p.str("Name")).unwrap_or(""));
        let flag = |k: &str| p.bool(k) == Some(true);
        Known {
            entry: BtEntry {
                path: path.to_string(),
                label: if alias.is_empty() { "?".to_string() } else { alias },
                kind: kind_of_icon(p.str("Icon")),
                paired: flag("Paired"),
                trusted: flag("Trusted"),
                connected: flag("Connected"),
            },
            named: p.str("Name").is_some(),
            rssi: p.int("RSSI").or(before.and_then(|b| b.rssi)),
            listed_rssi: before.and_then(|b| b.listed_rssi),
        }
    }

    /// Worth listing in `nearby`: unpaired, named, and seen advertising during this detail.
    fn listable(&self) -> bool {
        !self.entry.paired && self.named && self.rssi.is_some()
    }
}

/// The D-Bus thread's Bluetooth reading for one session: what it reads for the scope asked, and
/// what it remembers between two reads while the panel is open. Closed, it is back to its default:
/// nothing kept.
#[derive(Debug, Default)]
pub struct BtReader {
    scope: BtScope,
    /// BlueZ's unique name when the paths below were read: another owner, other objects.
    owner: Option<String>,
    adapter: Option<BtAdapter>,
    /// Detail only: the adapter's devices by path (at most MAX_DEVICES).
    devices: BTreeMap<String, Known>,
    /// Detail only: the paths of `nearby`, in the order they were listed (rows never move).
    order: Vec<String>,
    /// The last view sent.
    last: Option<BtView>,
    /// One failed read is logged per open.
    logged: bool,
}

impl BtReader {
    pub fn scope(&self) -> BtScope {
        self.scope
    }

    /// The loop asked for `scope` (a poke). The same scope: read again. Leaving the detail drops
    /// its devices; Closed forgets everything and reads nothing. Err when the bus itself went away
    /// (`UnexpectedEof`).
    pub fn rescope(&mut self, bus: &mut dyn Bus, scope: BtScope) -> io::Result<Option<BtView>> {
        if scope == BtScope::Closed {
            *self = BtReader::default();
            return Ok(None);
        }
        if scope != BtScope::Detail {
            self.devices.clear();
            self.order.clear();
        }
        self.scope = scope;
        self.reread(bus)
    }

    /// The view again (the machine resuming, a poke): None when closed. A failed read keeps the
    /// last view (logged once per open); the bus going away is an error.
    pub fn reread(&mut self, bus: &mut dyn Bus) -> io::Result<Option<BtView>> {
        if self.scope == BtScope::Closed {
            return Ok(None);
        }
        match self.read(bus) {
            Ok(v) => {
                self.last = Some(v.clone());
                Ok(Some(v))
            }
            Err(e) => self.failed(e),
        }
    }

    /// A signal of the panel (see `Signal::bt_change`). Closed: None. Outside the scope that reads
    /// it (a device while only the row is open) or for something not listed: the last view, no call.
    pub fn changed(&mut self, bus: &mut dyn Bus, c: &BtChange) -> io::Result<Option<BtView>> {
        if self.scope == BtScope::Closed {
            return Ok(None);
        }
        let (Some(owner), Some(adapter)) = (self.owner.clone(), self.adapter.clone()) else { return Ok(self.last.clone()) };
        let detail = self.scope == BtScope::Detail;
        match c {
            BtChange::Adapter(p) if *p == adapter.path => match bus.get_all(&owner, p, ADAPTER) {
                Ok(a) => self.adapter = Some(adapter_of(p, &a)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return self.failed(e),
            },
            BtChange::Device { path, rssi_only } if detail && under(path, &adapter.path) => {
                let known = self.devices.get(path);
                // An RSSI alone only matters to a device that waits for one.
                let reads = if *rssi_only { known.is_some_and(|k| !k.entry.paired && k.named && k.listed_rssi.is_none()) } else { true };
                if reads && (known.is_some() || self.devices.len() < MAX_DEVICES) {
                    return self.read_device(bus, &owner, path);
                }
            }
            BtChange::Added(path) if detail && under(path, &adapter.path) => {
                if self.devices.contains_key(path) || self.devices.len() < MAX_DEVICES {
                    return self.read_device(bus, &owner, path);
                }
            }
            BtChange::Removed(path) if detail => {
                self.devices.remove(path);
                self.order.retain(|p| p != path);
            }
            _ => return Ok(self.last.clone()),
        }
        Ok(self.publish())
    }

    /// One device read again, listed or dropped; the view after.
    fn read_device(&mut self, bus: &mut dyn Bus, owner: &str, path: &str) -> io::Result<Option<BtView>> {
        match bus.get_all(owner, path, DEVICE) {
            Ok(p) => {
                let known = Known::new(path, &p, self.devices.get(path));
                self.devices.insert(path.to_string(), known);
                self.relist(path);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.devices.remove(path);
                self.order.retain(|p| p != path);
            }
            Err(e) => return self.failed(e),
        }
        Ok(self.publish())
    }

    /// A failed read: the last view again, unchanged (none before the first: the default); the bus
    /// going away is an error. One line per open, from the error's kind only: a D-Bus error's text
    /// can carry an object path, which embeds an address.
    fn failed(&mut self, e: io::Error) -> io::Result<Option<BtView>> {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Err(e);
        }
        if !std::mem::replace(&mut self.logged, true) {
            log(&format!("D-Bus Bluetooth panel: read failed ({:?})", e.kind()));
        }
        Ok(Some(self.last.clone().unwrap_or_default()))
    }

    /// Another BlueZ (or none): every path read from the last one is someone else's now.
    fn forget(&mut self) {
        self.owner = None;
        self.adapter = None;
        self.devices.clear();
        self.order.clear();
    }

    fn read(&mut self, bus: &mut dyn Bus) -> io::Result<BtView> {
        let Some(bz) = bus.owner(BLUEZ)? else {
            self.forget();
            return Ok(BtView::default());
        };
        if self.owner.as_deref() != Some(bz.as_str()) {
            self.forget();
            self.owner = Some(bz.clone());
        }
        let objects = bus.managed_objects(&bz)?;
        // The first adapter in path order (hci0).
        let Some((root, props)) = objects.iter().find_map(|(p, i)| i.get(ADAPTER).map(|a| (p, a))) else {
            self.forget();
            self.owner = Some(bz);
            return Ok(BtView::default());
        };
        self.adapter = Some(adapter_of(root, props));
        if self.scope == BtScope::Detail {
            let before = std::mem::take(&mut self.devices);
            for (path, ifaces) in objects.iter().filter(|(p, _)| *p != root && under(p, root)) {
                let Some(p) = ifaces.get(DEVICE) else { continue };
                if self.devices.len() == MAX_DEVICES {
                    break;
                }
                self.devices.insert(path.clone(), Known::new(path, p, before.get(path)));
            }
            let devices = &self.devices;
            self.order.retain(|p| devices.get(p).is_some_and(Known::listable));
            let paths: Vec<String> = self.devices.keys().cloned().collect();
            for p in paths {
                self.relist(&p);
            }
        }
        Ok(self.view())
    }

    /// `path`'s place in `nearby` after a read: out when it is paired or not listable any more; in
    /// (once) when it just became listable, before the first listed entry whose RSSI at its own
    /// insertion is lower, else at the end. Listed entries never move.
    fn relist(&mut self, path: &str) {
        let Some(k) = self.devices.get(path) else { return };
        let listed = self.order.iter().any(|p| p == path);
        if !k.listable() {
            self.order.retain(|p| p != path);
        } else if !listed && let Some(rssi) = k.rssi {
            let at = self
                .order
                .iter()
                .position(|p| self.devices.get(p).and_then(|o| o.listed_rssi).is_some_and(|r| r < rssi))
                .unwrap_or(self.order.len());
            self.order.insert(at, path.to_string());
            if let Some(k) = self.devices.get_mut(path) {
                k.listed_rssi = Some(rssi);
            }
        }
    }

    fn view(&self) -> BtView {
        BtView {
            adapter: self.adapter.clone(),
            paired: self.devices.values().filter(|k| k.entry.paired).map(|k| k.entry.clone()).collect(),
            nearby: self.order.iter().filter_map(|p| self.devices.get(p)).map(|k| k.entry.clone()).collect(),
        }
    }

    fn publish(&mut self) -> Option<BtView> {
        let v = self.view();
        self.last = Some(v.clone());
        Some(v)
    }
}

/// An `Adapter1`'s properties: powered, and held off by rfkill (`PowerState` `off-blocked`).
fn adapter_of(path: &str, p: &Props) -> BtAdapter {
    BtAdapter { path: path.to_string(), powered: p.bool("Powered") == Some(true), blocked: p.str("PowerState") == Some("off-blocked") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::dbus::fake::{BLUEZ_OWNER, FakeBus};
    use crate::sources::dbus::props::{Prop, Props};

    const HCI0: &str = "/org/bluez/hci0";
    const HEADSET: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";

    fn dev(n: u8) -> String {
        format!("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_{n:02X}")
    }

    fn device1<'a>(bus: &'a mut FakeBus, path: &str) -> &'a mut Props {
        bus.bluez.get_mut(path).and_then(|i| i.get_mut("org.bluez.Device1")).expect("a device")
    }

    fn detail(bus: &mut FakeBus) -> (BtReader, BtView) {
        let mut r = BtReader::default();
        let v = r.rescope(bus, BtScope::Detail).unwrap().expect("open: a view");
        (r, v)
    }

    fn labels(v: &[BtEntry]) -> Vec<&str> {
        v.iter().map(|e| e.label.as_str()).collect()
    }

    #[test]
    fn closed_reads_nothing() {
        let mut bus = FakeBus::workstation();
        let mut r = BtReader::default();
        assert_eq!(r.rescope(&mut bus, BtScope::Closed).unwrap(), None);
        assert_eq!(r.reread(&mut bus).unwrap(), None);
        assert!(bus.calls.is_empty(), "{:?}", bus.calls);
        r.rescope(&mut bus, BtScope::Row).unwrap();
        let calls = bus.calls.len();
        assert_eq!(r.rescope(&mut bus, BtScope::Closed).unwrap(), None);
        assert_eq!(r.changed(&mut bus, &BtChange::Added(dev(0x10))).unwrap(), None);
        assert_eq!(bus.calls.len(), calls, "closed: not one call");
        assert_eq!(r.scope(), BtScope::Closed);
    }

    #[test]
    fn row_reads_the_adapter_and_blocked() {
        let mut bus = FakeBus::workstation();
        let v = BtReader::default().rescope(&mut bus, BtScope::Row).unwrap();
        let hci0 = |powered, blocked| Some(BtAdapter { path: HCI0.into(), powered, blocked });
        assert_eq!(v, Some(BtView { adapter: hci0(true, false), paired: vec![], nearby: vec![] }));
        assert_eq!(bus.calls, ["GetNameOwner org.bluez".to_string(), format!("GetManagedObjects {BLUEZ_OWNER}")]);
        let a = bus.bluez.get_mut(HCI0).and_then(|i| i.get_mut("org.bluez.Adapter1")).unwrap();
        a.0.insert("Powered".into(), Prop::Bool(false));
        a.0.insert("PowerState".into(), Prop::Str("off-blocked".into()));
        assert_eq!(BtReader::default().rescope(&mut bus, BtScope::Row).unwrap().unwrap().adapter, hci0(false, true));
        let mut none = FakeBus::empty();
        assert_eq!(BtReader::default().rescope(&mut none, BtScope::Row).unwrap(), Some(BtView::default()), "no BlueZ: empty");
        assert_eq!(none.calls, ["GetNameOwner org.bluez"]);
    }

    #[test]
    fn detail_lists_paired_and_skips_unnamed_and_unseen() {
        let mut bus = FakeBus::workstation();
        bus.nearby(&dev(0x10), "Keyboard-Test", Some(-50));
        bus.nearby(&dev(0x11), "Speaker-X", None); // named, no RSSI this open
        bus.nearby(&dev(0x12), "AA-BB-CC-DD-EE-12", Some(-40));
        device1(&mut bus, &dev(0x12)).0.remove("Name"); // an address only
        let (_, v) = detail(&mut bus);
        let casque = BtEntry {
            path: HEADSET.into(),
            label: "CasqueBT".into(),
            kind: BtKind::Headset,
            paired: true,
            trusted: true,
            connected: false,
        };
        assert_eq!(v.paired, [casque]);
        assert_eq!(labels(&v.nearby), ["Keyboard-Test"]);
    }

    #[test]
    fn nearby_inserted_by_rssi_then_never_moves() {
        let mut bus = FakeBus::workstation();
        let (mut r, _) = detail(&mut bus);
        let mut last = None;
        for (n, name, rssi) in [(0x10, "A-Test", -40), (0x11, "B-Test", -70), (0x12, "C-Test", -55)] {
            bus.nearby(&dev(n), name, Some(rssi));
            last = r.changed(&mut bus, &BtChange::Added(dev(n))).unwrap();
        }
        let v = last.expect("a view");
        assert_eq!(labels(&v.nearby), ["A-Test", "C-Test", "B-Test"]);
        let calls = bus.calls.len();
        for (n, rssi) in [(0x11, -30), (0x10, -90)] {
            device1(&mut bus, &dev(n)).0.insert("RSSI".into(), Prop::Int(rssi));
            let again = r.changed(&mut bus, &BtChange::Device { path: dev(n), rssi_only: true }).unwrap();
            assert_eq!(again, Some(v.clone()), "rows never move");
        }
        assert_eq!(bus.calls.len(), calls, "an RSSI change of a listed device reads nothing");
    }

    #[test]
    fn a_reread_keeps_the_nearby_order() {
        let mut bus = FakeBus::workstation();
        bus.nearby(&dev(0x10), "A-Test", Some(-40));
        bus.nearby(&dev(0x11), "B-Test", Some(-70));
        let (mut r, v) = detail(&mut bus);
        assert_eq!(labels(&v.nearby), ["A-Test", "B-Test"]);
        // B is now the strongest, A is gone from the air (no RSSI): the rows stay.
        device1(&mut bus, &dev(0x11)).0.insert("RSSI".into(), Prop::Int(-20));
        device1(&mut bus, &dev(0x10)).0.remove("RSSI");
        let again = r.reread(&mut bus).unwrap().unwrap();
        assert_eq!(labels(&again.nearby), ["A-Test", "B-Test"], "a reread never reorders");
        let again = r.rescope(&mut bus, BtScope::Detail).unwrap().unwrap();
        assert_eq!(labels(&again.nearby), ["A-Test", "B-Test"], "nor a poke on the same scope");
    }

    #[test]
    fn a_name_arriving_lists_a_device() {
        let mut bus = FakeBus::workstation();
        bus.nearby(&dev(0x10), "AA-BB-CC-DD-EE-10", Some(-60));
        device1(&mut bus, &dev(0x10)).0.remove("Name");
        let (mut r, v) = detail(&mut bus);
        assert!(v.nearby.is_empty(), "an address only: not listed");
        let calls = bus.calls.len();
        r.changed(&mut bus, &BtChange::Device { path: dev(0x10), rssi_only: true }).unwrap();
        assert_eq!(bus.calls.len(), calls, "still unnamed: its RSSI reads nothing");
        let d = device1(&mut bus, &dev(0x10));
        d.0.insert("Name".into(), Prop::Str("Keyboard-Test".into()));
        d.0.insert("Alias".into(), Prop::Str("Keyboard-Test".into()));
        let v = r.changed(&mut bus, &BtChange::Device { path: dev(0x10), rssi_only: false }).unwrap().unwrap();
        assert_eq!(labels(&v.nearby), ["Keyboard-Test"]);
    }

    #[test]
    fn removed_device_goes() {
        let mut bus = FakeBus::workstation();
        bus.nearby(&dev(0x10), "Keyboard-Test", Some(-50));
        let (mut r, v) = detail(&mut bus);
        assert_eq!(labels(&v.nearby), ["Keyboard-Test"]);
        bus.bluez.remove(&dev(0x10));
        let calls = bus.calls.len();
        let v = r.changed(&mut bus, &BtChange::Removed(dev(0x10))).unwrap().unwrap();
        assert!(v.nearby.is_empty());
        assert_eq!(bus.calls.len(), calls, "a removal reads nothing");
        let v = r.changed(&mut bus, &BtChange::Removed(HEADSET.into())).unwrap().unwrap();
        assert!(v.paired.is_empty());
        assert_eq!(bus.calls.len(), calls);
    }

    #[test]
    fn cap_64_devices() {
        let mut bus = FakeBus::workstation();
        for n in 0x10..0x56u8 {
            bus.nearby(&dev(n), &format!("Dev-{n}"), Some(-50));
        }
        let (mut r, v) = detail(&mut bus);
        assert_eq!(v.paired.len() + v.nearby.len(), MAX_DEVICES);
        assert_eq!(v.paired[0].label, "CasqueBT", "path order: the headset is read first");
        bus.nearby(&dev(0x60), "Late-Test", Some(-10));
        let calls = bus.calls.len();
        let v = r.changed(&mut bus, &BtChange::Added(dev(0x60))).unwrap().unwrap();
        assert_eq!(bus.calls.len(), calls, "over the cap: no read");
        assert!(!labels(&v.nearby).contains(&"Late-Test"));
    }

    #[test]
    fn alias_cleaned_and_capped() {
        let mut bus = FakeBus::workstation();
        bus.nearby(&dev(0x10), &format!("\u{202e}Clavier\u{7}BT\u{200b}{}", "x".repeat(300)), Some(-50));
        bus.nearby(&dev(0x11), "\u{202e}\u{7}", Some(-60));
        let (_, v) = detail(&mut bus);
        let first = &v.nearby[0].label;
        assert!(first.starts_with("ClavierBT"), "{first:?}");
        assert!(first.len() <= crate::pipewire::LABEL_MAX);
        assert!(!first.chars().any(char::is_control));
        assert_eq!(v.nearby[1].label, "?");
    }

    #[test]
    fn kind_of_icon_table() {
        let k = |i| kind_of_icon(Some(i));
        assert_eq!((k("audio-headset"), k("audio-headphones")), (BtKind::Headset, BtKind::Headset));
        assert_eq!(k("input-keyboard"), BtKind::Keyboard);
        assert_eq!(k("input-mouse"), BtKind::Mouse);
        assert_eq!((k("audio-card"), k("audio-speakers")), (BtKind::Speaker, BtKind::Speaker));
        assert_eq!(k("phone"), BtKind::Phone);
        assert_eq!((k("computer"), kind_of_icon(None)), (BtKind::Other, BtKind::Other));
    }
}
