use super::fake::{AP, BLUEZ_OWNER, FakeBus, NM_OWNER, SYSTEMD_OWNER, Step, WIFI_DEVICE, signal, with_names};
use super::props::Prop;
use super::wifi::WifiChange;
use super::*;
use crate::state::Msg;
use crate::system::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

fn workstation_network() -> NetState {
    NetState::Online { link: Link::Ethernet, vpns: 2, limited: false }
}

fn workstation_battery() -> PowerState {
    // The recorded mouse is the real G602 case: BatteryLevel 8 (Full),
    // its Percentage not trusted — hidden (2026-10-05).
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

/// `session` with the failed-units count's defaults and nobody to tell: what every test that is
/// not about the count uses.
fn plain_session(
    bus: &mut dyn Bus,
    domains: &[Domain],
    log_state: &mut DomainLog,
    wifi: &WifiTap,
    bt: &BtTap,
    vpn: &VpnTap,
    publish: &mut dyn FnMut(SystemState) -> bool,
) -> io::Result<()> {
    session(bus, domains, log_state, wifi, bt, vpn, &UnitsTap::default(), UnitsOptions::default(), publish, &mut || true)
}

/// A session over `domains`, every publication accepted.
fn run_over(bus: &mut FakeBus, domains: &[Domain]) -> (Vec<SystemState>, io::Result<()>) {
    let mut seen = Vec::new();
    let tap = bus.tap.clone();
    let bt_tap = bus.bt_tap.clone();
    let vpn_tap = bus.vpn_tap.clone();
    let r = plain_session(bus, domains, &mut DomainLog::default(), &tap, &bt_tap, &vpn_tap, &mut |s| {
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
        assert!(
            c.starts_with("GetNameOwner ") || *c == format!("StartServiceByName {UPOWER}") || destination.starts_with(':'),
            "{c}: a well-known name could be activated"
        );
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
            units: None,
            wifi: None,
            wifi_open: 0,
            bt: None,
            bt_open: 0,
            vpn: None,
            vpn_open: 0,
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
/// allowed properties that both parse to the same value.
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
    assert!(
        plain_session(
            &mut bus,
            &DOMAINS,
            &mut DomainLog::default(),
            &WifiTap::default(),
            &BtTap::default(),
            &VpnTap::default(),
            &mut |_| false
        )
        .is_ok()
    );
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

fn poke_the_units(b: &mut FakeBus) {
    b.units_tap.poke();
}

/// A poke is each tap's own word. The failed-units count asked for again reads the
/// count, and nothing of the open Wi-Fi row.
#[test]
fn a_units_poke_rereads_no_panel() {
    let mut bus = FakeBus::workstation();
    bus.with_wifi();
    bus.with_systemd(1);
    let steps: [Step; 2] = [(Signal::poke(), open_the_row), (Signal::poke(), poke_the_units)];
    bus.script = VecDeque::from(steps);
    let (_, _, r) = run_units(&mut bus, &[Domain::Network, Domain::Units], opts(false, 3600));
    assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    let device_reads = bus.calls.iter().filter(|c| c.contains(WIFI_DEVICE)).count();
    assert_eq!(device_reads, 2, "the open at the first poke only: {:?}", bus.calls);
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

/// A network signal whose own read fails (the network domain's outage) and whose
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

fn close_and_reopen_the_row(b: &mut FakeBus) {
    b.tap.set(WifiScope::Closed);
    b.tap.set(WifiScope::Row);
}

fn close_and_reopen_the_detail(b: &mut FakeBus) {
    b.tap.set(WifiScope::Closed);
    b.tap.set(WifiScope::Detail);
}

/// A close and a reopen the thread reads as one poke (the same
/// scope before and after) are still a new open: everything read again (the device found again,
/// the saved profiles listed again) and published, tagged with the new open.
#[test]
fn a_close_and_reopen_read_as_one_poke_is_a_new_open() {
    let mut bus = FakeBus::workstation();
    bus.with_wifi();
    let steps: [Step; 2] = [(Signal::poke(), open_the_row), (Signal::poke(), close_and_reopen_the_row)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert_eq!(seen.len(), 3, "initial, the row, the reopened row: {seen:?}");
    assert!(seen[2].wifi.as_ref().is_some_and(|w| w.device.is_some()), "{:?}", seen[2].wifi);
    assert!(seen[2].wifi_open > seen[1].wifi_open, "tagged with the new open");
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 2, "found again: {:?}", bus.calls);
    let mut bus = FakeBus::workstation();
    bus.with_wifi();
    let steps: [Step; 2] = [(Signal::poke(), open_the_detail), (Signal::poke(), close_and_reopen_the_detail)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("ListConnections ")).count(), 2, "listed again: {:?}", bus.calls);
    assert_eq!(bus.rules.len(), dbusrs_bus::WIFI_RULES.len(), "the rules removed then added again, never doubled");
}

/// Decision "A Wi-Fi attempt on a new network outlives the quick panel" (docs/decisions.md): a follower keeps the tap at Detail through the panel's close and reopen, so
/// the open never changes and the reader keeps its lists (a reset would read a profile skipped
/// before as new).
#[test]
fn a_tap_that_never_closes_keeps_its_open() {
    let tap = WifiTap::default();
    tap.set(WifiScope::Detail);
    let (_, first) = tap.snapshot();
    tap.set(WifiScope::Detail);
    tap.set(WifiScope::Row.max(WifiScope::Detail));
    assert_eq!(tap.snapshot(), (WifiScope::Detail, first), "never Closed: the same open");
    tap.set(WifiScope::Closed);
    tap.set(WifiScope::Row);
    assert_eq!(tap.snapshot(), (WifiScope::Row, first + 1), "closed then opened: a new open");
    tap.set(WifiScope::Detail);
    assert_eq!(tap.snapshot().1, first + 1, "Row to Detail: the same open");
}

fn close_the_detail_a_rule_refused(b: &mut FakeBus) {
    b.failing_rules.insert((false, dbusrs_bus::WIFI_RULES[0].to_string()));
    b.tap.set(WifiScope::Row);
}

/// A detail rule the bus refuses to remove would wake the thread for nothing
/// until the next reconnection: the session ends now (the outage of the network domain alone, the
/// other modules kept), and the new connection holds none of them.
#[test]
fn a_rule_left_on_the_bus_ends_the_session() {
    let mut bus = FakeBus::workstation();
    bus.with_wifi();
    let steps: [Step; 3] =
        [(Signal::poke(), open_the_detail), (Signal::poke(), close_the_detail_a_rule_refused), (radio_changed(), nothing)];
    bus.script = VecDeque::from(steps);
    let (_, r) = run(&mut bus);
    assert_eq!(outage_of(&r.unwrap_err()), Outage::Domain(Domain::Network));
    assert_eq!(bus.script.len(), 1, "ended at that poke");
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

const HEADSET: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";
const KEYBOARD_TEST: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_05";

fn bt_row(b: &mut FakeBus) {
    b.bt_tap.set(BtScope::Row);
}
fn bt_detail(b: &mut FakeBus) {
    b.bt_tap.set(BtScope::Detail);
}
fn bt_closed(b: &mut FakeBus) {
    b.bt_tap.set(BtScope::Closed);
}
fn keyboard_nearby(b: &mut FakeBus) {
    b.nearby(KEYBOARD_TEST, "Keyboard-Test", Some(-50));
}

fn bt_added(path: &str) -> Signal {
    with_names(signal("/", OBJECT_MANAGER, "InterfacesAdded", Some(path)), &["org.freedesktop.DBus.Introspectable", "org.bluez.Device1"])
}

fn bt_labels(v: &[BtEntry]) -> Vec<&str> {
    v.iter().map(|e| e.label.as_str()).collect()
}

#[test]
fn the_bt_view_is_read_on_a_poke_followed_in_detail_and_dropped_on_close() {
    let mut bus = FakeBus::workstation();
    let steps: [Step; 4] =
        [(Signal::poke(), bt_row), (Signal::poke(), bt_detail), (bt_added(KEYBOARD_TEST), keyboard_nearby), (Signal::poke(), bt_closed)];
    bus.script = VecDeque::from(steps);
    let (seen, r) = run(&mut bus);
    assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    let shape: Vec<_> =
        seen.iter().map(|s| s.bt.as_ref().map(|v| (v.adapter.is_some(), bt_labels(&v.paired), bt_labels(&v.nearby)))).collect();
    assert_eq!(
        shape,
        [
            None,
            Some((true, vec![], vec![])),
            Some((true, vec!["CasqueBT"], vec![])),
            Some((true, vec!["CasqueBT"], vec!["Keyboard-Test"])),
            None
        ],
        "initial, the row, the detail, the device found, closed"
    );
    assert_eq!(seen.iter().map(|s| s.bt_open).collect::<Vec<_>>(), [0, 1, 1, 1, 1]);
    assert!(seen.iter().all(|s| s.bluetooth == Some(BtState::On { devices: vec![] })), "the bar's state unchanged");
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetManagedObjects")).count(), 3, "the bar's, the row's, the detail's");
    let device_reads: Vec<_> = bus.calls.iter().filter(|c| c.starts_with("GetAll") && c.ends_with("org.bluez.Device1")).collect();
    assert_eq!(device_reads, [&format!("GetAll {BLUEZ_OWNER} {KEYBOARD_TEST} org.bluez.Device1")]);
}

#[test]
fn a_bt_device_signal_reads_nothing_outside_the_detail() {
    let mut bus = FakeBus::workstation();
    let alias = with_names(signal(HEADSET, PROPERTIES, "PropertiesChanged", Some("org.bluez.Device1")), &["Alias"]);
    let steps: [Step; 5] = [
        (bt_added(KEYBOARD_TEST), keyboard_nearby),
        (alias.clone(), nothing),
        (Signal::poke(), bt_row),
        (bt_added(KEYBOARD_TEST), nothing),
        (alias, nothing),
    ];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert_eq!(seen.len(), 2, "initial and the row");
    assert!(!bus.calls.iter().any(|c| c.starts_with("GetAll") && c.contains("/org/bluez/")), "{:?}", bus.calls);
}

/// Never `continue`d as Wi-Fi's signals are: Connected matters to the bar and to the open detail.
#[test]
fn a_device_connecting_in_the_detail_reaches_the_bar_and_the_panel() {
    let mut bus = FakeBus::workstation();
    let connected = with_names(signal(HEADSET, PROPERTIES, "PropertiesChanged", Some("org.bluez.Device1")), &["Connected"]);
    let steps: [Step; 2] = [(Signal::poke(), bt_detail), (connected, bluetooth_connected)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    let last = seen.last().expect("published");
    assert!(matches!(&last.bluetooth, Some(BtState::On { devices }) if devices.len() == 2), "{:?}", last.bluetooth);
    assert_eq!(last.bt.as_ref().map(|v| v.paired[0].connected), Some(true));
}

#[test]
fn messages_has_six_and_bt_none_when_down() {
    assert_eq!(messages(None)[5], Msg::Bt { open: None, state: None });
    let s = SystemState { bt: Some(BtView::default()), bt_open: 4, ..SystemState::default() };
    assert_eq!(messages(Some(s))[5], Msg::Bt { open: Some(4), state: Some(BtView::default()) });
}

#[test]
fn a_bluetooth_outage_resends_no_bt_view() {
    let shown = SystemState { bt: Some(BtView::default()), bt_open: 2, ..SystemState::default() };
    for (domain, expected) in [(Domain::Bluetooth, SystemState { bt: None, ..shown.clone() }), (Domain::Battery, shown.clone())] {
        let sends = RefCell::new(Vec::new());
        run_supervised(
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                publish(shown.clone());
                Err(tag(io::Error::other("wedged"), Outage::Domain(domain)))
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        assert_eq!(sends.borrow()[1], Some(expected), "{domain:?} down");
    }
}

#[test]
fn messages_for_the_bar() {
    assert_eq!(
        messages(None),
        [
            Msg::Network(None),
            Msg::Bluetooth(None),
            Msg::Battery(None),
            Msg::Profile(None),
            Msg::Wifi { open: None, state: None },
            Msg::Bt { open: None, state: None },
            Msg::Vpn { open: None, state: None },
            Msg::Units(None)
        ],
        "bus down: \"–\" everywhere"
    );
    let s = SystemState {
        network: Some(NetState::Offline),
        bluetooth: None,
        battery: Some(PowerState::default()),
        power: Some(ProfileState::default()),
        units: None,
        wifi: None,
        wifi_open: 0,
        bt: None,
        bt_open: 0,
        vpn: None,
        vpn_open: 0,
    };
    assert_eq!(
        messages(Some(s)),
        [
            Msg::Network(Some(NetState::Offline)),
            Msg::Bluetooth(None),
            Msg::Battery(Some(PowerState::default())),
            Msg::Profile(Some(ProfileState::default())),
            Msg::Wifi { open: Some(0), state: None },
            Msg::Bt { open: Some(0), state: None },
            Msg::Vpn { open: Some(0), state: None },
            Msg::Units(None)
        ]
    );
}

#[test]
fn bt_changes_are_told_apart() {
    use bt::BtChange;
    const DEV: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_10";
    let changed =
        |at: &str, iface: &str, names: &[&str]| with_names(signal(at, PROPERTIES, "PropertiesChanged", Some(iface)), names).bt_change();
    let device = |rssi_only| Some(BtChange::Device { path: DEV.into(), rssi_only });
    for n in ["Name", "Alias", "Icon", "Paired", "Trusted", "Connected"] {
        assert_eq!(changed(DEV, "org.bluez.Device1", &[n]), device(false), "{n}");
    }
    assert_eq!(changed(DEV, "org.bluez.Device1", &["RSSI"]), device(true));
    assert_eq!(changed(DEV, "org.bluez.Device1", &["RSSI", "TxPower", "ManufacturerData"]), device(true));
    assert_eq!(changed(DEV, "org.bluez.Device1", &["RSSI", "Connected"]), device(false));
    assert_eq!(changed(DEV, "org.bluez.Device1", &["ManufacturerData"]), None);
    assert_eq!(changed(DEV, "org.bluez.Device1", &[]), device(false), "unknown names: read");
    for n in ["Powered", "PowerState"] {
        assert_eq!(changed("/org/bluez/hci0", "org.bluez.Adapter1", &[n]), Some(BtChange::Adapter("/org/bluez/hci0".into())));
    }
    assert_eq!(changed("/org/bluez/hci0", "org.bluez.Adapter1", &["Discovering"]), None);
    assert_eq!(changed(DEV, "org.bluez.Battery1", &["Percentage"]), None, "the bar's only");
    let objects = |member, arg0, names: &[&str]| with_names(signal("/", OBJECT_MANAGER, member, arg0), names).bt_change();
    let both = ["org.freedesktop.DBus.Introspectable", "org.bluez.Device1"];
    assert_eq!(objects("InterfacesAdded", Some(DEV), &both), Some(BtChange::Added(DEV.into())));
    assert_eq!(objects("InterfacesRemoved", Some(DEV), &["org.bluez.Device1"]), Some(BtChange::Removed(DEV.into())));
    assert_eq!(objects("InterfacesAdded", Some(DEV), &["org.bluez.Battery1"]), None);
    assert_eq!(objects("InterfacesAdded", None, &both), None, "no path: nothing to read");
    assert_eq!(objects("InterfacesAdded", Some("/org/freedesktop/x"), &both), None, "not BlueZ's");
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
    assert_eq!(d("/org/freedesktop/systemd1", PROPERTIES, "PropertiesChanged", None), Some(Domain::Units));
    assert_eq!(d("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(SYSTEMD1)), Some(Domain::Units));
    assert_eq!(
        d("/org/freedesktop/systemd1/unit/example_2eservice", PROPERTIES, "PropertiesChanged", None),
        None,
        "a unit's, not the Manager's"
    );
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
    // Symmetric with InterfacesAdded: a connected device
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
    // is relevant.
    const SCANNED: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_99";
    let changed = |names: &[&str]| with_names(signal(SCANNED, PROPERTIES, "PropertiesChanged", Some("org.bluez.Device1")), names).domain();
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
    let changed_adapter =
        |names: &[&str]| with_names(signal("/org/bluez/hci0", PROPERTIES, "PropertiesChanged", Some("org.bluez.Adapter1")), names).domain();
    assert_eq!(changed_adapter(&["Discovering"]), None);
    assert_eq!(changed_adapter(&["Powered"]), Some(Domain::Bluetooth));
}

#[test]
fn upower_raw_battery_and_line_power_signals_are_dropped_display_device_carries_them() {
    // The match rule can't exclude a few device names by
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
    // `read` is retried fresh on every backoff
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
    assert_eq!(optional(Ok(Props::default())).unwrap(), Some(Props::default()));
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
fn clock(step: std::time::Duration) -> impl FnMut() -> Instant {
    let mut t = Instant::now();
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
        units: None,
        wifi: None,
        wifi_open: 0,
        bt: None,
        bt_open: 0,
        vpn: None,
        vpn_open: 0,
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

/// The state re-sent for a domain's outage carries no Wi-Fi: the last one
/// read may be a detail's whole saved list, stale by the time the session is back.
#[test]
fn a_domain_outage_resends_no_wifi() {
    let sends = RefCell::new(Vec::new());
    let shown = SystemState {
        network: Some(NetState::Offline),
        wifi: Some(WifiState { enabled: true, device: None, stale: false }),
        wifi_open: 3,
        ..SystemState::default()
    };
    run_supervised(
        |publish: &mut dyn FnMut(SystemState) -> bool| {
            publish(shown.clone());
            Err(tag(io::Error::other("wedged"), Outage::Domain(Domain::Network)))
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
    assert_eq!(seen[1], Some(SystemState { wifi: None, ..shown.clone() }), "the rest as it was, the Wi-Fi none");
    drop(seen);
    // Another domain's outage (BlueZ, UPower, the power profile) says nothing about
    // NetworkManager: the Wi-Fi stays, an open detail and its password field with it; marked
    // stale (F4), read before the outage: never enough for an attempt to delete a profile.
    for domain in [Domain::Bluetooth, Domain::Battery, Domain::Power] {
        sends.borrow_mut().clear();
        run_supervised(
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                publish(shown.clone());
                Err(tag(io::Error::other("wedged"), Outage::Domain(domain)))
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        let kept = shown.wifi.clone().map(|w| WifiState { stale: true, ..w });
        assert_eq!(sends.borrow()[1], Some(SystemState { wifi: kept, ..shown.clone() }), "{domain:?} down: the Wi-Fi kept");
        assert!(!shown.wifi.as_ref().is_some_and(|w| w.stale), "the first publish fresh");
    }
}

/// The VPN as the Wi-Fi: NetworkManager's own outage re-sends none (the row hides, an open detail
/// closes); another domain's outage keeps it.
#[test]
fn a_network_outage_resends_no_vpn() {
    let shown = SystemState { network: Some(NetState::Offline), vpn: Some(VpnState::default()), vpn_open: 2, ..SystemState::default() };
    let down = |domain: Domain| {
        let sends = RefCell::new(Vec::new());
        run_supervised(
            |publish: &mut dyn FnMut(SystemState) -> bool| {
                publish(shown.clone());
                Err(tag(io::Error::other("wedged"), Outage::Domain(domain)))
            },
            &|v: Option<SystemState>| {
                sends.borrow_mut().push(v);
                sends.borrow().len() < 2
            },
            |_| {},
            clock(Duration::ZERO),
            |_| {},
        );
        sends.into_inner().remove(1)
    };
    assert_eq!(down(Domain::Network), Some(SystemState { vpn: None, ..shown.clone() }), "the rest as it was, the VPN none");
    for domain in [Domain::Bluetooth, Domain::Battery, Domain::Power] {
        assert_eq!(down(domain), Some(shown.clone()), "{domain:?} down: the VPN kept");
    }
}

#[test]
fn a_bus_outage_blanks_everything_during_the_backoff() {
    let sends = RefCell::new(Vec::new());
    let shown = SystemState {
        network: Some(NetState::Offline),
        bluetooth: Some(BtState::Absent),
        battery: Some(PowerState::default()),
        power: None,
        units: None,
        wifi: None,
        wifi_open: 0,
        bt: None,
        bt_open: 0,
        vpn: None,
        vpn_open: 0,
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

/// A single domain's own read
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

/// Within one `read_all` call, a `Bus` outage
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
    let e = read_all(&mut bus, &DOMAINS, &mut s, &mut DomainLog::default(), &mut false).expect("both reads failed");
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(outage_of(&e), Outage::Bus, "the later bus outage must win over the earlier domain one");
}

#[test]
fn a_wedged_bluez_leaves_network_and_battery_shown_while_bluetooth_shows_dash() {
    // Through the real `session` and
    // a `FakeBus`: BlueZ's GetManagedObjects always times out.
    let sends = RefCell::new(Vec::new());
    let domains = DOMAINS;
    run_supervised(
        |publish: &mut dyn FnMut(SystemState) -> bool| {
            let mut bus = FakeBus::workstation();
            bus.failing.insert("/".into());
            plain_session(
                &mut bus,
                &domains,
                &mut DomainLog::default(),
                &WifiTap::default(),
                &BtTap::default(),
                &VpnTap::default(),
                publish,
            )
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
            plain_session(
                &mut FakeBus::workstation(),
                &domains,
                &mut DomainLog::default(),
                &WifiTap::default(),
                &BtTap::default(),
                &VpnTap::default(),
                publish,
            )
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
    // An unused module costs nothing.
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
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    start(
        vec![],
        None,
        None,
        None,
        None,
        UnitsOptions::default(),
        move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        },
        || true,
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no connection, no thread, nothing published");
}

/// A panel reading failing drops that reading only, and names its own domain's outage (the bus's
/// when the bus went away).
#[test]
fn a_panel_reading_failing_is_its_own_domains_outage() {
    let full = || SystemState { wifi: Some(WifiState::default()), bt: Some(BtView::default()), ..SystemState::default() };
    let eof = || io::Error::new(io::ErrorKind::UnexpectedEof, "gone");

    let mut s = full();
    let e = wifi_into(Err(io::Error::other("rule")), &mut s).unwrap_err();
    assert_eq!(outage_of(&e), Outage::Domain(Domain::Network));
    assert_eq!((s.wifi, s.bt.is_some()), (None, true), "only its own reading");
    let mut s = full();
    let e = bt_into(Err(io::Error::other("read")), &mut s).unwrap_err();
    assert_eq!(outage_of(&e), Outage::Domain(Domain::Bluetooth));
    assert_eq!((s.bt, s.wifi.is_some()), (None, true), "only its own reading");

    let mut s = full();
    assert_eq!(outage_of(&wifi_into(Err(eof()), &mut s).unwrap_err()), Outage::Bus);
    assert_eq!(outage_of(&bt_into(Err(eof()), &mut s).unwrap_err()), Outage::Bus);
}

fn vpn_row(b: &mut FakeBus) {
    b.vpn_tap.set(VpnScope::Row);
}
fn vpn_closed(b: &mut FakeBus) {
    b.vpn_tap.set(VpnScope::Closed);
}
fn close_and_reopen_the_vpn_row(b: &mut FakeBus) {
    b.vpn_tap.set(VpnScope::Closed);
    b.vpn_tap.set(VpnScope::Row);
}
fn vpn_up(b: &mut FakeBus) {
    b.props_mut("/org/freedesktop/NetworkManager/ActiveConnection/20", network::ACTIVE).0.insert("State".into(), Prop::Int(1));
}

fn calls_of(b: &FakeBus, prefix: &str) -> usize {
    b.calls.iter().filter(|c| c.starts_with(prefix)).count()
}

#[test]
fn the_vpn_follows_its_tap() {
    let mut bus = vpn::tests::vpn_bus();
    let steps: [Step; 2] = [(Signal::poke(), vpn_row), (Signal::poke(), vpn_closed)];
    bus.script = VecDeque::from(steps);
    let (seen, r) = run(&mut bus);
    assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(seen.len(), 3, "initial, the row, closed");
    assert_eq!(seen[0].vpn, None);
    let row = seen[1].vpn.as_ref().expect("the row's state");
    assert_eq!(row.profiles.len(), 2);
    assert_eq!(row.active.len(), 3);
    assert_eq!(seen[2].vpn, None, "closed: the state goes");
    assert_eq!(seen.iter().map(|s| s.vpn_open).collect::<Vec<_>>(), [0, 1, 1]);
    assert_eq!(calls_of(&bus, "ListConnections "), 1, "listed once, at the open: {:?}", bus.calls);
}

#[test]
fn a_closed_vpn_tap_reads_nothing() {
    let mut bus = vpn::tests::vpn_bus();
    let (seen, _) = run(&mut bus);
    assert_eq!(seen[0].vpn, None);
    assert_eq!(calls_of(&bus, "ListConnections "), 0);
    assert_eq!(calls_of(&bus, "GetSettings "), 0);
}

/// An active connection's `State` re-reads the actives at the row, never the list.
#[test]
fn a_network_signal_rereads_the_open_vpn() {
    let mut bus = vpn::tests::vpn_bus();
    let state = with_names(
        signal("/org/freedesktop/NetworkManager/ActiveConnection/20", PROPERTIES, "PropertiesChanged", Some(network::ACTIVE)),
        &["State"],
    );
    let steps: [Step; 2] = [(Signal::poke(), vpn_row), (state, vpn_up)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    let states: Vec<Option<u32>> =
        seen.iter().map(|s| s.vpn.as_ref().and_then(|v| v.active.iter().find(|a| a.uuid.ends_with("20")).map(|a| a.state))).collect();
    assert_eq!(states, [None, Some(2), Some(1)], "{seen:?}");
    assert_eq!(calls_of(&bus, "ListConnections "), 1, "the actives again, not the list: {:?}", bus.calls);
    assert_eq!(calls_of(&bus, "GetSettings "), 2);
}

/// A close and a reopen the thread reads as one poke (the scope unchanged) are a new open: the
/// connections are listed again, and the state is published tagged with it.
#[test]
fn a_vpn_close_and_reopen_read_as_one_poke_is_a_new_open() {
    let mut bus = vpn::tests::vpn_bus();
    let steps: [Step; 2] = [(Signal::poke(), vpn_row), (Signal::poke(), close_and_reopen_the_vpn_row)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert_eq!(seen.len(), 3, "initial, the row, the reopened row: {seen:?}");
    assert!(seen[2].vpn.is_some());
    assert!(seen[2].vpn_open > seen[1].vpn_open, "tagged with the new open");
    assert_eq!(calls_of(&bus, "ListConnections "), 2, "listed again: {:?}", bus.calls);
}

#[test]
fn messages_has_eight_and_vpn_none_when_down() {
    assert_eq!(messages(None).len(), 8);
    assert_eq!(messages(None)[6], Msg::Vpn { open: None, state: None });
    let s = SystemState { vpn: Some(VpnState::default()), vpn_open: 3, ..SystemState::default() };
    assert_eq!(messages(Some(s))[6], Msg::Vpn { open: Some(3), state: Some(VpnState::default()) });
}

fn vpn_dropped(b: &mut FakeBus) {
    b.set_actives(&[10, 9, 3, 2]);
}

/// The network domain's own re-read (the root's ActiveConnections) refreshes an open VPN reading
/// too; closed, it reads no VPN.
#[test]
fn the_network_domains_reread_refreshes_the_open_vpn() {
    let mut bus = vpn::tests::vpn_bus();
    let root = with_names(signal(network::PATH, PROPERTIES, "PropertiesChanged", Some(network::INTERFACE)), &["ActiveConnections"]);
    let steps: [Step; 2] = [(Signal::poke(), vpn_row), (root, vpn_dropped)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    let actives: Vec<Option<usize>> = seen.iter().map(|s| s.vpn.as_ref().map(|v| v.active.len())).collect();
    assert_eq!(actives, [None, Some(3), Some(2)], "{seen:?}");
}

/// A network signal with the VPN tap Closed: the network domain re-reads as it always does, and
/// the VPN reader asks NetworkManager nothing (no list, no settings, no active connection's
/// properties beyond what the domain's own link read takes).
#[test]
fn a_network_signal_with_the_vpn_tap_closed_calls_no_vpn() {
    let mut bus = vpn::tests::vpn_bus();
    let state = with_names(
        signal("/org/freedesktop/NetworkManager/ActiveConnection/20", PROPERTIES, "PropertiesChanged", Some(network::ACTIVE)),
        &["State"],
    );
    let steps: [Step; 1] = [(state, vpn_up)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert!(seen.iter().all(|s| s.vpn.is_none()), "{seen:?}");
    assert_eq!(calls_of(&bus, "ListConnections "), 0, "{:?}", bus.calls);
    assert_eq!(calls_of(&bus, "GetSettings "), 0, "{:?}", bus.calls);
    // The domain reads each active connection once at the start and once after the signal; a VPN
    // reading would add more.
    for n in [10, 9, 3, 2, 20] {
        let path = format!("/org/freedesktop/NetworkManager/ActiveConnection/{n} ");
        let reads = bus.calls.iter().filter(|c| c.starts_with("GetAll ") && c.contains(&path)).count();
        assert_eq!(reads, 2, "active connection {n}: {:?}", bus.calls);
    }
}

fn forget_the_calls_then_vpn_up(b: &mut FakeBus) {
    b.calls.clear();
    vpn_up(b);
}

/// Opened at the row then closed: a later network signal reads only the domain's own (the root and
/// each active connection once), never the VPN reader's again.
#[test]
fn a_network_signal_after_the_vpn_row_closed_calls_no_vpn() {
    let mut bus = vpn::tests::vpn_bus();
    let state = with_names(
        signal("/org/freedesktop/NetworkManager/ActiveConnection/20", PROPERTIES, "PropertiesChanged", Some(network::ACTIVE)),
        &["State"],
    );
    let steps: [Step; 3] = [(Signal::poke(), vpn_row), (Signal::poke(), vpn_closed), (state, forget_the_calls_then_vpn_up)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert_eq!(seen.last().map(|s| s.vpn.is_none()), Some(true), "{seen:?}");
    assert_eq!(calls_of(&bus, "ListConnections "), 0, "{:?}", bus.calls);
    assert_eq!(calls_of(&bus, "GetSettings "), 0, "{:?}", bus.calls);
    let root = format!("GetAll {NM_OWNER} {} {}", network::PATH, network::INTERFACE);
    assert_eq!(bus.calls.iter().filter(|c| **c == root).count(), 1, "the domain's root read only: {:?}", bus.calls);
    for n in [10, 9, 3, 2, 20] {
        let path = format!("/org/freedesktop/NetworkManager/ActiveConnection/{n} ");
        let reads = bus.calls.iter().filter(|c| c.starts_with("GetAll ") && c.contains(&path)).count();
        assert_eq!(reads, 1, "active connection {n}: {:?}", bus.calls);
    }
}

fn starts(bus: &FakeBus) -> Vec<&String> {
    bus.calls.iter().filter(|c| c.starts_with("StartServiceByName")).collect()
}

/// "UPower is the one service the daemon may start" (docs/decisions.md): UPower absent and the battery wanted, one activation of UPower and
/// nothing else, then the owner appearing brings the battery in.
#[test]
fn upower_absent_and_the_battery_wanted_is_asked_for_once_and_its_appearance_is_followed() {
    let mut bus = FakeBus::workstation();
    bus.owners.remove(UPOWER);
    bus.upower_activatable = true;
    let steps: [Step; 1] = [(signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(UPOWER)), nothing)];
    bus.script = VecDeque::from(steps);
    let (seen, _) = run(&mut bus);
    assert_eq!(starts(&bus), ["StartServiceByName org.freedesktop.UPower"], "{:?}", bus.calls);
    assert_eq!(seen[0].battery, Some(PowerState::default()), "nothing to show before UPower is there");
    assert_eq!(seen.last().unwrap().battery, Some(workstation_battery()), "read when the name appears");
    assert_unique_destinations(&bus.calls);
}

#[test]
fn the_activation_comes_after_the_first_publication() {
    use std::sync::atomic::Ordering;
    let mut bus = FakeBus::workstation();
    bus.owners.remove(UPOWER);
    bus.upower_activatable = true;
    let steps: [Step; 1] = [(signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(UPOWER)), nothing)];
    bus.script = VecDeque::from(steps);
    let starts = bus.starts.clone();
    let (tap, bt, vpn) = (bus.tap.clone(), bus.bt_tap.clone(), bus.vpn_tap.clone());
    let mut seen = Vec::new();
    let _ = plain_session(&mut bus, &DOMAINS, &mut DomainLog::default(), &tap, &bt, &vpn, &mut |_| {
        seen.push(starts.load(Ordering::SeqCst));
        true
    });
    assert_eq!(seen.first(), Some(&0), "nothing asked when the first state is published");
    assert_eq!(seen.last(), Some(&1), "asked before the owner's appearance is read");
    assert_eq!(starts.load(Ordering::SeqCst), 1);
}

#[test]
fn no_battery_wanted_no_activation() {
    let mut bus = FakeBus::workstation();
    bus.owners.remove(UPOWER);
    bus.upower_activatable = true;
    let _ = run_over(&mut bus, &[Domain::Network, Domain::Bluetooth, Domain::Power]);
    assert!(starts(&bus).is_empty(), "{:?}", bus.calls);
    assert!(!bus.owners.contains_key(UPOWER));
}

#[test]
fn upower_already_running_is_not_activated() {
    let mut bus = FakeBus::workstation();
    let _ = run(&mut bus);
    assert!(starts(&bus).is_empty(), "{:?}", bus.calls);
}

#[test]
fn a_refused_activation_is_not_retried_and_leaves_the_session_alone() {
    let mut bus = FakeBus::workstation();
    bus.owners.remove(UPOWER);
    let steps: [Step; 2] = [
        (signal(network::PATH, PROPERTIES, "PropertiesChanged", None), wifi_primary),
        (signal(network::PATH, PROPERTIES, "PropertiesChanged", None), nothing),
    ];
    bus.script = VecDeque::from(steps);
    let (seen, r) = run(&mut bus);
    assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof, "ended by the script, not by the activation");
    assert_eq!(starts(&bus).len(), 1);
    assert!(seen.len() >= 2);
}

#[test]
fn the_activation_is_logged_with_fixed_words() {
    use super::upower::activate_if_absent;
    let mut bus = FakeBus::workstation();
    bus.owners.remove(UPOWER);
    let mut lines = Vec::new();
    activate_if_absent(&mut bus, &mut |m| lines.push(m.to_string()));
    assert_eq!(lines, ["upower: activation failed (org.freedesktop.DBus.Error.ServiceUnknown)"]);
    let mut bus = FakeBus::workstation();
    bus.owners.remove(UPOWER);
    bus.upower_activatable = true;
    lines.clear();
    activate_if_absent(&mut bus, &mut |m| lines.push(m.to_string()));
    assert_eq!(lines, ["upower: activation asked"]);
}

// The failed-units count (diagnostics spec § 6).

/// A session over `domains` with `opts`: the publications, how many times the loop was told the
/// count was read, and the session's end.
fn run_units(bus: &mut FakeBus, domains: &[Domain], opts: UnitsOptions) -> (Vec<SystemState>, usize, io::Result<()>) {
    let mut seen = Vec::new();
    let mut told = 0;
    let (tap, bt_tap, vpn_tap, units_tap) = (bus.tap.clone(), bus.bt_tap.clone(), bus.vpn_tap.clone(), bus.units_tap.clone());
    let r = session(
        bus,
        domains,
        &mut DomainLog::default(),
        &tap,
        &bt_tap,
        &vpn_tap,
        &units_tap,
        opts,
        &mut |s| {
            seen.push(s);
            true
        },
        &mut || {
            told += 1;
            true
        },
    );
    (seen, told, r)
}

fn opts(subscribe: bool, piggyback_secs: u64) -> UnitsOptions {
    UnitsOptions { subscribe, piggyback: Duration::from_secs(piggyback_secs) }
}

const MANAGER_CHANGED: fn() -> Signal =
    || with_names(signal(units::PATH, PROPERTIES, "PropertiesChanged", Some(units::MANAGER)), &["NFailedUnits"]);

fn battery_wakes() -> Signal {
    with_names(signal("/org/freedesktop/UPower/devices/DisplayDevice", PROPERTIES, "PropertiesChanged", None), &["Percentage"])
}

fn systemd_owner_changes() -> Signal {
    signal("/org/freedesktop/DBus", "org.freedesktop.DBus", "NameOwnerChanged", Some(SYSTEMD1))
}

#[test]
fn units_read_never_gets_all() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(3);
    assert_eq!(units::read(&mut bus).unwrap(), Some(3));
    assert_eq!(
        bus.calls,
        [format!("GetNameOwner {SYSTEMD1}"), format!("Get {SYSTEMD_OWNER} {} {} NFailedUnits", units::PATH, units::MANAGER),],
        "one property through the owner's unique name: the Manager's GetAll carries its Environment"
    );
    assert!(bus.calls.iter().all(|c| !c.starts_with("GetAll")));
}

#[test]
fn no_systemd_owner_is_none() {
    let mut bus = FakeBus::empty();
    assert_eq!(units::read(&mut bus).unwrap(), None);
    assert_eq!(bus.calls, [format!("GetNameOwner {SYSTEMD1}")], "nothing called on a name nobody owns");
}

#[test]
fn piggyback_rule() {
    let t0 = Instant::now();
    let interval = Duration::from_secs(60);
    assert!(units::should_piggyback(None, t0, interval), "never read: read");
    assert!(!units::should_piggyback(Some(t0), t0 + interval - Duration::from_secs(1), interval));
    assert!(units::should_piggyback(Some(t0), t0 + interval, interval));
    assert!(!units::should_piggyback(Some(t0 + interval), t0, interval), "a clock that went back: not due");
}

#[test]
fn the_manager_signal_maps_to_units() {
    assert_eq!(MANAGER_CHANGED().domain(), Some(Domain::Units));
    assert_eq!(systemd_owner_changes().domain(), Some(Domain::Units));
    let other = with_names(signal(units::PATH, PROPERTIES, "PropertiesChanged", Some(units::MANAGER)), &["SystemState"]);
    assert_eq!(other.domain(), None, "only NFailedUnits emits a change; anything else is not ours");
}

#[test]
fn the_first_read_is_published_and_the_loop_told() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(2);
    let (seen, told, r) = run_units(&mut bus, &[Domain::Units], opts(true, 60));
    assert!(r.is_err(), "the scripted bus ends");
    assert_eq!(seen[0].units, Some(2));
    assert_eq!(told, 1);
    // No systemd1 on the bus: unknown, and the loop told all the same (the read succeeded).
    let mut bus = FakeBus::empty();
    let (seen, told, _) = run_units(&mut bus, &[Domain::Units], opts(true, 60));
    assert_eq!((seen[0].units, told), (None, 1));
    assert_eq!(bus.subscribes(), 0, "nothing to subscribe to");
}

#[test]
fn subscribes_unless_turned_off() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    let _ = run_units(&mut bus, &[Domain::Units], opts(true, 60));
    assert_eq!(bus.subscribes(), 1, "once, after the first read: {:?}", bus.calls);
    let order: Vec<&str> = bus.calls.iter().map(|c| c.split(' ').next().unwrap()).collect();
    assert_eq!(order, ["GetNameOwner", "Get", "GetNameOwner", "Subscribe"]);
    assert!(bus.calls.last().unwrap().contains(SYSTEMD_OWNER), "to the owner's unique name");
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    let _ = run_units(&mut bus, &[Domain::Units], opts(false, 60));
    assert_eq!(bus.subscribes(), 0, "[failed] subscribe = false");
    // A count read again does not subscribe again.
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    bus.script = VecDeque::from([(MANAGER_CHANGED(), nothing as fn(&mut FakeBus))]);
    let _ = run_units(&mut bus, &[Domain::Units], opts(true, 60));
    assert_eq!((bus.units_gets(), bus.subscribes()), (2, 1));
}

fn five_seconds_pass(b: &mut FakeBus) {
    b.advance(Duration::from_secs(5));
}

fn six_more_seconds_pass(b: &mut FakeBus) {
    b.advance(Duration::from_secs(6));
}

fn a_unit_fails(b: &mut FakeBus) {
    b.n_failed = 4;
}

#[test]
fn a_refused_subscribe_falls_back_to_piggyback() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.refuse_subscribe = true;
    let steps: [Step; 3] =
        [(battery_wakes(), five_seconds_pass), (battery_wakes(), a_unit_fails), (battery_wakes(), six_more_seconds_pass)];
    // Each step's change comes just before its signal: 5 s, then a failure at 5 s, then 11 s.
    bus.script = VecDeque::from(steps);
    let (seen, _, _) = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(true, 10));
    assert_eq!(bus.subscribes(), 1, "refused once, never asked again: {:?}", bus.calls);
    assert_eq!(bus.units_gets(), 2, "a battery signal past the interval reads the count once: {:?}", bus.calls);
    assert_eq!(seen.last().unwrap().units, Some(4), "and it is the new count");
}

#[test]
fn another_domains_signal_piggybacks_once_per_interval() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    // Three signals within the interval, one past it, two more after it: at start, then once at
    // 11 s, then once at 22 s.
    let steps: [Step; 6] = [
        (battery_wakes(), five_seconds_pass),
        (battery_wakes(), nothing),
        (battery_wakes(), six_more_seconds_pass),
        (battery_wakes(), nothing),
        (battery_wakes(), eleven_more),
        (battery_wakes(), nothing),
    ];
    bus.script = VecDeque::from(steps);
    let _ = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(false, 10));
    assert_eq!(bus.units_gets(), 3, "at start, past 10 s, past 10 s again: {:?}", bus.calls);
}

/// A subscribed count is pushed by systemd, so another domain's wake-up never
/// reads it (the piggyback is the unsubscribed way).
#[test]
fn a_subscribed_count_is_not_piggybacked() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    let steps: [Step; 2] = [(battery_wakes(), eleven_more), (battery_wakes(), eleven_more)];
    bus.script = VecDeque::from(steps);
    let _ = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(true, 10));
    assert_eq!((bus.units_gets(), bus.subscribes()), (1, 1), "only the first read: {:?}", bus.calls);
}

fn eleven_more(b: &mut FakeBus) {
    b.advance(Duration::from_secs(11));
}

#[test]
fn a_piggyback_needs_the_units_domain() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    let steps: [Step; 1] = [(battery_wakes(), eleven_more)];
    bus.script = VecDeque::from(steps);
    let _ = run_units(&mut bus, &[Domain::Battery], opts(true, 10));
    assert_eq!((bus.units_gets(), bus.subscribes()), (0, 0), "the count is not asked for: nothing read, nothing subscribed");
    assert!(bus.calls.iter().all(|c| !c.contains(SYSTEMD1) && !c.contains(SYSTEMD_OWNER)), "{:?}", bus.calls);
}

fn systemd_restarts(b: &mut FakeBus) {
    // A `daemon-reexec` or a restart: the same name, another owner with no subscriber.
    b.n_failed = 2;
}

#[test]
fn a_new_systemd_owner_resubscribes() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    let steps: [Step; 1] = [(systemd_owner_changes(), systemd_restarts)];
    bus.script = VecDeque::from(steps);
    let (seen, told, _) = run_units(&mut bus, &[Domain::Units], opts(true, 60));
    assert_eq!(bus.subscribes(), 2, "once per owner: {:?}", bus.calls);
    assert_eq!(seen.last().unwrap().units, Some(2), "re-read on the new owner");
    assert_eq!(told, 2);
}

fn poked(b: &mut FakeBus) {
    b.n_failed = 5;
    b.units_tap.poke();
}

#[test]
fn a_poke_rereads_units() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    bus.script = VecDeque::from([(Signal::poke(), poked as fn(&mut FakeBus))]);
    let (seen, told, _) = run_units(&mut bus, &[Domain::Units], opts(false, 60));
    assert_eq!(seen.last().unwrap().units, Some(5));
    assert_eq!((bus.units_gets(), told), (2, 2), "{:?}", bus.calls);
    // A poke of another tap (the panel's) is not a poke of this one.
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    bus.script = VecDeque::from([(Signal::poke(), nothing as fn(&mut FakeBus))]);
    let _ = run_units(&mut bus, &[Domain::Units], opts(false, 60));
    assert_eq!(bus.units_gets(), 1, "no word for the count: not read again");
}

#[test]
fn units_read_is_sent_outside_the_compared_state() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    bus.script = VecDeque::from([(MANAGER_CHANGED(), nothing as fn(&mut FakeBus))]);
    let (seen, told, _) = run_units(&mut bus, &[Domain::Units], opts(false, 60));
    assert_eq!(seen.len(), 1, "the same count twice: one publication");
    assert_eq!(told, 2, "but two reads, two words");
}

fn battery_reads(bus: &FakeBus) -> usize {
    bus.calls.iter().filter(|c| c.starts_with("EnumerateDevices")).count()
}

/// A read error of the count (systemd1 timing out, an odd reply) is the count's alone: unknown for
/// that read, and the session, with the other domains, goes on (diagnostics spec § 6).
#[test]
fn a_units_read_error_hides_the_count_and_the_session_goes_on() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.failing.insert(units::PATH.into());
    let steps: [Step; 2] = [(battery_wakes(), nothing), (Signal::poke(), poke_the_units)];
    bus.script = VecDeque::from(steps);
    let (seen, told, r) = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(true, 60));
    assert_eq!(seen[0].units, None);
    assert!(seen[0].battery.is_some(), "the other domains are shown: {seen:?}");
    assert_eq!(told, 1, "a read that failed is not a read, but the poke's word still reaches the loop (the user count)");
    assert_eq!(bus.subscribes(), 0);
    // The scripted bus's own end, not the count's outage: both scripted signals were handled.
    let e = r.unwrap_err();
    assert_eq!((e.kind(), outage_of(&e)), (io::ErrorKind::UnexpectedEof, Outage::Bus), "{e}");
    assert_eq!(battery_reads(&bus), 2, "the battery signal was read after the count failed: {:?}", bus.calls);
}

/// The same on a resume and on the count's own signal.
#[test]
fn a_units_read_error_ends_no_session_on_resume_or_its_own_signal() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.failing.insert(units::PATH.into());
    let steps: [Step; 3] = [(MANAGER_CHANGED(), nothing), (prepare_for_sleep("false"), nothing), (battery_wakes(), nothing)];
    bus.script = VecDeque::from(steps);
    let (_, told, r) = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(false, 60));
    assert_eq!(told, 0);
    assert_eq!(outage_of(&r.unwrap_err()), Outage::Bus, "only the scripted end");
    assert_eq!(battery_reads(&bus), 3, "start, the resume, the signal: {:?}", bus.calls);
}

/// A systemd that times out is not asked again at every other domain's wake-up: the failed
/// attempt counts as the last read for the piggyback.
#[test]
fn a_failed_count_is_not_piggybacked_before_its_interval() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.failing.insert(units::PATH.into());
    let steps: [Step; 4] = [
        (battery_wakes(), five_seconds_pass),
        (battery_wakes(), nothing),
        (battery_wakes(), six_more_seconds_pass),
        (battery_wakes(), nothing),
    ];
    bus.script = VecDeque::from(steps);
    let _ = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(false, 10));
    assert_eq!(bus.units_gets(), 2, "at start, then once past 10 s: {:?}", bus.calls);
}

