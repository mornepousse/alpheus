use super::*;
use crate::quick_app::WifiFollow;
use crate::quick_secret::CAPACITY;
use crate::system::{SavedWifi, WifiDevice};
use std::time::Duration;

const HOME: &str = "11111111-2222-4333-8444-555555555555";
const CAFE: &str = "33333333-4444-4555-8666-777777777777";

fn ap(ssid: &str, strength: u8, security: Security) -> AccessPoint {
    AccessPoint { ssid: ssid.as_bytes().to_vec(), strength, security }
}

/// Connected to Maison-Test (80 %, WPA2, saved); Labo-Test (70 %, 802.1X), Bureau-Test (60 %,
/// WPA2) and Cafe-Libre (40 %, open) around (fictional networks).
fn state() -> WifiState {
    let home = ap("Maison-Test", 80, Security::Psk);
    WifiState {
        enabled: true,
        device: Some(WifiDevice {
            state: ACTIVATED,
            active: Some(home.clone()),
            active_uuid: Some(HOME.into()),
            access_points: Some(vec![
                ap("Cafe-Libre", 40, Security::Open),
                home,
                ap("Bureau-Test", 60, Security::Psk),
                ap("Labo-Test", 70, Security::Enterprise),
            ]),
            saved: Some(vec![SavedWifi { uuid: HOME.into(), ssid: b"Maison-Test".to_vec() }]),
        }),
        stale: false,
    }
}

fn device(s: &mut WifiState) -> &mut WifiDevice {
    s.device.as_mut().expect("a device")
}

fn config() -> QuickConfig {
    QuickConfig { wifi_more: Some(vec!["fake-more".into()]), ..QuickConfig::default() }
}

/// The detail open on `s`, fitted (the first network chosen).
fn open(s: WifiState) -> Wifi {
    let mut w = Wifi { state: Some(s), ..Wifi::default() };
    w.open_detail();
    w.fit(&config());
    w
}

fn labels(w: &Wifi) -> Vec<String> {
    w.networks().iter().map(|n| n.label.clone()).collect()
}

fn run(parts: &[&str], what: &str) -> QuickAction {
    QuickAction::Run { argv: parts.iter().map(|s| s.to_string()).collect(), what: what.into() }
}

fn pick(w: &mut Wifi, label: &str) {
    w.pick = Pick::Network(label.as_bytes().to_vec());
}

#[test]
fn four_steps_of_signal() {
    let cases = [
        (0, 1, "▂___"),
        (24, 1, "▂___"),
        (25, 2, "▂▄__"),
        (49, 2, "▂▄__"),
        (50, 3, "▂▄▆_"),
        (74, 3, "▂▄▆_"),
        (75, 4, "▂▄▆█"),
        (100, 4, "▂▄▆█"),
    ];
    for (strength, n, drawn) in cases {
        assert_eq!((steps(strength), bars(strength).as_str()), (n, drawn), "{strength} %");
    }
}

#[test]
fn the_detail_orders_dedups_caps_and_hides() {
    assert_eq!(labels(&open(state())), ["Maison-Test", "Labo-Test", "Bureau-Test", "Cafe-Libre"], "connected, then by signal");
    let mut s = state();
    device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
    device(&mut s).access_points.as_mut().expect("listed").extend([ap("Bureau-Test", 90, Security::Open), ap("", 99, Security::Open)]);
    let w = open(s);
    assert_eq!(labels(&w), ["Maison-Test", "Cafe-Libre", "Bureau-Test", "Labo-Test"], "saved before stronger; one row per SSID; no hidden");
    let bureau = &w.networks()[2];
    assert_eq!((bureau.strength, bureau.security), (90, Security::Open), "the strongest access point's");
    let mut s = state();
    device(&mut s).access_points = Some((0..40).map(|n| ap(&format!("Reseau-Test-{n:02}"), 50, Security::Psk)).collect());
    let w = open(s);
    assert_eq!(w.networks().len(), NETWORKS_MAX);
    assert_eq!(labels(&w)[0], "Maison-Test", "the connected one, even when the list misses it");
}

#[test]
fn the_marks_and_the_rows() {
    let w = open(state());
    let nets = w.networks();
    let row = |line: Line, selected: bool| {
        let mut g = Grid::empty(crate::quick::WIDTH + 2);
        w.draw(&mut g, line, &nets, selected);
        g
    };
    assert_eq!(row(Line::Network(0), false).text().trim_end(), format!(" ● Maison-Test      ▂▄▆█ {LOCK} saved"));
    assert_eq!(row(Line::Network(1), false).text().trim_end(), format!("   Labo-Test        ▂▄▆_ {LOCK}"));
    assert_eq!(row(Line::Network(3), false).text().trim_end(), "   Cafe-Libre       ▂▄__", "open: no lock");
    let g = row(Line::Network(0), false);
    assert_eq!((&g.styles()[1..2], &g.styles()[27..32]), ("a", "mmmmm"), "● in the accent, saved dimmed");
    assert!(row(Line::Network(2), true).styles().chars().all(|c| c == 'S'), "selected: the band across the row");
    assert_eq!(row(Line::WifiHeader, false).text().trim_end(), " ‹ Wi-Fi");
    assert_eq!(row(Line::WifiMore, false).text().trim_end(), "   More…");
    assert_eq!(row(Line::WifiNote(OFF_NOTE), false).text().trim(), OFF_NOTE);
}

