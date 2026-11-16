use super::*;
use crate::actions::Button;
use crate::config::QuickConfig;
use crate::grid::{Grid, Style, text_width};
use crate::keys::Key;
use crate::quick::Line;
use crate::quick_wifi::{ACTIVATION_BOUND, CONNECT_TIMEOUT};
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
    assert_eq!(set(&mut v, &[], t + 2 * S), Some("the connection may need a secret"));
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
    assert_eq!(v.set_state(Some(s), t + S), Some("the profile went away"), "no profile left, NetworkManager there");
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

/// Down then up at once: NetworkManager may still list the old, deactivated (4) active connection
/// beside the new activating one for the same uuid. The most advanced state counts (2 > 1 > 3 > 4),
/// so the stale entry never fails the fresh attempt, whatever the order.
#[test]
fn a_stale_deactivated_entry_never_fails_a_fresh_attempt() {
    let t = Instant::now();
    for active in [[(1, 4), (1, ACTIVATING)], [(1, ACTIVATING), (1, 4)]] {
        let mut v = three(&[]);
        v.enter(&uuid(1), t);
        assert_eq!(set(&mut v, &active, t + S), None, "{active:?}: waiting, not failed");
        assert!(v.attempt.as_ref().is_some_and(|a| a.activating), "{active:?}: activating seen");
        assert_eq!(v.failed, None);
        assert_eq!(v.listed()[0].state, Some(ACTIVATING), "{active:?}: listed as activating");
        let both = [(1, 4), (1, DEACTIVATING), (1, ACTIVATED), (1, ACTIVATING)];
        assert_eq!(set(&mut v, &both, t + 2 * S), None, "activated wins over all");
        assert_eq!(v.attempt, None, "connected");
    }
    // Deactivating beats deactivated.
    assert_eq!(three(&[(2, 4), (2, DEACTIVATING)]).value(), "…");
}

