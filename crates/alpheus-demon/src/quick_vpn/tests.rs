use super::*;
use crate::grid::text_width;
use crate::quick_wifi::{ACTIVATION_BOUND, CONNECT_TIMEOUT, NM_GONE};
use crate::system::{ActiveVpn, VpnProfile};
use std::time::Duration;

const S: Duration = Duration::from_secs(1);

fn uuid(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn profile(n: usize, name: &str, kind: VpnKind, last_used: u64, asks: bool) -> VpnProfile {
    VpnProfile { uuid: uuid(n), name: name.into(), kind, last_used, asks }
}

fn vpn(profiles: Vec<VpnProfile>, active: &[(usize, u32)]) -> Vpn {
    let active = active.iter().map(|(n, state)| ActiveVpn { uuid: uuid(*n), state: *state }).collect();
    Vpn { state: Some(VpnState { profiles, active }), ..Vpn::default() }
}

/// VPN-Test-1 and VPN-Test-2 (plugin), WG-Test (WireGuard), fictional.
fn three(active: &[(usize, u32)]) -> Vpn {
    vpn(
        vec![
            profile(1, "VPN-Test-1", VpnKind::Plugin, 100, false),
            profile(2, "VPN-Test-2", VpnKind::Plugin, 300, false),
            profile(3, "WG-Test", VpnKind::WireGuard, 200, false),
        ],
        active,
    )
}

fn run(parts: &[&str], what: &str) -> QuickAction {
    QuickAction::Run { argv: parts.iter().map(|s| s.to_string()).collect(), what: what.into() }
}

fn set(v: &mut Vpn, active: &[(usize, u32)], now: Instant) -> Option<&'static str> {
    let mut s = v.state.clone().expect("a state");
    s.active = active.iter().map(|(n, state)| ActiveVpn { uuid: uuid(*n), state: *state }).collect();
    v.set_state(Some(s), now)
}

#[test]
fn the_row_hides_without_a_profile() {
    assert!(!Vpn::default().shows(), "no state");
    assert!(!vpn(Vec::new(), &[]).shows(), "no profile");
    assert!(three(&[]).shows());
}

#[test]
fn the_value_is_the_active_name_or_off() {
    assert_eq!(three(&[(3, ACTIVATED)]).value(), "WG-Test");
    assert_eq!(three(&[(1, ACTIVATING)]).value(), "…", "activating only");
    assert_eq!(three(&[(1, DEACTIVATING)]).value(), "…", "ruling 6: a deactivating one reads as under way");
    assert_eq!(three(&[]).value(), "off");
    assert_eq!(Vpn::default().value(), "off");
    assert_eq!(three(&[(1, ACTIVATING), (3, ACTIVATED)]).value(), "WG-Test", "an activated one wins");
    let long = "x".repeat(40);
    let v = vpn(vec![profile(1, &long, VpnKind::Plugin, 0, false)], &[(1, ACTIVATED)]);
    assert_eq!(text_width(&v.value()), VALUE_CELLS, "a 40-cell name: 16 cells");
}

#[test]
fn active_first_then_by_name_at_most_16() {
    let profiles: Vec<VpnProfile> = (0..20).map(|n| profile(n, &format!("VPN-Test-{n:02}"), VpnKind::Plugin, 0, false)).collect();
    let v = vpn(profiles, &[(19, ACTIVATED)]);
    let l = v.listed();
    assert_eq!(l.len(), PROFILES_MAX);
    assert_eq!(
        l[0],
        Listed { uuid: uuid(19), label: "VPN-Test-19".into(), kind: VpnKind::Plugin, state: Some(ACTIVATED) },
        "the 19th by name is first"
    );
    assert_eq!(l[1].label, "VPN-Test-00");
    assert_eq!(l[15].label, "VPN-Test-14", "cut after sorting");
    assert!(l[1..].iter().all(|p| p.state.is_none()));
    // Deactivating and activating count as active; case is ignored, then the uuid breaks ties.
    let v = vpn(
        vec![
            profile(1, "beta", VpnKind::Plugin, 0, false),
            profile(2, "Alpha", VpnKind::Plugin, 0, false),
            profile(4, "alpha", VpnKind::Plugin, 0, false),
            profile(3, "zed", VpnKind::WireGuard, 0, false),
        ],
        &[(3, DEACTIVATING)],
    );
    let order: Vec<_> = v.listed().iter().map(|p| p.uuid.clone()).collect();
    assert_eq!(order, [uuid(3), uuid(2), uuid(4), uuid(1)]);
}