#[test]
fn enter_on_each_kind_of_network() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Maison-Test");
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "down", "uuid", HOME], "wifi disconnect"), "connected: down");
    let mut s = state();
    let d = device(&mut s);
    (d.state, d.active, d.active_uuid) = (30, None, None);
    let mut w = open(s);
    pick(&mut w, "Maison-Test");
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", HOME], "wifi connect (saved)"));
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
    pick(&mut w, "Labo-Test");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
    assert_eq!(w.pick, Pick::More, "802.1X: More… instead");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::RunAndClose { argv: vec!["fake-more".into()], what: "wifi more".into() });
    let mut w = open(state());
    pick(&mut w, "Labo-Test");
    assert_eq!(w.key(&QuickConfig { wifi_more: Some(vec![]), ..config() }, Key::Enter, t), QuickAction::Nothing, "no More…");
}

/// Review Focus 2: an SSID that cannot be one nmcli argument as is (a leading '-', a NUL, not
/// UTF-8) is shown cleaned and never passed: More… unsaved, by uuid saved.
#[test]
fn refused_ssids_go_to_more_unless_saved() {
    let latin1 = vec![0x43, 0x61, 0x66, 0xE9];
    assert!(refused(b"-x") && refused(b"a\0b") && refused(&latin1) && refused(b""));
    assert!(!refused(b"Cafe-Libre") && !refused("Café".as_bytes()) && !refused(b"a-b"));
    let (c, t) = (config(), Instant::now());
    let mut s = state();
    let latin = AccessPoint { ssid: latin1.clone(), strength: 45, security: Security::Open };
    device(&mut s).access_points.as_mut().expect("listed").extend([ap("-x", 50, Security::Open), latin]);
    device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: latin1.clone() });
    let mut w = open(s);
    assert!(labels(&w).contains(&"Caf\u{fffd}".to_string()), "shown cleaned: {:?}", labels(&w));
    pick(&mut w, "-x");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
    assert_eq!(w.pick, Pick::More, "never `nmcli device wifi connect -x`");
    w.pick = Pick::Network(latin1);
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", CAFE], "wifi connect (saved)"), "saved: by uuid");
}

#[test]
fn arrows_move_over_the_networks_and_more_never_wrapping() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the first network");
    assert_eq!(w.key(&c, Key::Up, t), QuickAction::Nothing);
    for _ in 0..3 {
        assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
    }
    assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
    assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
    assert_eq!(w.pick, Pick::More);
    assert_eq!(w.key(&c, Key::Down, t), QuickAction::Nothing, "the end: no wrap");
    assert_eq!(w.key(&c, Key::Home, t), QuickAction::Redraw);
    assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()));
    assert_eq!(w.key(&c, Key::End, t), QuickAction::Redraw);
    assert_eq!(w.key(&c, Key::Left, t), QuickAction::Redraw);
    assert!(!w.open, "← back to the rows");
}

#[test]
fn the_detail_lines_and_its_notes() {
    let c = config();
    assert_eq!(
        open(state()).lines(&c),
        [
            Line::WifiHeader,
            Line::Separator,
            Line::Network(0),
            Line::Network(1),
            Line::Network(2),
            Line::Network(3),
            Line::Separator,
            Line::WifiMore
        ]
    );
    let mut s = state();
    let d = device(&mut s);
    (d.state, d.active, d.access_points) = (30, None, None);
    assert_eq!(open(s.clone()).lines(&c)[2], Line::WifiNote(SEARCHING), "before the list arrives");
    device(&mut s).access_points = Some(vec![]);
    assert_eq!(open(s).lines(&c)[2], Line::WifiNote(NO_NETWORKS));
    let mut off = state();
    off.enabled = false;
    let w = open(off);
    assert_eq!(w.lines(&c)[2..], [Line::WifiNote(OFF_NOTE), Line::Separator, Line::WifiMore], "the radio off: the list emptied");
    assert_eq!(w.pick, Pick::More);
    assert_eq!(open(state()).lines(&QuickConfig::default()).last(), Some(&Line::Network(3)), "no wifi_more: no More…, no separator");
}

#[test]
fn networkmanager_going_away_closes_the_detail() {
    let t = Instant::now();
    let mut w = open(state());
    w.set_state(None, t);
    w.fit(&config());
    assert!(!w.open && !w.shows() && w.pick == Pick::None);
    let mut w = open(state());
    w.set_state(Some(WifiState { enabled: true, device: None, stale: false }), t);
    w.fit(&config());
    assert!(!w.open, "the device gone");
}

#[test]
fn a_press_acts_on_its_line() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    assert_eq!(
        w.press(&c, Line::Network(3), Button::Left, t),
        run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)")
    );
    assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
    assert_eq!(w.press(&c, Line::Network(1), Button::Left, t), QuickAction::Redraw, "802.1X: More… chosen");
    assert_eq!(w.pick, Pick::More);
    assert_eq!(w.press(&c, Line::Network(0), Button::Right, t), QuickAction::Nothing, "the left button only");
    assert_eq!(
        w.press(&c, Line::WifiMore, Button::Left, t),
        QuickAction::RunAndClose { argv: vec!["fake-more".into()], what: "wifi more".into() }
    );
    assert_eq!(w.press(&c, Line::WifiHeader, Button::Left, t), QuickAction::Redraw);
    assert!(!w.open, "the header goes back");
}

