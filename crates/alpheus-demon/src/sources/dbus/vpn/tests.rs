use super::*;
use crate::system::VpnProfile;

use crate::sources::dbus::fake::{FakeBus, NM_OWNER};
use crate::sources::dbus::props::{Prop, Props};

const SETTINGS: &str = "/org/freedesktop/NetworkManager/Settings";
const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection";
const ROOT: &str = "/org/freedesktop/NetworkManager";

/// A fictional OpenVPN profile (uuid ...20) and a WireGuard one (the workstation's tunnel ...2),
/// saved; the OpenVPN one active too (a plugin VPN: `Vpn` true).
pub(in crate::sources::dbus) fn vpn_bus() -> FakeBus {
    let mut bus = FakeBus::workstation();
    let (plugin, wg) = (format!("{SETTINGS}/20"), format!("{SETTINGS}/21"));
    bus.connections = Some(vec![plugin.clone(), wg.clone()]);
    let profile = |uuid: &str, name: &str, kind| {
        Some(Saved::Vpn(VpnProfile { uuid: uuid.into(), name: name.into(), kind, last_used: 0, asks: false }))
    };
    bus.settings.insert(plugin, profile("00000000-0000-0000-0000-000000000020", "Travail-Test", VpnKind::Plugin));
    bus.settings.insert(wg, profile("00000000-0000-0000-0000-000000000002", "Maison-Test", VpnKind::WireGuard));
    let active = |uuid: &str, kind: &str, vpn: bool| {
        Props(
            [
                ("Uuid".to_string(), Prop::Str(uuid.into())),
                ("Type".to_string(), Prop::Str(kind.into())),
                ("Vpn".to_string(), Prop::Bool(vpn)),
                ("State".to_string(), Prop::Int(2)),
            ]
            .into(),
        )
    };
    bus.props.insert((format!("{ACTIVE}/20"), network::ACTIVE.into()), active("00000000-0000-0000-0000-000000000020", "vpn", true));
    bus.set_actives(&[10, 9, 3, 2, 20]);
    bus
}

impl FakeBus {
    /// The root's ActiveConnections, by number.
    pub(in crate::sources::dbus) fn set_actives(&mut self, ns: &[u32]) {
        let paths = ns.iter().map(|n| format!("{ACTIVE}/{n}")).collect();
        self.props_mut(ROOT, network::INTERFACE).0.insert("ActiveConnections".into(), Prop::Paths(paths));
    }
}

fn count(bus: &FakeBus, prefix: &str) -> usize {
    bus.calls.iter().filter(|c| c.starts_with(prefix)).count()
}

fn open(bus: &mut FakeBus, scope: VpnScope) -> (VpnReader, VpnState) {
    let mut r = VpnReader::default();
    let s = r.rescope(bus, scope).unwrap().expect("open: a state");
    (r, s)
}

#[test]
fn the_row_reads_the_profiles_and_the_active_vpns() {
    let mut bus = vpn_bus();
    let (_, s) = open(&mut bus, VpnScope::Row);
    let names: Vec<&str> = s.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Travail-Test", "Maison-Test"], "path order");
    let uuids: Vec<&str> = s.active.iter().map(|a| a.uuid.as_str()).collect();
    assert_eq!(
        uuids,
        ["00000000-0000-0000-0000-000000000003", "00000000-0000-0000-0000-000000000002", "00000000-0000-0000-0000-000000000020"]
    );
    assert!(s.active.iter().all(|a| a.state == 2));
    for c in &bus.calls {
        assert!(c.starts_with("GetNameOwner ") || c.split(' ').nth(1) == Some(NM_OWNER), "{c}: only NetworkManager's unique name");
    }
}

#[test]
fn closed_reads_nothing() {
    let mut bus = vpn_bus();
    let mut r = VpnReader::default();
    assert_eq!(r.rescope(&mut bus, VpnScope::Closed).unwrap(), None);
    assert_eq!(r.reread(&mut bus).unwrap(), None);
    assert!(bus.calls.is_empty(), "{:?}", bus.calls);
    let _ = r.rescope(&mut bus, VpnScope::Row).unwrap();
    let calls = bus.calls.len();
    assert_eq!(r.rescope(&mut bus, VpnScope::Closed).unwrap(), None);
    assert_eq!(r.scope(), VpnScope::Closed);
    assert_eq!(bus.calls.len(), calls);
}

