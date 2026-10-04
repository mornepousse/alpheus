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
pub mod zbus_bus;

#[cfg(test)]
pub mod fake;

use super::{Backoff, STABLE_DURATION, SourceGuard, panic_text, prefixed};
use crate::log::write as log;
use crate::memory::release_memory;
use crate::state::Msg;
use crate::system::{BtState, NetState, PowerState};
use props::{Objects, Props};
use std::io;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

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

/// `path` is `root` or below it.
fn under(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// The NetworkManager root object's properties the bar reads.
const NM_ROOT_PROPS: [&str; 3] = ["PrimaryConnection", "ActiveConnections", "Connectivity"];
/// An active connection's properties the bar reads.
const NM_ACTIVE_PROPS: [&str; 1] = ["State"];
/// A UPower device's properties the bar reads (the laptop's DisplayDevice,
/// or a peripheral — never a raw battery_BAT0 or line_power_*, dropped
/// below by `upower_device_ignored` regardless of which property changed).
/// `Model`: a peripheral's name (`upower::read`'s `Peripheral::name`) —
/// missing from this list (re-review, follow-up): a rename wouldn't have
/// re-read it until something else happened to change too.
const UPOWER_PROPS: [&str; 6] = ["Percentage", "State", "IsPresent", "Type", "PowerSupply", "Model"];
/// BlueZ: the properties the bar reads, by interface.
const BLUEZ_ADAPTER_PROPS: [&str; 1] = ["Powered"];
// Deviation from the brief's Device1{Connected, Alias, Name} (reported):
// `Name` stays out of the live allowlist. `bluez::read` does fall back to
// it, but only once Alias is absent — rare for anything that ever got
// paired — while BlueZ keeps correcting a scanned device's advertised Name
// as it collects more of it, right alongside RSSI/UUIDs; keeping it in
// would defeat the scan-storm test below (a batch carrying Name, even with
// nothing else relevant, would re-read on every such correction).
const BLUEZ_DEVICE_PROPS: [&str; 2] = ["Connected", "Alias"];
const BLUEZ_BATTERY_PROPS: [&str; 1] = ["Percentage"];

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

/// UPower emits `PropertiesChanged` for every device it tracks, including
/// the laptop's raw battery (`battery_BAT0`…) and the line power supplies
/// (M5, review): the bar never reads those objects directly — `DisplayDevice`
/// already aggregates the laptop battery, and a peripheral is read by its
/// own device path, named differently (`battery_hidpp_battery_1`…). A
/// `path_namespace` match rule can't exclude a few device names by itself,
/// so this drops them here instead, regardless of which property changed.
fn upower_device_ignored(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    name.starts_with("line_power_")
        || name.strip_prefix("battery_BAT").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
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
        let d = if under(&self.path, "/org/freedesktop/NetworkManager") {
            Domain::Network
        } else if under(&self.path, "/org/freedesktop/UPower") {
            Domain::Battery
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemState {
    pub network: Option<NetState>,
    pub bluetooth: Option<BtState>,
    pub battery: Option<PowerState>,
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

/// Re-reads `d` into `s`. Err: the read failed for real (a timeout…), the
/// domain is now None, tagged `Outage::Domain(d)` (I2, review) so a caller
/// can blank just that domain rather than the whole state. An absence
/// (NotFound) also gives None, but is not an error.
fn read(bus: &mut dyn Bus, d: Domain, s: &mut SystemState) -> io::Result<()> {
    let r = match d {
        Domain::Network => network::read(bus).map(|v| s.network = Some(v)),
        Domain::Bluetooth => bluez::read(bus).map(|v| s.bluetooth = Some(v)),
        Domain::Battery => upower::read(bus).map(|v| s.battery = Some(v)),
    };
    let Err(e) = r else { return Ok(()) };
    match d {
        Domain::Network => s.network = None,
        Domain::Bluetooth => s.bluetooth = None,
        Domain::Battery => s.battery = None,
    }
    if worth_logging(e.kind()) {
        log(&format!("D-Bus {d:?}: {e}"));
    }
    if e.kind() == io::ErrorKind::NotFound {
        return Ok(());
    }
    Err(tag(e, Outage::Domain(d)))
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
    let mut last = SystemState { network: None, bluetooth: None, battery: None };
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
/// all three.
pub fn start(domains: Vec<Domain>, send: impl Fn(Option<SystemState>) -> bool + Send + 'static) {
    if !wanted(&domains) {
        return;
    }
    let result = std::thread::Builder::new().name("D-Bus".into()).spawn(move || {
        let _guard = SourceGuard { name: "D-Bus", send: &send, value: PhantomData };
        let run = move |p: &mut dyn FnMut(SystemState) -> bool| session(&mut zbus_bus::ZbusBus::system(&domains)?, &domains, p);
        run_supervised(run, &send, std::thread::sleep, Instant::now, |m| log(&prefixed("D-Bus", m)));
    });
    if let Err(e) = result {
        log(&format!("D-Bus thread: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{BLUEZ_OWNER, FakeBus, NM_OWNER, Step, signal, with_names};
    use super::props::Prop;
    use super::*;
    use crate::state::Msg;
    use crate::system::*;
    use std::cell::RefCell;
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
    fn only_non_notfound_read_errors_are_worth_logging() {
        assert!(!worth_logging(io::ErrorKind::NotFound), "an absence: its NameOwnerChanged follows, not worth a line");
        assert!(worth_logging(io::ErrorKind::Other), "a real failure (a timeout…)");
        assert!(worth_logging(io::ErrorKind::UnexpectedEof));
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
        let shown = SystemState { network: Some(NetState::Offline), bluetooth: None, battery: Some(PowerState::default()) };
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
        let shown =
            SystemState { network: Some(NetState::Offline), bluetooth: Some(BtState::Absent), battery: Some(PowerState::default()) };
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
                session(&mut bus, &domains, publish)
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
            |publish: &mut dyn FnMut(SystemState) -> bool| session(&mut FakeBus::workstation(), &domains, publish),
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

    #[test]
    fn start_with_no_domains_connects_nothing() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        start(vec![], move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no connection, no thread, nothing published");
    }
}