const NEW: &str = "22222222-3333-4444-8555-666666666666";

fn typed(w: &mut Wifi, text: &str, t: Instant) {
    for c in text.chars() {
        w.key(&config(), Key::Char(c), t);
    }
}

/// `w`'s state with the device at `state`, on `active` (an access point of that SSID) or none.
fn device_at(w: &mut Wifi, state: u32, active: Option<&str>, t: Instant) -> (QuickAction, Option<&'static str>) {
    let mut s = w.state.clone().expect("a state");
    let d = device(&mut s);
    d.state = state;
    d.active = active.map(|a| ap(a, 60, Security::Psk));
    w.set_state(Some(s), t)
}

fn saved_added(w: &mut Wifi, uuid: &str, ssid: &str, t: Instant) {
    let mut s = w.state.clone().expect("a state");
    device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: uuid.into(), ssid: ssid.as_bytes().to_vec() });
    w.set_state(Some(s), t);
}

/// The text of `label`'s row as drawn.
fn row_of(w: &Wifi, label: &str) -> Grid {
    let nets = w.networks();
    let i = nets.iter().position(|n| n.label == label).expect("listed");
    let mut g = Grid::empty(crate::quick::WIDTH + 2);
    w.draw(&mut g, Line::Network(i), &nets, false);
    g
}

fn password_row_text(w: &Wifi) -> String {
    let nets = w.networks();
    let mut g = Grid::empty(crate::quick::WIDTH + 2);
    w.draw(&mut g, Line::Password, &nets, false);
    g.text().trim_end().to_string()
}

fn delete(uuid: &str) -> QuickAction {
    run(&["nmcli", "connection", "delete", "uuid", uuid], "wifi delete the failed profile")
}

#[test]
fn a_new_secured_network_asks_for_its_password_in_a_field() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
    assert_eq!(w.pick, Pick::Password);
    assert_eq!(w.lines(&c)[4..6], [Line::Network(2), Line::Password], "the field under its network");
    typed(&mut w, "fake-pass", t);
    assert_eq!(password_row_text(&w), format!("   Password  {}", "•".repeat(9)), "one dot per character, never the text");
    let send = w.key(&c, Key::Enter, t);
    assert_eq!(
        send,
        QuickAction::RunSecret {
            argv: argv(&["nmcli", "--ask", "device", "wifi", "connect", "Bureau-Test"]),
            what: "wifi connect (password)".into()
        }
    );
    assert!(!format!("{send:?}").contains("fake-pass"), "never in the action, its argv or its Debug");
    assert_eq!(w.password().map(|s| s.bytes()), Some(&b"fake-pass"[..]), "kept for the wiring to hand over");
    w.password_sent();
    assert!(w.field.is_none() && w.password().is_none(), "closed: its buffer wiped as it drops");
    assert_eq!(w.pick, Pick::Network(b"Bureau-Test".to_vec()));
    assert!(row_of(&w, "Bureau-Test").text().trim_end().ends_with(" …"));
}

#[test]
fn the_field_s_keys() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "empty: nothing sent");
    typed(&mut w, "abc", t);
    assert_eq!(w.key(&c, Key::Backspace, t), QuickAction::Redraw);
    assert_eq!(w.password().map(|s| s.bytes()), Some(&b"ab"[..]));
    assert_eq!(w.key(&c, Key::CtrlU, t), QuickAction::Redraw);
    assert!(w.password().is_some_and(|s| s.is_empty()));
    typed(&mut w, "x y", t);
    assert_eq!(w.password().map(|s| s.bytes()), Some(&b"x y"[..]), "Space is a character here");
    assert_eq!(w.key(&c, Key::Escape, t), QuickAction::Redraw);
    assert!(w.field.is_none() && w.open, "Escape closes the field and keeps the detail");
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    typed(&mut w, "z", t);
    assert_eq!(w.key(&c, Key::Down, t), QuickAction::Redraw);
    assert!(w.field.is_none(), "moving away closes it");
    assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    w.close_detail();
    assert!(w.field.is_none(), "leaving the detail closes it");
}

/// A press on the field keeps it; a press anywhere else closes it (its buffer wiped), as moving
/// away does.
#[test]
fn a_press_elsewhere_closes_the_field() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    typed(&mut w, "fake-pass", t);
    assert_eq!(w.press(&c, Line::Password, Button::Left, t), QuickAction::Nothing);
    assert!(w.field.is_some(), "a press on the field keeps it");
    assert_eq!(
        w.press(&c, Line::Network(3), Button::Left, t),
        run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)")
    );
    assert!(w.field.is_none() && w.password().is_none(), "another network pressed: the field gone");
}

