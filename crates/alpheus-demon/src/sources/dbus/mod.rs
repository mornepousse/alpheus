//! The system D-Bus, READ ONLY: NetworkManager, UPower, BlueZ, power-profiles-daemon.
//! One connection on one thread, blocked on the bus while nothing changes; each
//! signal re-reads the domain it concerns. `Bus` only has reads (checked by
//! tests/guardrails.rs), and a service is only ever called through the
//! unique name of the process that owns it: never through its well-known
//! name, which D-Bus activation would use to start it.

pub mod bluez;
pub mod dbusrs_bus;
pub mod network;
pub mod power;
pub mod props;
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
use crate::system::{BtState, NetState, PowerState, ProfileState, SavedWifi, WifiScope, WifiState};
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
    /// AddMatch (`add`) or RemoveMatch of each rule, in order: the Wi-Fi detail's (`WIFI_RULES`).
    fn match_rules(&mut self, rules: &[&str], add: bool) -> io::Result<()>;
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

/// A Wi-Fi read's result into `s`: Err only when the bus itself went away, tagged so (every domain
/// blanked, never the network domain alone).
fn wifi_into(r: io::Result<Option<WifiState>>, s: &mut SystemState) -> io::Result<()> {
    match r {
        Ok(w) => {
            s.wifi = w;
            Ok(())
        }
        Err(e) => {
            s.wifi = None;
            Err(tag(e, Outage::Bus))
        }
    }
}

/// `wifi`: what the open quick panel asks (quick Wi-Fi spec § 5): read when the session starts,
/// again on each of its pokes and with every network re-read; Closed reads nothing.
pub fn session(
    bus: &mut dyn Bus,
    domains: &[Domain],
    log_state: &mut DomainLog,
    wifi: &WifiTap,
    publish: &mut dyn FnMut(SystemState) -> bool,
) -> io::Result<()> {
    let mut s = SystemState::default();
    let mut reader = WifiReader::default();
    let mut last = None;
    let mut failed = read_all(bus, domains, &mut s, log_state);
    // A panel already open when this session starts (a reconnection): its Wi-Fi read now. A word
    // left waiting from the previous session is answered by this read: drained, never read twice.
    wifi.drain();
    if let Err(e) = wifi_into(reader.rescope(bus, wifi.scope()), &mut s) {
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
            let r = wifi_into(reader.rescope(bus, wifi.scope()), &mut s);
            if !publish_if_changed(&mut last, &s, publish) {
                return Ok(());
            }
            // The panel closed: what this thread's arena held for it goes back, as at startup.
            if reader.scope() == WifiScope::Closed {
                release_memory();
            }
            r?;
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
        let Some(d) = signal.domain().filter(|d| domains.contains(d)) else { continue };
        let r = read(bus, d, &mut s, log_state);
        let w = if d == Domain::Network { wifi_into(reader.reread(bus), &mut s) } else { Ok(()) };
        if !publish_if_changed(&mut last, &s, publish) {
            return Ok(());
        }
        // The Wi-Fi read fails only with the bus itself gone: that outage wins over the domain's.
        w?;
        r?;
    }
}

