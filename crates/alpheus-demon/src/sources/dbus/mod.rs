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
mod outage;
pub mod power;
pub mod props;
mod run;
mod session;
mod signal;
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

pub use outage::{DomainLog, Outage, is_service_gone, optional, service_gone};
use outage::{outage_of, tag, worth_logging};
use run::run_supervised;
#[cfg(test)]
use session::{bt_into, wifi_into};
pub use session::{messages, session};
#[cfg(test)]
use session::{read, read_all};
use signal::under;

pub const NM: &str = "org.freedesktop.NetworkManager";
pub const UPOWER: &str = "org.freedesktop.UPower";
pub const BLUEZ: &str = "org.bluez";
/// power-profiles-daemon (quick spec § 5): read, never set (the panel runs powerprofilesctl).
pub const POWER_PROFILES: &str = "net.hadess.PowerProfiles";
/// The bus driver, which answers `GetNameOwner`, `AddMatch` and the rest.
pub const BUS_DRIVER: &str = "org.freedesktop.DBus";
pub const BUS_DRIVER_PATH: &str = "/org/freedesktop/DBus";
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
    /// lost to a timeout may come after the bus took it), their own refusals
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
    /// earlier open.
    pub wifi_open: u64,
    /// The quick panel's Bluetooth, only while it is open (quick settings delivery 3); None
    /// otherwise. The bar's `bluetooth` is separate and unchanged.
    pub bt: Option<BtView>,
    /// The open `bt` was read for (`BtTap::snapshot`), as `wifi_open`.
    pub bt_open: u64,
}

/// An unused module costs nothing: with no domain shown,
/// `start` neither connects nor spawns a thread.
fn wanted(domains: &[Domain]) -> bool {
    !domains.is_empty()
}

/// Starts the D-Bus source in its thread (`D-Bus`) for the `domains` of
/// the shown modules: one connection to the system bus per session, made
/// again with a growing delay if lost or if a domain's read fails. A
/// domain's own outage shows "–" for it alone, the others keep their last
/// value during the delay (I2); the bus itself going away blanks
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
            let mut bus = dbusrs_bus::DbusRsBus::system(&domains)?.waking(tap.clone()).waking(bt_tap.clone());
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