/// Review Focus 3: characters, not bytes; a whole character per Backspace; a character that
/// does not fit refuses the attempt until Ctrl+U: never a password sent cut short.
#[test]
fn the_password_field_counts_characters_and_refuses_an_overflow() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    typed(&mut w, "é🔑a", t);
    assert_eq!(password_row_text(&w), format!("   Password  {}", "•".repeat(3)), "three characters, seven bytes");
    w.key(&c, Key::Backspace, t);
    w.key(&c, Key::Backspace, t);
    assert_eq!(w.password().map(|s| s.bytes()), Some("é".as_bytes()));
    w.key(&c, Key::CtrlU, t);
    typed(&mut w, &"a".repeat(CAPACITY), t);
    assert!(w.field.as_ref().is_some_and(|f| !f.overflow), "256 bytes fit");
    assert!(password_row_text(&w).starts_with("   Password  …"), "the end of the dots: {}", password_row_text(&w));
    w.key(&c, Key::Char('b'), t);
    assert_eq!(password_row_text(&w), format!("   Password  {TOO_LONG}"));
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "never sent cut short");
    w.key(&c, Key::Backspace, t);
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "still refused until Ctrl+U");
    w.key(&c, Key::CtrlU, t);
    typed(&mut w, "fake-pass", t);
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::RunSecret { .. }));
}

#[test]
fn connecting_then_connected() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
    assert!(row_of(&w, "Cafe-Libre").text().trim_end().ends_with(" …"));
    assert_eq!(w.deadline(), Some(t + CONNECT_TIMEOUT));
    assert_eq!(device_at(&mut w, ACTIVATED, Some("Maison-Test"), t), (QuickAction::Redraw, None), "still on the old one: waiting");
    assert_eq!(device_at(&mut w, 50, Some("Cafe-Libre"), t), (QuickAction::Redraw, None), "activating");
    assert_eq!(device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t), (QuickAction::Redraw, None));
    assert!(w.attempt.is_none() && w.deadline().is_none(), "activated on that SSID: done");
    assert!(row_of(&w, "Cafe-Libre").text().starts_with(" ● Cafe-Libre"), "● once NetworkManager says so");
}

/// Spec § 2's mockup: the bars and the lock sit in one column whatever the tag; the tag's
/// room is the widest tag's, `failed` (6 cells; Mae's short tags, 2026-10-12: `…` while
/// connecting), so an SSID keeps 16 cells.
#[test]
fn the_bars_and_the_lock_sit_in_one_column() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    let column = |label: &str| {
        let chars: Vec<char> = row_of(&w, label).text().chars().collect();
        let bars = chars.iter().position(|ch| BARS.contains(ch)).expect("bars");
        (bars, chars[bars + 5])
    };
    assert!(row_of(&w, "Cafe-Libre").text().trim_end().ends_with(" …"), "{:?}", row_of(&w, "Cafe-Libre").text());
    let (at, _) = column("Cafe-Libre");
    assert_eq!(column("Labo-Test"), (at, LOCK), "untagged: the same column");
    assert_eq!(column("Maison-Test"), (at, LOCK), "saved: the same column");
    let end = usize::from(crate::quick::WIDTH + 2);
    assert_eq!(at, end - 1 - (4 + 1 + 1 + 1 + 6), "room for the widest tag, `failed`");
    let mut long = state();
    device(&mut long).access_points.as_mut().expect("aps")[0].ssid = b"Sixteen-Cells-Id-and-more".to_vec();
    let w = open(long);
    let row = row_of(&w, "Sixteen-Cells-Id-and-more").text();
    assert!(row.starts_with("   Sixteen-Cells-I… "), "16 cells for an SSID: {row:?}");
}

#[test]
fn a_failed_new_network_shows_failed_and_its_new_profile_is_deleted() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    typed(&mut w, "fake-pass", t);
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::RunSecret { .. }));
    w.password_sent();
    device_at(&mut w, 60, Some("Bureau-Test"), t);
    saved_added(&mut w, NEW, "Bureau-Test", t);
    let later = t + Duration::from_secs(3);
    assert_eq!(device_at(&mut w, 30, None, later), (delete(NEW), Some("the activation failed")));
    let g = row_of(&w, "Bureau-Test");
    assert!(g.text().trim_end().ends_with("failed"));
    assert!(g.styles().trim_end_matches('.').ends_with("wwwwww"), "in warn: {}", g.styles());
    assert_eq!(w.deadline(), Some(later + FAILED_SHOWN));
    assert_eq!(w.due(later + FAILED_SHOWN), (vec![], None, true), "4 s later: gone");
    assert!(w.failed.is_none() && w.deadline().is_none());
}

#[test]
fn state_120_fails_at_once_and_twenty_seconds_without_an_answer_too() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    assert_eq!(
        device_at(&mut w, FAILED, None, t),
        (QuickAction::Redraw, Some("the activation failed")),
        "no new profile: nothing to delete"
    );
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    saved_added(&mut w, NEW, "Cafe-Libre", t);
    assert_eq!(w.due(t + CONNECT_TIMEOUT - Duration::from_millis(1)), (vec![], None, false));
    assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![delete(NEW)], Some("no answer in 20 s"), true));
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    assert_eq!(w.set_state(None, t), (QuickAction::Redraw, Some("NetworkManager went away")));
    assert!(w.attempt.is_none() && w.failed.is_none());
}

