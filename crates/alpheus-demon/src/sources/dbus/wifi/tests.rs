use super::*;
use crate::sources::dbus::fake::{AP, FakeBus, NM_OWNER, WIFI_DEVICE, WIRED_DEVICE};
use crate::sources::dbus::props::Prop;

/// The recorded Wi-Fi connection's uuid (dbus-nm-active-9.json).
const RECORDED_UUID: &str = "00000000-0000-0000-0000-000000000009";

fn wifi_bus() -> FakeBus {
    let mut bus = FakeBus::workstation();
    bus.with_wifi();
    bus
}

fn row(bus: &mut FakeBus) -> WifiState {
    WifiReader::default().rescope(bus, WifiScope::Row).unwrap().expect("open: a state")
}

#[test]
fn the_row_reads_the_radio_the_device_and_its_access_point() {
    let mut bus = wifi_bus();
    let home = AccessPoint { ssid: b"MON_WIFI_1".to_vec(), strength: 70, security: Security::Psk };
    let device = WifiDevice { state: 100, active: Some(home), active_uuid: Some(RECORDED_UUID.into()), access_points: None, saved: None };
    assert_eq!(row(&mut bus), WifiState { enabled: true, device: Some(device), stale: false });
    let get_all: Vec<&String> = bus.calls.iter().filter(|c| c.starts_with("GetAll ")).collect();
    assert!(get_all.iter().any(|c| c.contains(WIRED_DEVICE)), "the wired device read, then passed over: {get_all:?}");
    assert!(get_all.iter().any(|c| c.ends_with(&format!("{AP}/10 {ACCESS_POINT}"))), "{get_all:?}");
    assert!(
        !get_all.iter().any(|c| c.contains(&format!("{AP}/11")) || c.contains(&format!("{AP}/12"))),
        "the row reads its own access point only: {get_all:?}"
    );
    for c in &bus.calls {
        assert!(c.starts_with("GetNameOwner ") || c.split(' ').nth(1) == Some(NM_OWNER), "{c}: only NetworkManager's unique name");
    }
}

#[test]
fn the_device_is_found_once_per_open_then_kept() {
    let mut bus = wifi_bus();
    let mut r = WifiReader::default();
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    r.reread(&mut bus).unwrap();
    r.reread(&mut bus).unwrap();
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "{:?}", bus.calls);
    assert_eq!(bus.calls.iter().filter(|c| c.contains(WIRED_DEVICE)).count(), 1, "the wired one passed over once");
    r.rescope(&mut bus, WifiScope::Closed).unwrap();
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 2, "a new open finds it again");
}

#[test]
fn closed_reads_nothing_and_keeps_nothing() {
    let mut bus = wifi_bus();
    let mut r = WifiReader::default();
    assert_eq!(r.reread(&mut bus).unwrap(), None);
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    let calls = bus.calls.len();
    assert_eq!(r.rescope(&mut bus, WifiScope::Closed).unwrap(), None);
    assert_eq!(r.reread(&mut bus).unwrap(), None);
    assert_eq!(bus.calls.len(), calls, "closed: not one call");
    assert_eq!(r.scope(), WifiScope::Closed);
}

#[test]
fn no_networkmanager_no_wifi_device_or_the_radio_off() {
    let mut bus = FakeBus::empty();
    assert_eq!(row(&mut bus), WifiState::default(), "no NetworkManager: the row hidden");
    assert_eq!(bus.calls, ["GetNameOwner org.freedesktop.NetworkManager"], "never a call to a name nobody owns");
    let mut bus = FakeBus::workstation();
    assert_eq!(row(&mut bus), WifiState { enabled: true, device: None, stale: false }, "GetDevices unknown (the recording): no device");
    let mut bus = wifi_bus();
    bus.props_mut(WIFI_DEVICE, DEVICE).0.insert("DeviceType".into(), Prop::Int(1));
    assert_eq!(row(&mut bus).device, None, "a wired device only: no Wi-Fi row");
    let mut bus = wifi_bus();
    bus.props_mut(super::super::network::PATH, super::super::network::INTERFACE).0.insert("WirelessEnabled".into(), Prop::Bool(false));
    assert!(!row(&mut bus).enabled, "the radio off");
}

