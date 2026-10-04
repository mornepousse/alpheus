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

#[cfg(test)]
mod tests {
    use super::fake::{FakeBus, signal, with_names};
    use super::*;

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
