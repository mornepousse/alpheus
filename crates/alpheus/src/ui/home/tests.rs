//! Tests of the Home dashboard (`ui/home.rs`): the layout at several sizes, the clickable regions,
//! the hints, and the whole screen at 80 by 24 as a snapshot.

use super::*;
use crate::app::action::{Action, Tab};
use crate::app::testing::{app, ready_app};
use crate::app::worker::{Op, Request};
use crate::app::{Item, SliderId};
use crate::backends::{Connection, NodeRef};
use crate::theme::Shape;
use crate::ui::Chrome;
use crate::ui::shape::darker;
use crate::ui::testing::{T, buffer_at, draw, draw_at};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::Instant;

/// The text of `r` on the drawn rows, one line per row.
fn inside(rows: &[String], r: Rect) -> String {
    rows[usize::from(r.y)..usize::from(r.bottom())]
        .iter()
        .map(|row| row.chars().skip(usize::from(r.x)).take(usize::from(r.width)).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(a: &mut App, hits: &HitMap, x: u16, y: u16) {
    let ev = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y, modifiers: KeyModifiers::NONE };
    a.on_mouse(ev, hits, Instant::now());
}

fn item(hits: &HitMap, i: usize) -> Rect {
    hits.find(Target::Item(i)).unwrap_or_else(|| panic!("Item({i}) missing"))
}

#[test]
fn home_item_indices_match_the_app() {
    let items = ready_app().items();
    assert_eq!(items.len(), 7);
    assert_eq!(items[VOLUME], Item::Slider(SliderId::Volume));
    assert_eq!(items[BRIGHTNESS], Item::Slider(SliderId::Brightness));
    assert_eq!(items[WIFI], Item::Toggle(ToggleId::Wifi));
    assert_eq!(items[BLUETOOTH], Item::Toggle(ToggleId::Bluetooth));
    assert_eq!(items[DND], Item::Toggle(ToggleId::Dnd));
    assert_eq!(items[KEEP_AWAKE], Item::Toggle(ToggleId::KeepAwake));
    assert_eq!(items[PROFILE], Item::Profile);
}

#[test]
fn the_greeting_changes_at_5_12_and_18_hours() {
    let (m, a, e) = ("Good morning", "Good afternoon", "Good evening");
    for (h, want) in [(0, e), (4, e), (5, m), (11, m), (12, a), (17, a), (18, e), (23, e)] {
        assert_eq!(greeting(h), want, "{h} h");
    }
}

#[test]
fn the_summary_counts_connections_and_devices_with_their_plurals() {
    let mut a = ready_app();
    assert_eq!(summary(&a), "2 connections · 1 device · Balanced");
    if let Loadable::Ready(n) = &mut a.data.network {
        n.active.truncate(1);
    }
    if let Loadable::Ready(b) = &mut a.data.bluetooth {
        let d = b.connected[0].clone();
        b.connected.push(d);
    }
    assert_eq!(summary(&a), "1 connection · 2 devices · Balanced");
    a.data.profiles = Loadable::Loading;
    assert_eq!(summary(&a), "1 connection · 2 devices", "no profile part while the profiles are not ready");
}

#[test]
fn the_dashboard_needs_a_64x20_body() {
    let at = |w, h| fits(Rect::new(3, 4, w, h));
    assert!(!at(63, 20) && !at(64, 19) && !at(63, 19));
    assert!(at(64, 20) && at(67, 20) && at(100, 30));
}

#[test]
fn the_dashboard_at_120_by_36_matches_its_snapshot() {
    let a = ready_app();
    let rows = draw_at(&a, 120, 36, Some(T)).0;
    let all = rows.join("\n");
    for s in ["Sunday", "15 November", "Good afternoon", summary(&a).as_str()] {
        assert!(all.contains(s), "{s:?} missing:\n{all}");
    }
    insta::assert_snapshot!("home_dashboard", all);
}

/// An 87 by 26 window: the rail and the whole dashboard, nothing cut.
#[test]
fn an_87_by_26_window_shows_the_whole_dashboard() {
    let a = ready_app();
    let (rows, hits) = draw_at(&a, 87, 26, Some(T));
    let all = rows.join("\n");
    assert!(rows[1].contains("◆ alpheus"), "the rail");
    // The clock: "14:05" in block digits, then the date and the greeting.
    assert!(rows.iter().any(|r| r.contains("▄█  █ █")), "the clock's digits:\n{all}");
    for s in ["Sunday", "15 November", "Good afternoon", "2 connections · 1 device · Balanced"] {
        assert!(all.contains(s), "{s:?} missing:\n{all}");
    }
    // The three cards and the four tiles, each with its hit region.
    for (i, title) in [(VOLUME, "Volume"), (BRIGHTNESS, "Brightness"), (PROFILE, "Energy")] {
        assert!(inside(&rows, item(&hits, i)).lines().next().unwrap().contains(title), "{title}:\n{all}");
    }
    for (i, label) in [(WIFI, "Wi-Fi"), (BLUETOOTH, "Bluetooth"), (DND, "Do not disturb"), (KEEP_AWAKE, "Keep awake")] {
        assert!(inside(&rows, item(&hits, i)).contains(label), "{label}:\n{all}");
    }
    assert!(inside(&rows, item(&hits, VOLUME)).contains('%'), "the volume's value");
    assert!(hits.find(Target::Slider { item: VOLUME }).is_some() && hits.find(Target::Slider { item: BRIGHTNESS }).is_some());
    for (j, name) in ["Performance", "Balanced", "Power saver"].iter().enumerate() {
        let r = hits.find(Target::ProfileOption(j)).unwrap();
        assert!(inside(&rows, r).contains(name), "{name}");
    }
    // The lists: at least the first connection, whole.
    assert!(all.contains("MON_WIFI_1") && all.contains("Connections") && all.contains("Devices"), "{all}");
    assert!(!all.contains('…'), "something is cut:\n{all}");
    // Every region inside the body, below the header.
    for i in 0..7 {
        let r = item(&hits, i);
        assert!(r.x >= RAIL_BODY_X && r.right() <= 85 && r.y >= 5 && r.bottom() <= 25, "Item({i}) {r:?}");
    }
    insta::assert_snapshot!("home_dashboard_87x26", all);
}

/// Where the rail's body starts (`RAIL_W` + 2).
const RAIL_BODY_X: u16 = crate::ui::rail::RAIL_W + 2;

#[test]
fn dashboard_hits_reach_today_s_actions() {
    let mut a = ready_app();
    let (_, hits) = draw_at(&a, 120, 36, Some(T));
    let r = item(&hits, DND);
    press(&mut a, &hits, r.x + r.width / 2, r.y + r.height / 2);
    assert_eq!(a.focus, DND);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDnd(true))]);
    let bar = hits.find(Target::Slider { item: VOLUME }).unwrap();
    assert_eq!(bar.height, 2, "a two-row gauge");
    press(&mut a, &hits, bar.x, bar.y + 1);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetVolume(NodeRef::DefaultSink, 0))]);
    // Performance is not the active profile: a click chooses it, as on the classic row.
    let (_, hits) = draw_at(&a, 120, 36, Some(T));
    let p = hits.find(Target::ProfileOption(0)).unwrap();
    press(&mut a, &hits, p.x + p.width / 2, p.y);
    assert_eq!(a.data.profiles.ready().unwrap().active, "performance");
    assert_eq!(a.focus, PROFILE);
    assert_eq!(a.take_requests(), vec![Request::Do(Op::SetProfile("performance".into()))]);
}