/// Review Focus 4: one attempt at a time; More… still works.
#[test]
fn one_attempt_at_a_time() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }));
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "Enter again: one nmcli");
    pick(&mut w, "Maison-Test");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "a disconnect waits too");
    pick(&mut w, "Bureau-Test");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Nothing, "no field while connecting");
    assert!(w.field.is_none());
    pick(&mut w, "Labo-Test");
    assert_eq!(w.key(&c, Key::Enter, t), QuickAction::Redraw);
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::RunAndClose { .. }), "More… still runs");
}

/// Review Focus 5: the cleanup deletes only a profile that appeared during the attempt, and only
/// while it is still saved.
#[test]
fn the_cleanup_deletes_only_the_profile_the_attempt_made() {
    let (c, t) = (config(), Instant::now());
    let mut s = state();
    device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
    let mut w = open(s);
    pick(&mut w, "Cafe-Libre");
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "connection", "up", "uuid", CAFE], "wifi connect (saved)"));
    device_at(&mut w, 50, Some("Cafe-Libre"), t);
    assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, Some("the activation failed")), "an old profile: kept");
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    device_at(&mut w, 50, Some("Cafe-Libre"), t);
    saved_added(&mut w, NEW, "Cafe-Libre", t);
    let mut gone = w.state.clone().expect("a state");
    device(&mut gone).saved.as_mut().expect("saved").retain(|p| p.uuid != NEW);
    device(&mut gone).state = 30;
    assert_eq!(
        w.set_state(Some(gone), t),
        (QuickAction::Redraw, Some("the activation failed")),
        "nmcli deleted it first: nothing to delete"
    );
}

/// Decision 10: the detail's reading outlives the detail while connecting.
#[test]
fn the_detail_s_reading_outlives_the_detail_while_connecting() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    w.close_detail();
    assert_eq!(w.scope(), WifiScope::Detail, "the new profile must still be seen");
    device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t);
    assert_eq!(w.scope(), WifiScope::Row);
}

/// The panel closing on `w` with its attempt under way: the follower it leaves (Mae's ruling P1).
fn closed_during(w: &mut Wifi) -> Option<WifiFollow> {
    WifiFollow::leaving(w)
}

/// `base` with its device at `state` on `active`, and `saved` added.
fn followed(base: &WifiState, state: u32, active: Option<&str>, saved: &[(&str, &str)]) -> WifiState {
    let mut s = base.clone();
    let d = device(&mut s);
    d.state = state;
    d.active = active.map(|a| ap(a, 60, Security::Psk));
    for (uuid, ssid) in saved {
        d.saved.get_or_insert_with(Vec::new).push(SavedWifi { uuid: (*uuid).into(), ssid: ssid.as_bytes().to_vec() });
    }
    s
}

/// Mae's ruling P1: closing the panel during an attempt on a new network leaves a follower that
/// still deletes the profile a failure leaves behind, by NetworkManager's word or the timeout.
#[test]
fn closing_during_a_failing_new_network_attempt_still_deletes_its_profile() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    typed(&mut w, "fake-pass", t);
    w.key(&c, Key::Enter, t);
    w.password_sent();
    let mut f = closed_during(&mut w).expect("a new network: followed");
    assert!(w.attempt.is_none(), "moved out of the panel");
    assert_eq!((f.scope(), f.deadline()), (WifiScope::Detail, t + CONNECT_TIMEOUT));
    let base = state();
    assert_eq!(f.set_state(Some(followed(&base, 60, Some("Bureau-Test"), &[(NEW, "Bureau-Test")]))), Outcome::Waiting);
    assert_eq!(
        f.set_state(Some(followed(&base, 30, None, &[(NEW, "Bureau-Test")]))),
        Outcome::Failed { why: "the activation failed", cleanup: Some(delete(NEW)) }
    );
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    let mut f = closed_during(&mut w).expect("an open network: followed");
    assert_eq!(f.set_state(Some(followed(&base, ACTIVATED, Some("Maison-Test"), &[(NEW, "Cafe-Libre")]))), Outcome::Waiting);
    assert_eq!(f.due(t + CONNECT_TIMEOUT - Duration::from_millis(1)), Outcome::Waiting);
    assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Failed { why: "no answer in 20 s", cleanup: Some(delete(NEW)) });
}

/// Mae's ruling P1: a follower whose attempt succeeds deletes nothing; a saved network's
/// attempt is dropped with the panel; a panel opened again takes the attempt back.
#[test]
fn a_follower_deletes_nothing_on_success_and_a_saved_network_is_not_followed() {
    let (c, t) = (config(), Instant::now());
    let base = state();
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    let mut f = closed_during(&mut w).expect("followed");
    assert_eq!(f.set_state(Some(followed(&base, 50, Some("Cafe-Libre"), &[(NEW, "Cafe-Libre")]))), Outcome::Waiting);
    assert_eq!(f.set_state(Some(followed(&base, ACTIVATED, Some("Cafe-Libre"), &[(NEW, "Cafe-Libre")]))), Outcome::Connected);
    assert_eq!(f.set_state(None), Outcome::Gone, "NetworkManager gone: nothing left to clean");
    let mut s = state();
    let d = device(&mut s);
    (d.state, d.active, d.active_uuid) = (30, None, None);
    let mut w = open(s);
    pick(&mut w, "Maison-Test");
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }), "connection up uuid");
    assert!(closed_during(&mut w).is_none(), "a saved network leaves no profile behind: not followed");
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    let f = closed_during(&mut w).expect("followed");
    let mut again = open(state());
    f.resume(&mut again);
    assert!(again.attempt.as_ref().is_some_and(|a| a.ssid == b"Cafe-Libre"), "the reopened panel inherits it");
    assert_eq!(again.scope(), WifiScope::Detail);
    pick(&mut again, "Bureau-Test");
    assert_eq!(again.key(&c, Key::Enter, t), QuickAction::Nothing, "no second attempt");
}

