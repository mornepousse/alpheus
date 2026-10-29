//! The system D-Bus, READ ONLY: NetworkManager, UPower, BlueZ, power-profiles-daemon.
//! One connection on one thread, blocked on the bus while nothing changes; each
//! signal re-reads the domain it concerns. `Bus` only has reads (checked by
//! tests/guardrails.rs), and a service is only ever called through the
//! unique name of the process that owns it: never through its well-known
//! name, which D-Bus activation would use to start it.

pub mod bluez;
pub mod bt;
pub mod bt_act;
pub mod dbusrs_bus;
pub mod network;
pub mod power;
pub mod props;
pub mod tap;
pub mod upower;
pub mod wifi;

#[cfg(test)]
#[path = "../../../tests/support/busctl_wire.rs"]
mod busctl_wire;
#[cfg(test)]
pub mod fake;

use super::{Backoff, STABLE_DURATION, SourceGuard, panic_text, prefixed};
use crate::log::write as log;
use crate::memory::release_memory;
use crate::state::Msg;
use crate::system::{BtScope, BtState, BtView, NetState, PowerState, ProfileState, SavedWifi, WifiScope, WifiState};
use bt::{BtReader, BtTap};
use props::{Objects, Props};
use std::io;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wifi::{WifiReader, WifiTap};

pub const NM: &str = "org.freedesktop.NetworkManager";
pub const UPOWER: &str = "org.freedesktop.UPower";
pub const BLUEZ: &str = "org.bluez";
/// power-profiles-daemon (quick spec § 5): read, never set (the panel runs powerprofilesctl).
pub const POWER_PROFILES: &str = "net.hadess.PowerProfiles";
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
    /// `org.freedesktop.NetworkManager.GetDevices` (quick Wi-Fi spec § 5): the devices' paths.
    fn get_devices(&mut self, destination: &str) -> io::Result<Vec<String>>;
    /// `org.freedesktop.NetworkManager.Settings.ListConnections`: the saved connections' paths.
    fn list_connections(&mut self, destination: &str) -> io::Result<Vec<String>>;
    /// `org.freedesktop.NetworkManager.Settings.Connection.GetSettings` on `path`, reduced to a Wi-Fi
    /// profile's uuid and SSID (`wifi::saved_wifi`); None for any other kind. Never its secrets.
    fn get_settings(&mut self, destination: &str, path: &str) -> io::Result<Option<SavedWifi>>;
    /// AddMatch (`add`) or RemoveMatch of one rule.
    fn match_rule(&mut self, rule: &str, add: bool) -> io::Result<()>;
    /// AddMatch (`add`) or RemoveMatch of each rule, in order: the Wi-Fi detail's (`WIFI_RULES`).
    /// All or nothing when adding: one refused takes back those already added and itself (a reply
    /// lost to a timeout may come after the bus took it; final review A-M4), their own refusals
    /// ignored (MatchRuleNotFound: never taken), so none is left on the bus that nobody would
    /// remove. Removing tries every rule and returns the first refusal; a rule the bus no longer
    /// holds (MatchRuleNotFound) is what removing asks, not one.
    fn match_rules(&mut self, rules: &[&str], add: bool) -> io::Result<()> {
        let mut first = None;
        for (i, r) in rules.iter().enumerate() {
            if let Err(e) = self.match_rule(r, add) {
                if add {
                    for done in &rules[..=i] {
                        let _ = self.match_rule(done, false);
                    }
                    return Err(e);
                }
                if e.kind() != io::ErrorKind::NotFound {
                    first.get_or_insert(e);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
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
    Power,
}

/// Every domain, in reading order.
pub const DOMAINS: [Domain; 4] = [Domain::Network, Domain::Bluetooth, Domain::Battery, Domain::Power];

/// `path` is `root` or below it.
fn under(path: &str, root: &str) -> bool {
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

/// The properties of `path`/`interface` the bar reads (I1, review): an
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
/// the laptop's raw battery (`battery_BAT0`…) and the line power supplies
/// (M5, review): the bar never reads those objects directly — `DisplayDevice`
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
                _ => None,
            };
        }
        let d = if under(&self.path, "/org/freedesktop/NetworkManager") {
            Domain::Network
        } else if under(&self.path, "/org/freedesktop/UPower") {
            Domain::Battery
        } else if under(&self.path, power::PATH) {
            Domain::Power
        } else if under(&self.path, "/org/bluez") || (self.path == "/" && self.interface == OBJECT_MANAGER) {
            Domain::Bluetooth
        } else {
            return None;
        };
        self.relevant(d).then_some(d)
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

    /// Whether this signal, already narrowed to `d` by its path, can
    /// actually change what the bar shows for `d` (I1, review): an
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
            // Symmetric with InterfacesAdded (review): a connected device
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

/// The service a call went to is gone (ServiceUnknown, NameHasNoOwner): an absence (NotFound) like
/// any other, marked so a reader can tell it from one object gone (UnknownObject).
#[derive(Debug)]
struct ServiceGone(String);

impl std::fmt::Display for ServiceGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceGone {}

pub fn service_gone(text: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, ServiceGone(text))
}

/// `e` says the service itself is gone (`service_gone`), not one of its objects.
pub fn is_service_gone(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|r| r.is::<ServiceGone>())
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemState {
    pub network: Option<NetState>,
    pub bluetooth: Option<BtState>,
    pub battery: Option<PowerState>,
    pub power: Option<ProfileState>,
    /// The quick panel's Wi-Fi, only while it is open (quick Wi-Fi spec § 5); None otherwise.
    pub wifi: Option<WifiState>,
    /// The open `wifi` was read for (`WifiTap::snapshot`): the panel drops a state read for an
    /// earlier open (final review B-M1).
    pub wifi_open: u64,
    /// The quick panel's Bluetooth, only while it is open (quick settings delivery 3); None
    /// otherwise. The bar's `bluetooth` is separate and unchanged.
    pub bt: Option<BtView>,
    /// The open `bt` was read for (`BtTap::snapshot`), as `wifi_open`.
    pub bt_open: u64,
}

/// Why a session ended (I2, review): a single domain's read failing, its
/// own "–" already published while the others keep their last published
/// value — versus the bus itself going away (the connection lost, or never
/// made), which blanks all three: nothing can be trusted then. Carried by
/// the `io::Error` `session` returns, as an explicit tag (`tag`,
/// `outage_of`), never by matching the error's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outage {
    Domain(Domain),
    Bus,
}