#[test]
fn a_failed_read_hides_the_row_and_a_lost_bus_is_an_error() {
    let mut bus = wifi_bus();
    bus.failing.insert(WIFI_DEVICE.into());
    assert_eq!(row(&mut bus), WifiState::default(), "a timeout: the row hidden, not an outage of the network module");
    let mut bus = wifi_bus();
    bus.disconnecting.insert(WIFI_DEVICE.into());
    let e = WifiReader::default().rescope(&mut bus, WifiScope::Row).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "the bus itself gone: the session's to handle");
}

#[test]
fn an_access_point_is_read_bounded() {
    let p = |ssid: &[u8], strength: i64, rsn: i64| {
        Props(
            [
                ("Ssid".to_string(), Prop::Bytes(ssid.to_vec())),
                ("Strength".to_string(), Prop::Int(strength)),
                ("Flags".to_string(), Prop::Int(1)),
                ("RsnFlags".to_string(), Prop::Int(rsn)),
            ]
            .into(),
        )
    };
    assert_eq!(
        access_point(&p(b"Cafe-Libre", 55, 0)).map(|a| (a.strength, a.security)),
        Some((55, Security::Other)),
        "privacy, no key: WEP"
    );
    assert_eq!(
        access_point(&p(&[b'x'; 32], 250, 0x188)).map(|a| (a.ssid.len(), a.strength)),
        Some((32, 100)),
        "32 bytes kept; strength capped"
    );
    assert_eq!(access_point(&p(&[b'x'; 33], 50, 0)), None, "longer than 802.11 allows");
    assert_eq!(access_point(&p(b"", 50, 0)), None, "hidden");
    assert_eq!(access_point(&Props::default()), None);
}

#[test]
fn security_from_the_flags() {
    assert_eq!(Security::from_flags(0, 0, 0), Security::Open);
    assert_eq!(Security::from_flags(1, 0, 0x188), Security::Psk, "WPA2 personal");
    assert_eq!(Security::from_flags(1, 0x148, 0), Security::Psk, "WPA personal");
    assert_eq!(Security::from_flags(1, 0, 0x400 | 0x8), Security::Psk, "WPA3 SAE");
    assert_eq!(Security::from_flags(1, 0, 0x200 | 0x8), Security::Enterprise, "802.1X");
    assert_eq!(Security::from_flags(1, 0, 0x300), Security::Enterprise, "802.1X wins over PSK: a password field would not do");
    assert_eq!(Security::from_flags(1, 0, 0x2000), Security::Enterprise, "Suite B 192");
    assert_eq!(Security::from_flags(0, 0, 0x800), Security::Open, "OWE: no password");
    assert_eq!(Security::from_flags(1, 0, 0), Security::Other, "WEP");
    assert!(!Security::Open.secured() && Security::Psk.secured() && Security::Enterprise.secured() && Security::Other.secured());
}

#[test]
fn uuids_are_checked_before_any_use() {
    assert!(valid_uuid("11111111-2222-4333-8444-555555555555"));
    assert!(valid_uuid(RECORDED_UUID));
    for bad in ["", "-1111", "1111 2222", "1111;rm", "g111", &"a".repeat(65)] {
        assert!(!valid_uuid(bad), "{bad:?}");
    }
}