/// F4: the state the D-Bus thread resends through another domain's outage (BlueZ, UPower, the
/// power profile) was read before it, and nothing new comes for the backoff: an attempt that
/// expires on it never deletes (NetworkManager may have connected meanwhile); a state read
/// after the outage, still disconnected, deletes as before. The display stays as it was.
#[test]
fn an_attempt_expiring_on_a_state_from_before_an_outage_keeps_its_profile() {
    let (c, t) = (config(), Instant::now());
    let base = state();
    // Between the old network's deactivation and ours preparing, the new profile listed.
    let between = followed(&base, 30, None, &[(NEW, "Bureau-Test")]);
    let stale = WifiState { stale: true, ..between.clone() };
    let attempt = || {
        let mut w = open(state());
        pick(&mut w, "Bureau-Test");
        w.key(&c, Key::Enter, t);
        typed(&mut w, "fake-pass", t);
        w.key(&c, Key::Enter, t);
        w.password_sent();
        w
    };
    // The follower.
    let mut f = closed_during(&mut attempt()).expect("followed");
    assert_eq!(f.set_state(Some(between.clone())), Outcome::Waiting);
    assert_eq!(f.set_state(Some(stale.clone())), Outcome::Waiting);
    assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Failed { why: "no fresh state since an outage", cleanup: None });
    let mut f = closed_during(&mut attempt()).expect("followed");
    assert_eq!(f.set_state(Some(stale.clone())), Outcome::Waiting);
    assert_eq!(f.set_state(Some(between.clone())), Outcome::Waiting, "read after the outage");
    assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Failed { why: "no answer in 20 s", cleanup: Some(delete(NEW)) });
    // The panel: `failed` shown all the same, the profile kept.
    let mut w = attempt();
    w.set_state(Some(between.clone()), t);
    let shown = (w.value(), w.networks());
    w.set_state(Some(stale.clone()), t);
    assert_eq!((w.value(), w.networks()), shown, "the display unchanged");
    let (run, failed, redraw) = w.due(t + CONNECT_TIMEOUT);
    assert_eq!((run, failed, redraw), (vec![], Some("no fresh state since an outage"), true));
    assert!(w.failed.as_ref().is_some_and(|(ssid, _)| ssid == b"Bureau-Test"), "shown failed");
    let mut w = attempt();
    w.set_state(Some(stale), t);
    w.set_state(Some(between), t);
    assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![delete(NEW)], Some("no answer in 20 s"), true));
}

/// Controller reconciliation P12: an open field whose network leaves the list (an access point
/// aged out while typing) stays, its row with it, for FIELD_KEPT; then it goes, wiped.
#[test]
fn a_field_outlives_its_network_leaving_the_list_for_a_while() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Bureau-Test");
    w.key(&c, Key::Enter, t);
    typed(&mut w, "fake-pass", t);
    let mut s = state();
    device(&mut s).access_points.as_mut().expect("listed").retain(|a| a.ssid != b"Bureau-Test");
    w.set_state(Some(s.clone()), t);
    w.fit(&c);
    assert!(w.field.is_some() && labels(&w).contains(&"Bureau-Test".to_string()), "kept, its row too");
    assert_eq!(w.deadline(), Some(t + FIELD_KEPT));
    assert_eq!(w.due(t + FIELD_KEPT - Duration::from_millis(1)), (vec![], None, false));
    w.set_state(Some(state()), t + Duration::from_secs(5));
    assert_eq!(w.deadline(), None, "back in the list: no deadline");
    w.set_state(Some(s.clone()), t + Duration::from_secs(6));
    assert_eq!(w.deadline(), Some(t + Duration::from_secs(6) + FIELD_KEPT), "a new absence, counted again");
    assert_eq!(w.due(t + Duration::from_secs(6) + FIELD_KEPT), (vec![], None, true));
    w.fit(&c);
    assert!(w.field.is_none() && !labels(&w).contains(&"Bureau-Test".to_string()), "gone after FIELD_KEPT");
    assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the first pick");
}

/// Task 5 review: an SSID of zeros or one that cleans to nothing is hidden; a connected hidden
/// network shows `hidden` as the row's value.
#[test]
fn hidden_ssids_are_left_out_and_a_connected_one_says_so() {
    let mut s = state();
    let aps = device(&mut s).access_points.as_mut().expect("listed");
    aps.push(AccessPoint { ssid: vec![0, 0, 0], strength: 99, security: Security::Open });
    aps.push(AccessPoint { ssid: b"\x07\x1b".to_vec(), strength: 98, security: Security::Open });
    assert_eq!(labels(&open(s)), ["Maison-Test", "Labo-Test", "Bureau-Test", "Cafe-Libre"]);
    let mut s = state();
    device(&mut s).active = Some(AccessPoint { ssid: vec![0; 8], strength: 80, security: Security::Psk });
    let w = open(s);
    assert_eq!(w.value(), "hidden");
    assert!(!labels(&w).iter().any(|l| l.is_empty()), "{:?}", labels(&w));
}