/// Ties an `Outage` to the real error without losing it: `Display` and
/// `source` both delegate to it, only `outage_of` reads the tag back.
#[derive(Debug)]
struct Tagged {
    outage: Outage,
    source: io::Error,
}

impl std::fmt::Display for Tagged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for Tagged {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn tag(source: io::Error, outage: Outage) -> io::Error {
    io::Error::new(source.kind(), Tagged { outage, source })
}

/// The `Outage` an `io::Error` from `session` carries; `Bus` if it isn't
/// tagged (`next_signal` failing, the connection never coming up — nothing
/// ties those to one domain).
fn outage_of(e: &io::Error) -> Outage {
    e.get_ref().and_then(|r| r.downcast_ref::<Tagged>()).map_or(Outage::Bus, |t| t.outage)
}

/// Whether a read failure is worth a log line (M2, review): a real failure
/// is; an absence (NotFound: the service left after `GetNameOwner`, its
/// `NameOwnerChanged` follows and re-reads it) isn't — logging it on every
/// such ordinary comings and goings of a service would be noise. Rate
/// limiting for a failure that keeps recurring stays in `supervise`
/// (`first || succeeded`), not duplicated here.
fn worth_logging(kind: io::ErrorKind) -> bool {
    kind != io::ErrorKind::NotFound
}

/// `d`'s index into `DomainLog`'s per-domain array.
fn domain_index(d: Domain) -> usize {
    match d {
        Domain::Network => 0,
        Domain::Bluetooth => 1,
        Domain::Battery => 2,
        Domain::Power => 3,
    }
}

/// Per-domain log rate limiting (follow-up, re-review): `read` runs again,
/// fresh, on every backoff round — a new `session`, a new `SystemState` —
/// so without state that outlives one round, a domain stuck down would
/// repeat the same log line every round, forever. One `DomainLog` lives for
/// as long as the D-Bus thread does (owned by `start`'s closure, across
/// every `run_supervised` round), not reset by a fresh `SystemState`.
#[derive(Debug, Default)]
pub struct DomainLog([bool; DOMAINS.len()]);

impl DomainLog {
    /// Whether this failure of `d` is worth a line: the first one, or the
    /// first since its last success. Records the new state either way.
    fn note_failure(&mut self, d: Domain) -> bool {
        let i = domain_index(d);
        let first = !self.0[i];
        self.0[i] = true;
        first
    }