#[test]
fn the_tap_wakes_only_on_a_change_and_drains() {
    let tap = WifiTap::new().unwrap();
    let fd = tap.wake_fd().expect("a socket pair");
    let readable = || {
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: poll(2) on one pollfd owned here, no wait.
        unsafe { libc::poll(&mut p, 1, 0) == 1 }
    };
    assert_eq!(tap.scope(), WifiScope::Closed);
    assert!(!readable());
    assert!(tap.set(WifiScope::Row));
    assert!(readable(), "a change wakes the thread");
    tap.drain();
    assert!(!readable(), "drained");
    assert!(!tap.set(WifiScope::Row), "the same scope: no word");
    assert!(!readable());
    assert_eq!(tap.scope(), WifiScope::Row);
    // SAFETY: fcntl(2) F_GETFD only reads the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "a child would inherit the tap");
    let idle = WifiTap::default();
    assert!(idle.set(WifiScope::Detail) && idle.wake_fd().is_none(), "the tests' tap: a scope, no socket");
}

/// Review Focus 1, the row's part: NetworkManager restarting under a new unique name while the
/// panel is open: the device's path, an object of the old instance, is dropped and found again.
#[test]
fn a_new_networkmanager_owner_finds_the_device_again() {
    let mut bus = wifi_bus();
    let mut r = WifiReader::default();
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    r.reread(&mut bus).unwrap();
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "{:?}", bus.calls);
    bus.owners.insert(NM, ":1.20");
    bus.calls.clear();
    assert!(r.reread(&mut bus).unwrap().is_some_and(|w| w.device.is_some()));
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetDevices")).count(), 1, "the new owner asked again: {:?}", bus.calls);
    for c in &bus.calls {
        assert!(c.starts_with("GetNameOwner ") || c.split(' ').nth(1) == Some(":1.20"), "{c}: only the new owner");
    }
}

/// Final review A-M3: a read that fails (a timeout) once the device is known keeps the last
/// state, unchanged, so a followed attempt never takes it for NetworkManager gone; NetworkManager
/// really gone (no owner) or no Wi-Fi device any more still reads as no device.
#[test]
fn a_failed_reread_keeps_the_last_state_a_real_absence_does_not() {
    let mut bus = wifi_bus();
    let mut r = WifiReader::default();
    let first = r.rescope(&mut bus, WifiScope::Row).unwrap();
    assert!(first.as_ref().is_some_and(|s| s.device.is_some()));
    bus.failing.insert(WIFI_DEVICE.into());
    assert_eq!(r.reread(&mut bus).unwrap(), first, "a timeout: the last state, no change");
    bus.failing.clear();
    bus.owners.remove(NM);
    assert_eq!(r.reread(&mut bus).unwrap(), Some(WifiState::default()), "no owner: gone");
    let mut bus = wifi_bus();
    let mut r = WifiReader::default();
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    bus.props_mut(WIFI_DEVICE, DEVICE).0.insert("DeviceType".into(), Prop::Int(1));
    assert_eq!(r.reread(&mut bus).unwrap().map(|s| s.device), Some(None), "no Wi-Fi device any more: none");
}

/// The flag that keeps the `D-Bus Wi-Fi:` line to one per open: raised by a failure, kept for
/// the rest of the open, lowered on close (the log call itself is not captured here).
#[test]
fn the_failure_flag_is_raised_once_and_lowered_on_close() {
    let mut bus = wifi_bus();
    bus.failing.insert(WIFI_DEVICE.into());
    let mut r = WifiReader::default();
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    assert!(r.logged, "the first failure logged");
    bus.failing.clear();
    r.reread(&mut bus).unwrap();
    assert!(r.logged, "still this open: no second line");
    r.rescope(&mut bus, WifiScope::Closed).unwrap();
    assert!(!r.logged, "closed: the next open may log");
    bus.failing.insert(WIFI_DEVICE.into());
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    assert!(r.logged);
}

fn detail(bus: &mut FakeBus) -> (WifiReader, WifiDevice) {
    let mut r = WifiReader::default();
    let s = r.rescope(bus, WifiScope::Detail).unwrap().expect("open: a state");
    (r, s.device.expect("the Wi-Fi device"))
}

