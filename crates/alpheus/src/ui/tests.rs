use super::testing::draw;
use super::*;
use crate::app::keymap::Keymap;
use crate::app::testing::{app, node, ready_app};
use crate::app::worker::{Op, Update};

#[test]
fn the_selected_tab_sits_on_selection_and_the_title_stays_accent() {
    let a = ready_app();
    let tab = draw(&a, 80, 24).1.find(Target::Tab(a.tab)).unwrap();
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    t.draw(|f| {
        render(f, &a);
    })
    .unwrap();
    let cell = &t.backend().buffer()[(tab.x + 1, tab.y)];
    assert_eq!((cell.fg, cell.bg), (a.theme.fg, a.theme.selection), "spec § 4: a selected tab is on selection");
    assert_ne!(a.theme.selection, a.theme.accent, "the test bites only if the two differ");
}
use crate::app::{Confirm, Loadable, Overlay, Palette, SliderId};
use crate::backends::PowerAction;
use crate::config::KeysConfig;
use std::collections::BTreeMap;
use std::time::Instant;

#[test]
fn home_frame() {
    let (rows, _) = draw(&ready_app(), 80, 24);
    assert!(rows[0].contains(" 1 Home ") && rows[0].contains(" 5 Power "));
    assert!(rows[0].contains(" 7 Theme ") && rows[0].contains(" 4 BT "), "seven tabs in 80 columns: {:?}", rows[0]);
    assert!(rows[0].contains("alpheus"), "the brand stays at 80 columns: {:?}", rows[0]);
    assert!(rows[2].starts_with(" ▶ Volume"), "{:?}", rows[2]);
    assert!(rows[2].contains("  62 %"));
    assert!(rows[4].contains("Brightness") && rows[4].contains("  20 %"));
    assert!(rows[6].contains("Wi-Fi") && rows[6].contains("[■] on"));
    assert!(rows[10].contains("Do not disturb") && rows[10].contains("[ ] off"));
    assert!(rows[14].contains("Energy profile") && rows[14].contains("Balanced"));
    assert!(rows[22].contains("Home · ready"));
    assert!(rows[23].contains("F1 Help  Ctrl+P Palette  Esc Back  q Quit"), "{:?}", rows[23]);
    assert!(rows[23].contains("Space Toggle"));
}

#[test]
fn proportional_slider_bar() {
    let (rows, _) = draw(&ready_app(), 80, 24);
    // 62 % of 24 cells = 15 full.
    assert!(rows[2].contains(&format!("{}{}", "█".repeat(15), "░".repeat(9))), "{:?}", rows[2]);
}

#[test]
fn clickable_regions() {
    let (_, hits) = draw(&ready_app(), 80, 24);
    let sound = hits.find(Target::Tab(Tab::Sound)).unwrap();
    assert_eq!((sound.y, sound.width), (0, 9));
    assert_eq!(hits.find(Target::Slider { item: 0 }), Some(Rect::new(23, 2, 24, 1)));
    assert_eq!(hits.at(30, 2).map(|h| h.1), Some(Target::Slider { item: 0 }));
    assert_eq!(hits.at(5, 6).map(|h| h.1), Some(Target::Item(2)));
    assert!(hits.find(Target::ProfileOption(2)).is_some());
}

#[test]
fn loading_and_failure() {
    let (rows, _) = draw(&app(), 80, 24);
    assert!(rows[2].contains('…'));
    let mut a = ready_app();
    a.data.brightness = Loadable::Failed("brightnessctl not found".into());
    let (rows, hits) = draw(&a, 80, 24);
    assert!(rows[4].contains("✗ brightnessctl not found"));
    assert!(hits.find(Target::Slider { item: 1 }).is_none());
}

#[test]
fn sound_tab() {
    let mut a = ready_app();
    a.go_tab(Tab::Sound, Instant::now());
    let (rows, hits) = draw(&a, 80, 24);
    let all = rows.join("\n");
    for expected in ["Outputs", "● CasqueBT", "Dock D6000 Dell", "Inputs", "● Stereo Microphone", "[ Per-app mixer ]"] {
        assert!(all.contains(expected), "{expected} missing:\n{all}");
    }
    assert!(hits.find(Target::Slider { item: 2 }).is_some());
    assert!(hits.find(Target::Item(3)).is_some(), "clickable button");
    assert!(rows[23].contains("Enter Default"));
}

