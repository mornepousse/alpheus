//! A system bus in memory for the tests, filled with the replies recorded on
//! the workstation (tests/fixtures/dbus-*.json, anonymized). Never a real
//! bus. Records every call, and refuses — panics on — a call addressed to a
//! well-known name: on a real bus, that call could start the service.

use super::props::{Objects, Prop, Props, busctl};
use super::{BLUEZ, Bus, NM, Signal, UPOWER};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;

const NM_ROOT: &str = include_str!("../../../tests/fixtures/dbus-nm-root.json");
const NM_ACTIVE: [(u32, &str); 4] = [
    (10, include_str!("../../../tests/fixtures/dbus-nm-active-10.json")),
    (9, include_str!("../../../tests/fixtures/dbus-nm-active-9.json")),
    (3, include_str!("../../../tests/fixtures/dbus-nm-active-3.json")),
    (2, include_str!("../../../tests/fixtures/dbus-nm-active-2.json")),
];
const UPOWER_DEVICES: [(&str, &str); 4] = [
    ("DisplayDevice", include_str!("../../../tests/fixtures/dbus-upower-display.json")),
    ("battery_BAT0", include_str!("../../../tests/fixtures/dbus-upower-bat0.json")),
    ("battery_hidpp_battery_1", include_str!("../../../tests/fixtures/dbus-upower-mouse.json")),
    ("line_power_AC", include_str!("../../../tests/fixtures/dbus-upower-ac.json")),
];
const UPOWER_LIST: &str = include_str!("../../../tests/fixtures/dbus-upower-devices.json");
const BLUEZ_OBJECTS: &str = include_str!("../../../tests/fixtures/dbus-bluez-objects.json");

/// Unique names of the recorded services: what `owner` (GetNameOwner) answers.
pub const NM_OWNER: &str = ":1.10";
pub const UPOWER_OWNER: &str = ":1.11";
pub const BLUEZ_OWNER: &str = ":1.12";

/// A step of a scripted session: the signal, and what changes on the bus
/// just before it is received.
pub type Step = (Signal, fn(&mut FakeBus));

pub fn signal(path: &str, interface: &str, member: &str, arg0: Option<&str>) -> Signal {
    Signal { path: path.into(), interface: interface.into(), member: member.into(), arg0: arg0.map(String::from), names: Vec::new() }
}

/// The same signal, carrying property names (PropertiesChanged) or
/// interfaces (InterfacesAdded, InterfacesRemoved).
pub fn with_names(mut s: Signal, names: &[&str]) -> Signal {
    s.names = names.iter().map(|n| n.to_string()).collect();
    s
}

pub struct FakeBus {
    /// Running services: well-known name → unique name.
    pub owners: BTreeMap<&'static str, &'static str>,
    /// (path, interface) → properties.
    pub props: BTreeMap<(String, String), Props>,
    /// Paths whose GetAll fails with a real error (an outage, not an absence).
    pub failing: BTreeSet<String>,
    /// Paths whose GetAll fails as the bus itself going away mid-call would
    /// (`io::ErrorKind::UnexpectedEof`, as `dbusrs_bus::error` maps
    /// `Disconnected`): distinct from `failing` (a service that just times
    /// out), to prove a domain's own read failing this way is tagged
    /// `Outage::Bus`, not `Outage::Domain` (I2 carry-over, tray plan 1).
    pub disconnecting: BTreeSet<String>,
    pub bluez: Objects,
    pub devices: Vec<String>,
    pub script: VecDeque<Step>,
    pub calls: Vec<String>,
}

impl FakeBus {
    /// A bus where nothing runs.
    pub fn empty() -> FakeBus {
        FakeBus {
            owners: BTreeMap::new(),
            props: BTreeMap::new(),
            failing: BTreeSet::new(),
            disconnecting: BTreeSet::new(),
            bluez: Objects::new(),
            devices: Vec::new(),
            script: VecDeque::new(),
            calls: Vec::new(),
        }
    }