    /// `d` just succeeded: its next failure is worth a line again.
    fn note_success(&mut self, d: Domain) {
        self.0[domain_index(d)] = false;
    }
}

/// Re-reads `d` into `s`. Err: the read failed for real (a timeout…), the
/// domain is now None, tagged `Outage::Domain(d)` (I2, review) so a caller
/// can blank just that domain rather than the whole state — unless the
/// failure is the bus itself going away mid-call (`UnexpectedEof`), tagged
/// `Outage::Bus` instead (I2 carry-over, tray plan 1 review): nothing can be
/// trusted then, not just `d`. An absence (NotFound) also gives None, but is
/// not an error.
fn read(bus: &mut dyn Bus, d: Domain, s: &mut SystemState, log_state: &mut DomainLog) -> io::Result<()> {
    let r = match d {
        Domain::Network => network::read(bus).map(|v| s.network = Some(v)),
        Domain::Bluetooth => bluez::read(bus).map(|v| s.bluetooth = Some(v)),
        Domain::Battery => upower::read(bus).map(|v| s.battery = Some(v)),
        Domain::Power => power::read(bus).map(|v| s.power = Some(v)),
    };
    let Err(e) = r else {
        log_state.note_success(d);
        return Ok(());
    };
    match d {
        Domain::Network => s.network = None,
        Domain::Bluetooth => s.bluetooth = None,
        Domain::Battery => s.battery = None,
        Domain::Power => s.power = None,
    }
    // Rate limited per domain (follow-up, re-review): NotFound never logs
    // (worth_logging), and a short-circuiting `&&` means it never touches
    // `log_state` either — an absence doesn't arm or disarm the limiter.
    if worth_logging(e.kind()) && log_state.note_failure(d) {
        log(&format!("D-Bus {d:?}: {e}"));
    }
    if e.kind() == io::ErrorKind::NotFound {
        return Ok(());
    }
    // A bus disconnect mid-call (I2 carry-over, tray plan 1 review) is the
    // whole connection going away, not this domain alone: tagging it
    // `Outage::Domain(d)` would leave the other domains showing their last
    // value for a whole backoff round after nothing can be trusted any more.
    let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(d) };
    Err(tag(e, outage))
}

/// Reads every domain in `domains` into `s`; the first real failure (not an
/// absence), if any — unless a later failure is the bus itself going away
/// (`Outage::Bus`, fix round, review item 6): that one always wins over an
/// earlier `Outage::Domain`, stale the moment the connection itself is
/// gone. Used both for the initial state and for logind's
/// `PrepareForSleep(false)`, which re-reads all of them at once.
fn read_all(bus: &mut dyn Bus, domains: &[Domain], s: &mut SystemState, log_state: &mut DomainLog) -> Option<io::Error> {
    let mut failed: Option<io::Error> = None;
    for d in domains {
        if let Err(e) = read(bus, *d, s, log_state) {
            let already_bus = failed.as_ref().is_some_and(|f| outage_of(f) == Outage::Bus);
            if failed.is_none() || (!already_bus && outage_of(&e) == Outage::Bus) {
                failed = Some(e);
            }
        }
    }
    failed
}

/// Reads the wanted domains, publishes, then waits for signals: each one of
/// a wanted domain re-reads it and publishes the whole state. Returns Err
/// when the bus goes away or a domain's read fails (after publishing it as
/// "–"): the supervisor shows "–" and retries with a growing delay.
/// `log_state` rate-limits each domain's own failure log line (follow-up,
/// re-review) — it must outlive this one call to do that, so it's the
/// caller's (`start`'s closure persists across every backoff round;
/// `session`'s own locals, like `s`, do not).
/// Calls `publish` only when `s` differs from the last state actually
/// published (B-m6, review): a signal that passed `relevant`'s allowlist
/// can still leave every field `session` tracks unchanged (two different
/// allowed properties parsing to the same `SystemState`, a duplicate
/// signal…), and publishing it anyway would reach the bar's channel, and
/// cost a redraw, for nothing. `last` starts at `None`, which no real
/// `SystemState` equals, so the very first call always publishes — the bar
/// must leave "…" (Waiting) even when every domain reads as absent.
/// Returns what `publish` returned, or `true` (nothing to report) when
/// skipped; `last` is only updated on an actual, accepted publish.
fn publish_if_changed(last: &mut Option<SystemState>, s: &SystemState, publish: &mut dyn FnMut(SystemState) -> bool) -> bool {
    if last.as_ref() == Some(s) {
        return true;
    }
    let ok = publish(s.clone());
    if ok {
        *last = Some(s.clone());
    }
    ok
}