/// Task 5 review: nmcli reads a BSSID-looking SSID as a BSSID, and h, he, hel, help as help.
#[test]
fn bssid_and_help_lookalikes_are_refused() {
    for ssid in ["00:00:5E:00:53:01", "00-00-5e-00-53-01", "0:0:5E:0:53:1", "h", "he", "hel", "help"] {
        assert!(refused(ssid.as_bytes()), "{ssid}");
    }
    for ssid in ["helpdesk", "Help", "00:00:5E:00:53", "00:00:5E:00:53:0G", "Maison-Test:01"] {
        assert!(!refused(ssid.as_bytes()), "{ssid}");
    }
}

/// Task 5 review: the selection is an SSID, not a position: it follows its network across a
/// reorder.
/// Task 7 (headless): the detail opens before its list is read (`Searching…`); the selection
/// waits for the list, then lands on the first network, never left on `More…`. An empty list
/// read, or the radio off: `More…` is the only pick.
#[test]
fn the_selection_waits_for_the_list_then_takes_the_first_network() {
    let mut unread = state();
    let d = device(&mut unread);
    (d.state, d.active, d.active_uuid, d.access_points) = (30, None, None, None);
    let mut w = open(unread);
    assert_eq!(w.pick, Pick::None, "searching: nothing picked yet");
    w.state = Some(state());
    w.fit(&config());
    assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the list read: its first network");
    let mut empty = state();
    device(&mut empty).access_points = Some(vec![]);
    device(&mut empty).active = None;
    assert_eq!(open(empty).pick, Pick::More, "no networks: More…");
    let mut off = state();
    off.enabled = false;
    assert_eq!(open(off).pick, Pick::More, "off: More…");
}

/// Fix round 1 (minor a): a Down while searching picks `More…` (the only pick then); the first
/// list read moves it to the first network, once: `More…` chosen after the list stays.
#[test]
fn more_chosen_while_searching_gives_way_to_the_first_network() {
    let mut unread = state();
    let d = device(&mut unread);
    (d.state, d.active, d.active_uuid, d.access_points) = (30, None, None, None);
    let mut w = open(unread);
    let t = Instant::now();
    w.key(&config(), Key::Down, t);
    assert_eq!(w.pick, Pick::More, "searching: More… the only pick");
    w.state = Some(state());
    w.fit(&config());
    assert_eq!(w.pick, Pick::Network(b"Maison-Test".to_vec()), "the list read: its first network");
    w.key(&config(), Key::End, t);
    w.fit(&config());
    assert_eq!(w.pick, Pick::More, "chosen once the list is read: kept");
}

#[test]
fn the_pick_follows_its_ssid_across_a_reorder() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    let mut s = state();
    for a in device(&mut s).access_points.as_mut().expect("listed") {
        if a.ssid == b"Cafe-Libre" {
            a.strength = 75;
        }
    }
    w.set_state(Some(s), t);
    w.fit(&c);
    assert_eq!(labels(&w)[1], "Cafe-Libre", "moved up");
    assert_eq!(w.pick, Pick::Network(b"Cafe-Libre".to_vec()));
    let nets = w.networks();
    assert!(w.is_selected(Line::Network(1), &nets) && !w.is_selected(Line::Network(3), &nets));
    assert_eq!(w.key(&c, Key::Enter, t), run(&["nmcli", "device", "wifi", "connect", "Cafe-Libre"], "wifi connect (open)"));
}

/// Fix round 1 (I2): an attempt begun before the saved list was read whole (a failed listing,
/// more than MAX_SAVED profiles: the reader sends None) knows no profile, so it deletes none,
/// even one of its SSID that shows later.
#[test]
fn an_attempt_begun_without_the_whole_saved_list_deletes_nothing() {
    let (c, t) = (config(), Instant::now());
    let mut s = state();
    device(&mut s).saved = None;
    let mut w = open(s);
    pick(&mut w, "Cafe-Libre");
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }));
    assert_eq!(w.attempt.as_ref().map(|a| a.known.clone()), Some(None));
    let mut listed = state();
    device(&mut listed).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
    w.set_state(Some(listed), t);
    device_at(&mut w, 50, Some("Cafe-Libre"), t);
    assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, Some("the activation failed")), "an older profile, never deleted");
}

/// Fix round 1 (minor 1): a saved network's `connection up` never deletes, even a profile of its
/// SSID that appeared meanwhile.
#[test]
fn a_connection_up_attempt_never_deletes() {
    let (c, t) = (config(), Instant::now());
    let mut s = state();
    device(&mut s).saved.as_mut().expect("saved").push(SavedWifi { uuid: CAFE.into(), ssid: b"Cafe-Libre".to_vec() });
    let mut w = open(s);
    pick(&mut w, "Cafe-Libre");
    assert!(matches!(w.key(&c, Key::Enter, t), QuickAction::Run { .. }));
    saved_added(&mut w, NEW, "Cafe-Libre", t);
    device_at(&mut w, 50, Some("Cafe-Libre"), t);
    assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, Some("the activation failed")));
}