/// Review Focus 1: below the rail (or a body under 64×20) Home keeps today's rows; each frame's hit
/// regions are what it drew.
#[test]
fn small_bodies_keep_the_rows() {
    let a = ready_app();
    let rows_at_3 = |rows: &[String]| rows.iter().any(|r| r.chars().skip(3).take(6).collect::<String>() == "Volume");
    for (w, h) in [(83, 26), (84, 25)] {
        let (rows, hits) = draw(&a, w, h);
        let all = rows.join("\n");
        assert!(rows_at_3(&rows) && !all.contains("Good"), "{w}x{h}:\n{all}");
        let r = item(&hits, DND);
        assert_eq!(r.height, 1, "{w}x{h}: a row");
        assert!(inside(&rows, r).contains("Do not disturb"), "{w}x{h}");
    }
    let (rows, hits) = draw(&a, 84, 26);
    let all = rows.join("\n");
    assert!(all.contains("Good afternoon") && !rows_at_3(&rows), "84x26 (a 64x20 body):\n{all}");
    let r = item(&hits, DND);
    assert!(r.height > 1 && r.x >= RAIL_BODY_X && r.right() <= 84 && r.bottom() <= 25, "{r:?}");
    assert!(inside(&rows, r).contains("Do not"), "{}", inside(&rows, r));
    // The body alone decides under the rail: 63×20 and 64×19 keep the rows, 64×20 is the dashboard.
    for (w, h, dashboard) in [(63, 20, false), (64, 19, false), (64, 20, true)] {
        let area = Rect::new(0, 0, 70, 22);
        let mut buf = Buffer::empty(area);
        let mut hits = HitMap::default();
        let body = Rect::new(2, 1, w, h);
        crate::ui::tabs::render(&mut buf, body, &a, &mut hits, Chrome::Rail, Some(T));
        let rows: Vec<String> = (0..22).map(|y| (0..70).map(|x| buf[(x, y)].symbol()).collect()).collect();
        let all = rows.join("\n");
        assert_eq!(all.contains("Good afternoon"), dashboard, "{w}x{h}:\n{all}");
        assert_eq!(item(&hits, DND).height > 1, dashboard, "{w}x{h}");
    }
    // A click after a resize lands on the new layout.
    for (w, h) in [(84, 26), (83, 26)] {
        let mut a = ready_app();
        let (_, hits) = draw(&a, w, h);
        let r = item(&hits, DND);
        press(&mut a, &hits, r.x + r.width / 2, r.y + r.height / 2);
        assert_eq!(a.take_requests(), vec![Request::Do(Op::SetDnd(true))], "{w}x{h}");
    }
}

