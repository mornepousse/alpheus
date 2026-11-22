//! Which signals matter: the allowlists of properties per object, and what a `Signal` says
//! (the domain to re-read, a detail change, a wake, a poke).

use super::*;

/// `path` is `root` or below it.
pub(super) fn under(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// The NetworkManager root object's properties the bar reads (the quick panel's Wi-Fi row follows
/// the radio).
const NM_ROOT_PROPS: [&str; 4] = ["PrimaryConnection", "ActiveConnections", "Connectivity", "WirelessEnabled"];
/// An active connection's properties the bar reads.
const NM_ACTIVE_PROPS: [&str; 1] = ["State"];
/// A UPower device's properties the bar reads (the laptop's DisplayDevice,
/// or a peripheral — never a raw battery_BAT0 or line_power_*, dropped
/// below by `upower_device_ignored` regardless of which property changed).
/// `Model`: a peripheral's name (`upower::read`'s `Peripheral::name`) —
/// missing from this list (re-review, follow-up): a rename wouldn't have
/// re-read it until something else happened to change too. `BatteryLevel`
/// (Mae, 2026-10-05): a coarse-reporting peripheral (a Logitech HID++
/// receiver…) going Low or Critical, or recovering from it, must re-read.
const UPOWER_PROPS: [&str; 7] = ["Percentage", "State", "IsPresent", "Type", "PowerSupply", "Model", "BatteryLevel"];
/// BlueZ: the properties the bar reads, by interface.
const BLUEZ_ADAPTER_PROPS: [&str; 1] = ["Powered"];
// Deviation from the brief's Device1{Connected, Alias, Name} (reported):
// `Name` stays out of the live allowlist. `bluez::read` does fall back to
// it, but only once Alias is absent — rare for anything that ever got
// paired — while BlueZ keeps correcting a scanned device's advertised Name
// as it collects more of it, right alongside RSSI/UUIDs; keeping it in
// would defeat the scan-storm test below (a batch carrying Name, even with
// nothing else relevant, would re-read on every such correction).
// Settled (re-review, follow-up): a device with no alias of its own has
// BlueZ mirror its Name into Alias, so a rename of an unaliased device
// already arrives as an Alias change too — Name carries nothing Alias
// doesn't, for anything the bar actually shows.
const BLUEZ_DEVICE_PROPS: [&str; 2] = ["Connected", "Alias"];
const BLUEZ_BATTERY_PROPS: [&str; 1] = ["Percentage"];
/// power-profiles-daemon: what the quick panel shows.
const POWER_PROFILES_PROPS: [&str; 2] = ["ActiveProfile", "Profiles"];

/// systemd's Manager: the failed-units count, the only property of it that emits a change.
const SYSTEMD_PROPS: [&str; 1] = [units::PROPERTY];

/// The properties of `path`/`interface` the bar reads (I1): an
/// ALLOWLIST, replacing the denylist (`NOISE`) this used to be — a property
/// not named here can't change what the bar shows. None: an interface the
/// bar never reads at all (BlueZ's MediaTransport1, MediaPlayer1…).
fn relevant_properties(path: &str, interface: Option<&str>) -> Option<&'static [&'static str]> {
    if under(path, "/org/freedesktop/NetworkManager/ActiveConnection") {
        Some(&NM_ACTIVE_PROPS)
    } else if under(path, "/org/freedesktop/NetworkManager") {
        Some(&NM_ROOT_PROPS)
    } else if under(path, "/org/freedesktop/UPower") {
        Some(&UPOWER_PROPS)
    } else if under(path, power::PATH) {
        Some(&POWER_PROFILES_PROPS)
    } else if path == units::PATH {
        Some(&SYSTEMD_PROPS)
    } else if under(path, "/org/bluez") {
        match interface {
            Some("org.bluez.Adapter1") => Some(&BLUEZ_ADAPTER_PROPS),
            Some("org.bluez.Device1") => Some(&BLUEZ_DEVICE_PROPS),
            Some("org.bluez.Battery1") => Some(&BLUEZ_BATTERY_PROPS),
            _ => None,
        }
    } else {
        None
    }
}

/// UPower's own native name for the laptop's built-in battery (whatever
/// kernel driver exposes it): short, no underscore — `BAT0`, `BAT1`, the
/// ThinkPad-style `BATC`/`BATT`, `CMB0`/`CMB1`… A peripheral's native name
/// always carries its own driver's prefix, joined with an underscore
/// (`hidpp_battery_1`, `sony_controller_battery_0`…), which is what still
/// tells the two apart (follow-up, re-review: not only `BATn`).
fn looks_like_builtin_battery_name(native: &str) -> bool {
    !native.is_empty() && !native.contains('_')
}

/// UPower emits `PropertiesChanged` for every device it tracks, including
/// the laptop's raw battery (`battery_BAT0`…) and the line power supplies:
/// the bar never reads those objects directly — `DisplayDevice`
/// already aggregates the laptop battery, and a peripheral is read by its
/// own device path, named differently (`battery_hidpp_battery_1`…). A
/// `path_namespace` match rule can't exclude a few device names by itself,
/// so this drops them here instead, regardless of which property changed.
fn upower_device_ignored(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    name.starts_with("line_power_") || name.strip_prefix("battery_").is_some_and(looks_like_builtin_battery_name)
}

impl Signal {
    /// The loop's word through the `WifiTap` (quick Wi-Fi spec § 5): made by `next_signal`, never
    /// received: every D-Bus signal carries a path and a member, this one neither.
    pub fn poke() -> Signal {
        Signal { path: String::new(), interface: String::new(), member: String::new(), arg0: None, names: Vec::new() }
    }