/// The bus itself going away under the count's read stays the whole session's outage.
#[test]
fn the_bus_going_away_under_the_count_ends_the_session() {
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.disconnecting.insert(units::PATH.into());
    bus.script = VecDeque::from([(battery_wakes(), nothing as fn(&mut FakeBus))]);
    let (_, told, r) = run_units(&mut bus, &[Domain::Battery, Domain::Units], opts(true, 60));
    assert_eq!(told, 0);
    assert_eq!(outage_of(&r.unwrap_err()), Outage::Bus);
    assert_eq!(bus.script.len(), 1, "no signal was reached");
}

#[test]
fn another_domains_failure_does_not_stand_for_the_counts_read() {
    // Network fails first, the count's own read fails too: nobody is told.
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.failing.insert(network::PATH.into());
    bus.failing.insert(units::PATH.into());
    let (_, told, _) = run_units(&mut bus, &[Domain::Network, Domain::Units], opts(true, 60));
    assert_eq!(told, 0, "the count was not read: {:?}", bus.calls);
    assert_eq!(bus.subscribes(), 0);
    // The count read fine beside a failed network: told, as before.
    let mut bus = FakeBus::workstation();
    bus.with_systemd(1);
    bus.failing.insert(network::PATH.into());
    let (_, told, _) = run_units(&mut bus, &[Domain::Network, Domain::Units], opts(true, 60));
    assert_eq!(told, 1);
}

#[test]
fn an_already_subscribed_manager_counts_as_subscribed() {
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    bus.subscribe_error = Some("org.freedesktop.systemd1.AlreadySubscribed: already");
    let mut link = units::UnitsLink::new(opts(true, 60));
    let now = bus.now();
    link.read_done(&mut bus, Some(1), now).unwrap();
    assert!(link.subscribed(), "daemon-reexec keeps our subscription: not a refusal");
    // Any other error is a refusal.
    let mut bus = FakeBus::empty();
    bus.with_systemd(1);
    bus.subscribe_error = Some("org.freedesktop.DBus.Error.AccessDenied: no");
    let mut link = units::UnitsLink::new(opts(true, 60));
    let now = bus.now();
    link.read_done(&mut bus, Some(1), now).unwrap();
    assert!(!link.subscribed());
    // And a refused one is not asked again.
    let now = bus.now();
    link.read_done(&mut bus, Some(1), now).unwrap();
    assert_eq!(bus.subscribes(), 1);
}