#[test]
fn enter_ups_an_inactive_and_downs_an_active() {
    let t = Instant::now();
    let mut v = three(&[(2, ACTIVATED)]);
    assert_eq!(v.enter(&uuid(1), t), run(&["nmcli", "connection", "up", "uuid", &uuid(1)], "vpn up"));
    assert!(v.attempt.is_some());
    let mut v = three(&[(2, ACTIVATED)]);
    assert_eq!(v.enter(&uuid(2), t), run(&["nmcli", "connection", "down", "uuid", &uuid(2)], "vpn down"));
    assert!(v.attempt.is_none(), "down is not an attempt");
    let mut v = three(&[(2, ACTIVATING)]);
    assert_eq!(v.enter(&uuid(2), t), run(&["nmcli", "connection", "down", "uuid", &uuid(2)], "vpn down"), "activating: down");
    let mut v = three(&[(2, DEACTIVATING)]);
    assert_eq!(v.enter(&uuid(2), t), QuickAction::Nothing, "already going down");
    assert_eq!(v.enter("not-a-profile", t), QuickAction::Nothing);
    assert_eq!(Vpn::default().enter(&uuid(1), t), QuickAction::Nothing);
}

#[test]
fn one_attempt_at_a_time() {
    let t = Instant::now();
    let mut v = three(&[]);
    v.failed = Some((uuid(3), t));
    assert!(matches!(v.enter(&uuid(1), t), QuickAction::Run { .. }));
    assert_eq!(v.failed, None, "a new attempt clears the old mark");
    let a = v.attempt.clone().expect("an attempt");
    assert_eq!((a.uuid.as_str(), a.started, a.until, a.activating, a.asks), (uuid(1).as_str(), t, t + CONNECT_TIMEOUT, false, false));
    assert_eq!(v.enter(&uuid(1), t + S), QuickAction::Nothing, "a held Enter");
    assert_eq!(v.enter(&uuid(2), t + S), QuickAction::Nothing, "another profile");
    assert_eq!(v.enter(&uuid(1), t + S), QuickAction::Nothing);
    assert_eq!(v.attempt, Some(a), "unchanged");
    let mut asking = vpn(vec![profile(1, "VPN-Test-1", VpnKind::Plugin, 0, true)], &[]);
    asking.enter(&uuid(1), t);
    assert!(asking.attempt.expect("an attempt").asks);
}

#[test]
fn space_downs_the_active_one_else_ups_the_last_used() {
    let t = Instant::now();
    // The highest last_used (VPN-Test-2, 300) is upped.
    assert_eq!(three(&[]).toggle(t), run(&["nmcli", "connection", "up", "uuid", &uuid(2)], "vpn up"));
    // With a WireGuard active, it is downed.
    assert_eq!(three(&[(3, ACTIVATED)]).toggle(t), run(&["nmcli", "connection", "down", "uuid", &uuid(3)], "vpn down"));
    // The first active in the detail's order (active, then by name): VPN-Test-1 before WG-Test.
    assert_eq!(three(&[(3, ACTIVATED), (1, ACTIVATING)]).toggle(t), run(&["nmcli", "connection", "down", "uuid", &uuid(1)], "vpn down"));
    // Ties and none: the first in the detail's order.
    let tie = vpn(vec![profile(2, "b", VpnKind::Plugin, 0, false), profile(1, "a", VpnKind::Plugin, 0, false)], &[]);
    assert_eq!(tie.clone().toggle(t), run(&["nmcli", "connection", "up", "uuid", &uuid(1)], "vpn up"));
    // During an attempt: nothing.
    let mut v = three(&[]);
    v.toggle(t);
    assert_eq!(v.toggle(t + S), QuickAction::Nothing);
    assert_eq!(Vpn::default().toggle(t), QuickAction::Nothing, "no profile");
}

#[test]
fn the_attempt_connects_or_fails() {
    let t = Instant::now();
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    assert_eq!(set(&mut v, &[(1, ACTIVATING)], t + S), None);
    assert!(v.attempt.as_ref().expect("still going").activating);
    assert_eq!(set(&mut v, &[(1, ACTIVATED)], t + 2 * S), None, "connected: no why");
    assert_eq!((v.attempt.as_ref(), v.failed.as_ref()), (None, None));

    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    set(&mut v, &[(1, ACTIVATING)], t + S);
    assert_eq!(set(&mut v, &[], t + 2 * S), Some("the activation failed"), "activating then absent");
    assert_eq!(v.failed, Some((uuid(1), t + 2 * S + FAILED_SHOWN)));
    assert!(v.attempt.is_none());

    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    set(&mut v, &[(1, ACTIVATING)], t + S);
    assert_eq!(set(&mut v, &[(1, DEACTIVATING)], t + 2 * S), Some("the activation failed"), "state 3");

    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    assert_eq!(set(&mut v, &[(1, 4)], t + S), Some("the activation failed"), "state 4 straight away");

    // Absent before it ever activated: nothing yet.
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    assert_eq!(set(&mut v, &[], t + S), None);
    assert!(v.attempt.is_some());

    // A profile that asks for a secret says so.
    let mut v = vpn(vec![profile(1, "VPN-Test-1", VpnKind::Plugin, 0, true)], &[]);
    v.enter(&uuid(1), t);
    set(&mut v, &[(1, ACTIVATING)], t + S);
    assert_eq!(set(&mut v, &[], t + 2 * S), Some("the connection needs a secret"));
}