fn ssids(d: &WifiDevice) -> Vec<String> {
    d.access_points.iter().flatten().map(|a| String::from_utf8_lossy(&a.ssid).into_owned()).collect()
}

#[test]
fn the_detail_adds_its_rules_and_reads_both_lists() {
    let mut bus = wifi_bus();
    let (_, d) = detail(&mut bus);
    assert_eq!(ssids(&d), ["MON_WIFI_1", "Voisin-Test", "Cafe-Libre"], "the hidden one left out");
    assert_eq!(d.saved, Some(vec![SavedWifi { uuid: RECORDED_UUID.into(), ssid: b"MON_WIFI_1".to_vec() }]), "the wired profile left out");
    let added: Vec<&String> = bus.calls.iter().filter(|c| c.starts_with("AddMatch ")).collect();
    assert_eq!(added.len(), WIFI_RULES.len(), "{added:?}");
    for n in 11..=13 {
        assert_eq!(bus.calls.iter().filter(|c| c.contains(&format!("{AP}/{n} "))).count(), 1, "AccessPoint/{n} read once");
    }
    assert_eq!(bus.calls.iter().filter(|c| c.contains(&format!("{AP}/10 "))).count(), 2, "the device's own: as the row's, and in the list");
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetSettings ")).count(), 2);
    assert!(bus.calls.iter().all(|c| !c.contains("Secret")), "no secret asked: {:?}", bus.calls);
}

#[test]
fn leaving_the_detail_removes_its_rules_and_drops_the_lists() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    let row = r.rescope(&mut bus, WifiScope::Row).unwrap().unwrap().device.unwrap();
    assert_eq!((row.access_points, row.saved), (None, None));
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("RemoveMatch ")).count(), WIFI_RULES.len());
    r.rescope(&mut bus, WifiScope::Detail).unwrap();
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("AddMatch ")).count(), 2 * WIFI_RULES.len(), "added again");
    assert_eq!(bus.calls.iter().filter(|c| c.contains(&format!("{AP}/11 "))).count(), 2, "read again: nothing kept between");
    r.rescope(&mut bus, WifiScope::Closed).unwrap();
    assert_eq!(
        bus.calls.iter().filter(|c| c.starts_with("RemoveMatch ")).count(),
        2 * WIFI_RULES.len(),
        "closing from the detail removes them too"
    );
    let calls = bus.calls.len();
    r.rescope(&mut bus, WifiScope::Detail).unwrap();
    let again = bus.calls.len();
    r.rescope(&mut bus, WifiScope::Detail).unwrap();
    assert_eq!(
        bus.calls[calls..].iter().filter(|c| c.starts_with("AddMatch ")).count(),
        WIFI_RULES.len(),
        "a poke for the same scope adds nothing"
    );
    let kept = |c: &&String| c.starts_with("ListConnections ") || c.starts_with("GetSettings ") || c.contains(&format!("{AP}/11 "));
    assert_eq!(bus.calls[again..].iter().filter(kept).count(), 0, "nor reads the lists again: {:?}", &bus.calls[again..]);
}

#[test]
fn one_access_points_change_reads_that_access_point_only() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.props_mut(&format!("{AP}/11"), ACCESS_POINT).0.insert("Strength".into(), Prop::Int(90));
    let calls = bus.calls.len();
    let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/11"))).unwrap().unwrap().device.unwrap();
    assert_eq!(bus.calls[calls..], [format!("GetAll {NM_OWNER} {AP}/11 {ACCESS_POINT}")]);
    assert_eq!(d.access_points.unwrap().iter().find(|a| a.ssid == b"Voisin-Test").map(|a| a.strength), Some(90));
    let calls = bus.calls.len();
    r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/99"))).unwrap();
    assert_eq!(bus.calls.len(), calls, "an access point the device does not list: nothing read");
}