#[test]
fn network_bluetooth_power_tabs() {
    let mut a = ready_app();
    a.go_tab(Tab::Network, Instant::now());
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(all.contains("Active connections") && all.contains("MON_WIFI_1") && all.contains("[ Choose a network ]"));
    a.go_tab(Tab::Bluetooth, Instant::now());
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(all.contains("Connected devices") && all.contains("CasqueBT") && all.contains("[ Pair a device ]"));
    a.go_tab(Tab::Power, Instant::now());
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(all.contains("[ Lock ]") && all.contains("[ Power off ]"));
}

#[test]
fn search_row() {
    let mut a = ready_app();
    a.go_tab(Tab::Sound, Instant::now());
    a.search.editing = true;
    a.search.text = "casq".into();
    let (rows, _) = draw(&a, 80, 24);
    assert!(rows[2].contains("Search: casq▏"));
    assert!(!rows.join("\n").contains("Dock D6000 Dell"));
    assert!(rows[23].contains("Enter Confirm") && rows[23].contains("Esc Clear"));
}

#[test]
fn error_in_the_status_line() {
    let mut a = ready_app();
    a.warn("invalid config.toml: x".into());
    assert!(draw(&a, 80, 24).0[22].contains("✗ invalid config.toml: x"));
    // A successful action does not clear the startup warning.
    a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
    assert!(draw(&a, 80, 24).0[22].contains("✗ invalid config.toml: x"), "{:?}", draw(&a, 80, 24).0[22]);
    a.status.error = Some("Wi-Fi: boom".into());
    let line = &draw(&a, 100, 24).0[22];
    assert!(line.contains("✗ Wi-Fi: boom") && line.contains("✗ invalid config.toml: x"), "{line:?}");
}

#[test]
fn reconfigured_key_shown() {
    let mut b = BTreeMap::new();
    b.insert("palette".to_string(), vec!["ctrl+k".to_string()]);
    let mut a = ready_app();
    a.keymap = Keymap::from_config(&KeysConfig { dvorak_nav: true, bindings: b }).unwrap();
    assert!(draw(&a, 80, 24).0[23].contains("Ctrl+K Palette"));
}

#[test]
fn shortcut_bar_never_cut_mid_word() {
    let mut a = ready_app();
    a.go_tab(Tab::Network, Instant::now());
    let (rows, _) = draw(&a, 80, 24);
    let bar = &rows[23];
    // "Open" must never appear truncated as "Ope".
    assert!(!bar.contains("Enter Ope ") || bar.contains("Enter Open"), "{bar:?}");
}

#[test]
fn small_sizes_never_panic() {
    let a = ready_app();
    for (w, h) in [(1, 1), (10, 3), (29, 24), (80, 7), (30, 8), (31, 9), (200, 60)] {
        let (rows, _) = draw(&a, w, h);
        if w < MIN_W || h < MIN_H {
            assert!("alpheus: window too small".starts_with(rows[0].trim_end()), "{:?}", rows[0]);
        }
    }
    let (rows, _) = draw(&a, 10, 3);
    assert_eq!(rows[0], "alpheus: w");
}

#[test]
fn scrolling_keeps_the_focus_visible() {
    let mut a = ready_app();
    if let Loadable::Ready(st) = &mut a.data.audio {
        st.sinks = (0..30).map(|i| node(i, &format!("Output {i}"), 50, i == 0)).collect();
    }
    a.go_tab(Tab::Sound, Instant::now());
    a.focus = 29;
    let (rows, hits) = draw(&a, 80, 12);
    let all = rows.join("\n");
    assert!(all.contains("Output 29"), "{all}");
    assert!(!all.contains("Output 0 "), "the top of the list has scrolled away");
    assert!(hits.find(Target::Slider { item: 29 }).is_some());
    assert_eq!(a.items()[29], crate::app::Item::Slider(SliderId::Node(29)));
}

// ── Displays tab ─────────────────────────────────────────────────

use crate::app::SaveDialog;

fn displays_tab() -> crate::app::App {
    let mut a = ready_app();
    a.go_tab(Tab::Displays, Instant::now());
    a.take_requests();
    a
}