/// Fix round 1 (minor 2): an activation on another network (an autoconnect) is not ours; an
/// activated device whose access point could not be read is no word yet; another network taking
/// the device once ours was under way is a failure.
#[test]
fn only_its_own_network_counts_for_an_attempt() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    assert_eq!(device_at(&mut w, 50, Some("Bureau-Test"), t), (QuickAction::Redraw, None), "an autoconnect elsewhere");
    assert_eq!(device_at(&mut w, 30, None, t), (QuickAction::Redraw, None), "ours never began: still waiting");
    assert!(w.attempt.as_ref().is_some_and(|a| !a.activating));
    assert_eq!(device_at(&mut w, 60, None, t), (QuickAction::Redraw, None), "unread access point: not ours yet");
    device_at(&mut w, 60, Some("Cafe-Libre"), t);
    assert_eq!(device_at(&mut w, ACTIVATED, None, t), (QuickAction::Redraw, None), "activated, its access point unread");
    assert!(w.attempt.is_some());
    assert_eq!(device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t), (QuickAction::Redraw, None));
    assert!(w.attempt.is_none(), "connected");
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    device_at(&mut w, 70, Some("Cafe-Libre"), t);
    assert_eq!(device_at(&mut w, 50, Some("Bureau-Test"), t), (QuickAction::Redraw, Some("the activation failed")), "another took over");
}

/// Fix round 2: activated on the profile the attempt made counts even with its access point
/// unread; and a timeout while the device was last seen activated deletes nothing.
#[test]
fn an_activated_device_is_never_cleaned_up() {
    let (c, t) = (config(), Instant::now());
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    saved_added(&mut w, NEW, "Cafe-Libre", t);
    let mut s = w.state.clone().expect("a state");
    let d = device(&mut s);
    (d.state, d.active, d.active_uuid) = (ACTIVATED, None, Some(NEW.into()));
    assert_eq!(w.set_state(Some(s), t), (QuickAction::Redraw, None));
    assert!(w.attempt.is_none(), "activated on its new profile: connected");
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    saved_added(&mut w, NEW, "Cafe-Libre", t);
    let mut s = w.state.clone().expect("a state");
    let d = device(&mut s);
    (d.state, d.active, d.active_uuid) = (ACTIVATED, None, None);
    w.set_state(Some(s), t);
    assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![], Some("no answer in 20 s"), true), "last seen activated: nothing deleted");
    let mut w = open(state());
    pick(&mut w, "Cafe-Libre");
    w.key(&c, Key::Enter, t);
    saved_added(&mut w, NEW, "Cafe-Libre", t);
    let mut s = w.state.clone().expect("a state");
    device(&mut s).active = None;
    w.set_state(Some(s), t);
    assert_eq!(w.due(t + CONNECT_TIMEOUT).0, vec![delete(NEW)], "activated on another profile's uuid (the old one): a failure, cleaned");
}

/// Fix round 3 (controller ruling): a slow activation never loses its profile. Still activating
/// on its network at 20 s: the deadline moves to 60 s from the start; still activating then: it
/// ends, nothing deleted; activated meanwhile: connected. The follower keeps the same deadline.
#[test]
fn a_slow_activation_keeps_its_profile() {
    let (c, t) = (config(), Instant::now());
    let slow = |active: Option<&str>| {
        let mut w = open(state());
        pick(&mut w, "Cafe-Libre");
        w.key(&c, Key::Enter, t);
        saved_added(&mut w, NEW, "Cafe-Libre", t);
        device_at(&mut w, 70, active, t);
        w
    };
    let mut w = slow(Some("Cafe-Libre"));
    assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![], None, false), "20 s, still activating: no delete");
    assert_eq!(w.deadline(), Some(t + ACTIVATION_BOUND));
    assert!(row_of(&w, "Cafe-Libre").text().trim_end().ends_with(" …"));
    assert_eq!(w.due(t + ACTIVATION_BOUND), (vec![], Some("still activating after 60 s"), true), "60 s: ends, its profile kept");
    assert!(w.attempt.is_none());
    let mut w = slow(None);
    assert_eq!(w.due(t + CONNECT_TIMEOUT), (vec![], None, false), "its access point unread: waits too");
    let mut w = slow(Some("Cafe-Libre"));
    w.due(t + CONNECT_TIMEOUT);
    assert_eq!(device_at(&mut w, ACTIVATED, Some("Cafe-Libre"), t + Duration::from_secs(30)), (QuickAction::Redraw, None));
    assert!(w.attempt.is_none() && w.failed.is_none(), "connected at 30 s, nothing deleted");
    let mut w = slow(Some("Bureau-Test"));
    assert_eq!(w.due(t + CONNECT_TIMEOUT).0, vec![delete(NEW)], "activating another network: a failure, cleaned");
    let mut w = slow(Some("Cafe-Libre"));
    let mut f = WifiFollow::leaving(&mut w).expect("followed");
    assert_eq!(f.due(t + CONNECT_TIMEOUT), Outcome::Waiting);
    assert_eq!(f.deadline(), t + ACTIVATION_BOUND, "the follower's timer moves too");
    assert_eq!(f.due(t + ACTIVATION_BOUND), Outcome::Failed { why: "still activating after 60 s", cleanup: None });
}