    /// The workstation as recorded on 2026-10-04: Ethernet primary, the
    /// Wi-Fi up too, two WireGuard tunnels; battery charging at 92 %, a
    /// mouse at 100 %; Bluetooth on, the headset paired but not connected.
    pub fn workstation() -> FakeBus {
        let mut b = FakeBus::empty();
        b.owners = [(NM, NM_OWNER), (UPOWER, UPOWER_OWNER), (BLUEZ, BLUEZ_OWNER)].into();
        b.set("/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager", NM_ROOT);
        for (n, json) in NM_ACTIVE {
            b.set(
                &format!("/org/freedesktop/NetworkManager/ActiveConnection/{n}"),
                "org.freedesktop.NetworkManager.Connection.Active",
                json,
            );
        }
        for (device, json) in UPOWER_DEVICES {
            b.set(&format!("/org/freedesktop/UPower/devices/{device}"), "org.freedesktop.UPower.Device", json);
        }
        b.devices = busctl::paths(UPOWER_LIST);
        b.bluez = busctl::objects(BLUEZ_OBJECTS);
        b
    }

    fn set(&mut self, path: &str, interface: &str, json: &str) {
        self.props.insert((path.into(), interface.into()), busctl::get_all(json));
    }

    /// A recorded object's properties, to change one in a test.
    pub fn props_mut(&mut self, path: &str, interface: &str) -> &mut Props {
        self.props.get_mut(&(path.to_string(), interface.to_string())).expect("recorded object")
    }

    /// Synthesized (nothing was connected during the recording): the
    /// recorded headset connected, its battery at 72 %, and a second
    /// connected device, ClavierBT, with no battery.
    pub fn connect_bluetooth(&mut self) {
        let headset = self.bluez.get_mut("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02").expect("recorded headset");
        headset.get_mut("org.bluez.Device1").expect("Device1").0.insert("Connected".into(), Prop::Bool(true));
        headset.insert("org.bluez.Battery1".into(), Props([("Percentage".to_string(), Prop::Int(72))].into()));
        let keyboard = Props(
            [
                ("Address".to_string(), Prop::Str("AA:BB:CC:DD:EE:04".into())),
                ("Alias".to_string(), Prop::Str("ClavierBT".into())),
                ("Connected".to_string(), Prop::Bool(true)),
            ]
            .into(),
        );
        self.bluez.insert("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_04".into(), [("org.bluez.Device1".to_string(), keyboard)].into());
    }

    /// A call's destination must be a unique name (see the module comment).
    /// A unique name nobody holds any more: NotFound, like a real bus.
    fn reach(&self, destination: &str) -> io::Result<()> {
        assert!(destination.starts_with(':'), "call to the well-known name {destination}: D-Bus activation could start it");
        if self.owners.values().any(|o| *o == destination) {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, format!("ServiceUnknown: {destination}")))
        }
    }
}

impl Bus for FakeBus {
    fn owner(&mut self, service: &str) -> io::Result<Option<String>> {
        self.calls.push(format!("GetNameOwner {service}"));
        Ok(self.owners.get(service).map(|o| o.to_string()))
    }

    fn get_all(&mut self, destination: &str, path: &str, interface: &str) -> io::Result<Props> {
        self.calls.push(format!("GetAll {destination} {path} {interface}"));
        self.reach(destination)?;
        if self.disconnecting.contains(path) {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("Disconnected: {path}")));
        }
        if self.failing.contains(path) {
            return Err(io::Error::other(format!("timeout: {path}")));
        }
        self.props
            .get(&(path.to_string(), interface.to_string()))
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("UnknownObject: {path}")))
    }

    fn managed_objects(&mut self, destination: &str) -> io::Result<Objects> {
        self.calls.push(format!("GetManagedObjects {destination}"));
        self.reach(destination)?;
        // "/": the only path `managed_objects` ever reads (GetManagedObjects
        // is always called at the root) — a wedged BlueZ, every read timing
        // out.
        if self.failing.contains("/") {
            return Err(io::Error::other("timeout: /"));
        }
        Ok(self.bluez.clone())
    }

    fn enumerate_devices(&mut self, destination: &str) -> io::Result<Vec<String>> {
        self.calls.push(format!("EnumerateDevices {destination}"));
        self.reach(destination)?;
        Ok(self.devices.clone())
    }

    fn next_signal(&mut self) -> io::Result<Signal> {
        let Some((s, change)) = self.script.pop_front() else {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "end of the scripted session"));
        };
        change(self);
        Ok(s)
    }
}