#[test]
fn displays_tab_rendering() {
    let mut a = displays_tab();
    a.focus = 2; // Alpha
    // 120 columns: with the rail the body is 100 wide, enough for the longest row.
    let (rows, hits) = draw(&a, 120, 28);
    let all = rows.join("\n");
    for expected in [
        "Profiles",
        "[ Laptop seul ]",
        "[ Bureau ]",
        "[ + Save current ]",
        "Map",
        "Displays",
        "1 Laptop",
        "2 Alpha",
        "DVI-I-1 ⚠",
        "3 Bravo",
        "1920×1080@240",
        "scale 1.0",
        "position 0, 1080",
        "origin",
        "right of Alpha",
    ] {
        assert!(all.contains(expected), "{expected} missing:\n{all}");
    }
    assert!(all.contains('●'), "the map marks the focused screen:\n{all}");
    assert!(hits.find(Target::DisplayProfile(2)).is_some());
    assert!(hits.find(Target::OutputToggle(3)).is_some());
    assert!(rows[27].contains("Space  Toggle") && rows[27].contains("Enter  Adjust"), "{:?}", rows[27]);
    a.focus = 0;
    assert!(draw(&a, 120, 28).0[27].contains("Enter  Apply"));
}

#[test]
fn last_screen_greyed_out() {
    let mut a = displays_tab();
    a.data.displays = Loadable::Ready(vec![crate::displays::testing::machine()[0].clone()]);
    assert!(draw(&a, 100, 28).0.join("\n").contains("[■] only on"));
}

/// Under any compositor the tab shows what the daemon says, as it says it: its absence, a daemon
/// too old, its status; the profiles stay visible.
#[test]
fn daemon_absent_shows_its_message() {
    use crate::app::testing::FakeDisplays;
    use crate::app::worker::{Domain, fetch};
    use crate::displays::wire::{Snapshot, Status, TrialState};
    use std::sync::Arc;
    let dir = alpheus_commun::temp_path::TempPath::new("ui-daemon-absent");
    let mut b = crate::backends::Backends::system(Arc::new(crate::exec::fake::FakeExecutor::new()), dir.to_path_buf());
    let fake = Arc::new(FakeDisplays::default());
    b.displays = fake.clone();
    let off = |status| Ok(Snapshot { status, outputs: Vec::new(), trial: TrialState::None });
    for (answer, shown) in [
        (Err("the alpheus daemon is not running".to_string()), "✗ the alpheus daemon is not running"),
        (Err("the daemon is too old for displays".to_string()), "✗ the daemon is too old for displays"),
        (off(Status::Off), "✗ displays are off in bar.toml"),
        (off(Status::NoManager), "✗ the compositor has no output management"),
    ] {
        *fake.snapshot.lock().unwrap() = answer;
        let mut a = displays_tab();
        a.on_update(fetch(&b, Domain::Displays), Instant::now());
        let all = draw(&a, 100, 24).0.join("\n");
        assert!(all.contains(shown), "{shown}:\n{all}");
        assert!(all.contains("[ Bureau ]"), "the profiles stay visible");
        assert!(!all.contains("1 Laptop"), "no output listed");
    }
    let mut a = displays_tab();
    a.data.displays = Loadable::Loading;
    assert!(draw(&a, 80, 24).0.join("\n").contains('…'));
}

#[test]
fn scaled_map() {
    use super::displays::{MAP_ROWS, map_canvas};
    use crate::displays::testing::{ALPHA, BRAVO, machine, on};
    let text = |g: &Vec<Vec<Option<(char, bool)>>>| -> Vec<String> {
        g.iter().map(|r| r.iter().map(|c| c.map_or(' ', |(ch, _)| ch)).collect()).collect()
    };
    let g = map_canvas(&machine(), Some("HDMI-A-1"), 76);
    let lines = text(&g);
    assert!(lines.len() <= usize::from(MAP_ROWS));
    let all = lines.join("\n");
    assert!(all.contains("Alpha") && all.contains("Bravo") && all.contains("Laptop"), "{all}");
    for n in ["1", "2", "3"] {
        assert!(all.contains(n), "number {n}:\n{all}");
    }
    assert!(g.iter().flatten().flatten().any(|(ch, foc)| *ch == '●' && *foc), "● on Bravo");
    let g = map_canvas(&[on("DP-1", ALPHA, 1920, 1080, 0, 0), on("DP-2", BRAVO, 1920, 1080, 1920, 0)], None, 20);
    assert!(!g.is_empty() && g.iter().all(|r| r.len() <= 20), "bounded by the width");
    assert!(map_canvas(&[], None, 76).is_empty());
    assert!(map_canvas(&machine(), None, 2).is_empty(), "too narrow: nothing");
}