#[test]
fn row_lists_once_per_open() {
    let mut bus = vpn_bus();
    let (mut r, _) = open(&mut bus, VpnScope::Row);
    r.reread(&mut bus).unwrap();
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "ListConnections"), 1, "{:?}", bus.calls);
    r.rescope(&mut bus, VpnScope::Closed).unwrap();
    r.rescope(&mut bus, VpnScope::Row).unwrap();
    assert_eq!(count(&bus, "ListConnections"), 2, "a new open lists again");
}

#[test]
fn detail_lists_on_every_read() {
    let mut bus = vpn_bus();
    let (mut r, _) = open(&mut bus, VpnScope::Detail);
    r.reread(&mut bus).unwrap();
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "ListConnections"), 3, "{:?}", bus.calls);
    // Row to Detail is an entry too: the list is read again.
    let mut bus = vpn_bus();
    let (mut r, _) = open(&mut bus, VpnScope::Row);
    r.rescope(&mut bus, VpnScope::Detail).unwrap();
    assert_eq!(count(&bus, "ListConnections"), 2);
}

#[test]
fn only_new_paths_are_read_again() {
    let mut bus = vpn_bus();
    let (mut r, _) = open(&mut bus, VpnScope::Detail);
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "GetSettings"), 2, "one per path across rereads");
    let added = format!("{SETTINGS}/22");
    bus.connections.as_mut().unwrap().push(added.clone());
    bus.settings.insert(added, None);
    let s = r.reread(&mut bus).unwrap().unwrap();
    assert_eq!(count(&bus, "GetSettings"), 3, "the new path only");
    assert_eq!(s.profiles.len(), 2, "not a VPN: dropped");
    // A path that left the list is forgotten, and read again if it comes back.
    let gone = bus.connections.as_mut().unwrap().remove(0);
    r.reread(&mut bus).unwrap();
    bus.connections.as_mut().unwrap().push(gone);
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "GetSettings"), 4);
}

#[test]
fn refused_or_gone_profiles_are_cached_as_none() {
    let mut bus = vpn_bus();
    bus.denied.insert(format!("{SETTINGS}/20"));
    let (mut r, s) = open(&mut bus, VpnScope::Detail);
    assert_eq!(s.profiles.len(), 1);
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "GetSettings"), 2, "the refusal is not asked again");
}

#[test]
fn a_failed_profile_is_not_cached_and_the_list_stays_stale() {
    let mut bus = vpn_bus();
    bus.failing.insert(format!("{SETTINGS}/21"));
    let (mut r, s) = open(&mut bus, VpnScope::Row);
    assert_eq!(s, VpnState::default(), "the read ends on the failure: no state yet");
    bus.failing.clear();
    let s = r.reread(&mut bus).unwrap().unwrap();
    assert_eq!(count(&bus, "ListConnections"), 2, "stale: Row lists again");
    assert_eq!(count(&bus, "GetSettings"), 3, "the first profile kept, the failed one asked again");
    assert_eq!(s.profiles.len(), 2);
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "ListConnections"), 2, "then whole: not again");
}

#[test]
fn a_hung_settings_call_ends_the_listing_after_one_timeout() {
    let mut bus = vpn_bus();
    let hung = format!("{SETTINGS}/20");
    bus.failing.insert(hung.clone());
    let paths: Vec<String> = (100..164).map(|n| format!("{SETTINGS}/{n}")).collect();
    for p in &paths {
        bus.settings.insert(p.clone(), None);
        bus.failing.insert(p.clone());
    }
    bus.connections = Some(std::iter::once(hung).chain(paths).collect());
    let (mut r, s) = open(&mut bus, VpnScope::Row);
    assert!(s.profiles.is_empty());
    assert_eq!(count(&bus, "GetSettings"), 1, "one timeout per read, not one per path: {:?}", bus.calls);
    assert_eq!(count(&bus, "GetAll"), 0, "nor a second deadline on the actives: {:?}", bus.calls);
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "GetSettings"), 2, "stale: listed again on the next read, one call");
}