/// Outside the detail its signals read nothing (their rules are off; one still in flight when
/// the detail closed is ignored).
#[test]
fn detail_changes_outside_the_detail_read_nothing() {
    let mut bus = wifi_bus();
    let mut r = WifiReader::default();
    for scope in [WifiScope::Closed, WifiScope::Row] {
        r.rescope(&mut bus, scope).unwrap();
        let calls = bus.calls.len();
        for c in [WifiChange::AccessPoint(format!("{AP}/11")), WifiChange::AccessPoints, WifiChange::Saved] {
            r.changed(&mut bus, &c).unwrap();
        }
        assert_eq!(bus.calls[calls..], [] as [String; 0], "{scope:?}: nothing read");
    }
}

#[test]
fn added_and_removed_access_points_are_reconciled() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.props.insert(
        (format!("{AP}/14"), ACCESS_POINT.into()),
        Props([("Ssid".to_string(), Prop::Bytes(b"Labo-Test".to_vec())), ("Strength".to_string(), Prop::Int(40))].into()),
    );
    let listed: Vec<String> = [10, 11, 14].iter().map(|n| format!("{AP}/{n}")).collect();
    bus.props_mut(WIFI_DEVICE, WIRELESS).0.insert("AccessPoints".into(), Prop::Paths(listed));
    let calls = bus.calls.len();
    let d = r.changed(&mut bus, &WifiChange::AccessPoints).unwrap().unwrap().device.unwrap();
    assert_eq!(ssids(&d), ["MON_WIFI_1", "Voisin-Test", "Labo-Test"], "12 gone, 14 come");
    let aps: Vec<&String> = bus.calls[calls..].iter().filter(|c| c.ends_with(ACCESS_POINT)).collect();
    assert_eq!(aps.len(), 2, "the new one, and the device's own access point (the row's): {aps:?}");
}

#[test]
fn saved_profiles_are_read_on_their_signal_only() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    r.reread(&mut bus).unwrap();
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("ListConnections ")).count(), 1, "a network re-read does not list them again");
    bus.connections.as_mut().unwrap().push("/org/freedesktop/NetworkManager/Settings/5".into());
    bus.settings.insert(
        "/org/freedesktop/NetworkManager/Settings/5".into(),
        Some(SavedWifi { uuid: "11111111-2222-4333-8444-555555555555".into(), ssid: b"Voisin-Test".to_vec() }),
    );
    let calls = bus.calls.len();
    let d = r.changed(&mut bus, &WifiChange::Saved).unwrap().unwrap().device.unwrap();
    assert_eq!(d.saved.map(|s| s.len()), Some(2));
    let settings: Vec<&String> = bus.calls[calls..].iter().filter(|c| c.starts_with("GetSettings ")).collect();
    assert_eq!(settings.len(), 1, "the new one only: {settings:?}");
}

#[test]
fn the_lists_are_capped() {
    let mut bus = wifi_bus();
    let mut listed = Vec::new();
    for n in 100..170 {
        let path = format!("{AP}/{n}");
        let ssid = format!("Reseau-Test-{n}");
        bus.props.insert((path.clone(), ACCESS_POINT.into()), Props([("Ssid".to_string(), Prop::Bytes(ssid.into_bytes()))].into()));
        listed.push(path);
    }
    bus.props_mut(WIFI_DEVICE, WIRELESS).0.insert("AccessPoints".into(), Prop::Paths(listed));
    let connections: Vec<String> = (100..170).map(|n| format!("/org/freedesktop/NetworkManager/Settings/{n}")).collect();
    for c in &connections {
        bus.settings.insert(c.clone(), None);
    }
    bus.connections = Some(connections);
    let (_, d) = detail(&mut bus);
    assert_eq!(d.access_points.map(|a| a.len()), Some(MAX_APS));
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("GetSettings ")).count(), MAX_SAVED);
    assert_eq!(d.saved, None, "more than MAX_SAVED: not the whole list, so unknown");
}