#[test]
fn keep_these_settings_dialog() {
    let mut a = displays_tab();
    a.toggle_output("DVI-I-1", Instant::now());
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(all.contains("Keep these settings?") && all.contains("Alpha disabled") && all.contains("Alpha: off"), "{all}");
    assert!(all.contains("⚠ DisplayLink output: known freeze risk on this machine"), "{all}");
    assert!(all.contains("Applying…") && all.contains("[ Cancel ]") && all.contains("[ Keep ]"), "{all}");
    let r = a.take_requests();
    let Some(crate::app::worker::Request::Do(op)) = r.into_iter().next() else { panic!() };
    a.on_update(Update::Done(op, Ok(())), Instant::now());
    assert!(draw(&a, 80, 24).0.join("\n").contains("Automatic rollback in 15 s"));
    let (_, hits) = draw(&a, 80, 24);
    let (no, yes) = (hits.find(Target::ConfirmNo).unwrap(), hits.find(Target::ConfirmYes).unwrap());
    assert!(no.x < yes.x && no.y == yes.y, "Cancel on the left");
}

#[test]
fn keep_dialog_in_short_windows_keeps_the_countdown() {
    let mut a = displays_tab();
    a.apply_display_profile("Bureau", Instant::now());
    let r = a.take_requests();
    let Some(crate::app::worker::Request::Do(op)) = r.into_iter().next() else { panic!() };
    a.on_update(Update::Done(op, Ok(())), Instant::now());
    for h in [MIN_H, MIN_H + 1, 10] {
        let all = draw(&a, 80, h).0.join("\n");
        assert!(all.contains("Automatic rollback in 15 s"), "h={h}\n{all}");
        assert!(all.contains("⚠ DisplayLink output"), "h={h}\n{all}");
        assert!(all.contains("[ Cancel ]"), "h={h}\n{all}");
    }
}

#[test]
fn settings_panel() {
    let mut a = displays_tab();
    a.open_panel("HDMI-A-1", Instant::now());
    let (rows, hits) = draw(&a, 80, 24);
    let all = rows.join("\n");
    for expected in [
        "Settings — Bravo (HDMI-A-1)",
        "Mode",
        "1920×1080@240  2/2",
        "Scale",
        "Rotation",
        "0°",
        "Placement",
        "right of",
        "Reference",
        "Alpha",
        "Alignment",
        "top",
        "✗ no change",
        "[ Cancel ]",
        "[ Apply ]",
    ] {
        assert!(all.contains(expected), "{expected} missing:\n{all}");
    }
    assert!(hits.find(Target::PanelStep { field: crate::app::PanelField::Align, dir: -1 }).is_some());
    // Origin: no more reference or alignment.
    if let Overlay::Panel(p) = &mut a.overlay {
        p.place = crate::app::PlaceChoice::Origin;
    }
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(!all.contains("Reference") && all.contains("origin"), "{all}");
}

#[test]
fn profile_name_entry() {
    let mut a = displays_tab();
    a.overlay = Overlay::SaveProfile(SaveDialog { name: "Maison".into(), ..SaveDialog::default() });
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(all.contains("Save current") && all.contains("> Maison▏"), "{all}");
    a.overlay = Overlay::SaveProfile(SaveDialog { name: "Bureau".into(), replace: true, yes_focused: false });
    let all = draw(&a, 80, 24).0.join("\n");
    assert!(all.contains("Replace profile \"Bureau\"?") && all.contains("[ Replace ]"), "{all}");
}

#[test]
fn displays_never_panic_in_a_small_window() {
    let mut a = displays_tab();
    a.open_panel("eDP-1", Instant::now());
    let panel = a.overlay.clone();
    a.overlay = Overlay::None;
    a.toggle_output("DVI-I-1", Instant::now());
    let keep = a.overlay.clone();
    for o in [Overlay::None, panel, keep, Overlay::SaveProfile(SaveDialog::default())] {
        a.overlay = o;
        for (w, h) in [(30, 8), (31, 9), (40, 10), (50, 12), (200, 60)] {
            draw(&a, w, h);
        }
    }
}

#[test]
fn displays_snapshots() {
    let mut a = displays_tab();
    a.focus = 2;
    insta::assert_snapshot!("displays_list", draw(&a, 100, 28).0.join("\n"));
    a.open_panel("HDMI-A-1", Instant::now());
    insta::assert_snapshot!("displays_panel", draw(&a, 80, 24).0.join("\n"));
    a.overlay = Overlay::None;
    a.apply_display_profile("Bureau", Instant::now());
    insta::assert_snapshot!("displays_keep", draw(&a, 80, 24).0.join("\n"));
}