#[test]
fn a_listing_that_times_out_keeps_the_list_stale_and_logs_once() {
    let mut bus = vpn_bus();
    bus.failing.insert(crate::sources::dbus::wifi::SETTINGS_PATH.into());
    let (mut r, s) = open(&mut bus, VpnScope::Row);
    assert!(s.profiles.is_empty());
    assert_eq!(count(&bus, "GetSettings"), 0);
    assert!(r.logged, "the failure is noted");
    // One deadline per hung read: the active connections are not read after it (no state yet).
    assert_eq!(s, VpnState::default());
    assert_eq!(count(&bus, "GetAll"), 0, "{:?}", bus.calls);
    bus.failing.clear();
    let s = r.reread(&mut bus).unwrap().unwrap();
    assert_eq!(s.profiles.len(), 2, "stale: listed again once it works");
    // Closing resets the flag: the next open logs again.
    r.rescope(&mut bus, VpnScope::Closed).unwrap();
    assert!(!r.logged);
}

#[test]
fn a_hung_listing_after_a_state_returns_the_last_state() {
    let mut bus = vpn_bus();
    let (mut r, first) = open(&mut bus, VpnScope::Detail);
    let before = count(&bus, "GetAll");
    bus.failing.insert(crate::sources::dbus::wifi::SETTINGS_PATH.into());
    assert_eq!(r.reread(&mut bus).unwrap().unwrap(), first, "the last state, not one with fresh actives");
    assert_eq!(count(&bus, "GetAll"), before, "the actives are not read after a hung listing: {:?}", bus.calls);
    assert!(r.stale, "the list stays stale");
}

#[test]
fn an_active_connection_that_fails_keeps_the_last_state() {
    let mut bus = vpn_bus();
    let (mut r, first) = open(&mut bus, VpnScope::Row);
    assert_eq!(first.active.len(), 3);
    bus.failing.insert(format!("{ACTIVE}/20"));
    assert_eq!(r.reread(&mut bus).unwrap().unwrap(), first);
}

#[test]
fn a_service_gone_midway_stops_listing_and_stays_stale() {
    let mut bus = vpn_bus();
    bus.gone.insert(format!("{SETTINGS}/20"));
    let (mut r, s) = open(&mut bus, VpnScope::Row);
    assert!(s.profiles.is_empty());
    assert_eq!(count(&bus, "GetSettings"), 1, "listing stopped at the first");
    bus.gone.clear();
    let s = r.reread(&mut bus).unwrap().unwrap();
    assert_eq!(s.profiles.len(), 2);
}

#[test]
fn wireguard_counts_as_a_vpn() {
    let mut bus = vpn_bus();
    let (_, s) = open(&mut bus, VpnScope::Row);
    assert!(s.active.iter().any(|a| a.uuid.ends_with("2")), "wireguard with Vpn false");
    assert!(!s.active.iter().any(|a| a.uuid.ends_with("9") || a.uuid.ends_with("10")), "Wi-Fi and Ethernet: not VPNs");
    assert_eq!(s.active.len(), 3);
    // A bad uuid is dropped.
    bus.props_mut(&format!("{ACTIVE}/2"), network::ACTIVE).0.insert("Uuid".into(), Prop::Str("-x".into()));
    let (_, s) = open(&mut bus, VpnScope::Row);
    assert_eq!(s.active.len(), 2);
}

#[test]
fn networkmanager_absent_is_an_empty_state() {
    let mut bus = FakeBus::empty();
    let (_, s) = open(&mut bus, VpnScope::Row);
    assert_eq!(s, VpnState::default());
    assert_eq!(bus.calls, ["GetNameOwner org.freedesktop.NetworkManager"]);
}

#[test]
fn a_new_owner_reads_everything_again() {
    let mut bus = vpn_bus();
    let (mut r, _) = open(&mut bus, VpnScope::Row);
    assert_eq!((count(&bus, "ListConnections"), count(&bus, "GetSettings")), (1, 2));
    bus.owners.insert(NM, ":1.99");
    let s = r.reread(&mut bus).unwrap().unwrap();
    assert_eq!((count(&bus, "ListConnections"), count(&bus, "GetSettings")), (2, 4));
    assert_eq!(s.profiles.len(), 2);
    // NetworkManager gone: forgotten too, so its return lists again.
    bus.owners.remove(NM);
    assert_eq!(r.reread(&mut bus).unwrap().unwrap(), VpnState::default());
    bus.owners.insert(NM, ":1.99");
    r.reread(&mut bus).unwrap();
    assert_eq!(count(&bus, "ListConnections"), 3, "the same name back: lists again");
}

