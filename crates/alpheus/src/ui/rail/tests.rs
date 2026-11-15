use super::*;
use crate::app::action::Action;
use crate::app::appearance::tests::{open, to_row};
use crate::app::keymap::Keymap;
use crate::app::testing::{app, node, ready_app};
use crate::config::KeysConfig;
use crate::ui::testing::draw;
use crate::ui::{Chrome, chrome, render as ui_render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::collections::BTreeMap;
use std::time::Instant;

/// The cells `ui::render` leaves on a w×h terminal.
fn cells(a: &App, w: u16, h: u16) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| {
        ui_render(f, a);
    })
    .unwrap();
    t.backend().buffer().clone()
}

fn entry_row(i: usize) -> u16 {
    5 + 2 * i as u16
}

#[test]
fn the_rail_needs_84_columns_and_26_rows() {
    let a = ready_app();
    let area = |w, h| Rect::new(0, 0, w, h);
    assert_eq!(chrome(area(83, 26)), Chrome::Classic);
    assert_eq!(chrome(area(84, 25)), Chrome::Classic);
    assert_eq!(chrome(area(84, 26)), Chrome::Rail);
    for (w, h) in [(83, 40), (120, 25)] {
        let rows = draw(&a, w, h).0;
        assert!(rows[0].contains("1 Home  2 Sound"), "{w}x{h}: {:?}", rows[0]);
    }
    let rows = draw(&a, 84, 26).0;
    assert!(!rows[0].contains("1 Home  2 Sound"), "{:?}", rows[0]);
    assert!(rows[1].contains("◆ alpheus"), "{:?}", rows[1]);
}

#[test]
fn mae_window_87x26_shows_the_rail() {
    let a = ready_app();
    let (rows, hits) = draw(&a, 87, 26);
    assert!(rows[1].contains("◆ alpheus") && rows[2].contains("control center"), "{:?}", &rows[..3]);
    for (i, t) in Tab::ALL.iter().enumerate() {
        assert_eq!(hits.find(Target::Tab(*t)), Some(Rect::new(1, entry_row(i), RAIL_W - 2, 1)), "{t:?}");
    }
    assert!(rows[25].contains("F1") && rows[25].contains("Help"), "{:?}", rows[25]);
    // The body: 87 - 16 - 4 = 67 columns, 26 - 2 - 4 = 20 rows, below the header.
    let body = rail_body(&a, 87, 26);
    assert_eq!(body, Rect::new(RAIL_W + 2, 5, 67, 20));
}

fn rail_body(a: &App, w: u16, h: u16) -> Rect {
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    render(&mut buf, Rect::new(0, 0, w, h), a, &mut HitMap::default())
}

#[test]
fn rail_entries_are_tab_targets() {
    let mut a = ready_app();
    let (_, hits) = draw(&a, 120, 36);
    for (i, t) in Tab::ALL.iter().enumerate() {
        assert_eq!(hits.find(Target::Tab(*t)), Some(Rect::new(1, entry_row(i), RAIL_W - 2, 1)), "{t:?}");
    }
    let r = hits.find(Target::Tab(Tab::Bluetooth)).unwrap();
    let ev =
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + r.width / 2, row: r.y, modifiers: KeyModifiers::NONE };
    a.on_mouse(ev, &hits, Instant::now());
    assert_eq!(a.tab, Tab::Bluetooth);
}

#[test]
fn the_active_entry_takes_its_tab_hue() {
    let mut a = ready_app();
    let th = a.theme;
    for (i, t) in Tab::ALL.iter().enumerate() {
        a.tab = *t;
        let b = cells(&a, 120, 36);
        let title = &b[(4, entry_row(i))];
        assert_eq!((title.bg, title.fg), (tab_hue(&th, *t), th.on_accent), "{t:?}'s title");
        for (j, u) in Tab::ALL.iter().enumerate().filter(|(j, _)| *j != i) {
            let ic = &b[(2, entry_row(j))];
            assert_eq!((ic.fg, ic.bg), (tab_hue(&th, *u), th.surface), "{u:?}'s icon while {t:?} is active");
        }
    }
}