/// Ruling P4: one access point or one profile that fails to read (a timeout) is left out; the
/// rest of the detail stays, and the failure is logged once per open.
#[test]
fn one_unreadable_item_is_left_out_never_the_whole_detail() {
    let mut bus = wifi_bus();
    bus.failing.insert(format!("{AP}/11"));
    let voisin = "/org/freedesktop/NetworkManager/Settings/5";
    bus.connections.as_mut().unwrap().push(voisin.into());
    bus.settings
        .insert(voisin.into(), Some(SavedWifi { uuid: "11111111-2222-4333-8444-555555555555".into(), ssid: b"Voisin-Test".to_vec() }));
    bus.failing.insert("/org/freedesktop/NetworkManager/Settings/3".into());
    let (mut r, d) = detail(&mut bus);
    assert_eq!(ssids(&d), ["MON_WIFI_1", "Cafe-Libre"], "Voisin-Test left out, the others kept");
    assert_eq!(d.saved, None, "a profile unread: the list unknown (fix round 2), so nothing is ever deleted");
    assert!(d.active.is_some(), "the row's part kept");
    assert!(r.logged, "the failure noted");
    bus.failing.clear();
    let d = r.reread(&mut bus).unwrap().unwrap().device.unwrap();
    let saved: Vec<Vec<u8>> = d.saved.expect("read whole at last").into_iter().map(|s| s.ssid).collect();
    assert!(saved.contains(&b"Voisin-Test".to_vec()) && saved.len() == 2, "the unread one read again: {saved:?}");
    let mut bus = wifi_bus();
    bus.disconnecting.insert(format!("{AP}/11"));
    let e = WifiReader::default().rescope(&mut bus, WifiScope::Detail).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "the bus itself gone: still the session's");
}

/// Final review A-M1: NetworkManager gone while the saved profiles are listed or read
/// (ServiceUnknown), or ListConnections not there: the list is unknown, never empty, so a
/// profile saved before is never taken for an attempt's. One profile gone (UnknownObject) is
/// skipped, the list still whole.
#[test]
fn networkmanager_gone_while_listing_leaves_the_saved_list_unknown() {
    let mut bus = wifi_bus();
    bus.gone.insert(SETTINGS_PATH.into());
    assert_eq!(detail(&mut bus).1.saved, None, "ListConnections: ServiceUnknown");
    let mut bus = wifi_bus();
    bus.connections = None;
    assert_eq!(detail(&mut bus).1.saved, None, "ListConnections: UnknownMethod");
    let mut bus = wifi_bus();
    bus.gone.insert("/org/freedesktop/NetworkManager/Settings/3".into());
    let (mut r, d) = detail(&mut bus);
    assert_eq!(d.saved, None, "GetSettings: ServiceUnknown");
    bus.gone.clear();
    assert_eq!(r.reread(&mut bus).unwrap().unwrap().device.unwrap().saved.map(|s| s.len()), Some(1), "listed again");
    let mut bus = wifi_bus();
    bus.settings.remove("/org/freedesktop/NetworkManager/Settings/4");
    let saved = detail(&mut bus).1.saved.expect("whole");
    assert_eq!(saved.len(), 1, "the wired one gone (UnknownObject): skipped, the list whole");
}

/// Final review A-M2: a profile NetworkManager will not show this user (PermissionDenied) is
/// not ours to delete: skipped, the list still whole, nothing logged.
#[test]
fn a_profile_refused_to_this_user_is_skipped() {
    let mut bus = wifi_bus();
    bus.denied.insert("/org/freedesktop/NetworkManager/Settings/4".into());
    let (r, d) = detail(&mut bus);
    assert_eq!(d.saved.map(|s| s.len()), Some(1), "whole, the refused one left out");
    assert!(!r.logged, "not a failure");
}