#[test]
fn tab_snapshots() {
    let mut a = ready_app();
    for t in Tab::ALL {
        a.go_tab(t, Instant::now());
        insta::assert_snapshot!(format!("tab_{}", t.index() + 1), draw(&a, 80, 24).0.join("\n"));
    }
}

#[test]
fn help_screen() {
    let mut a = ready_app();
    a.overlay = Overlay::Help;
    let (rows, hits) = draw(&a, 80, 30);
    let all = rows.join("\n");
    assert!(all.contains("Command palette") && all.contains("Ctrl+P"));
    assert!(all.contains("Help — Esc to close"));
    assert_eq!(hits.at(40, 15).map(|h| h.1), Some(Target::Overlay));
}

#[test]
fn full_help_fits_in_80x26() {
    let mut a = ready_app();
    a.overlay = Overlay::Help;
    let (rows, _) = draw(&a, 80, 26);
    let all = rows.join("\n");
    assert!(all.contains("Help — Esc to close"), "{all}");
    assert!(all.contains("Mouse") && all.contains("click, drag, scroll"), "last row cut off:\n{all}");
    assert!(all.contains("Tabs 1 to 7") && all.contains("1, 2, 3, 4, 5, 6, 7"), "{all}");
    assert!(!all.contains("Tab Sound"), "the seven tabs fit on one line");
}

#[test]
fn palette_screen() {
    let mut a = ready_app();
    a.overlay = Overlay::Palette(Palette { query: "wifi".into(), selected: 0 });
    let (rows, hits) = draw(&a, 80, 24);
    let all = rows.join("\n");
    assert!(all.contains("> wifi▏") && all.contains("Wi-Fi"));
    assert!(hits.find(Target::PaletteEntry(0)).is_some());
    a.overlay = Overlay::Palette(Palette { query: "zzz".into(), selected: 0 });
    assert!(draw(&a, 80, 24).0.join("\n").contains("no command"));
}

#[test]
fn palette_scrolls_to_the_selection() {
    let mut a = ready_app();
    let n = a.palette_matches().len();
    a.overlay = Overlay::Palette(Palette { query: String::new(), selected: n - 1 });
    let (rows, hits) = draw(&a, 80, 24);
    assert!(rows.join("\n").contains("Power off"), "last entry visible");
    assert!(hits.find(Target::PaletteEntry(n - 1)).is_some());
    assert!(hits.find(Target::PaletteEntry(0)).is_none());
}

#[test]
fn confirmation_screen() {
    let mut a = ready_app();
    a.overlay = Overlay::Confirm(Confirm { action: PowerAction::Poweroff, yes_focused: false });
    let (rows, hits) = draw(&a, 80, 24);
    let all = rows.join("\n");
    assert!(all.contains("Shut down the computer?") && all.contains("[ Cancel ]") && all.contains("[ Confirm ]"));
    let no = hits.find(Target::ConfirmNo).unwrap();
    let yes = hits.find(Target::ConfirmYes).unwrap();
    assert!(no.x < yes.x && no.y == yes.y);
}

#[test]
fn corner_messages() {
    let mut a = ready_app();
    a.on_update(Update::Done(Op::SetWifi(false), Ok(())), Instant::now());
    let rows = draw(&a, 80, 24).0;
    assert!(rows[1].trim_end().ends_with("✓ Wi-Fi: off"), "{:?}", rows[1]);
}

#[test]
fn apply_greyed_out_if_refused() {
    let mut a = ready_app();
    a.go_tab(Tab::Displays, Instant::now());
    a.open_panel("HDMI-A-1", Instant::now());
    // Text color of "[ Apply ]" after rendering.
    let fg = |a: &crate::app::App| {
        let r = draw(a, 80, 24).1.find(Target::PanelApply).unwrap();
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| {
            render(f, a);
        })
        .unwrap();
        t.backend().buffer()[(r.x + 2, r.y)].fg
    };
    assert_eq!(fg(&a), a.theme.muted, "no change: greyed out");
    if let Overlay::Panel(p) = &mut a.overlay {
        p.scale = crate::displays::Scale(1250);
    }
    assert_ne!(fg(&a), a.theme.muted, "valid setting: active");
}

#[test]
fn overlays_never_panic_in_a_small_window() {
    let mut a = ready_app();
    for o in
        [Overlay::Help, Overlay::Palette(Palette::default()), Overlay::Confirm(Confirm { action: PowerAction::Reboot, yes_focused: true })]
    {
        a.overlay = o;
        for (w, h) in [(30, 8), (31, 9), (40, 10)] {
            draw(&a, w, h);
        }
    }
}