    pub fn is_poke(&self) -> bool {
        self.path.is_empty() && self.member.is_empty()
    }

    /// A Wi-Fi detail signal (quick Wi-Fi spec § 5), heard only while its rules are on: an access
    /// point's properties the panel shows, the device's list, the saved connections. None: not one.
    pub fn wifi_change(&self) -> Option<wifi::WifiChange> {
        const AP_PROPS: [&str; 5] = ["Ssid", "Strength", "Flags", "WpaFlags", "RsnFlags"];
        match self.member.as_str() {
            "PropertiesChanged" if under(&self.path, wifi::AP_ROOT) => {
                let read = self.names.is_empty() || self.names.iter().any(|n| AP_PROPS.contains(&n.as_str()));
                read.then(|| wifi::WifiChange::AccessPoint(self.path.clone()))
            }
            "AccessPointAdded" | "AccessPointRemoved" if self.interface == wifi::WIRELESS => Some(wifi::WifiChange::AccessPoints),
            "NewConnection" | "ConnectionRemoved" if self.interface == wifi::SETTINGS => Some(wifi::WifiChange::Saved),
            _ => None,
        }
    }

    /// A Bluetooth panel signal (quick settings delivery 3), heard only while the panel is open:
    /// what a device's or the adapter's properties changed to show, or a device appearing or going.
    /// None: not one (the bar's own signals, a scan's noise). `domain` does not change.
    pub fn bt_change(&self) -> Option<bt::BtChange> {
        const DEVICE_PROPS: [&str; 6] = ["Name", "Alias", "Icon", "Paired", "Trusted", "Connected"];
        const ADAPTER_PROPS: [&str; 2] = ["Powered", "PowerState"];
        let has = |n: &str| self.names.iter().any(|x| x == n);
        match self.member.as_str() {
            "PropertiesChanged" if under(&self.path, "/org/bluez") => match self.arg0.as_deref() {
                Some(bluez::DEVICE) if self.names.is_empty() || DEVICE_PROPS.iter().any(|n| has(n)) => {
                    Some(bt::BtChange::Device { path: self.path.clone(), rssi_only: false })
                }
                Some(bluez::DEVICE) if has("RSSI") => Some(bt::BtChange::Device { path: self.path.clone(), rssi_only: true }),
                Some(bluez::ADAPTER) if self.names.is_empty() || ADAPTER_PROPS.iter().any(|n| has(n)) => {
                    Some(bt::BtChange::Adapter(self.path.clone()))
                }
                _ => None,
            },
            "InterfacesAdded" | "InterfacesRemoved" if self.interface == OBJECT_MANAGER && has(bluez::DEVICE) => {
                let at = self.arg0.as_deref().filter(|p| under(p, "/org/bluez"))?;
                Some(if self.member == "InterfacesAdded" { bt::BtChange::Added(at.into()) } else { bt::BtChange::Removed(at.into()) })
            }
            _ => None,
        }
    }

    /// The domain to re-read on this signal; None: not ours, or nothing the
    /// bar shows can have changed.
    pub fn domain(&self) -> Option<Domain> {
        if self.member == "NameOwnerChanged" {
            return match self.arg0.as_deref()? {
                NM => Some(Domain::Network),
                BLUEZ => Some(Domain::Bluetooth),
                UPOWER => Some(Domain::Battery),
                POWER_PROFILES => Some(Domain::Power),
                SYSTEMD1 => Some(Domain::Units),
                _ => None,
            };
        }
        let d = if under(&self.path, "/org/freedesktop/NetworkManager") {
            Domain::Network
        } else if under(&self.path, "/org/freedesktop/UPower") {
            Domain::Battery
        } else if under(&self.path, power::PATH) {
            Domain::Power
        } else if self.path == units::PATH {
            Domain::Units
        } else if under(&self.path, "/org/bluez") || (self.path == "/" && self.interface == OBJECT_MANAGER) {
            Domain::Bluetooth
        } else {
            return None;
        };
        self.relevant(d).then_some(d)
    }

    /// `logind`'s `PrepareForSleep(false)`: the machine just resumed, not
    /// going to sleep. Every shown domain is re-read once — a VPN
    /// may have reconnected, a Bluetooth device too, the battery drained
    /// while asleep. Reading only: the daemon never calls a logind method
    /// (Suspend, Hibernate, PowerOff… the guardrail already forbids the
    /// words).
    pub fn is_wake(&self) -> bool {
        self.interface == LOGIN1_MANAGER && self.member == "PrepareForSleep" && self.arg0.as_deref() == Some("false")
    }

    /// Whether this signal, already narrowed to `d` by its path, can
    /// actually change what the bar shows for `d` (I1): an
    /// allowlist of the properties/interfaces the bar reads, not a denylist
    /// of known noise.
    fn relevant(&self, d: Domain) -> bool {
        if d == Domain::Battery && upower_device_ignored(&self.path) {
            return false;
        }
        match self.member.as_str() {
            "PropertiesChanged" => {
                if self.names.is_empty() {
                    return true; // unknown change: the signal counts
                }
                relevant_properties(&self.path, self.arg0.as_deref())
                    .is_some_and(|allowed| self.names.iter().any(|n| allowed.contains(&n.as_str())))
            }
            // Symmetric with InterfacesAdded: a connected device
            // always sends Connected=false before being removed, so its own
            // removal re-reads nothing; only a new/removed adapter or
            // battery can change the bar.
            "InterfacesAdded" | "InterfacesRemoved" if d == Domain::Bluetooth => {
                self.names.is_empty() || self.names.iter().any(|i| i == "org.bluez.Adapter1" || i == "org.bluez.Battery1")
            }
            _ => true,
        }
    }
}