/// Per rule, how many times the bus holds it for this session.
fn rules_left(bus: &FakeBus) -> Vec<usize> {
    WIFI_RULES.iter().map(|r| bus.rules.iter().filter(|h| h == r).count()).collect()
}

/// Fix round 1: one rule refused (a timeout, LimitsExceeded) takes back the ones already added:
/// none left on the bus to wake the thread once the panel closes, none doubled at the next open.
#[test]
fn a_partly_added_set_of_rules_is_taken_back() {
    let mut bus = wifi_bus();
    bus.failing_rules.insert((true, WIFI_RULES[3].to_string()));
    let (mut r, d) = detail(&mut bus);
    assert_eq!(rules_left(&bus), [0; 5], "{:?}", bus.calls);
    assert!(d.access_points.is_some_and(|a| !a.is_empty()), "the detail still read once");
    assert!(r.logged, "the refusal noted");
    r.rescope(&mut bus, WifiScope::Row).unwrap();
    assert_eq!(rules_left(&bus), [0; 5], "nothing removed twice");
    bus.failing_rules.clear();
    r.rescope(&mut bus, WifiScope::Detail).unwrap();
    assert_eq!(rules_left(&bus), [1; 5], "added whole at the next open");
    r.rescope(&mut bus, WifiScope::Closed).unwrap();
    assert_eq!(rules_left(&bus), [0; 5]);
}

/// Fix round 1: one RemoveMatch refused does not keep the others on the bus; final review A-M4:
/// the refused one is never tried again here, so the error ends the session (a reconnection
/// drops every rule).
#[test]
fn every_rule_is_removed_even_after_one_fails() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.failing_rules.insert((false, WIFI_RULES[0].to_string()));
    let e = r.rescope(&mut bus, WifiScope::Row).unwrap_err();
    assert_ne!(e.kind(), io::ErrorKind::NotFound, "{e}");
    assert_eq!(bus.calls.iter().filter(|c| c.starts_with("RemoveMatch ")).count(), WIFI_RULES.len(), "every rule tried");
    assert_eq!(rules_left(&bus), [1, 0, 0, 0, 0], "only the refused one left");
}

/// Final review A-M4: an AddMatch whose reply was lost may still have been applied, so the
/// rollback removes it too; a rule the bus never took answers MatchRuleNotFound, ignored. A
/// rule already gone when the detail closes (MatchRuleNotFound) is not a failure either.
#[test]
fn a_timed_out_rule_is_rolled_back_too() {
    let mut bus = wifi_bus();
    bus.late_rules.insert(WIFI_RULES[2].to_string());
    let (mut r, _) = detail(&mut bus);
    assert_eq!(rules_left(&bus), [0; 5], "{:?}", bus.calls);
    r.rescope(&mut bus, WifiScope::Closed).unwrap();
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.rules.retain(|h| h != WIFI_RULES[1]);
    assert!(r.rescope(&mut bus, WifiScope::Row).is_ok(), "already gone: what removing asks");
    assert_eq!(rules_left(&bus), [0; 5]);
}

/// Fix round 1 (P4): an access point whose re-read fails is left out, like at the first read.
#[test]
fn an_access_points_failed_reread_leaves_it_out() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.failing.insert(format!("{AP}/11"));
    let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/11"))).unwrap().unwrap().device.unwrap();
    assert_eq!(ssids(&d), ["MON_WIFI_1", "Cafe-Libre"], "Voisin-Test left out, never its old strength");
    assert!(d.active.is_some(), "the rest kept");
}

/// Fix round 1: the device's own access point changing reaches the row too (its strength, its
/// glyph), not only the list.
#[test]
fn the_active_access_points_change_reaches_the_row() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.props_mut(&format!("{AP}/10"), ACCESS_POINT).0.insert("Strength".into(), Prop::Int(20));
    let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/10"))).unwrap().unwrap().device.unwrap();
    assert_eq!(d.active.map(|a| a.strength), Some(20));
    assert_eq!(d.access_points.unwrap()[0].strength, 20);
    bus.props_mut(&format!("{AP}/11"), ACCESS_POINT).0.insert("Strength".into(), Prop::Int(99));
    let d = r.changed(&mut bus, &WifiChange::AccessPoint(format!("{AP}/11"))).unwrap().unwrap().device.unwrap();
    assert_eq!(d.active.map(|a| a.strength), Some(20), "another access point: the row's stays");
}