#[test]
fn the_profile_going_away_ends_the_attempt_without_a_mark() {
    let t = Instant::now();
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    let mut s = v.state.clone().expect("state");
    s.profiles.retain(|p| p.uuid != uuid(1));
    assert_eq!(v.set_state(Some(s), t + S), Some("the profile went away"));
    assert_eq!((v.attempt.as_ref(), v.failed.as_ref()), (None, None));
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    let mut s = v.state.clone().expect("state");
    s.profiles.clear();
    assert_eq!(v.set_state(Some(s), t + S), Some(NM_GONE), "no profile at all: NetworkManager's");
}

#[test]
fn no_word_fails_at_20_s_still_activating_waits_to_60_s() {
    let t = Instant::now();
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    assert_eq!(v.due(t + CONNECT_TIMEOUT - S), (None, false), "before the deadline");
    assert_eq!(v.deadline(), Some(t + CONNECT_TIMEOUT));
    assert_eq!(v.due(t + CONNECT_TIMEOUT), (Some("no answer in 20 s"), true));
    assert_eq!(v.failed, Some((uuid(1), t + CONNECT_TIMEOUT + FAILED_SHOWN)));
    assert!(v.attempt.is_none());

    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    set(&mut v, &[(1, ACTIVATING)], t + S);
    assert_eq!(v.due(t + CONNECT_TIMEOUT), (None, false), "still activating: it waits");
    assert_eq!(v.deadline(), Some(t + ACTIVATION_BOUND), "to 60 s from Enter");
    assert_eq!(v.due(t + ACTIVATION_BOUND - S), (None, false));
    assert_eq!(v.due(t + ACTIVATION_BOUND), (Some("still activating after 60 s"), true));
    assert!(v.attempt.is_none());
    assert_eq!(v.failed, Some((uuid(1), t + ACTIVATION_BOUND + FAILED_SHOWN)));
}

#[test]
fn nm_gone_ends_the_attempt_without_a_mark() {
    let t = Instant::now();
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    v.failed = Some((uuid(3), t + S));
    assert_eq!(v.set_state(None, t + S), Some("NetworkManager went away"));
    assert_eq!(v.failed, None);
    assert_eq!(v.attempt, None);
    assert!(!v.shows(), "the row hides");
    // Without an attempt, a lost state says nothing.
    let mut v = three(&[]);
    assert_eq!(v.set_state(None, t), None);
}

#[test]
fn failed_shows_4_s() {
    let t = Instant::now();
    let mut v = three(&[]);
    v.enter(&uuid(1), t);
    set(&mut v, &[(1, ACTIVATING)], t + S);
    set(&mut v, &[], t + 2 * S);
    let end = t + 2 * S + FAILED_SHOWN;
    assert_eq!(v.deadline(), Some(end));
    assert_eq!(v.due(end - Duration::from_millis(1)), (None, false));
    assert!(v.failed.is_some());
    assert_eq!(v.due(end), (None, true), "gone at +4 s, redrawn");
    assert_eq!(v.failed, None);
    assert_eq!(v.deadline(), None);
    // The earliest of the attempt's and the mark's.
    let mut v = three(&[]);
    v.failed = Some((uuid(3), t + S));
    v.enter(&uuid(1), t);
    assert_eq!(v.deadline(), Some(t + CONNECT_TIMEOUT), "enter cleared the mark");
    v.failed = Some((uuid(3), t + S));
    assert_eq!(v.deadline(), Some(t + S));
}

#[test]
fn the_scope_is_detail_while_open_or_attempting() {
    let t = Instant::now();
    let mut v = three(&[]);
    assert_eq!(v.scope(), VpnScope::Row);
    v.enter(&uuid(1), t);
    assert_eq!(v.scope(), VpnScope::Detail, "an attempt");
    v.never_started();
    assert_eq!((v.attempt.as_ref(), v.failed.as_ref(), v.scope()), (None, None, VpnScope::Row));
    assert_eq!(v.open_detail(), QuickAction::Redraw);
    assert!(v.open);
    assert_eq!(v.pick, Pick::Profile(uuid(1)), "the first listed");
    assert_eq!(v.scope(), VpnScope::Detail);
    v.close_detail();
    assert_eq!((v.open, v.pick.clone(), v.scope()), (false, Pick::None, VpnScope::Row));
}