/// A Wi-Fi read's result into `s`. Err when the bus itself went away, tagged so (every domain
/// blanked, never the network domain alone), or when a detail rule could not be removed: the
/// network domain's outage (final review A-M4), so the session ends and the reconnection drops
/// every rule, the other modules kept meanwhile.
fn wifi_into(r: io::Result<Option<WifiState>>, s: &mut SystemState) -> io::Result<()> {
    match r {
        Ok(w) => {
            s.wifi = w;
            Ok(())
        }
        Err(e) => {
            s.wifi = None;
            let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(Domain::Network) };
            Err(tag(e, outage))
        }
    }
}

/// A Bluetooth read's result into `s`, as `wifi_into`: the bus going away is a bus outage, any other
/// error the Bluetooth domain's (the view dropped either way).
fn bt_into(r: io::Result<Option<BtView>>, s: &mut SystemState) -> io::Result<()> {
    match r {
        Ok(v) => {
            s.bt = v;
            Ok(())
        }
        Err(e) => {
            s.bt = None;
            let outage = if e.kind() == io::ErrorKind::UnexpectedEof { Outage::Bus } else { Outage::Domain(Domain::Bluetooth) };
            Err(tag(e, outage))
        }
    }
}

/// `wifi`, `bt`: what the open quick panel asks (quick Wi-Fi spec § 5): read when the session starts,
/// again on each of its pokes and with every network re-read; Closed reads nothing.
pub fn session(
    bus: &mut dyn Bus,
    domains: &[Domain],
    log_state: &mut DomainLog,
    wifi: &WifiTap,
    bt: &BtTap,
    publish: &mut dyn FnMut(SystemState) -> bool,
) -> io::Result<()> {
    let mut s = SystemState::default();
    let mut reader = WifiReader::default();
    let mut bt_reader = BtReader::default();
    let mut last = None;
    let mut failed = read_all(bus, domains, &mut s, log_state);
    // A panel already open when this session starts (a reconnection): its Wi-Fi read now. A word
    // left waiting from the previous session is answered by this read: drained, never read twice.
    wifi.drain();
    let (scope, open) = wifi.snapshot();
    s.wifi_open = open;
    if let Err(e) = wifi_into(reader.rescope(bus, scope), &mut s) {
        failed = Some(e);
    }
    bt.drain();
    let (bt_scope, bt_now) = bt.snapshot();
    s.bt_open = bt_now;
    if let Err(e) = bt_into(bt_reader.rescope(bus, bt_scope), &mut s) {
        failed = Some(e);
    }
    if !publish_if_changed(&mut last, &s, publish) {
        return Ok(());
    }
    // Right after the initial publish (follow-up, re-review): the startup
    // GetAll/GetManagedObjects/EnumerateDevices replies are done allocating
    // by now, and `malloc_trim` is cheap (a few µs) — freeing here, rather
    // than waiting for a first signal that may be seconds or minutes away,
    // gives the lower startup footprint (budget: 15 MB) sooner.
    release_memory();
    if let Some(e) = failed {
        return Err(e);
    }
    // A second, equally cheap call once the first signal's own read off the
    // socket (inside `next_signal`) has had its chance to grow a buffer of
    // its own — kept alongside the one above rather than instead of it.
    let mut drained = false;
    loop {
        let signal = bus.next_signal()?;
        if !drained {
            release_memory();
            drained = true;
        }
        if signal.is_poke() {
            // A close and a reopen read as one poke (the scope unchanged) are still a new open:
            // closed first, so everything is read again; its state tagged with the new open, and so
            // published even when it reads the same.
            let (scope, open) = wifi.snapshot();
            let reopened = open != s.wifi_open && scope != WifiScope::Closed;
            s.wifi_open = open;
            let r = if reopened {
                reader.rescope(bus, WifiScope::Closed).and_then(|_| reader.rescope(bus, scope))
            } else {
                reader.rescope(bus, scope)
            };
            let r = wifi_into(r, &mut s);
            // The same for Bluetooth, read whichever tap spoke (a poke does not say which).
            let (bscope, bopen) = bt.snapshot();
            let breopened = bopen != s.bt_open && bscope != BtScope::Closed;
            s.bt_open = bopen;
            let b = if breopened {
                bt_reader.rescope(bus, BtScope::Closed).and_then(|_| bt_reader.rescope(bus, bscope))
            } else {
                bt_reader.rescope(bus, bscope)
            };
            let b = bt_into(b, &mut s);
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            // The panel closed: what this thread's arena held for it goes back, as at startup.
            if reader.scope() == WifiScope::Closed && bt_reader.scope() == BtScope::Closed {
                release_memory();
            }
            r?;
            b?;
            continue;
        }
        if signal.is_wake() {
            // The machine just resumed: anything may have changed while it
            // slept (a VPN reconnected, a Bluetooth device, the battery) —
            // re-read every shown domain, not just the one a match rule
            // would otherwise point to.
            let mut failed = read_all(bus, domains, &mut s, log_state);
            if let Err(e) = wifi_into(reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if let Err(e) = bt_into(bt_reader.reread(bus), &mut s) {
                failed = Some(e);
            }
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            if let Some(e) = failed {
                return Err(e);
            }
            continue;
        }
        // A detail signal (its rules are on only while the detail is open): never a domain's
        // re-read, an access point's change reads that access point only.
        let change = signal.wifi_change();
        if change.is_some() || under(&signal.path, wifi::AP_ROOT) {
            if let Some(change) = change
                && reader.scope() == WifiScope::Detail
            {
                let r = wifi_into(reader.changed(bus, &change), &mut s);
                if !publish_if_changed(&mut last, &s, publish) {
                    return Ok(());
                }
                r?;
            }
            continue;
        }
        // A Bluetooth panel signal: never `continue`d (Connected matters to the bar as well).
        let change = signal.bt_change();
        let b = match &change {
            // `Ok(None)`: nothing read (the row, a device outside the detail): `s.bt` is left as it is.
            Some(c) => match bt_reader.changed(bus, c) {
                Ok(None) => Ok(()),
                r => bt_into(r, &mut s),
            },
            None => Ok(()),
        };
        let Some(d) = signal.domain().filter(|d| domains.contains(d)) else {
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            b?;
            continue;
        };
        let r = read(bus, d, &mut s, log_state);
        let w = if d == Domain::Network { wifi_into(reader.reread(bus), &mut s) } else { Ok(()) };
        // BlueZ itself changed (its owner, the adapter's or a battery's properties): the panel's
        // view is read again on the owner now there.
        let b = if d == Domain::Bluetooth && change.is_none() { bt_into(bt_reader.reread(bus), &mut s).and(b) } else { b };
        if !publish_if_changed(&mut last, &s, publish) {
            return Ok(());
        }
        // The Wi-Fi read fails only with the bus itself gone: that outage wins over the domain's.
        w?;
        b?;
        r?;
    }
}

/// The bar's messages for a published state; None (the whole bus down): "–" for every module, no
/// power profile, no Wi-Fi.
pub fn messages(s: Option<SystemState>) -> [Msg; 6] {
    match s {
        Some(s) => [
            Msg::Network(s.network),
            Msg::Bluetooth(s.bluetooth),
            Msg::Battery(s.battery),
            Msg::Profile(s.power),
            Msg::Wifi { open: Some(s.wifi_open), state: s.wifi },
            Msg::Bt { open: Some(s.bt_open), state: s.bt },
        ],
        None => [
            Msg::Network(None),
            Msg::Bluetooth(None),
            Msg::Battery(None),
            Msg::Profile(None),
            Msg::Wifi { open: None, state: None },
            Msg::Bt { open: None, state: None },
        ],
    }
}

/// Like `sources::supervise`, specialized for the D-Bus source (I2,
/// review): the "–" shown during the backoff delay depends on why the
/// session ended — a single domain's outage (`Outage::Domain`) keeps the
/// others' last published value (the session already showed "–" for just
/// that one domain), the bus itself going away (`Outage::Bus`) blanks all
/// three, since nothing can be trusted then. Otherwise the same rules as
/// `sources::supervise`: growing delay, a panic handled like an outage, a
/// stable session resets the backoff.
fn run_supervised(
    mut session: impl FnMut(&mut dyn FnMut(SystemState) -> bool) -> io::Result<()>,
    send: &(impl Fn(Option<SystemState>) -> bool + ?Sized),
    mut sleep: impl FnMut(Duration),
    mut now: impl FnMut() -> Instant,
    mut log: impl FnMut(&str),
) {
    let mut backoff = Backoff::new();
    let mut first = true;
    let mut last = SystemState::default();
    loop {
        let mut publications = 0u32;
        let mut closed = false;
        let start = now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            session(&mut |s: SystemState| {
                publications += 1;
                last = s.clone();
                let ok = send(Some(s));
                closed |= !ok;
                ok
            })
        }));
        let r: io::Result<()> = result.unwrap_or_else(|payload| Err(io::Error::other(format!("panic: {}", panic_text(&*payload)))));
        if closed {
            return;
        }
        let elapsed = now().saturating_duration_since(start);
        let succeeded = publications >= 2 || (publications >= 1 && elapsed >= STABLE_DURATION);
        if succeeded {
            backoff.on_success();
        }
        let down = match &r {
            Err(e) => {
                if first || succeeded {
                    log(&format!("source down: {e}"));
                }
                match outage_of(e) {
                    // Never the Wi-Fi last read on NetworkManager's own outage (final review
                    // A-M5): a detail's lists, stale by the time the session is back; the panel
                    // hears none until it is. Another domain's outage keeps it (F3 I1), marked
                    // stale (F4): read before the outage, it may no longer hold by the backoff's
                    // end, so an attempt never deletes on it; the next session reads it afresh.
                    Outage::Domain(Domain::Network) => Some(SystemState { wifi: None, ..last.clone() }),
                    // BlueZ's own outage: the panel's Bluetooth view is dropped (nothing to trust
                    // until the next session reads it), the Wi-Fi kept stale as any other's.
                    Outage::Domain(Domain::Bluetooth) => {
                        let mut kept = SystemState { bt: None, ..last.clone() };
                        if let Some(w) = kept.wifi.as_mut() {
                            w.stale = true;
                        }
                        Some(kept)
                    }
                    Outage::Domain(_) => {
                        let mut kept = last.clone();
                        if let Some(w) = kept.wifi.as_mut() {
                            w.stale = true;
                        }
                        Some(kept)
                    }
                    Outage::Bus => None,
                }
            }
            Ok(()) => None,
        };
        first = false;
        if !send(down) {
            return;
        }
        sleep(backoff.next_delay());
    }
}

