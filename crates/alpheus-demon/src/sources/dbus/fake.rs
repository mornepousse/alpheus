//! A system bus in memory for the tests, filled with the replies recorded on
//! the workstation (tests/fixtures/dbus-*.json, anonymized). Never a real
//! bus. Records every call, and refuses — panics on — a call addressed to a
//! well-known name: on a real bus, that call could start the service.

use super::bt::BtTap;
use super::props::{Objects, Prop, Props, busctl};
use super::vpn::VpnTap;
use super::wifi::{self, WifiTap};
use super::{BLUEZ, Bus, NM, POWER_PROFILES, Signal, UPOWER, power, service_gone, under};
use crate::system::{Saved, SavedWifi};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Arc;

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
/// Fictional: power-profiles-daemon's shape, made-up drivers and version.
const POWER_PROFILES_JSON: &str = include_str!("../../../tests/fixtures/dbus-powerprofiles.json");

/// Unique names of the recorded services: what `owner` (GetNameOwner) answers.
pub const NM_OWNER: &str = ":1.10";
pub const UPOWER_OWNER: &str = ":1.11";
pub const BLUEZ_OWNER: &str = ":1.12";
pub const POWER_OWNER: &str = ":1.13";

/// Synthesized around the recording (it has no device objects): a wired device, and the Wi-Fi
/// device of the recorded Wi-Fi connection (ActiveConnection/9).
pub const WIRED_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/1";
pub const WIFI_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/5";
pub const AP: &str = "/org/freedesktop/NetworkManager/AccessPoint";

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
    /// `Outage::Bus`, not `Outage::Domain`.
    pub disconnecting: BTreeSet<String>,
    pub bluez: Objects,
    pub devices: Vec<String>,
    /// NetworkManager's GetDevices answer; None: not there (the recording has none): NotFound.
    pub nm_devices: Option<Vec<String>>,
    /// ListConnections' answer; None: NotFound.
    pub connections: Option<Vec<String>>,
    /// GetSettings' answers by path, already reduced (None: not a Wi-Fi profile).
    pub settings: BTreeMap<String, Option<Saved>>,
    /// Paths whose ListConnections or GetSettings finds NetworkManager gone mid-call (ServiceUnknown).
    pub gone: BTreeSet<String>,
    /// Paths whose GetSettings NetworkManager refuses (Settings.PermissionDenied: another user's).
    pub denied: BTreeSet<String>,
    /// (AddMatch?, rule) refused, as a bus at its limits or timing out would (the call recorded).
    pub failing_rules: BTreeSet<(bool, String)>,
    /// AddMatch rules the bus applies, its reply then lost (a timeout after the bus took it).
    pub late_rules: BTreeSet<String>,
    /// The match rules the bus holds for this connection (added, not refused, not removed yet).
    pub rules: Vec<String>,
    /// The quick panel's tap, which a script's step sets (no socket pair: nothing to wake).
    pub tap: Arc<WifiTap>,
    /// The Bluetooth panel's, the same way.
    pub bt_tap: Arc<BtTap>,
    /// The VPN reading's, the same way.
    pub vpn_tap: Arc<VpnTap>,
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
            nm_devices: None,
            connections: None,
            settings: BTreeMap::new(),
            gone: BTreeSet::new(),
            denied: BTreeSet::new(),
            failing_rules: BTreeSet::new(),
            late_rules: BTreeSet::new(),
            rules: Vec::new(),
            tap: Arc::default(),
            bt_tap: Arc::default(),
            vpn_tap: Arc::default(),
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

    /// power-profiles-daemon running, `active` its active profile (the fixture's three offered).
    pub fn with_power_profiles(&mut self, active: &str) {
        self.owners.insert(POWER_PROFILES, POWER_OWNER);
        self.set(power::PATH, power::INTERFACE, POWER_PROFILES_JSON);
        self.props_mut(power::PATH, power::INTERFACE).0.insert("ActiveProfile".into(), Prop::Str(active.into()));
    }

    /// Synthesized around the recording (fictional networks): a wired device, then the Wi-Fi device
    /// of the recorded Wi-Fi connection, activated, on AccessPoint/10 ("MON_WIFI_1" as the recording
    /// names it, 70 %, WPA2), with 11 ("Voisin-Test", 30 %, WPA2), 12 ("Cafe-Libre", 55 %, open) and 13
    /// (hidden) around. Hardware addresses from the documentation range.
    pub fn with_wifi(&mut self) {
        let props = |pairs: Vec<(&str, Prop)>| Props(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
        self.nm_devices = Some(vec![WIRED_DEVICE.into(), WIFI_DEVICE.into()]);
        self.props.insert((WIRED_DEVICE.into(), wifi::DEVICE.into()), props(vec![("DeviceType", Prop::Int(1)), ("State", Prop::Int(100))]));
        let active = Prop::Path("/org/freedesktop/NetworkManager/ActiveConnection/9".into());
        self.props.insert(
            (WIFI_DEVICE.into(), wifi::DEVICE.into()),
            props(vec![("DeviceType", Prop::Int(2)), ("State", Prop::Int(100)), ("ActiveConnection", active)]),
        );
        let listed: Vec<String> = (10..=13).map(|n| format!("{AP}/{n}")).collect();
        self.props.insert(
            (WIFI_DEVICE.into(), wifi::WIRELESS.into()),
            props(vec![("ActiveAccessPoint", Prop::Path(format!("{AP}/10"))), ("AccessPoints", Prop::Paths(listed))]),
        );
        for (n, ssid, strength, rsn) in
            [(10, "MON_WIFI_1", 70, 0x188), (11, "Voisin-Test", 30, 0x188), (12, "Cafe-Libre", 55, 0), (13, "", 20, 0x188)]
        {
            self.props.insert(
                (format!("{AP}/{n}"), wifi::ACCESS_POINT.into()),
                props(vec![
                    ("Ssid", Prop::Bytes(ssid.as_bytes().to_vec())),
                    ("Strength", Prop::Int(strength)),
                    ("Flags", Prop::Int(if rsn == 0 { 0 } else { 1 })),
                    ("WpaFlags", Prop::Int(0)),
                    ("RsnFlags", Prop::Int(rsn)),
                    ("HwAddress", Prop::Str(format!("00:00:5E:00:53:{n:02X}"))),
                ]),
            );
        }
        // The recorded Wi-Fi connection's profile (Settings/3, its uuid in dbus-nm-active-9.json),
        // and a wired one.
        let (home, wired) = ("/org/freedesktop/NetworkManager/Settings/3", "/org/freedesktop/NetworkManager/Settings/4");
        self.connections = Some(vec![home.into(), wired.into()]);
        self.settings.insert(
            home.into(),
            Some(Saved::Wifi(SavedWifi { uuid: "00000000-0000-0000-0000-000000000009".into(), ssid: b"MON_WIFI_1".to_vec() })),
        );
        self.settings.insert(wired.into(), None);
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

    /// A device BlueZ has seen while scanning, unpaired and unconnected: named `alias` (its `Name`
    /// too), with the `rssi` of its last advertisement when it had one. Fictional.
    pub fn nearby(&mut self, path: &str, alias: &str, rssi: Option<i64>) {
        let mut p = BTreeMap::from([
            ("Name".to_string(), Prop::Str(alias.into())),
            ("Alias".to_string(), Prop::Str(alias.into())),
            ("Paired".to_string(), Prop::Bool(false)),
            ("Trusted".to_string(), Prop::Bool(false)),
            ("Connected".to_string(), Prop::Bool(false)),
            ("Adapter".to_string(), Prop::Path("/org/bluez/hci0".into())),
        ]);
        if let Some(r) = rssi {
            p.insert("RSSI".into(), Prop::Int(r));
        }
        self.bluez.insert(path.into(), [("org.bluez.Device1".to_string(), Props(p))].into());
    }

    /// A call's destination must be a unique name (see the module comment).
    /// A unique name nobody holds any more: NotFound, like a real bus.
    fn reach(&self, destination: &str) -> io::Result<()> {
        assert!(destination.starts_with(':'), "call to the well-known name {destination}: D-Bus activation could start it");
        if self.owners.values().any(|o| *o == destination) { Ok(()) } else { Err(service_gone(format!("ServiceUnknown: {destination}"))) }
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
            .or_else(|| {
                // BlueZ's objects live in `bluez` (the recording's ObjectManager answer).
                if !under(path, "/org/bluez") {
                    return None;
                }
                self.bluez.get(path).and_then(|i| i.get(interface)).cloned()
            })
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

    fn get_devices(&mut self, destination: &str) -> io::Result<Vec<String>> {
        self.calls.push(format!("GetDevices {destination}"));
        self.reach(destination)?;
        self.nm_devices.clone().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "UnknownMethod: GetDevices"))
    }

    fn list_connections(&mut self, destination: &str) -> io::Result<Vec<String>> {
        self.calls.push(format!("ListConnections {destination}"));
        self.reach(destination)?;
        if self.gone.contains(wifi::SETTINGS_PATH) {
            return Err(service_gone(format!("ServiceUnknown: {destination}")));
        }
        if self.failing.contains(wifi::SETTINGS_PATH) {
            return Err(io::Error::other(format!("timeout: {}", wifi::SETTINGS_PATH)));
        }
        self.connections.clone().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "UnknownMethod: ListConnections"))
    }

    fn get_settings(&mut self, destination: &str, path: &str) -> io::Result<Option<Saved>> {
        self.calls.push(format!("GetSettings {destination} {path}"));
        self.reach(destination)?;
        if self.gone.contains(path) {
            return Err(service_gone(format!("ServiceUnknown: {destination}")));
        }
        if self.denied.contains(path) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("PermissionDenied: {path}")));
        }
        if self.failing.contains(path) {
            return Err(io::Error::other(format!("timeout: {path}")));
        }
        self.settings.get(path).cloned().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("UnknownObject: {path}")))
    }

    fn match_rule(&mut self, rule: &str, add: bool) -> io::Result<()> {
        self.calls.push(format!("{} {rule}", if add { "AddMatch" } else { "RemoveMatch" }));
        if self.failing_rules.contains(&(add, rule.to_string())) {
            return Err(io::Error::other(format!("timeout: {rule}")));
        }
        if add {
            self.rules.push(rule.to_string());
            if self.late_rules.contains(rule) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("NoReply: {rule}")));
            }
        } else {
            // As the bus driver answers a rule it does not hold.
            let i =
                self.rules.iter().position(|r| r == rule).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "MatchRuleNotFound"))?;
            self.rules.remove(i);
        }
        Ok(())
    }

    fn next_signal(&mut self) -> io::Result<Signal> {
        let Some((s, change)) = self.script.pop_front() else {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "end of the scripted session"));
        };
        change(self);
        Ok(s)
    }
}