/// A tall card leaves one row between its title and the big digits (below 7 inside rows there is no room).
#[test]
fn a_tall_card_leaves_a_row_above_its_digits() {
    let a = ready_app();
    for (w, h, margin) in [(120, 36, true), (87, 26, false)] {
        let (rows, hits) = draw_at(&a, w, h, Some(T));
        let r = item(&hits, VOLUME);
        let inner = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
        assert_eq!(inner.height >= 7, margin, "{w}x{h}: inner height {}", inner.height);
        let digits = |y: u16| inside(&rows, Rect::new(inner.x, y, inner.width, 1)).chars().any(|c| matches!(c, '█' | '▀' | '▄'));
        assert_eq!(digits(inner.y), !margin, "{w}x{h}: the row under the title:\n{}", rows.join("\n"));
        assert!(digits(inner.y + 1), "{w}x{h}");
        // The gauge and the value still fit whatever the height.
        assert!(inside(&rows, inner).contains('%'), "{w}x{h}");
    }
}

#[test]
fn the_focus_frame_follows_the_seven_items() {
    let mut a = ready_app();
    let th = a.theme;
    let hues = [th.blue, th.yellow, th.cyan, th.purple, th.red, th.orange, th.green];
    for (w, h) in [(120, 36), (87, 26)] {
        for (f, hue) in hues.iter().enumerate() {
            a.focus = f;
            let (_, hits) = draw_at(&a, w, h, Some(T));
            let b = buffer_at(&a, w, h, Some(T));
            let r = item(&hits, f);
            assert_eq!(b[(r.x, r.y)].fg, *hue, "{w}x{h}: focus {f}");
            let other = if f == VOLUME { BRIGHTNESS } else { VOLUME };
            let o = item(&hits, other);
            assert_eq!(b[(o.x, o.y)].fg, th.raised, "{w}x{h}: card {other} at rest while {f} is focused");
        }
    }
}

/// Review Focus 2: data loading or failed shows inside the cards; the clock still draws.
#[test]
fn cards_show_an_ellipsis_while_loading_and_the_error_when_failed() {
    let a = app();
    for (w, h) in [(120, 36), (87, 26)] {
        let (rows, hits) = draw_at(&a, w, h, Some(T));
        assert!(inside(&rows, item(&hits, VOLUME)).contains('…'), "{w}x{h}");
        assert!(rows.join("\n").contains("Sunday") && rows.iter().any(|r| r.contains('█')), "{w}x{h}: the clock");
    }
    let mut a = ready_app();
    a.data.brightness = Loadable::Failed("no backlight".into());
    a.data.profiles = Loadable::Failed("no daemon".into());
    for (w, h) in [(120, 36), (87, 26)] {
        let (rows, hits) = draw_at(&a, w, h, Some(T));
        assert!(inside(&rows, item(&hits, BRIGHTNESS)).contains("✗ no backlight"), "{w}x{h}");
        assert!(inside(&rows, item(&hits, PROFILE)).contains('✗'), "{w}x{h}");
    }
}

