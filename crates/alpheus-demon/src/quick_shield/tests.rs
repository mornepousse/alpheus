use super::*;
use crate::actions::Button;
use crate::grid::{Grid, text_width};
use crate::keys::Key;
use std::time::{Duration, Instant};

fn view(state: Option<bool>, can_list: bool, can_allow: bool) -> ShieldView {
    ShieldView { state, switching: false, failed_until: None, can_list, can_allow }
}

fn dev(id: u32, name: &str) -> UsbDevice {
    UsbDevice { id, vid_pid: "1234:5678".into(), name: name.into() }
}

fn drawn(p: &ShieldPanel, v: &ShieldView) -> Vec<String> {
    p.lines()
        .into_iter()
        .map(|l| {
            let mut g = Grid::empty(34);
            p.draw(&mut g, l, v);
            g.text().trim_end().to_string()
        })
        .collect()
}

fn listed(devices: Vec<UsbDevice>, v: &ShieldView) -> ShieldPanel {
    let mut p = ShieldPanel::default();
    p.open_detail(v);
    assert!(p.set_listing(v, Ok(devices)));
    p
}

#[test]
fn the_notes() {
    let on = view(Some(true), true, true);
    let mut p = ShieldPanel::default();
    p.open_detail(&on);
    assert_eq!(p.lines(), [Line::ShieldHeader, Line::Separator, Line::ShieldNote(NOTE_READING)]);
    assert!(p.set_listing(&on, Ok(vec![])));
    assert_eq!(p.lines()[2], Line::ShieldNote(NOTE_NONE));
    assert!(p.set_listing(&on, Err(())));
    assert_eq!(p.lines()[2], Line::ShieldNote(NOTE_FAILED));
    let off = view(Some(false), true, true);
    let mut p = ShieldPanel::default();
    assert_eq!(p.open_detail(&off), QuickAction::Redraw);
    assert_eq!(p.lines()[2], Line::ShieldNote(NOTE_OFF));
    assert!(!p.read_wanted, "off: nothing is run");
    let texts = drawn(&p, &off);
    assert_eq!(texts[0], " ‹ Shield");
    assert_eq!(texts[2], format!("   {NOTE_OFF}"));
}

#[test]
fn opening_asks_for_one_read_only_while_on_and_listable() {
    let mut p = ShieldPanel::default();
    assert_eq!(p.open_detail(&view(Some(true), false, true)), QuickAction::Nothing, "blocked = []: no detail");
    assert!(!p.open);
    p.open_detail(&view(Some(true), true, false));
    assert!(p.open && p.read_wanted);
}

#[test]
fn a_flag_edge_reevaluates_off_and_reading() {
    let mut p = ShieldPanel::default();
    p.open_detail(&view(Some(false), true, true));
    p.fit(Some(&view(Some(true), true, true)));
    assert_eq!(p.listing, Some(Listing::Reading));
    assert!(p.read_wanted, "the mode came on while the detail shows: one read");
    p.read_wanted = false;
    p.fit(Some(&view(Some(true), true, true)));
    assert!(!p.read_wanted, "no second ask for the same state");
    p.set_listing(&view(Some(true), true, true), Ok(vec![dev(1, "A")]));
    p.fit(Some(&view(Some(false), true, true)));
    assert_eq!(p.listing, Some(Listing::Off), "the mode went off: the list is dropped");
}

#[test]
fn a_listing_when_not_waiting_for_one_is_dropped() {
    let on = view(Some(true), true, true);
    let mut p = ShieldPanel::default();
    assert!(!p.set_listing(&on, Ok(vec![dev(1, "A")])), "no detail open");
    assert_eq!(p.listing, None);
    p.open_detail(&view(Some(false), true, true));
    assert!(!p.set_listing(&view(Some(false), true, true), Ok(vec![dev(1, "A")])), "off: nothing shown");
    assert_eq!(p.listing, Some(Listing::Off));
}

#[test]
fn devices_are_not_selectable_without_allow() {
    let v = view(Some(true), true, false);
    let mut p = listed(vec![dev(7, "Test Stick"), dev(8, "Test Keyboard")], &v);
    assert!(!p.is_selected(Line::ShieldDevice(0), v.can_allow));
    assert_eq!(p.key(&v, Key::Down, Instant::now()), QuickAction::Nothing);
    assert_eq!(p.key(&v, Key::Enter, Instant::now()), QuickAction::Nothing, "no allow: Enter allows nothing");
    assert_eq!(p.press(&v, Line::ShieldDevice(0), Button::Left, Instant::now()), QuickAction::Nothing);
    let v = view(Some(true), true, true);
    assert!(p.is_selected(Line::ShieldDevice(0), v.can_allow));
    assert_eq!(p.key(&v, Key::Down, Instant::now()), QuickAction::Redraw);
    assert!(p.is_selected(Line::ShieldDevice(1), v.can_allow));
}

