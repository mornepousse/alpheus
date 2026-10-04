//! The system D-Bus, READ ONLY: NetworkManager, UPower, BlueZ. One
//! connection on one thread, blocked on the bus while nothing changes; each
//! signal re-reads the domain it concerns. `Bus` only has reads (checked by
//! tests/guardrails.rs), and a service is only ever called through the
//! unique name of the process that owns it: never through its well-known
//! name, which D-Bus activation would use to start it.

pub mod bluez;
pub mod network;
pub mod props;
pub mod upower;

#[cfg(test)]
pub mod fake;

use crate::log::write as log;
use crate::memory::release_memory;
use crate::state::Msg;
use crate::system::{BtState, NetState, PowerState};
use props::{Objects, Props};
use std::io;

pub const NM: &str = "org.freedesktop.NetworkManager";
pub const UPOWER: &str = "org.freedesktop.UPower";
pub const BLUEZ: &str = "org.bluez";
pub const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
/// Not called (reading only): only its `PrepareForSleep` signal is heard.
pub const LOGIN1: &str = "org.freedesktop.login1";
pub const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";

/// What the bar asks of the system bus: reads, and waiting for a signal.
/// `io::ErrorKind::NotFound`: the service or the object isn't there.
pub trait Bus {
    /// `GetNameOwner`: the unique name (":1.42") of the process that owns
    /// `service`, None if nobody does — asking never starts it. Asked before
    /// any other call, and every call then goes to that unique name: a call
    /// to the well-known name of a stopped service would START it (D-Bus
    /// activation); a call to a unique name never does — if that process is
    /// gone, the call fails with NotFound.
    fn owner(&mut self, service: &str) -> io::Result<Option<String>>;
    /// `org.freedesktop.DBus.Properties.GetAll`, to `destination` (a unique name).
    fn get_all(&mut self, destination: &str, path: &str, interface: &str) -> io::Result<Props>;
    /// `org.freedesktop.DBus.ObjectManager.GetManagedObjects` at `/`.
    fn managed_objects(&mut self, destination: &str) -> io::Result<Objects>;
    /// `org.freedesktop.UPower.EnumerateDevices`.
    fn enumerate_devices(&mut self, destination: &str) -> io::Result<Vec<String>>;
    /// Blocks until the next signal the bar subscribed to; Err: connection lost.
    fn next_signal(&mut self) -> io::Result<Signal>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub path: String,
    pub interface: String,
    pub member: String,
    /// First argument when it is a string: the service of `NameOwnerChanged`,
    /// the interface of `PropertiesChanged`.
    pub arg0: Option<String>,
    /// `PropertiesChanged`: the changed and invalidated property names;
    /// `InterfacesAdded` / `InterfacesRemoved`: the interfaces. Empty
    /// otherwise, or when unknown (then the signal counts).
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Network,
    Bluetooth,
    Battery,
}

/// Every domain, in reading order.
pub const DOMAINS: [Domain; 3] = [Domain::Network, Domain::Bluetooth, Domain::Battery];

/// Property changes that can't change what the bar shows: a device seen by
/// a scan (BlueZ sends these by the dozen each second), UPower's
/// bookkeeping. A signal carrying only these re-reads nothing.
const NOISE: [&str; 13] = [
    "RSSI",
    "ManufacturerData",
    "ServiceData",
    "TxPower",
    "AdvertisingFlags",
    "UpdateTime",
    "Energy",
    "EnergyRate",
    "Voltage",
    "TimeToEmpty",
    "TimeToFull",
    "Temperature",
    "Luminosity",
];

/// The BlueZ interfaces the bar reads (task 12).
const BLUEZ_READ: [&str; 3] = ["org.bluez.Adapter1", "org.bluez.Device1", "org.bluez.Battery1"];