/// Review Focus 3: a 40-character SSID and 8 connections stay inside their card and tile.
#[test]
fn long_names_are_cut_inside_their_cards_and_tiles() {
    let mut a = ready_app();
    let ssid = "A very long network name for this test 1";
    assert_eq!(ssid.chars().count(), 40);
    if let Loadable::Ready(n) = &mut a.data.network {
        n.active = (0..8)
            .map(|k| {
                let name = if k == 0 { ssid.to_string() } else { format!("Connection number {k} with a long name") };
                let kind = if k == 0 { "Wi-Fi" } else { "WireGuard" };
                Connection { name, kind: kind.into(), device: format!("dev{k}") }
            })
            .collect();
    }
    for (w, h) in [(120, 36), (87, 26)] {
        let (rows, hits) = draw_at(&a, w, h, Some(T));
        let b = buffer_at(&a, w, h, Some(T));
        // The Connections card: its corner left of the title, its right border on every row.
        let y0 = rows.iter().position(|r| r.contains("Connections")).unwrap() as u16;
        let tx = rows[usize::from(y0)].chars().position(|c| c == '󰛳').unwrap() as u16;
        let left = (0..tx).rev().find(|&x| b[(x, y0)].symbol() == "┌").unwrap();
        let right = (tx..w).find(|&x| b[(x, y0)].symbol() == "┐").unwrap();
        let bottom = (y0 + 1..h).find(|&y| b[(left, y)].symbol() == "└").unwrap();
        for y in y0 + 1..bottom {
            assert_eq!(b[(right, y)].symbol(), "│", "{w}x{h}: row {y} of the Connections card");
        }
        assert_eq!(b[(right, bottom)].symbol(), "┘");
        // The Wi-Fi tile's subtitle is cut with "…" inside the tile.
        let r = item(&hits, WIFI);
        let text = inside(&rows, r);
        let sub = text.lines().find(|l| l.contains("A very")).unwrap_or_else(|| panic!("{w}x{h}:\n{text}"));
        assert!(sub.trim_end().ends_with('…'), "{w}x{h}: {sub:?}");
        // The 40-character SSID row of the Connections card is cut with "…" before the border.
        let row = (y0 + 1..bottom)
            .map(|y| (left + 1..right).map(|x| b[(x, y)].symbol()).collect::<String>())
            .find(|t| t.contains("A very"))
            .unwrap_or_else(|| panic!("{w}x{h}: no SSID row in the Connections card:\n{}", rows.join("\n")));
        // Whole where the card is wide (120), cut at 87×26.
        if w == 87 {
            assert!(row.trim_end().ends_with('…'), "{w}x{h}: {row:?}");
        } else {
            assert!(row.contains(ssid), "{w}x{h}: {row:?}");
        }
    }
}

#[test]
fn tiles_and_cards_take_the_shape() {
    let mut a = ready_app();
    a.focus = PROFILE;
    let cell = |a: &App, x: u16, y: u16| buffer_at(a, 120, 36, Some(T))[(x, y)].clone();
    let rects = |a: &App| {
        let hits = draw_at(a, 120, 36, Some(T)).1;
        (item(&hits, WIFI), item(&hits, VOLUME), item(&hits, DND))
    };
    a.theme.shape = Shape::Round;
    let (wifi, _, _) = rects(&a);
    assert_eq!(cell(&a, wifi.x, wifi.y).symbol(), "▗");
    a.theme.shape = Shape::Float;
    let (wifi, vol, _) = rects(&a);
    let shadow = cell(&a, wifi.right(), wifi.bottom());
    assert_eq!((shadow.symbol(), shadow.fg), ("▀", darker(a.theme.bg)), "the shadow right of and under the tile");
    assert_eq!(cell(&a, vol.x, vol.y).symbol(), "╔");
    a.theme.shape = Shape::Line;
    let (_, vol, dnd) = rects(&a);
    for y in dnd.y..dnd.bottom() {
        assert_eq!(cell(&a, dnd.x, y).symbol(), "▎", "an off tile's left bar, row {y}");
    }
    assert_eq!(cell(&a, vol.right() - 1, vol.y).symbol(), "─", "the rule runs to the card's right end");
    assert_ne!(cell(&a, vol.x, vol.y + 1).symbol(), "│", "a top rule only");
    assert_ne!(cell(&a, vol.right() - 1, vol.y + 1).symbol(), "│", "a top rule only");
    a.theme.shape = Shape::Flat;
    let (_, vol, _) = rects(&a);
    assert_eq!(cell(&a, vol.x, vol.y).symbol(), "┌");
}