#[test]
fn a_timeout_keeps_the_last_state() {
    let mut bus = vpn_bus();
    let (mut r, first) = open(&mut bus, VpnScope::Row);
    bus.failing.insert(ROOT.into());
    assert_eq!(r.reread(&mut bus).unwrap().unwrap(), first);
    let mut bus = vpn_bus();
    bus.failing.insert(ROOT.into());
    let (_, s) = open(&mut bus, VpnScope::Row);
    assert_eq!(s, VpnState::default(), "before any state: empty");
}

#[test]
fn the_bus_gone_is_an_error() {
    let mut bus = vpn_bus();
    bus.disconnecting.insert(ROOT.into());
    let e = VpnReader::default().rescope(&mut bus, VpnScope::Row).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn at_most_64_connections_and_16_actives_read() {
    let mut bus = vpn_bus();
    let paths: Vec<String> = (100..200).map(|n| format!("{SETTINGS}/{n}")).collect();
    for p in &paths {
        bus.settings.insert(p.clone(), None);
    }
    bus.connections = Some(paths);
    let actives: Vec<u32> = (100..130).collect();
    for n in &actives {
        let p = Props(
            [("Uuid".to_string(), Prop::Str(format!("00000000-0000-0000-0000-{n:012}"))), ("Type".to_string(), Prop::Str("vpn".into()))]
                .into(),
        );
        bus.props.insert((format!("{ACTIVE}/{n}"), network::ACTIVE.into()), p);
    }
    bus.set_actives(&actives);
    let (_, s) = open(&mut bus, VpnScope::Row);
    assert_eq!(count(&bus, "GetSettings"), 64);
    assert_eq!(bus.calls.iter().filter(|c| c.contains(&format!("{ACTIVE}/"))).count(), 16);
    assert_eq!(s.active.len(), 16);
}

use crate::sources::dbus::props::busctl as fixture;

fn connection(json: &str) -> Props {
    fixture::get_all(&format!(r#"{{"type":"a{{sv}}","data":[{json}]}}"#))
}

#[test]
fn a_hostile_name_is_cleaned_cut_and_never_empty() {
    let with = |id: &str| {
        let id = serde_json::to_string(id).unwrap();
        let json = format!(
            r#"{{"id":{{"type":"s","data":{id}}},"uuid":{{"type":"s","data":"11111111-2222-4333-8444-555555555555"}},"type":{{"type":"s","data":"vpn"}}}}"#
        );
        saved_vpn(Some(&connection(&json)), false).unwrap().name
    };
    assert_eq!(with("\u{202e}a\u{7}b"), "ab");
    let long = with(&"é".repeat(300));
    assert!(long.len() <= NAME_MAX && !long.is_empty());
    assert_eq!(with("\u{7}"), "?");
    assert_eq!(with(""), "?", "an empty id");
    let bad = connection(r#"{"uuid":{"type":"s","data":"-x"},"type":{"type":"s","data":"vpn"}}"#);
    assert_eq!(saved_vpn(Some(&bad), false), None);
}

#[test]
fn last_use_is_zero_when_absent_or_negative() {
    let with = |ts: &str| {
        let json =
            format!(r#"{{"uuid":{{"type":"s","data":"11111111-2222-4333-8444-555555555555"}},"type":{{"type":"s","data":"vpn"}}{ts}}}"#);
        saved_vpn(Some(&connection(&json)), false).unwrap().last_used
    };
    assert_eq!(with(""), 0, "absent");
    assert_eq!(with(r#","timestamp":{"type":"x","data":-5}"#), 0, "negative");
    assert_eq!(with(r#","timestamp":{"type":"t","data":1700000000}"#), 1700000000);
}

#[test]
fn secret_flags() {
    assert!(secret_asked(1) && secret_asked(2) && secret_asked(3));
    assert!(!secret_asked(0) && !secret_asked(4) && !secret_asked(5));
}