/// The bar's messages for a published state; None (the whole bus down): "–" for every module, no
/// power profile, no Wi-Fi.
pub fn messages(s: Option<SystemState>) -> [Msg; 5] {
    match s {
        Some(s) => {
            [Msg::Network(s.network), Msg::Bluetooth(s.bluetooth), Msg::Battery(s.battery), Msg::Profile(s.power), Msg::Wifi(s.wifi)]
        }
        None => [Msg::Network(None), Msg::Bluetooth(None), Msg::Battery(None), Msg::Profile(None), Msg::Wifi(None)],
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
                    Outage::Domain(_) => Some(last.clone()),
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
pub fn start(domains: Vec<Domain>, wifi: Option<Arc<WifiTap>>, send: impl Fn(Option<SystemState>) -> bool + Send + 'static) {
    if !wanted(&domains) {
        return;
    }
    let result = std::thread::Builder::new().name("D-Bus".into()).spawn(move || {
        let _guard = SourceGuard { name: "D-Bus", send: &send, value: PhantomData };
        // Owned here, across every `run_supervised` round (follow-up,
        // re-review): a domain stuck down must log once, not once per round.
        let mut log_state = DomainLog::default();
        let tap = wifi.unwrap_or_default();
        let run = move |p: &mut dyn FnMut(SystemState) -> bool| {
            let mut bus = dbusrs_bus::DbusRsBus::system(&domains)?.waking(tap.clone());
            session(&mut bus, &domains, &mut log_state, &tap, p)
        };
        run_supervised(run, &send, std::thread::sleep, Instant::now, |m| log(&prefixed("D-Bus", m)));
    });
    if let Err(e) = result {
        log(&format!("D-Bus thread: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{AP, BLUEZ_OWNER, FakeBus, NM_OWNER, Step, WIFI_DEVICE, signal, with_names};
    use super::props::Prop;
    use super::wifi::WifiChange;
    use super::*;
    use crate::state::Msg;
    use crate::system::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    fn workstation_network() -> NetState {
        NetState::Online { link: Link::Ethernet, vpns: 2, limited: false }
    }

    fn workstation_battery() -> PowerState {
        // The recorded mouse is the real G602 case: BatteryLevel 8 (Full),
        // its Percentage not trusted — hidden (Mae, 2026-10-05).
        PowerState {
            laptop: Some(Battery { percent: 92, charge: Charge::Charging }),
            peripherals: vec![Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: None, low: false }],
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
        let tap = bus.tap.clone();
        let r = session(bus, domains, &mut DomainLog::default(), &tap, &mut |s| {
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
                battery: Some(workstation_battery()),
                power: Some(ProfileState::default()),
                wifi: None
            }
        );
        assert!(matches!(&seen[1].network, Some(NetState::Online { link: Link::Wifi, .. })), "{:?}", seen[1]);
        assert_eq!(seen[1].battery, Some(workstation_battery()), "other domains carried along");
        assert!(matches!(&seen[2].bluetooth, Some(BtState::On { devices }) if devices.len() == 2), "{:?}", seen[2]);
        assert_eq!(seen[3].battery, Some(PowerState::default()), "UPower gone: nothing to show");
    }

    /// A signal can pass `relevant`'s allowlist (or carry no property names
    /// at all, counted as "unknown change") and still leave every field
    /// `session` tracks unchanged — a duplicate signal, or two different
    /// allowed properties that both parse to the same value (B-m6, review).
    #[test]
    fn an_unchanged_state_is_not_republished() {
        let mut bus = FakeBus::workstation();
        let steps: [Step; 2] = [
            (signal(network::PATH, PROPERTIES, "PropertiesChanged", None), nothing),
            (signal(network::PATH, PROPERTIES, "PropertiesChanged", None), nothing),
        ];
        bus.script = VecDeque::from(steps);
        let (seen, r) = run(&mut bus);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(seen.len(), 1, "two relevant signals that change nothing: only the initial publish");
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
        assert!(session(&mut bus, &DOMAINS, &mut DomainLog::default(), &WifiTap::default(), &mut |_| false).is_ok());
        assert_eq!(bus.script.len(), 1, "no signal read after the bar said stop");
    }

    fn open_the_row(b: &mut FakeBus) {
        b.tap.set(WifiScope::Row);
    }

    fn close_the_panel(b: &mut FakeBus) {
        b.tap.set(WifiScope::Closed);
    }

    fn radio_off(b: &mut FakeBus) {
        b.props_mut(network::PATH, network::INTERFACE).0.insert("WirelessEnabled".into(), Prop::Bool(false));
    }

    fn radio_changed() -> Signal {
        with_names(signal(network::PATH, PROPERTIES, "PropertiesChanged", Some(network::INTERFACE)), &["WirelessEnabled"])
    }

    /// Quick Wi-Fi spec § 5: closed, a network signal reads no Wi-Fi; the loop's word reads the row;
    /// a network signal re-reads it while open; closed again, the state goes and nothing is read.
    #[test]
    fn the_wifi_row_is_read_on_a_poke_followed_while_open_and_dropped_on_close() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        let steps: [Step; 5] = [
            (radio_changed(), nothing),
            (Signal::poke(), open_the_row),
            (radio_changed(), radio_off),
            (Signal::poke(), close_the_panel),
            (radio_changed(), nothing),
        ];
        bus.script = VecDeque::from(steps);
        let (seen, r) = run(&mut bus);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let wifi: Vec<Option<bool>> = seen.iter().map(|s| s.wifi.as_ref().map(|w| w.enabled)).collect();
        assert_eq!(wifi, [None, Some(true), Some(false), None], "initial, the row, the radio off, closed");
        assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "{:?}", bus.calls);
        let device_reads = bus.calls.iter().filter(|c| c.contains(WIFI_DEVICE)).count();
        assert_eq!(device_reads, 4, "Device and Device.Wireless, at the poke and at the radio's signal; none closed");
    }

    /// A session (re)starting while the panel is open (a reconnection) reads the row at once.
    #[test]
    fn a_session_starting_with_the_panel_open_reads_the_row_at_once() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        bus.tap.set(WifiScope::Row);
        let (seen, _) = run(&mut bus);
        assert!(seen[0].wifi.as_ref().is_some_and(|w| w.device.is_some()), "{:?}", seen[0].wifi);
    }

    /// The bus going away during the Wi-Fi read is the bus's outage (every domain blanked), never a
    /// domain's.
    #[test]
    fn the_bus_going_away_during_the_wifi_read_is_a_bus_outage() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        bus.disconnecting.insert(WIFI_DEVICE.into());
        let steps: [Step; 2] = [(Signal::poke(), open_the_row), (radio_changed(), nothing)];
        bus.script = VecDeque::from(steps);
        let (_, r) = run(&mut bus);
        let e = r.unwrap_err();
        assert_eq!(outage_of(&e), Outage::Bus);
        assert!(e.to_string().contains(WIFI_DEVICE), "the Wi-Fi read's error, not the script's end: {e}");
        assert_eq!(bus.script.len(), 1, "the session ended at the poke, the next signal never taken");
    }

    fn the_bus_goes_during_a_network_outage(b: &mut FakeBus) {
        b.failing.insert("/org/freedesktop/NetworkManager/ActiveConnection/2".into());
        b.disconnecting.insert(WIFI_DEVICE.into());
    }

    /// Review 3a: a network signal whose own read fails (the network domain's outage) and whose
    /// Wi-Fi read then finds the bus gone ends as the bus's outage: every domain blanked, never the
    /// network domain alone over a dead connection.
    #[test]
    fn the_bus_going_away_after_a_network_outage_wins() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        let steps: [Step; 2] = [(Signal::poke(), open_the_row), (radio_changed(), the_bus_goes_during_a_network_outage)];
        bus.script = VecDeque::from(steps);
        let (seen, r) = run(&mut bus);
        assert_eq!(seen.len(), 3, "initial, the row, then the failed re-read published");
        assert_eq!(outage_of(&r.unwrap_err()), Outage::Bus);
    }

    /// A word the loop wrote while the bus was down is answered by the new session's first read:
    /// drained, so the thread does not wake once more for it.
    #[test]
    fn a_session_starts_with_the_taps_word_drained() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        bus.tap = Arc::new(WifiTap::new().unwrap());
        bus.tap.set(WifiScope::Row);
        let fd = bus.tap.wake_fd().expect("a socket pair");
        let (seen, _) = run(&mut bus);
        assert!(seen[0].wifi.is_some(), "the row read at the start");
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: poll(2) on one pollfd owned here, no wait.
        assert_eq!(unsafe { libc::poll(&mut p, 1, 0) }, 0, "the word left waiting was drained");
    }

    #[test]
    fn the_poke_is_no_bus_signal() {
        let p = Signal::poke();
        assert!(p.is_poke() && !p.is_wake() && p.domain().is_none());
        assert!(!radio_changed().is_poke());
        assert_eq!(radio_changed().domain(), Some(Domain::Network), "WirelessEnabled re-reads the network domain");
    }

    fn open_the_detail(b: &mut FakeBus) {
        b.tap.set(WifiScope::Detail);
    }

    fn voisin_at_90(b: &mut FakeBus) {
        b.props_mut(&format!("{AP}/11"), wifi::ACCESS_POINT).0.insert("Strength".into(), Prop::Int(90));
    }

    fn voisin_at_45(b: &mut FakeBus) {
        b.props_mut(&format!("{AP}/11"), wifi::ACCESS_POINT).0.insert("Strength".into(), Prop::Int(45));
    }

    fn voisin_changed() -> Signal {
        with_names(signal(&format!("{AP}/11"), PROPERTIES, "PropertiesChanged", Some(wifi::ACCESS_POINT)), &["Strength"])
    }

    fn voisin(s: &SystemState) -> Option<u8> {
        let d = s.wifi.as_ref()?.device.as_ref()?;
        d.access_points.as_ref()?.iter().find(|a| a.ssid == b"Voisin-Test").map(|a| a.strength)
    }

    /// Quick Wi-Fi spec § 5: an access point's change is read only while the detail is open.
    #[test]
    fn detail_signals_are_followed_only_while_the_detail_is_open() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        let steps: [Step; 5] = [
            (voisin_changed(), nothing),
            (Signal::poke(), open_the_detail),
            (voisin_changed(), voisin_at_90),
            (Signal::poke(), open_the_row),
            (voisin_changed(), voisin_at_45),
        ];
        bus.script = VecDeque::from(steps);
        let (seen, r) = run(&mut bus);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(seen.iter().map(voisin).collect::<Vec<_>>(), [None, Some(30), Some(90), None], "initial, detail, the change, the row");
        let reads = bus.calls.iter().filter(|c| c.contains(&format!("{AP}/11 "))).count();
        assert_eq!(reads, 2, "at the detail's open and on its signal; never closed nor on the row");
    }

    /// A list's signal still in flight when the detail closed (its rules removed) re-reads nothing,
    /// never the network domain either: closed or on the row, not one call.
    #[test]
    fn a_late_list_signal_reads_nothing_outside_the_detail() {
        let mut bus = FakeBus::workstation();
        bus.with_wifi();
        let added = signal(WIFI_DEVICE, wifi::WIRELESS, "AccessPointAdded", None);
        let saved = signal(wifi::SETTINGS_PATH, wifi::SETTINGS, "NewConnection", None);
        let steps: [Step; 5] =
            [(added.clone(), nothing), (saved.clone(), nothing), (Signal::poke(), open_the_row), (added, nothing), (saved, nothing)];
        bus.script = VecDeque::from(steps);
        let (seen, _) = run(&mut bus);
        assert_eq!(seen.len(), 2, "initial and the row only");
        let mut quiet = FakeBus::workstation();
        quiet.with_wifi();
        quiet.script = VecDeque::from([(Signal::poke(), open_the_row as fn(&mut FakeBus))]);
        let _ = run(&mut quiet);
        assert_eq!(bus.calls, quiet.calls, "the same calls as without those signals");
    }

    #[test]
    fn wifi_signals_are_told_apart() {
        assert_eq!(voisin_changed().wifi_change(), Some(WifiChange::AccessPoint(format!("{AP}/11"))));
        let noise = with_names(signal(&format!("{AP}/11"), PROPERTIES, "PropertiesChanged", Some(wifi::ACCESS_POINT)), &["LastSeen"]);
        assert_eq!(noise.wifi_change(), None, "LastSeen changes every scan: never read");
        let device = "/org/freedesktop/NetworkManager/Devices/5";
        assert_eq!(signal(device, wifi::WIRELESS, "AccessPointAdded", None).wifi_change(), Some(WifiChange::AccessPoints));
        assert_eq!(signal(device, wifi::WIRELESS, "AccessPointRemoved", None).wifi_change(), Some(WifiChange::AccessPoints));
        assert_eq!(signal(wifi::SETTINGS_PATH, wifi::SETTINGS, "NewConnection", None).wifi_change(), Some(WifiChange::Saved));
        assert_eq!(signal(wifi::SETTINGS_PATH, wifi::SETTINGS, "ConnectionRemoved", None).wifi_change(), Some(WifiChange::Saved));
        assert_eq!(radio_changed().wifi_change(), None, "the root: the network domain's");
    }

    fn profile_performance(b: &mut FakeBus) {
        b.props_mut(power::PATH, power::INTERFACE).0.insert("ActiveProfile".into(), Prop::Str("performance".into()));
    }

    fn profiles_gone(b: &mut FakeBus) {
        b.owners.remove(POWER_PROFILES);
    }

    /// Quick spec § 5: read at start, followed on `PropertiesChanged` (only the two properties the
    /// panel shows), gone with its owner; every call to its one object, through its unique name.
    #[test]
    fn the_power_profile_is_read_followed_and_gone_with_its_daemon() {
        let mut bus = FakeBus::empty();
        bus.with_power_profiles("balanced");
        let changed = |names: &[&str]| with_names(signal(power::PATH, PROPERTIES, "PropertiesChanged", Some(power::INTERFACE)), names);
        bus.script = VecDeque::from([
            (changed(&["ActiveProfile"]), profile_performance as fn(&mut FakeBus)),
            (changed(&["PerformanceDegraded"]), nothing as fn(&mut FakeBus)),
            (
                signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(POWER_PROFILES)),
                profiles_gone as fn(&mut FakeBus),
            ),
        ]);
        let (seen, _) = run_over(&mut bus, &[Domain::Power]);
        let all = Profile::ALL.to_vec();
        assert_eq!(
            seen.iter().map(|s| s.power.clone()).collect::<Vec<_>>(),
            [
                Some(ProfileState { active: Some(Profile::Balanced), offered: all.clone() }),
                Some(ProfileState { active: Some(Profile::Performance), offered: all }),
                Some(ProfileState::default()),
            ]
        );
        let reads: Vec<&String> = bus.calls.iter().filter(|c| c.starts_with("GetAll")).collect();
        assert_eq!(reads.len(), 2, "PerformanceDegraded re-reads nothing: {:?}", bus.calls);
        assert!(reads.iter().all(|c| *c == "GetAll :1.13 /net/hadess/PowerProfiles net.hadess.PowerProfiles"), "{:?}", bus.calls);
    }

    #[test]
    fn power_profiles_signals_point_to_their_domain() {
        let changed = |names: &[&str]| with_names(signal(power::PATH, PROPERTIES, "PropertiesChanged", Some(power::INTERFACE)), names);
        assert_eq!(changed(&["ActiveProfile"]).domain(), Some(Domain::Power));
        assert_eq!(changed(&["Profiles"]).domain(), Some(Domain::Power));
        assert_eq!(changed(&["PerformanceDegraded", "ActiveProfileHolds"]).domain(), None, "not shown: nothing re-read");
        let owner = signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(POWER_PROFILES));
        assert_eq!(owner.domain(), Some(Domain::Power));
    }

    #[test]
    fn messages_for_the_bar() {
        assert_eq!(
            messages(None),
            [Msg::Network(None), Msg::Bluetooth(None), Msg::Battery(None), Msg::Profile(None), Msg::Wifi(None)],
            "bus down: \"–\" everywhere"
        );
        let s = SystemState {
            network: Some(NetState::Offline),
            bluetooth: None,
            battery: Some(PowerState::default()),
            power: Some(ProfileState::default()),
            wifi: None,
        };
        assert_eq!(
            messages(Some(s)),
            [
                Msg::Network(Some(NetState::Offline)),
                Msg::Bluetooth(None),
                Msg::Battery(Some(PowerState::default())),
                Msg::Profile(Some(ProfileState::default())),
                Msg::Wifi(None)
            ]
        );
    }

    #[test]
    fn signals_point_to_their_domain() {
        let d = |path, interface, member, arg0| signal(path, interface, member, arg0).domain();
        assert_eq!(d("/org/freedesktop/NetworkManager", PROPERTIES, "PropertiesChanged", None), Some(Domain::Network));
        assert_eq!(d("/org/freedesktop/UPower/devices/DisplayDevice", PROPERTIES, "PropertiesChanged", None), Some(Domain::Battery));
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
        // Symmetric with InterfacesAdded (I1, review): a connected device
        // always sends Connected=false before being removed, so Device1's
        // own removal re-reads nothing — only Adapter1 or Battery1 does.
        assert_eq!(
            objects("InterfacesRemoved", &["org.bluez.Device1"]),
            None,
            "a connected device always sends Connected=false before being removed"
        );
        assert_eq!(objects("InterfacesRemoved", &["org.bluez.Battery1"]), Some(Domain::Bluetooth));
        assert_eq!(objects("InterfacesRemoved", &["org.bluez.Adapter1"]), Some(Domain::Bluetooth), "rfkill-blocked or unplugged");
    }

    #[test]
    fn a_scan_storm_of_unconnected_devices_causes_zero_re_reads() {
        // A scan sees a device, then loses it again: Name, RSSI, UUIDs,
        // then InterfacesRemoved — none of it connects anything, none of it
        // is relevant (I1, review).
        const SCANNED: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_99";
        let changed =
            |names: &[&str]| with_names(signal(SCANNED, PROPERTIES, "PropertiesChanged", Some("org.bluez.Device1")), names).domain();
        assert_eq!(changed(&["Name"]), None);
        assert_eq!(changed(&["RSSI"]), None);
        assert_eq!(changed(&["UUIDs"]), None);
        assert_eq!(changed(&["Name", "RSSI", "UUIDs", "ManufacturerData"]), None, "not one of these ever re-reads");
        let removed = with_names(signal("/", OBJECT_MANAGER, "InterfacesRemoved", None), &["org.bluez.Device1"]).domain();
        assert_eq!(removed, None, "the scan lost sight of it: still not connected, still nothing to re-read");
    }

    #[test]
    fn only_the_allowlisted_properties_are_relevant() {
        // NM root: PrimaryConnection, ActiveConnections, Connectivity only.
        let changed_nm = |names: &[&str]| with_names(signal(network::PATH, PROPERTIES, "PropertiesChanged", Some(NM)), names).domain();
        assert_eq!(changed_nm(&["Version"]), None);
        assert_eq!(changed_nm(&["Connectivity"]), Some(Domain::Network));
        // An active connection: State only.
        const AC9: &str = "/org/freedesktop/NetworkManager/ActiveConnection/9";
        let changed_ac = |names: &[&str]| with_names(signal(AC9, PROPERTIES, "PropertiesChanged", Some(network::ACTIVE)), names).domain();
        assert_eq!(changed_ac(&["Id"]), None, "no name is ever read (Q1)");
        assert_eq!(changed_ac(&["State"]), Some(Domain::Network));
        // A UPower device (the DisplayDevice, or a peripheral): the properties `upower::read` looks at.
        const DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
        let changed_up = |names: &[&str]| {
            with_names(signal(DISPLAY, PROPERTIES, "PropertiesChanged", Some("org.freedesktop.UPower.Device")), names).domain()
        };
        assert_eq!(changed_up(&["Capacity"]), None);
        assert_eq!(changed_up(&["Type"]), Some(Domain::Battery));
        assert_eq!(changed_up(&["Model"]), Some(Domain::Battery), "a peripheral's name, read into Peripheral::name");
        assert_eq!(changed_up(&["BatteryLevel"]), Some(Domain::Battery), "a coarse-reporting peripheral going Low or Critical");
        // BlueZ's adapter: Powered only.
        let changed_adapter = |names: &[&str]| {
            with_names(signal("/org/bluez/hci0", PROPERTIES, "PropertiesChanged", Some("org.bluez.Adapter1")), names).domain()
        };
        assert_eq!(changed_adapter(&["Discovering"]), None);
        assert_eq!(changed_adapter(&["Powered"]), Some(Domain::Bluetooth));
    }

    #[test]
    fn upower_raw_battery_and_line_power_signals_are_dropped_display_device_carries_them() {
        // M5, review: the match rule can't exclude a few device names by
        // itself, so `Signal::domain` drops these, whatever the property.
        let changed = |path: &str, names: &[&str]| {
            with_names(signal(path, PROPERTIES, "PropertiesChanged", Some("org.freedesktop.UPower.Device")), names).domain()
        };
        assert_eq!(changed("/org/freedesktop/UPower/devices/battery_BAT0", &["Percentage"]), None);
        assert_eq!(changed("/org/freedesktop/UPower/devices/line_power_AC", &["Online"]), None);
        assert_eq!(changed("/org/freedesktop/UPower/devices/line_power_ucsi_source_psy_USBC000o001", &["Online"]), None);
        assert_eq!(
            changed("/org/freedesktop/UPower/devices/DisplayDevice", &["Percentage"]),
            Some(Domain::Battery),
            "the aggregate the bar actually reads"
        );
        assert_eq!(
            changed("/org/freedesktop/UPower/devices/battery_hidpp_battery_1", &["Percentage"]),
            Some(Domain::Battery),
            "a peripheral, not BATn-named"
        );
    }

    #[test]
    fn other_acpi_battery_naming_schemes_are_dropped_too() {
        // Follow-up (5), re-review: not only BATn — other machines' ACPI
        // battery drivers use their own short, underscore-free native name
        // (ThinkPads: BATC/BATT; some boards: CMB0/CMB1…). A peripheral's
        // native name always carries its driver's own prefix with an
        // underscore (hidpp_battery_1, sony_controller_battery_0…): that is
        // what still tells the two apart.
        let changed = |path: &str| {
            with_names(signal(path, PROPERTIES, "PropertiesChanged", Some("org.freedesktop.UPower.Device")), &["Percentage"]).domain()
        };
        assert_eq!(changed("/org/freedesktop/UPower/devices/battery_BAT1"), None, "a second laptop battery");
        assert_eq!(changed("/org/freedesktop/UPower/devices/battery_BATC"), None, "ThinkPad-style ACPI naming");
        assert_eq!(changed("/org/freedesktop/UPower/devices/battery_CMB0"), None, "another ACPI naming scheme");
        assert_eq!(
            changed("/org/freedesktop/UPower/devices/battery_sony_controller_battery_0"),
            Some(Domain::Battery),
            "a peripheral's underscore-carrying native name, still shown"
        );
    }

    #[test]
    fn only_non_notfound_read_errors_are_worth_logging() {
        assert!(!worth_logging(io::ErrorKind::NotFound), "an absence: its NameOwnerChanged follows, not worth a line");
        assert!(worth_logging(io::ErrorKind::Other), "a real failure (a timeout…)");
        assert!(worth_logging(io::ErrorKind::UnexpectedEof));
    }

    #[test]
    fn a_domains_failure_logs_once_until_it_recovers() {
        // Follow-up (2), re-review: `read` is retried fresh on every backoff
        // round (a new `session`, a new `SystemState`) — without state that
        // outlives a single session, a domain stuck down would repeat the
        // same line every round forever. `DomainLog` is the pure predicate
        // behind that rate limiting.
        let mut log_state = DomainLog::default();
        assert!(log_state.note_failure(Domain::Network), "the first failure is worth a line");
        assert!(!log_state.note_failure(Domain::Network), "still down: not worth a line again");
        assert!(!log_state.note_failure(Domain::Network), "and again: still quiet");
        log_state.note_success(Domain::Network);
        assert!(log_state.note_failure(Domain::Network), "recovered, then failed again: worth a line");
        // Independent per domain.
        assert!(log_state.note_failure(Domain::Bluetooth), "a different domain logs its own first failure");
        assert!(!log_state.note_failure(Domain::Network), "Bluetooth's failure didn't reset Network's state");
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

    /// Simulated clock: advances by `step` on each read (same helper as
    /// `sources::supervise`'s own tests).
    fn clock(step: std::time::Duration) -> impl FnMut() -> std::time::Instant {
        let mut t = std::time::Instant::now();
        move || {
            t += step;
            t
        }
    }

    #[test]
    fn a_domain_outage_blanks_only_that_domain_during_the_backoff() {
        // I2, review: a tagged domain failure keeps the others' last
        // published value showing during the backoff delay.
        let sends = RefCell::new(Vec::new());
        let shown = SystemState {
            network: Some(NetState::Offline),
            bluetooth: None,
            battery: Some(PowerState::default()),
            power: None,
            wifi: None,
        };
        run_supervised(
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                publish(shown.clone());
                Err(tag(io::Error::other("wedged"), Outage::Domain(Domain::Bluetooth)))
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        let seen = sends.borrow();
        assert_eq!(seen[0], Some(shown.clone()), "the initial publish: bluetooth already \"–\"");
        assert_eq!(seen[1], seen[0], "the backoff \"down\" value: the same state, not a full blank");
    }

    #[test]
    fn a_bus_outage_blanks_everything_during_the_backoff() {
        let sends = RefCell::new(Vec::new());
        let shown = SystemState {
            network: Some(NetState::Offline),
            bluetooth: Some(BtState::Absent),
            battery: Some(PowerState::default()),
            power: None,
            wifi: None,
        };
        run_supervised(
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                publish(shown.clone());
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "system bus closed"))
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        let seen = sends.borrow();
        assert_eq!(seen[0], Some(shown));
        assert_eq!(seen[1], None, "the bus itself gone: \"–\" everywhere, untagged errors default to it");
    }

    /// I2 carry-over (tray plan 1 review): a single domain's own read
    /// failing because the BUS went away mid-call (`UnexpectedEof`, as
    /// `dbusrs_bus::error` maps `Disconnected`) must be tagged `Outage::Bus`,
    /// never `Outage::Domain` — otherwise the other domains would keep
    /// showing their last value for a whole backoff round after the
    /// connection itself is already gone.
    #[test]
    fn a_bus_disconnect_during_a_domains_read_is_tagged_as_a_bus_outage() {
        let mut bus = FakeBus::workstation();
        bus.disconnecting.insert(network::PATH.into());
        let mut s = SystemState::default();
        let e = read(&mut bus, Domain::Network, &mut s, &mut DomainLog::default()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(outage_of(&e), Outage::Bus, "a bus disconnect blanks every domain, not just this one");
    }

    /// Fix round, review item 6: within one `read_all` call, a `Bus` outage
    /// must win over a `Domain` outage that came earlier — `DOMAINS` reads
    /// Network before Battery, so Network's own ordinary timeout is seen
    /// first, then the bus itself goes away while Battery is read. The
    /// whole call must still report `Outage::Bus` (nothing can be trusted
    /// any more), not the earlier, now stale, `Outage::Domain(Network)`.
    #[test]
    fn a_bus_outage_later_in_read_all_overrides_an_earlier_domain_outage() {
        let mut bus = FakeBus::workstation();
        bus.failing.insert(network::PATH.into()); // Network: an ordinary timeout
        bus.disconnecting.insert(upower::DISPLAY_DEVICE.into()); // Battery: the bus itself gone
        let mut s = SystemState::default();
        let e = read_all(&mut bus, &DOMAINS, &mut s, &mut DomainLog::default()).expect("both reads failed");
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(outage_of(&e), Outage::Bus, "the later bus outage must win over the earlier domain one");
    }

    #[test]
    fn a_wedged_bluez_leaves_network_and_battery_shown_while_bluetooth_shows_dash() {
        // The scenario named in the review, through the real `session` and
        // a `FakeBus`: BlueZ's GetManagedObjects always times out.
        let sends = RefCell::new(Vec::new());
        let domains = DOMAINS;
        run_supervised(
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                let mut bus = FakeBus::workstation();
                bus.failing.insert("/".into());
                session(&mut bus, &domains, &mut DomainLog::default(), &WifiTap::default(), publish)
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        let seen = sends.borrow();
        let initial = seen[0].clone().expect("an initial state, even with bluetooth down");
        assert_eq!(initial.bluetooth, None, "BlueZ wedged: \"–\"");
        assert_eq!(initial.network, Some(workstation_network()), "network still shown");
        assert_eq!(initial.battery, Some(workstation_battery()), "battery still shown");
        assert_eq!(seen[1], seen[0], "the backoff \"down\" value keeps them shown too");
    }

    #[test]
    fn bus_loss_blanks_everything_even_after_good_reads() {
        let sends = RefCell::new(Vec::new());
        let domains = DOMAINS;
        run_supervised(
            // An empty script: the next (and only) call to `next_signal`
            // ends the session — a real bus going away, untagged.
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                session(&mut FakeBus::workstation(), &domains, &mut DomainLog::default(), &WifiTap::default(), publish)
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        let seen = sends.borrow();
        assert!(seen[0].is_some(), "the initial state published fine");
        assert_eq!(seen[1], None, "the bus itself gone: \"–\" everywhere, not just the last domain touched");
    }

    #[test]
    fn wanted_is_false_only_when_no_domain_is_shown() {
        // M4, review: an unused module costs nothing.
        assert!(!wanted(&[]));
        assert!(wanted(&[Domain::Battery]));
        assert!(wanted(&DOMAINS));
    }

    /// A smoke test, not a bite-proof (re-review, follow-up 4): `start`'s
    /// own `if !wanted(&domains) { return; }` guard sits first, synchronously,
    /// in its body, so removing it here wouldn't reliably turn this test red
    /// (the thread it would then spawn runs asynchronously, and its real
    /// guard calls `DbusRsBus::system`, a real system bus connection — forbidden
    /// in a test regardless of how briefly, which is why that mutation was
    /// never run on the real `start`). `wanted_is_false_only_when_no_domain_is_shown`
    /// above is the actual proof; this just confirms `start` is wired to call
    /// it and return before publishing anything, cheaply and without a bus.
    #[test]
    fn start_with_no_domains_connects_nothing() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        start(vec![], None, move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no connection, no thread, nothing published");
    }
}