/// Fix round 1: ListConnections failing (not absent) keeps the row and the access points, the
/// saved list empty until a later read lists it.
#[test]
fn a_failed_listing_keeps_the_row_and_is_tried_again() {
    let mut bus = wifi_bus();
    bus.failing.insert(SETTINGS_PATH.into());
    let (mut r, d) = detail(&mut bus);
    assert!(d.active.is_some() && d.access_points.is_some_and(|a| a.len() == 3), "the row and the list kept");
    assert_eq!(d.saved, None, "not listed: unknown, never empty");
    assert!(r.logged);
    bus.failing.clear();
    let d = r.reread(&mut bus).unwrap().unwrap().device.unwrap();
    assert_eq!(d.saved.map(|s| s.len()), Some(1), "listed at the next read");
}

/// Review Focus 1: NetworkManager restarted with the detail open: another unique name, the same
/// paths now other objects. Every cached path is dropped and read again from the new owner.
#[test]
fn a_new_networkmanager_owner_drops_every_cached_path() {
    let mut bus = wifi_bus();
    let (mut r, _) = detail(&mut bus);
    bus.owners.insert(NM, ":1.99");
    bus.props_mut(&format!("{AP}/11"), ACCESS_POINT).0.insert("Ssid".into(), Prop::Bytes(b"Autre-Test".to_vec()));
    let calls = bus.calls.len();
    let d = r.reread(&mut bus).unwrap().unwrap().device.unwrap();
    assert!(ssids(&d).contains(&"Autre-Test".to_string()), "{:?}", ssids(&d));
    let new: Vec<&String> = bus.calls[calls..].iter().collect();
    assert!(new.iter().any(|c| c.starts_with("GetDevices :1.99")), "the device found again: {new:?}");
    assert!(new.iter().any(|c| *c == &format!("GetAll :1.99 {AP}/12 {ACCESS_POINT}")), "every access point read again");
    assert!(new.iter().any(|c| c.starts_with("ListConnections :1.99")), "the saved profiles listed again");
}

#[test]
fn a_saved_profile_keeps_its_uuid_and_ssid_only() {
    let p = |pairs: &[(&str, Prop)]| Props(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    let connection =
        p(&[("type", Prop::Str("802-11-wireless".into())), ("uuid", Prop::Str("11111111-2222-4333-8444-555555555555".into()))]);
    let wireless = p(&[("ssid", Prop::Bytes(b"Maison-Test".to_vec()))]);
    assert_eq!(
        saved_wifi(Some(&connection), Some(&wireless)),
        Some(SavedWifi { uuid: "11111111-2222-4333-8444-555555555555".into(), ssid: b"Maison-Test".to_vec() })
    );
    let ethernet = p(&[("type", Prop::Str("802-3-ethernet".into())), ("uuid", Prop::Str("aaaa".into()))]);
    assert_eq!(saved_wifi(Some(&ethernet), None), None);
    let odd = p(&[("type", Prop::Str("802-11-wireless".into())), ("uuid", Prop::Str("-rf".into()))]);
    assert_eq!(saved_wifi(Some(&odd), Some(&wireless)), None, "a uuid nmcli would read as an option");
    let long = p(&[("ssid", Prop::Bytes(vec![b'x'; 33]))]);
    assert_eq!(saved_wifi(Some(&connection), Some(&long)), None);
    assert_eq!(saved_wifi(Some(&connection), None), None, "no 802-11-wireless setting");
}