/// A failure started by Space on the row shows on the row too, for the same 4 s.
#[test]
fn the_row_reads_failed_while_marked() {
    let t = Instant::now();
    let mut v = three(&[]);
    assert_eq!(v.toggle(t), run(&["nmcli", "connection", "up", "uuid", &uuid(2)], "vpn up"));
    set(&mut v, &[(2, ACTIVATING)], t + S);
    assert_eq!(set(&mut v, &[], t + 2 * S), Some("the activation failed"));
    assert_eq!(v.value(), "failed");
    v.due(t + 2 * S + FAILED_SHOWN);
    assert_eq!(v.value(), "off", "after 4 s");
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

/// The last used is looked for among every profile read, not only the 16 listed.
#[test]
fn space_ups_the_last_used_even_past_the_listed_16() {
    let t = Instant::now();
    let profiles: Vec<VpnProfile> =
        (0..20).map(|n| profile(n, &format!("VPN-Test-{n:02}"), VpnKind::Plugin, if n == 17 { 999 } else { 1 }, false)).collect();
    let mut v = vpn(profiles, &[]);
    assert!(!v.listed().iter().any(|p| p.uuid == uuid(17)), "the 18th by name is not listed");
    assert_eq!(v.toggle(t), run(&["nmcli", "connection", "up", "uuid", &uuid(17)], "vpn up"));
    assert_eq!(v.attempt.as_ref().map(|a| a.uuid.clone()), Some(uuid(17)));
    // Enter from a detail row still needs a listed row.
    let mut v = vpn((0..20).map(|n| profile(n, &format!("VPN-Test-{n:02}"), VpnKind::Plugin, 0, false)).collect(), &[]);
    assert_eq!(v.enter(&uuid(17), t), QuickAction::Nothing);
}

fn config() -> QuickConfig {
    QuickConfig { vpn_more: Some(vec!["fake-vpn-more".into()]), ..QuickConfig::default() }
}

fn opened(v: Vpn) -> Vpn {
    let mut v = v;
    v.open_detail();
    v
}

fn row(v: &Vpn, line: Line, selected: bool) -> Grid {
    let mut g = Grid::empty(crate::quick::WIDTH + 2);
    v.draw(&mut g, line, &v.listed(), selected);
    g
}

#[test]
fn the_detail_lines_and_more() {
    let v = opened(three(&[(3, ACTIVATED)]));
    use Line::*;
    assert_eq!(
        v.lines(&config()),
        [VpnHeader, Separator, VpnProfile(0), VpnProfile(1), VpnProfile(2), Separator, VpnMore],
        "the header, a separator, the profiles, a separator and More…"
    );
    assert_eq!(
        v.lines(&QuickConfig::default()),
        [VpnHeader, Separator, VpnProfile(0), VpnProfile(1), VpnProfile(2)],
        "unresolved: no More…"
    );
    let hidden = QuickConfig { vpn_more: Some(vec![]), ..QuickConfig::default() };
    assert_eq!(v.lines(&hidden).last(), Some(&VpnProfile(2)), "[]: no More…, no separator");
    assert_eq!(more(&config()), ["fake-vpn-more"]);
    assert!(more(&QuickConfig::default()).is_empty());
    // The rows as drawn: ● on the activated one, the glyph by kind.
    assert_eq!(row(&v, VpnProfile(0), false).text().trim_end(), format!(" ● {WIREGUARD} WG-Test"));
    assert_eq!(row(&v, VpnProfile(1), false).text().trim_end(), format!("   {} VPN-Test-1", crate::segments::VPN));
    assert_eq!(row(&v, VpnHeader, false).text().trim_end(), " ‹ VPN");
    assert_eq!(row(&v, VpnMore, false).text().trim_end(), "   More…");
    assert!(row(&v, VpnProfile(1), true).styles().chars().all(|c| c == 'S'), "selected: the band across the row");
    assert_eq!(&row(&v, VpnProfile(0), false).styles()[1..2], "a", "● in the accent");
    // Selection by identity.
    let listed = v.listed();
    assert!(v.is_selected(VpnProfile(0), &listed) && !v.is_selected(VpnProfile(1), &listed) && !v.is_selected(VpnMore, &listed));
    let mut m = v.clone();
    m.pick = Pick::More;
    assert!(m.is_selected(VpnMore, &listed) && !m.is_selected(VpnProfile(0), &listed));
}

#[test]
fn a_long_name_keeps_the_tag_column() {
    let long = "é".repeat(150);
    assert_eq!(long.len(), 300);
    let mut v = opened(vpn(vec![profile(1, &long, VpnKind::Plugin, 0, false)], &[(1, ACTIVATED)]));
    let t = Instant::now();
    v.failed = Some((uuid(1), t + FAILED_SHOWN));
    let width = crate::quick::WIDTH + 2;
    let g = row(&v, Line::VpnProfile(0), false);
    let text = g.text();
    assert!(text.trim_end().ends_with("… failed"), "{text:?}");
    assert_eq!(text_width(&text), width, "within the row");
    // The tag's six cells end one cell before the end, the name never reaches them.
    assert_eq!(&g.styles()[usize::from(width - 7)..usize::from(width - 1)], "wwwwww", "{}", g.styles());
    assert_eq!(g.cells()[usize::from(width - 1)].ch, ' ', "the last cell stays blank");
    assert_eq!(g.cells()[usize::from(width - 8)].ch, ' ', "a gap before the tag");
    // The same under `…`: the column starts where `failed` does.
    v.failed = None;
    v.attempt = Some(Attempt { uuid: uuid(1), started: t, until: t + CONNECT_TIMEOUT, activating: false, asks: false });
    let g = row(&v, Line::VpnProfile(0), false);
    assert_eq!(g.cells()[usize::from(width - 7)].ch, '…');
    assert_eq!(text_width(&g.text()), width);
}

#[test]
fn the_tags() {
    let t = Instant::now();
    // Its attempt: `…`, muted.
    let mut v = opened(three(&[]));
    v.enter(&uuid(1), t);
    let g = row(&v, Line::VpnProfile(0), false);
    assert_eq!((g.cells()[27].ch, g.cells()[27].style), ('…', Style::MUTED));
    assert!(!row(&v, Line::VpnProfile(1), false).text().contains('…'), "another profile: nothing");
    // State 1 and 3 read from the state itself, no attempt.
    for state in [ACTIVATING, DEACTIVATING] {
        let v = opened(three(&[(1, state)]));
        let g = row(&v, Line::VpnProfile(0), false);
        assert_eq!((g.cells()[27].ch, g.cells()[27].style), ('…', Style::MUTED), "state {state}");
        assert_eq!(g.cells()[1].ch, ' ', "no ● until activated");
    }
    // Activated: ●, no tag.
    let v = opened(three(&[(1, ACTIVATED)]));
    assert_eq!(row(&v, Line::VpnProfile(0), false).text().trim_end(), format!(" ● {} VPN-Test-1", crate::segments::VPN));
    // `failed` in warn for FAILED_SHOWN, then nothing.
    let mut v = opened(three(&[]));
    v.enter(&uuid(1), t);
    assert_eq!(set(&mut v, &[(1, 4)], t + S), Some("the activation failed"));
    let g = row(&v, Line::VpnProfile(0), false);
    assert!(g.text().trim_end().ends_with("failed"));
    assert_eq!(&g.styles()[27..33], "wwwwww");
    assert_eq!(v.deadline(), Some(t + S + FAILED_SHOWN));
    assert_eq!(v.due(t + S + FAILED_SHOWN - Duration::from_millis(1)), (None, false), "still shown");
    assert_eq!(v.due(t + S + FAILED_SHOWN), (None, true), "4 s later: gone");
    assert!(!row(&v, Line::VpnProfile(0), false).text().contains("failed"));
}

#[test]
fn keys_walk_the_profiles_then_more_and_enter_acts() {
    let (c, t) = (config(), Instant::now());
    let mut v = opened(three(&[(3, ACTIVATED)]));
    assert_eq!(v.pick, Pick::Profile(uuid(3)), "the active one is first");
    assert_eq!(v.key(&c, Key::Up, t), QuickAction::Nothing, "clamped");
    assert_eq!(v.key(&c, Key::Down, t), QuickAction::Redraw);
    assert_eq!(v.pick, Pick::Profile(uuid(1)));
    assert_eq!(v.key(&c, Key::End, t), QuickAction::Redraw);
    assert_eq!(v.pick, Pick::More, "More… after the profiles");
    assert_eq!(v.key(&c, Key::Down, t), QuickAction::Nothing);
    assert_eq!(v.key(&c, Key::Home, t), QuickAction::Redraw);
    assert_eq!(v.pick, Pick::Profile(uuid(3)));
    assert_eq!(v.key(&c, Key::Enter, t), run(&["nmcli", "connection", "down", "uuid", &uuid(3)], "vpn down"));
    v.key(&c, Key::Down, t);
    assert_eq!(v.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", &uuid(1)], "vpn up"));
    assert_eq!(v.key(&c, Key::Enter, t), QuickAction::Nothing, "a repeat Enter runs nothing");
    v.key(&c, Key::End, t);
    assert_eq!(
        v.key(&c, Key::Enter, t),
        QuickAction::RunAndClose { argv: vec!["fake-vpn-more".into()], what: "vpn more".into() },
        "More… acts during an attempt"
    );
    let no_more = QuickConfig { vpn_more: Some(vec![]), ..QuickConfig::default() };
    let mut v = opened(three(&[]));
    v.key(&no_more, Key::End, t);
    assert_eq!(v.pick, Pick::Profile(uuid(3)), "no More… to reach");
    assert_eq!(v.key(&c, Key::Left, t), QuickAction::Redraw);
    assert!(!v.open, "← closes the detail");
    let mut v = opened(three(&[]));
    assert_eq!(v.key(&c, Key::Escape, t), QuickAction::Redraw);
    assert!(!v.open, "Escape closes the detail");
    assert_eq!(v.key(&c, Key::Enter, t), QuickAction::Nothing, "closed: nothing picked");
}

#[test]
fn presses_pick_enter_or_run() {
    let (c, t) = (config(), Instant::now());
    let mut v = opened(three(&[]));
    assert_eq!(v.press(&c, Line::VpnProfile(1), Button::Right, t), QuickAction::Nothing, "left only");
    assert_eq!(v.pick, Pick::Profile(uuid(1)), "unchanged");
    assert_eq!(v.press(&c, Line::VpnProfile(2), Button::Left, t), run(&["nmcli", "connection", "up", "uuid", &uuid(3)], "vpn up"));
    assert_eq!(v.pick, Pick::Profile(uuid(3)));
    // A click while an attempt runs only moves the pick.
    assert_eq!(v.press(&c, Line::VpnProfile(0), Button::Left, t), QuickAction::Redraw);
    assert_eq!(v.pick, Pick::Profile(uuid(1)));
    assert_eq!(v.press(&c, Line::VpnProfile(9), Button::Left, t), QuickAction::Nothing, "past the list");
    assert_eq!(
        v.press(&c, Line::VpnMore, Button::Left, t),
        QuickAction::RunAndClose { argv: vec!["fake-vpn-more".into()], what: "vpn more".into() }
    );
    assert_eq!(v.pick, Pick::More);
    assert_eq!(v.press(&c, Line::VpnHeader, Button::Left, t), QuickAction::Redraw);
    assert!(!v.open, "the header goes back");
}

#[test]
fn fit_closes_without_profiles_and_moves_the_pick_to_a_neighbour() {
    let (c, t) = (config(), Instant::now());
    // One of three gone: the pick moves to the next one down, else the one above.
    let gone = |v: &Vpn, n: usize| {
        let mut s = v.state.clone().expect("state");
        s.profiles.retain(|p| p.uuid != uuid(n));
        s
    };
    let mut v = opened(three(&[]));
    v.key(&c, Key::Down, t);
    assert_eq!(v.pick, Pick::Profile(uuid(2)));
    let s = gone(&v, 2);
    v.set_state(Some(s), t);
    v.fit(&c);
    assert_eq!(v.pick, Pick::Profile(uuid(3)), "the neighbour below");
    let s = gone(&v, 3);
    v.set_state(Some(s), t);
    v.fit(&c);
    assert_eq!(v.pick, Pick::Profile(uuid(1)), "none below: the one above");
    // Gone while it was the last: still a profile left, so open.
    assert!(v.open);
    // The last profile gone: the detail closes.
    let s = gone(&v, 1);
    v.set_state(Some(s), t);
    v.fit(&c);
    assert!(!v.open && v.pick == Pick::None, "nothing left: closed");
    // NetworkManager gone closes it too.
    let mut v = opened(three(&[]));
    v.set_state(None, t);
    v.fit(&c);
    assert!(!v.open);
    // A pick lost with no prior order (never seen): the first.
    let mut v = opened(three(&[]));
    v.pick = Pick::Profile("elsewhere".into());
    v.fit(&c);
    assert_eq!(v.pick, Pick::Profile(uuid(1)));
    // A closed detail is left alone.
    let mut v = three(&[]);
    v.fit(&c);
    assert!(!v.open && v.pick == Pick::None);
    // More… kept as a pick when it is configured; lost when hidden.
    let mut v = opened(three(&[]));
    v.pick = Pick::More;
    v.fit(&QuickConfig { vpn_more: Some(vec![]), ..QuickConfig::default() });
    assert_eq!(v.pick, Pick::Profile(uuid(1)));
}