/// `path` is `root` or below it.
fn under(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

impl Signal {
    /// The domain to re-read on this signal; None: not ours, or nothing the
    /// bar shows can have changed.
    pub fn domain(&self) -> Option<Domain> {
        if self.member == "NameOwnerChanged" {
            return match self.arg0.as_deref()? {
                NM => Some(Domain::Network),
                BLUEZ => Some(Domain::Bluetooth),
                UPOWER => Some(Domain::Battery),
                _ => None,
            };
        }
        if self.member == "PropertiesChanged" && !self.names.is_empty() && self.names.iter().all(|n| NOISE.contains(&n.as_str())) {
            return None;
        }
        if under(&self.path, "/org/freedesktop/NetworkManager") {
            Some(Domain::Network)
        } else if under(&self.path, "/org/freedesktop/UPower") {
            Some(Domain::Battery)
        } else if under(&self.path, "/org/bluez") || (self.path == "/" && self.interface == OBJECT_MANAGER) {
            self.bluez_relevant().then_some(Domain::Bluetooth)
        } else {
            None
        }
    }

    /// `logind`'s `PrepareForSleep(false)`: the machine just resumed, not
    /// going to sleep. Every shown domain is re-read once (task 13) — a VPN
    /// may have reconnected, a Bluetooth device too, the battery drained
    /// while asleep. Reading only: the daemon never calls a logind method
    /// (Suspend, Hibernate, PowerOff… the guardrail already forbids the
    /// words).
    pub fn is_wake(&self) -> bool {
        self.interface == LOGIN1_MANAGER && self.member == "PrepareForSleep" && self.arg0.as_deref() == Some("false")
    }

    fn bluez_relevant(&self) -> bool {
        match self.member.as_str() {
            // MediaTransport1's volume, MediaPlayer1's track…: never read.
            "PropertiesChanged" => self.arg0.as_deref().is_none_or(|i| BLUEZ_READ.contains(&i)),
            // A device appears disconnected (a scan found it); its connection
            // comes later, as a PropertiesChanged. Only a new adapter or a
            // battery can change the bar.
            "InterfacesAdded" => self.names.is_empty() || self.names.iter().any(|i| i == "org.bluez.Adapter1" || i == "org.bluez.Battery1"),
            _ => true,
        }
    }
}

/// An object gone between two reads (a connection going down): nothing to
/// read, not an outage. `pub`: its callers are the readers (tasks 11b, 12).
pub fn optional(r: io::Result<Props>) -> io::Result<Option<Props>> {
    match r {
        Ok(p) => Ok(Some(p)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// What the domains say. Per domain, None: "–" — its read failed, or it
/// isn't read (its module isn't shown, nothing draws it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemState {
    pub network: Option<NetState>,
    pub bluetooth: Option<BtState>,
    pub battery: Option<PowerState>,
}

/// Re-reads `d` into `s`. Err: the read failed for real (a timeout…), the
/// domain is now None. An absence (NotFound: the service left after
/// GetNameOwner) also gives None, but is not an error: its
/// NameOwnerChanged follows and re-reads it.
fn read(bus: &mut dyn Bus, d: Domain, s: &mut SystemState) -> io::Result<()> {
    let r = match d {
        Domain::Network => network::read(bus).map(|v| s.network = Some(v)),
        Domain::Bluetooth => bluez::read(bus).map(|v| s.bluetooth = Some(v)),
        Domain::Battery => upower::read(bus).map(|v| s.battery = Some(v)),
    };
    let Err(e) = r else { return Ok(()) };
    log(&format!("D-Bus {d:?}: {e}"));
    match d {
        Domain::Network => s.network = None,
        Domain::Bluetooth => s.bluetooth = None,
        Domain::Battery => s.battery = None,
    }
    if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
}

/// Reads every domain in `domains` into `s`; the first real failure (not an
/// absence), if any. Used both for the initial state and for logind's
/// `PrepareForSleep(false)`, which re-reads all of them at once.
fn read_all(bus: &mut dyn Bus, domains: &[Domain], s: &mut SystemState) -> Option<io::Error> {
    let mut failed = None;
    for d in domains {
        if let Err(e) = read(bus, *d, s) {
            failed.get_or_insert(e);
        }
    }
    failed
}

/// Reads the wanted domains, publishes, then waits for signals: each one of
/// a wanted domain re-reads it and publishes the whole state. Returns Err
/// when the bus goes away or a domain's read fails (after publishing it as
/// "–"): the supervisor shows "–" and retries with a growing delay.
pub fn session(bus: &mut dyn Bus, domains: &[Domain], publish: &mut dyn FnMut(SystemState) -> bool) -> io::Result<()> {
    let mut s = SystemState { network: None, bluetooth: None, battery: None };
    let failed = read_all(bus, domains, &mut s);
    if !publish(s.clone()) {
        return Ok(());
    }
    // The initial replies (GetManagedObjects, GetAll…) are dropped by now:
    // hand their pages back (startup budget, 15 MB).
    release_memory();
    if let Some(e) = failed {
        return Err(e);
    }
    loop {
        let signal = bus.next_signal()?;
        if signal.is_wake() {
            // The machine just resumed: anything may have changed while it
            // slept (a VPN reconnected, a Bluetooth device, the battery) —
            // re-read every shown domain, not just the one a match rule
            // would otherwise point to.
            let failed = read_all(bus, domains, &mut s);
            if !publish(s.clone()) {
                return Ok(());
            }
            if let Some(e) = failed {
                return Err(e);
            }
            continue;
        }
        let Some(d) = signal.domain().filter(|d| domains.contains(d)) else { continue };
        let r = read(bus, d, &mut s);
        if !publish(s.clone()) {
            return Ok(());
        }
        r?;
    }
}

/// The bar's messages for a published state; None (the whole bus down):
/// "–" for the three modules.
pub fn messages(s: Option<SystemState>) -> [Msg; 3] {
    match s {
        Some(s) => [Msg::Network(s.network), Msg::Bluetooth(s.bluetooth), Msg::Battery(s.battery)],
        None => [Msg::Network(None), Msg::Bluetooth(None), Msg::Battery(None)],
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{BLUEZ_OWNER, FakeBus, NM_OWNER, Step, signal, with_names};
    use super::props::Prop;
    use super::*;
    use crate::state::Msg;
    use crate::system::*;
    use std::collections::VecDeque;

    fn workstation_network() -> NetState {
        NetState::Online { link: Link::Ethernet, vpns: 2, limited: false }
    }

    fn workstation_battery() -> PowerState {
        PowerState {
            laptop: Some(Battery { percent: 92, charge: Charge::Charging }),
            peripherals: vec![Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: 100 }],
        }
    }

    fn wifi_primary(b: &mut FakeBus) {
        b.props_mut(network::PATH, network::INTERFACE)
            .0
            .insert("PrimaryConnection".into(), Prop::Path("/org/freedesktop/NetworkManager/ActiveConnection/9".into()));
    }

    fn nothing(_: &mut FakeBus) {}

    fn bluetooth_connected(b: &mut FakeBus) {
        b.connect_bluetooth();
    }

    fn upower_gone(b: &mut FakeBus) {
        b.owners.remove(UPOWER);
    }

    fn network_breaks(b: &mut FakeBus) {
        b.failing.insert(network::PATH.into());
    }

    /// A session over `domains`, every publication accepted.
    fn run_over(bus: &mut FakeBus, domains: &[Domain]) -> (Vec<SystemState>, io::Result<()>) {
        let mut seen = Vec::new();
        let r = session(bus, domains, &mut |s| {
            seen.push(s);
            true
        });
        (seen, r)
    }

    fn run(bus: &mut FakeBus) -> (Vec<SystemState>, io::Result<()>) {
        run_over(bus, &DOMAINS)
    }

    /// The calls' destinations: GetNameOwner aside, only unique names.
    fn assert_unique_destinations(calls: &[String]) {
        for c in calls {
            let destination = c.split(' ').nth(1).unwrap_or("");
            assert!(c.starts_with("GetNameOwner ") || destination.starts_with(':'), "{c}: a well-known name could be activated");
        }
    }

    #[test]
    fn initial_state_then_one_publication_per_relevant_signal() {
        let mut bus = FakeBus::workstation();
        let headset = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";
        let steps: [Step; 5] = [
            (signal(network::PATH, PROPERTIES, "PropertiesChanged", None), wifi_primary),
            (signal("/org/freedesktop/systemd1", PROPERTIES, "PropertiesChanged", None), nothing),
            (with_names(signal(headset, PROPERTIES, "PropertiesChanged", Some("org.bluez.Device1")), &["RSSI"]), nothing),
            (signal(headset, PROPERTIES, "PropertiesChanged", None), bluetooth_connected),
            (signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(UPOWER)), upower_gone),
        ];
        bus.script = VecDeque::from(steps);
        let (seen, r) = run(&mut bus);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof, "the bus going away ends the session");
        assert_eq!(seen.len(), 4, "initial state + 3 relevant signals (systemd's and the RSSI one are ignored)");
        let reads = bus.calls.iter().filter(|c| c.starts_with("GetManagedObjects")).count();
        assert_eq!(reads, 2, "BlueZ read at startup and on the connection, never on the RSSI noise");
        assert_unique_destinations(&bus.calls);
        assert_eq!(
            seen[0],
            SystemState {
                network: Some(workstation_network()),
                bluetooth: Some(BtState::On { devices: vec![] }),
                battery: Some(workstation_battery())
            }
        );
        assert!(matches!(&seen[1].network, Some(NetState::Online { link: Link::Wifi, .. })), "{:?}", seen[1]);
        assert_eq!(seen[1].battery, Some(workstation_battery()), "other domains carried along");
        assert!(matches!(&seen[2].bluetooth, Some(BtState::On { devices }) if devices.len() == 2), "{:?}", seen[2]);
        assert_eq!(seen[3].battery, Some(PowerState::default()), "UPower gone: nothing to show");
    }

    #[test]
    fn no_call_to_a_service_without_owner() {
        let mut bus = FakeBus::workstation();
        bus.owners.remove(BLUEZ);
        let (seen, _) = run(&mut bus);
        assert_eq!(seen[0].bluetooth, Some(BtState::Absent));
        let bluez: Vec<&String> = bus.calls.iter().filter(|c| c.contains(BLUEZ) || c.contains(BLUEZ_OWNER)).collect();
        assert_eq!(bluez, ["GetNameOwner org.bluez"], "activation would start bluetoothd");
        assert_unique_destinations(&bus.calls);
    }

    #[test]
    fn an_outage_in_one_domain_is_published_then_retried() {
        let mut bus = FakeBus::workstation();
        bus.failing.insert(network::PATH.into());
        bus.script = VecDeque::from([(signal(network::PATH, PROPERTIES, "PropertiesChanged", None), nothing as fn(&mut FakeBus))]);
        let (seen, r) = run(&mut bus);
        assert_eq!(seen.len(), 1, "the state is published once, the others with it");
        assert_eq!(seen[0].network, None, "network down: \"–\"");
        assert_eq!(seen[0].battery, Some(workstation_battery()));
        assert!(seen[0].bluetooth.is_some());
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::Other, "an Err: the supervisor retries with a growing delay");
        assert_eq!(bus.script.len(), 1, "no signal awaited with a domain down");
    }

    #[test]
    fn a_failed_re_read_also_ends_the_session() {
        let mut bus = FakeBus::workstation();
        bus.script = VecDeque::from([(signal(network::PATH, PROPERTIES, "PropertiesChanged", None), network_breaks as fn(&mut FakeBus))]);
        let (seen, r) = run(&mut bus);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].network, None);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::Other);
    }

    #[test]
    fn a_service_gone_between_owner_and_read_is_not_an_outage() {
        // NotFound: NetworkManager left after GetNameOwner; its
        // NameOwnerChanged follows and re-reads it. No retry needed.
        let mut bus = FakeBus::workstation();
        bus.props.remove(&(network::PATH.to_string(), network::INTERFACE.to_string()));
        let (seen, r) = run(&mut bus);
        assert_eq!(seen[0].network, None);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof, "the end of the script, not an outage");
    }

    fn prepare_for_sleep(arg0: &str) -> Signal {
        signal("/org/freedesktop/login1", LOGIN1_MANAGER, "PrepareForSleep", Some(arg0))
    }

    #[test]
    fn resume_from_sleep_re_reads_every_shown_domain() {
        let mut bus = FakeBus::workstation();
        bus.script = VecDeque::from([(prepare_for_sleep("false"), wifi_primary as fn(&mut FakeBus))]);
        let (seen, r) = run(&mut bus);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(seen.len(), 2, "initial state + one publication for the resume");
        assert!(matches!(&seen[1].network, Some(NetState::Online { link: Link::Wifi, .. })), "re-read after resume: {:?}", seen[1]);
        assert_eq!(seen[1].battery, Some(workstation_battery()), "every domain re-read, not only the one NetworkManager changed");
    }

    #[test]
    fn going_to_sleep_re_reads_nothing() {
        let mut bus = FakeBus::workstation();
        bus.script = VecDeque::from([(prepare_for_sleep("true"), wifi_primary as fn(&mut FakeBus))]);
        let (seen, r) = run(&mut bus);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(seen.len(), 1, "PrepareForSleep(true): not a wake, no re-read");
    }

    #[test]
    fn only_the_shown_domains_are_read_and_followed() {
        let mut bus = FakeBus::workstation();
        let steps: [Step; 2] = [
            (signal(network::PATH, PROPERTIES, "PropertiesChanged", None), wifi_primary),
            (signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(UPOWER)), upower_gone),
        ];
        bus.script = VecDeque::from(steps);
        let (seen, r) = run_over(&mut bus, &[Domain::Battery]);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(seen.len(), 2, "initial state + UPower's signal; the network's is not followed");
        assert_eq!((&seen[0].network, &seen[0].bluetooth), (&None, &None), "never read");
        assert_eq!(seen[0].battery, Some(workstation_battery()));
        let others = [NM, NM_OWNER, BLUEZ, BLUEZ_OWNER];
        assert!(bus.calls.iter().all(|c| others.iter().all(|o| !c.contains(o))), "NetworkManager or BlueZ asked: {:?}", bus.calls);
    }

    #[test]
    fn stops_when_the_bar_stops() {
        let mut bus = FakeBus::workstation();
        bus.script = VecDeque::from([(signal(network::PATH, PROPERTIES, "PropertiesChanged", None), nothing as fn(&mut FakeBus))]);
        assert!(session(&mut bus, &DOMAINS, &mut |_| false).is_ok());
        assert_eq!(bus.script.len(), 1, "no signal read after the bar said stop");
    }

    #[test]
    fn messages_for_the_bar() {
        assert_eq!(messages(None), [Msg::Network(None), Msg::Bluetooth(None), Msg::Battery(None)], "bus down: \"–\" everywhere");
        let s = SystemState { network: Some(NetState::Offline), bluetooth: None, battery: Some(PowerState::default()) };
        assert_eq!(
            messages(Some(s)),
            [Msg::Network(Some(NetState::Offline)), Msg::Bluetooth(None), Msg::Battery(Some(PowerState::default()))]
        );
    }

    #[test]
    fn signals_point_to_their_domain() {
        let d = |path, interface, member, arg0| signal(path, interface, member, arg0).domain();
        assert_eq!(d("/org/freedesktop/NetworkManager", PROPERTIES, "PropertiesChanged", None), Some(Domain::Network));
        assert_eq!(d("/org/freedesktop/UPower/devices/battery_BAT0", PROPERTIES, "PropertiesChanged", None), Some(Domain::Battery));
        assert_eq!(d("/org/freedesktop/UPower", "org.freedesktop.UPower", "DeviceAdded", None), Some(Domain::Battery));
        assert_eq!(d("/org/bluez/hci0", PROPERTIES, "PropertiesChanged", None), Some(Domain::Bluetooth));
        assert_eq!(d("/", OBJECT_MANAGER, "InterfacesAdded", None), Some(Domain::Bluetooth));
        assert_eq!(d("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(BLUEZ)), Some(Domain::Bluetooth));
        assert_eq!(d("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(":1.42")), None);
        assert_eq!(d("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameAcquired", None), None, "sent to every new client");
        assert_eq!(d("/org/freedesktop/systemd1", PROPERTIES, "PropertiesChanged", None), None);
        assert_eq!(d("/org/bluezz", PROPERTIES, "PropertiesChanged", None), None, "a path prefix, not a string prefix");
    }

    #[test]
    fn noise_and_unread_interfaces_point_nowhere() {
        const DEVICE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";
        const MOUSE: &str = "/org/freedesktop/UPower/devices/battery_hidpp_battery_1";
        let changed =
            |path, interface, names: &[&str]| with_names(signal(path, PROPERTIES, "PropertiesChanged", Some(interface)), names).domain();
        assert_eq!(changed(DEVICE, "org.bluez.Device1", &["RSSI"]), None, "a scan's noise re-reads nothing");
        assert_eq!(changed(DEVICE, "org.bluez.Device1", &["RSSI", "ManufacturerData", "TxPower"]), None);
        assert_eq!(changed(DEVICE, "org.bluez.Device1", &["RSSI", "Connected"]), Some(Domain::Bluetooth));
        assert_eq!(changed(DEVICE, "org.bluez.Battery1", &["Percentage"]), Some(Domain::Bluetooth));
        assert_eq!(changed(DEVICE, "org.bluez.MediaTransport1", &["Volume"]), None, "an interface the bar doesn't read");
        assert_eq!(changed(MOUSE, "org.freedesktop.UPower.Device", &["UpdateTime"]), None, "UPower's bookkeeping");
        assert_eq!(changed(MOUSE, "org.freedesktop.UPower.Device", &["UpdateTime", "Percentage"]), Some(Domain::Battery));
        let objects = |member, names: &[&str]| with_names(signal("/", OBJECT_MANAGER, member, None), names).domain();
        assert_eq!(
            objects("InterfacesAdded", &["org.freedesktop.DBus.Introspectable", "org.bluez.Device1"]),
            None,
            "a device seen by a scan, not connected"
        );
        assert_eq!(objects("InterfacesAdded", &["org.bluez.Battery1"]), Some(Domain::Bluetooth), "a connected device's battery");
        assert_eq!(objects("InterfacesRemoved", &["org.bluez.Device1"]), Some(Domain::Bluetooth));
    }

    #[test]
    fn optional_turns_an_absence_into_nothing_and_keeps_outages() {
        assert_eq!(optional(Ok(props::Props::default())).unwrap(), Some(props::Props::default()));
        assert_eq!(optional(Err(io::Error::new(io::ErrorKind::NotFound, "UnknownObject"))).unwrap(), None);
        assert_eq!(optional(Err(io::Error::other("timeout"))).unwrap_err().kind(), io::ErrorKind::Other);
    }

    #[test]
    fn prepare_for_sleep_false_is_a_wake_true_and_others_are_not() {
        let s = |member: &str, arg0: Option<&str>| Signal {
            path: "/org/freedesktop/login1".into(),
            interface: LOGIN1_MANAGER.into(),
            member: member.into(),
            arg0: arg0.map(String::from),
            names: Vec::new(),
        };
        assert!(s("PrepareForSleep", Some("false")).is_wake());
        assert!(!s("PrepareForSleep", Some("true")).is_wake(), "going to sleep: nothing to re-read yet");
        assert!(!s("PrepareForSleep", None).is_wake());
        assert!(!signal("/org/freedesktop/login1", LOGIN1_MANAGER, "SomethingElse", Some("false")).is_wake());
        assert!(!signal("/org/freedesktop/login1", PROPERTIES, "PrepareForSleep", Some("false")).is_wake(), "wrong interface");
    }

    #[test]
    #[should_panic(expected = "D-Bus activation")]
    fn the_fake_bus_refuses_a_call_to_a_well_known_name() {
        let _ = FakeBus::workstation().get_all(NM, "/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager");
    }

    #[test]
    fn the_fake_bus_answers_unique_names() {
        let mut bus = FakeBus::workstation();
        let nm = bus.owner(NM).unwrap().expect("NetworkManager runs");
        assert!(nm.starts_with(':'), "{nm}");
        assert!(bus.get_all(&nm, "/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager").is_ok());
        assert_eq!(bus.owner("org.example.Absent").unwrap(), None);
        assert_eq!(bus.get_all(":1.999", "/", "x").unwrap_err().kind(), io::ErrorKind::NotFound, "a unique name nobody holds");
        assert_eq!(bus.calls[0], "GetNameOwner org.freedesktop.NetworkManager");
    }
}