/// An 87 by 26 window, every shape, each tile focused in turn: the label and the state row both stay.
#[test]
fn a_focused_tile_keeps_its_state_row_at_87x26() {
    for shape in [Shape::Flat, Shape::Round, Shape::Arrow, Shape::Line, Shape::Float] {
        for (i, label, state) in [
            (WIFI, "Wi-Fi", "MON_WIFI_1"),
            (BLUETOOTH, "Bluetooth", "CasqueBT"),
            (DND, "Do not disturb", "off"),
            (KEEP_AWAKE, "Keep awake", "off"),
        ] {
            let mut a = ready_app();
            a.theme.shape = shape;
            a.focus = i;
            let (rows, hits) = draw_at(&a, 87, 26, Some(T));
            let text = inside(&rows, item(&hits, i));
            // The name may lose its tail to the frame's two columns (a floating tile has one less); the state row stays.
            let name: String = label.chars().take(6).collect();
            assert!(text.contains(&name) && text.contains(state), "{shape:?} {label} focused:\n{text}");
        }
    }
}

#[test]
fn the_dashboards_hints_follow_the_keymap() {
    let mut a = ready_app();
    a.focus = VOLUME;
    let km = &a.keymap;
    let want = format!("{}/{} adjust · click the gauge", km.label(Action::Decrease), km.label(Action::Increase));
    let (rows, hits) = draw_at(&a, 120, 36, Some(T));
    let r = item(&hits, VOLUME);
    let last = inside(&rows, Rect::new(r.x + 1, r.bottom() - 2, r.width - 2, 1));
    assert_eq!(last.trim(), want);
    // An off tile tells which key turns it on.
    let dnd = inside(&rows, item(&hits, DND));
    assert!(dnd.contains(&format!("{} to turn on", km.label(Action::Toggle))), "{dnd}");
    a.focus = BRIGHTNESS;
    let (rows, hits) = draw_at(&a, 120, 36, Some(T));
    assert!(!inside(&rows, item(&hits, VOLUME)).contains("adjust"), "only the focused card shows it");
}

#[test]
fn without_a_time_the_summary_stands_alone() {
    let a = ready_app();
    let (rows, hits) = draw_at(&a, 120, 36, None);
    let all = rows.join("\n");
    for s in ["Sunday", "November", "Good"] {
        assert!(!all.contains(s), "{s}:\n{all}");
    }
    assert!(all.contains(&summary(&a)), "the summary still shows");
    let hero = Rect::new(RAIL_BODY_X, 5, 100, 4);
    assert!(!inside(&rows, hero).contains('▀'), "no digits:\n{}", inside(&rows, hero));
    assert!(hits.find(Target::Item(VOLUME)).is_some());
}

#[test]
fn the_home_tab_at_80_by_24_matches_its_snapshot() {
    let mut a = ready_app();
    a.go_tab(Tab::Home, Instant::now());
    insta::assert_snapshot!("home_tab", draw(&a, 80, 24).0.join("\n"));
}

#[test]
fn a_time_field_that_does_not_fit_a_byte_is_refused() {
    assert_eq!(small(14), Some(14));
    assert_eq!(small(300), None);
    assert_eq!(small(-1), None);
}

/// The column where the day's name starts when the clock strip is drawn `width` columns wide.
fn day_column(a: &App, width: u16) -> usize {
    let r = Rect::new(0, 0, width, 4);
    let mut buf = Buffer::empty(r);
    clock(&mut buf, r, a, Some(T));
    let rows: Vec<String> = (0..r.height).map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect()).collect();
    let in_chars = |row: &String| row.find("Sunday").map(|byte| row[..byte].chars().count());
    rows.iter().find_map(in_chars).expect("the day is drawn")
}

#[test]
fn a_clock_strip_that_just_fits_gets_the_two_cell_digits() {
    let a = ready_app();
    // With the test's time and summary the large digits need the date, the greeting and the summary
    // to end exactly at the strip's right edge, 83 columns.
    let (just_enough, one_short) = (day_column(&a, 83), day_column(&a, 82));
    assert!(just_enough > one_short, "two-cell digits push the date right: {just_enough} against {one_short}");
}