#[test]
fn the_active_entry_under_the_line_style_has_no_fill() {
    let mut a = ready_app();
    a.theme.shape = Shape::Line;
    a.tab = Tab::Sound;
    let th = a.theme;
    let b = cells(&a, 120, 36);
    let title = &b[(4, entry_row(1))];
    assert_eq!((title.fg, title.bg), (th.blue, th.surface));
    assert!(title.modifier.contains(Modifier::UNDERLINED));
    assert_eq!(b[(2, entry_row(1))].fg, th.blue);
    assert_eq!(b[(1, entry_row(1))].symbol(), "▎");
}

#[test]
fn tab_hues() {
    let th = Theme::default();
    let want = [
        (Tab::Home, th.accent),
        (Tab::Sound, th.blue),
        (Tab::Network, th.cyan),
        (Tab::Bluetooth, th.purple),
        (Tab::Power, th.red),
        (Tab::Displays, th.green),
        (Tab::Appearance, th.orange),
    ];
    assert_eq!(want.len(), Tab::ALL.len());
    for (t, c) in want {
        assert_eq!(tab_hue(&th, t), c, "{t:?}");
    }
}

#[test]
fn subtitles() {
    let mut a = ready_app();
    let sub = |a: &mut App, t| {
        a.tab = t;
        subtitle(a)
    };
    assert_eq!(sub(&mut a, Tab::Home), "The everyday switches");
    assert_eq!(sub(&mut a, Tab::Sound), "2 outputs · 1 input · playing on CasqueBT");
    assert_eq!(sub(&mut a, Tab::Network), "Wi-Fi on · 2 active connections");
    assert_eq!(sub(&mut a, Tab::Bluetooth), "On · 1 connected");
    assert_eq!(sub(&mut a, Tab::Power), "Lock, leave or restart the session");
    let screens = a.data.displays.ready().unwrap().len();
    assert_eq!(sub(&mut a, Tab::Displays), format!("{screens} screens"));
    assert_eq!(sub(&mut a, Tab::Appearance), "Theme, mode, bar style and module colours");
}

#[test]
fn subtitles_use_singular_forms() {
    let mut a = ready_app();
    if let Loadable::Ready(audio) = &mut a.data.audio {
        audio.sinks = vec![node(116, "CasqueBT", 62, true)];
        audio.sources = vec![node(113, "Stereo Microphone", 100, true)];
    }
    if let Loadable::Ready(n) = &mut a.data.network {
        n.active.truncate(1);
    }
    let mut screens = a.data.displays.ready().unwrap().clone();
    screens.truncate(1);
    a.data.displays = Loadable::Ready(screens);
    let sub = |a: &mut App, t| {
        a.tab = t;
        subtitle(a)
    };
    assert_eq!(sub(&mut a, Tab::Sound), "1 output · 1 input · playing on CasqueBT");
    assert_eq!(sub(&mut a, Tab::Network), "Wi-Fi on · 1 active connection");
    assert_eq!(sub(&mut a, Tab::Displays), "1 screen");
}

#[test]
fn subtitles_while_loading_or_off() {
    let mut a = app();
    let sub = |a: &mut App, t| {
        a.tab = t;
        subtitle(a)
    };
    assert_eq!(sub(&mut a, Tab::Sound), "Outputs and inputs");
    assert_eq!(sub(&mut a, Tab::Network), "Connections");
    assert_eq!(sub(&mut a, Tab::Bluetooth), "Devices");
    assert_eq!(sub(&mut a, Tab::Displays), "Screens and profiles");
    let mut a = ready_app();
    if let Loadable::Ready(b) = &mut a.data.bluetooth {
        b.powered = false;
    }
    assert_eq!(sub(&mut a, Tab::Bluetooth), "Off");
}