/// An unused module costs nothing (M4, review): with no domain shown,
/// `start` neither connects nor spawns a thread.
fn wanted(domains: &[Domain]) -> bool {
    !domains.is_empty()
}

/// Starts the D-Bus source in its thread (`D-Bus`) for the `domains` of
/// the shown modules: one connection to the system bus per session, made
/// again with a growing delay if lost or if a domain's read fails. A
/// domain's own outage shows "–" for it alone, the others keep their last
/// value during the delay (I2, review); the bus itself going away blanks
/// all three. `wifi`: the quick panel's tap (quick Wi-Fi spec § 5), polled beside the bus; None: a
/// tap nothing ever sets.
pub fn start(
    domains: Vec<Domain>,
    wifi: Option<Arc<WifiTap>>,
    bt: Option<Arc<BtTap>>,
    send: impl Fn(Option<SystemState>) -> bool + Send + 'static,
) {
    if !wanted(&domains) {
        return;
    }
    let result = std::thread::Builder::new().name("D-Bus".into()).spawn(move || {
        let _guard = SourceGuard { name: "D-Bus", send: &send, value: PhantomData };
        // Owned here, across every `run_supervised` round (follow-up,
        // re-review): a domain stuck down must log once, not once per round.
        let mut log_state = DomainLog::default();
        let tap = wifi.unwrap_or_default();
        let bt_tap = bt.unwrap_or_default();
        let run = move |p: &mut dyn FnMut(SystemState) -> bool| {
            let mut bus = dbusrs_bus::DbusRsBus::system(&domains)?.waking(tap.clone()).waking_bt(bt_tap.clone());
            session(&mut bus, &domains, &mut log_state, &tap, &bt_tap, p)
        };
        run_supervised(run, &send, std::thread::sleep, Instant::now, |m| log(&prefixed("D-Bus", m)));
    });
    if let Err(e) = result {
        log(&format!("D-Bus thread: {e}"));
    }
}

#[cfg(test)]
mod tests;