#[test]
fn enter_on_a_device_allows_it_once() {
    let v = view(Some(true), true, true);
    let mut p = listed(vec![dev(7, "Test Stick"), dev(8, "Test Keyboard")], &v);
    let t = Instant::now();
    p.key(&v, Key::Down, t);
    assert_eq!(p.key(&v, Key::Enter, t), QuickAction::Shield(ShieldAsk::Allow(8)));
    assert_eq!(p.allowing, Some(8));
    assert_eq!(p.key(&v, Key::Enter, t), QuickAction::Nothing, "one action at a time");
    assert!(drawn(&p, &v)[3].ends_with("…"), "{:?}", drawn(&p, &v));
    // A click is the same.
    let mut q = listed(vec![dev(7, "Test Stick")], &v);
    assert_eq!(q.press(&v, Line::ShieldDevice(0), Button::Left, t), QuickAction::Shield(ShieldAsk::Allow(7)));
    let mut r = listed(vec![dev(7, "Test Stick")], &v);
    assert_eq!(r.press(&v, Line::ShieldDevice(0), Button::Right, t), QuickAction::Nothing);
}

#[test]
fn keys_back_out_of_the_detail() {
    let v = view(Some(true), true, true);
    for k in [Key::Escape, Key::Left] {
        let mut p = listed(vec![dev(7, "A")], &v);
        assert_eq!(p.key(&v, k, Instant::now()), QuickAction::Redraw);
        assert!(!p.open, "{k:?}");
    }
    let mut p = listed(vec![dev(7, "A")], &v);
    assert_eq!(p.press(&v, Line::ShieldHeader, Button::Left, Instant::now()), QuickAction::Redraw);
    assert!(!p.open);
}

#[test]
fn a_long_name_keeps_the_tag_column() {
    let v = view(Some(true), true, true);
    let long = "A very long device name that cannot fit in the box";
    let p = listed(vec![dev(7, "Test Stick"), dev(8, long)], &v);
    let rows = drawn(&p, &v);
    for (row, name) in [(&rows[2], "Test Stick"), (&rows[3], "A very long")] {
        assert!(row.contains(name), "{row}");
        assert!(row.ends_with("1234:5678"), "{row}");
        assert_eq!(text_width(row), 33, "the tag ends one cell before the row's end: {row:?}");
    }
    assert!(rows[3].contains('…'), "the name is cut: {}", rows[3]);
}

#[test]
fn failed_shows_four_seconds_on_its_row_then_goes() {
    let v = view(Some(true), true, true);
    let mut p = listed(vec![dev(7, "Test Stick"), dev(8, "Other")], &v);
    let t = Instant::now();
    p.key(&v, Key::Enter, t);
    p.allow_done(7, true, t);
    assert_eq!(p.allowing, None);
    let rows = drawn(&p, &v);
    assert!(rows[2].ends_with("   failed"), "{:?}", rows[2]);
    assert!(rows[3].ends_with("1234:5678"), "only that row: {:?}", rows[3]);
    assert_eq!(p.deadline(), Some(t + crate::quick::FAILED_SHOWN));
    assert!(!p.due(t + Duration::from_secs(3)));
    assert!(p.due(t + crate::quick::FAILED_SHOWN), "the tag goes with a redraw");
    assert_eq!(p.deadline(), None);
    assert!(drawn(&p, &v)[2].ends_with("1234:5678"));
    // A success only clears the action.
    p.key(&v, Key::Enter, t);
    p.allow_done(7, false, t);
    assert_eq!((p.allowing, p.failed), (None, None));
}

#[test]
fn at_most_32_rows() {
    let v = view(Some(true), true, true);
    let many: Vec<UsbDevice> = (0..40).map(|i| dev(i, "Dev")).collect();
    let mut p = listed(many, &v);
    assert_eq!(p.lines().len(), 2 + ROWS_MAX);
    for _ in 0..50 {
        p.key(&v, Key::Down, Instant::now());
    }
    assert_eq!(p.pick, ROWS_MAX - 1, "the selection stays on a drawn row");
}

#[test]
fn the_rows_failed_tag_has_its_own_deadline() {
    let t = Instant::now();
    let mut p = ShieldPanel::default();
    let mut v = view(Some(true), true, true);
    v.failed_until = Some(t + Duration::from_secs(4));
    p.fit(Some(&v));
    assert_eq!(p.deadline(), Some(t + Duration::from_secs(4)));
    assert!(p.due(t + Duration::from_secs(4)));
    assert!(p.row_failed_due, "the wiring clears the model's `failed` and republishes");
    assert_eq!(p.deadline(), None);
}

#[test]
fn an_unknown_state_has_its_own_note() {
    let unknown = view(None, true, true);
    let mut p = ShieldPanel::default();
    assert_eq!(p.open_detail(&unknown), QuickAction::Redraw);
    assert_eq!(p.lines()[2], Line::ShieldNote(NOTE_UNKNOWN));
    assert_ne!(NOTE_UNKNOWN, NOTE_OFF);
    assert!(!p.read_wanted, "unknown: nothing is run");
    // The state turning off, then known again, follows the view.
    p.fit(Some(&view(Some(false), true, true)));
    assert_eq!(p.lines()[2], Line::ShieldNote(NOTE_OFF));
    p.fit(Some(&unknown));
    assert_eq!(p.lines()[2], Line::ShieldNote(NOTE_UNKNOWN));
}