#[test]
fn keycaps_on_the_last_row() {
    let a = ready_app();
    let rows = draw(&a, 120, 36).0;
    let last: String = rows[35].chars().skip(usize::from(RAIL_W) + 2).collect();
    // The focused item's keys first, the generic ones after.
    assert!(last.starts_with(" Space "), "{last:?}");
    assert!(last.contains(" F1  Help"), "{last:?}");
    // An unassigned key: its pair is skipped.
    let mut b = BTreeMap::new();
    b.insert("help".to_string(), vec![]);
    let mut a = ready_app();
    a.keymap = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
    let rows = draw(&a, 120, 36).0;
    assert!(!rows[35].contains("Help") && rows[35].contains("Palette"), "{:?}", rows[35]);
}

#[test]
fn keycaps_follow_the_shape() {
    let mut a = ready_app();
    let x = RAIL_W + 2 + 1;
    let b = cells(&a, 120, 36);
    assert_eq!((b[(x, 35)].bg, b[(x, 35)].fg), (a.theme.raised, a.theme.fg));
    assert!(!b[(x, 35)].modifier.contains(Modifier::UNDERLINED));
    a.theme.shape = Shape::Line;
    let b = cells(&a, 120, 36);
    assert!(b[(x, 35)].modifier.contains(Modifier::UNDERLINED), "a line-style cap is underlined");
    assert_eq!(b[(x, 35)].fg, tab_hue(&a.theme, Tab::Home), "and takes the tab's hue");
}

#[test]
fn keycaps_stop_before_a_pair_that_would_not_fit() {
    let a = ready_app();
    // 84 columns: nothing may spill past the row, and no word is cut.
    let row = draw(&a, 84, 26).0[25].clone();
    assert_eq!(row.chars().count(), 84);
    for word in row.split_whitespace() {
        assert!(
            ["F1", "Help", "Ctrl+P", "Palette", "Esc", "Back", "q", "Quit", "Space", "Toggle", "-/+", "Adjust"].contains(&word),
            "cut word {word:?} in {row:?}"
        );
    }
}

#[test]
fn the_rail_foot_and_the_header_show_faults() {
    let mut a = ready_app();
    a.status.last = Some("Wi-Fi off".into());
    let (rows, _) = draw(&a, 120, 36);
    assert!(rows[33].contains("Wi-Fi off"), "{:?}", rows[33]);
    assert_eq!(cells(&a, 120, 36)[(2, 33)].fg, a.theme.ok);
    a.status.error = Some("boom".into());
    let b = cells(&a, 120, 36);
    assert_eq!(b[(2, 33)].fg, a.theme.error);
    let rows = draw(&a, 120, 36).0;
    assert!(rows[1].trim_end().ends_with("✗ boom"), "{:?}", rows[1]);
}

#[test]
fn two_faults_do_not_overwrite_each_other() {
    let mut a = ready_app();
    a.status.error = Some("boom".into());
    a.status.warning = Some("careful".into());
    let row = draw(&a, 120, 36).0[1].clone();
    assert!(row.contains("✗ boom") && row.contains("✗ careful"), "{row:?}");
}

#[test]
fn resizing_across_the_threshold_keeps_hits_honest() {
    let a = ready_app();
    let (_, hits) = draw(&a, 84, 26);
    assert_eq!(hits.find(Target::Tab(Tab::Sound)), Some(Rect::new(1, 7, RAIL_W - 2, 1)));
    let (_, hits) = draw(&a, 83, 26);
    assert_eq!(hits.find(Target::Tab(Tab::Sound)).map(|r| r.y), Some(0));
    assert!(!matches!(hits.at(1, 7).map(|h| h.1), Some(Target::Tab(_))), "the old rail row is not a tab any more");
}

#[test]
fn applying_a_style_reshapes_the_rail_at_once() {
    let (mut a, _dir) = open("rail-style", &[("theme.toml", "theme = \"nord\"\nstyle = \"flat\"\n")]);
    a.tab = Tab::Home;
    assert_eq!(a.theme.shape, Shape::Flat);
    a.tab = Tab::Appearance;
    to_row(&mut a, 3);
    a.on_key_action(Action::Right);
    a.appearance_apply(Instant::now());
    assert_eq!(a.theme.shape, Shape::Round);
    let b = cells(&a, 120, 36);
    let y = entry_row(Tab::Appearance.index());
    assert_eq!(b[(1, y)].symbol(), "\u{e0b6}");
}
