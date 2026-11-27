use super::*;
use crate::clock::LocalTime;
use crate::compositor::{Window, example};
use crate::config::{BarConfig, BatteryConfig, BatteryVisibility, ModuleId};
use crate::media::MediaView;
use crate::notify::NotifyView;
use crate::privacy::{Capture, Indicator};
use crate::state::Msg;
use crate::system::{Battery, Charge, Link, NetState, Peripheral, PeripheralKind, PowerState, SoundState, Volume};
use alpheus_common::theme::settings::{BarStyle, ModuleColors};

const SUN: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

fn bar() -> Bar {
    let mut b = Bar::new(BarConfig::default());
    b.apply(Msg::Compositor(Some(example())));
    b.apply(Msg::Clock(SUN));
    b
}

fn snapshot(g: &Grid) -> String {
    format!("{}\n{}", g.text(), g.styles())
}

#[test]
fn wide_bar_on_the_hdmi_screen() {
    let g = bar().grid("HDMI-A-1", 100);
    let (t, s) = (g.text(), g.styles());
    assert_eq!(g.width(), 100);
    assert!(t.starts_with(" \u{f0349}  ·   2·  4!····+  5   ·  \u{f0239} Web page — Mozilla Firefox "), "launcher button first: {t}");
    assert!(t.ends_with("  ·  Sun 4 Oct 14:05  ·  \u{23fb} "), "the power button last: {t}");
    assert!(s.starts_with("..mmmmmAAAAuuuuuuuuummmmmmmm"), "{s}");
    assert!(s.ends_with("mmmmmmmmmmmmmmmGGGGGmmmmm.."), "{s}");
    insta::assert_snapshot!("hdmi_100", snapshot(&g));
}

#[test]
fn title_truncated_then_removed_when_space_is_short() {
    let g = bar().grid("HDMI-A-1", 68);
    assert!(g.text().contains("Web page —…"), "{}", g.text());
    insta::assert_snapshot!("hdmi_68", snapshot(&g));
    let g = bar().grid("HDMI-A-1", 40);
    let t = g.text();
    assert!(!t.contains("Web") && t.contains("14:05") && t.contains(" 4!····+ "), "{t}");
    assert_eq!(g.width(), 40);
    insta::assert_snapshot!("hdmi_40", snapshot(&g));
}

#[test]
fn extreme_sizes_without_panic() {
    for w in [0u16, 1, 5, 12, 39] {
        assert_eq!(bar().grid("HDMI-A-1", w).width(), w);
    }
    // At 50 columns the title has disappeared (see
    // title_truncated_then_removed_when_space_is_short), but the
    // launcher button, workspaces, the clock and the power button are
    // all still shown whole (1 + 16 + 15 + 1 cells, 3 separators of 5, 2
    // margins). At 49 the launcher button goes; at 43 the power button
    // too (last of REMOVAL); workspaces and the clock, never removed,
    // stay whole.
    let t = bar().grid("HDMI-A-1", 50).text();
    assert!(t.starts_with(" \u{f0349}  ·  ") && t.contains(" 4!····+ ") && t.ends_with(" 14:05  ·  \u{23fb} "), "{t}");
    let t = bar().grid("HDMI-A-1", 49).text();
    assert!(!t.contains('\u{f0349}') && t.contains(" 4!····+ ") && t.ends_with(" 14:05  ·  \u{23fb} "), "{t}");
    let t = bar().grid("HDMI-A-1", 43).text();
    assert!(!t.contains('\u{23fb}') && t.contains(" 4!····+ ") && t.ends_with(" 14:05 "), "{t}");
}

#[test]
fn workspaces_of_each_screen() {
    let b = bar();
    let g = b.grid("eDP-1", 60);
    assert!(g.text().starts_with(" \u{f0349}  ·   1   ·  "), "{}", g.text());
    assert!(g.styles().starts_with("..mmmmmsss"), "visible without focus: surface");
    let g = b.grid("DVI-I-1", 60);
    assert!(g.text().starts_with(" \u{f0349}  ·   3··  web·   ·  "), "{}", g.text());
    let t = b.grid("HEADLESS-1", 120).text();
    assert!(t.contains(" 1 ") && t.contains(" web· "), "unknown screen: every workspace ({t})");
}

#[test]
fn the_clock_has_a_zone_only_with_the_calendar() {
    let clock = Target::Module(ModuleId::Clock);
    let g = bar().grid("HDMI-A-1", 100);
    let t = g.text();
    let start = t.chars().count() as u16 - " Sun 4 Oct 14:05  ·  \u{23fb} ".chars().count() as u16 + 1;
    assert_eq!(g.target_at(start), Some(&clock), "the date's first cell: {t}");
    assert_eq!(g.target_at(92), Some(&clock), "the time's last cell");
    assert_eq!(g.target_at(start - 1), None, "the space before the date");
    assert_eq!(g.target_at(95), None, "the separator after it");
    assert_eq!(g.target_at(98), Some(&Target::Module(ModuleId::Power)), "the power button, its own zone");
    assert_eq!(g.target_at(99), None, "the margin");
    let mut b = bar();
    b.config.clock.calendar = false;
    let g = b.grid("HDMI-A-1", 100);
    assert!(g.zones().iter().all(|z| z.target != clock), "no calendar: the clock is no zone");
}

#[test]
fn workspace_zones() {
    let g = bar().grid("HDMI-A-1", 100);
    let two = Target::Workspace { reference: "2".into(), output: "HDMI-A-1".into() };
    let four = Target::Workspace { reference: "4".into(), output: "HDMI-A-1".into() };
    let five = Target::Workspace { reference: "5".into(), output: "HDMI-A-1".into() };
    let launcher = Target::Module(ModuleId::Launcher);
    // Edges of each zone (launcher button at 1, the 5-cell separator at 2-6, "2·" at 7-10,
    // "4!····+" at 11-19, "5" at 20-22): a bound off by one cell would let clicks through or
    // cut them off.
    assert_eq!(g.target_at(0), None, "margin");
    assert_eq!(g.target_at(1), Some(&launcher), "the launcher button");
    assert_eq!(g.target_at(2), None, "separator after the button");
    assert_eq!(g.target_at(6), None, "separator after the button");
    assert_eq!(g.target_at(7), Some(&two), "first edge of workspace 2");
    assert_eq!(g.target_at(10), Some(&two), "last edge of workspace 2");
    assert_eq!(g.target_at(11), Some(&four), "first edge of workspace 4");
    assert_eq!(g.target_at(12), Some(&four));
    assert_eq!(g.target_at(19), Some(&four), "last edge of workspace 4");
    assert_eq!(g.target_at(22), Some(&five), "last edge of workspace 5");
    assert_eq!(g.target_at(23), None, "on the separator, no zone");
    assert_eq!(g.target_at(36), None, "title");
    let g = bar().grid("DVI-I-1", 60);
    let web = Target::Workspace { reference: "name:web".into(), output: "DVI-I-1".into() };
    assert_eq!(g.target_at(13), Some(&web));
    // Unknown screen: each workspace keeps ITS OWN output in the command.
    let g = bar().grid("HEADLESS-1", 120);
    assert_eq!(g.target_at(8), Some(&Target::Workspace { reference: "1".into(), output: "eDP-1".into() }));
}

#[test]
fn order_from_bar_toml() {
    let mut b = bar();
    b.config.modules = vec![ModuleId::Clock, ModuleId::Workspaces];
    let t = b.grid("HDMI-A-1", 60).text();
    assert!(t.starts_with(" Sun 4 Oct 14:05"), "{t}");
    assert!(t.ends_with("  ·   2·  4!····+  5  "), "free space before the last module: {t}");
    b.config.clock.date = String::new();
    assert!(b.grid("HDMI-A-1", 60).text().starts_with(" 14:05"));
}

#[test]
fn battery_config_reaches_the_segment() {
    // `bar.config.battery` (bar.toml's `[battery]`) must be the one
    // `grid()` actually hands to `segments::battery`, not a default
    // built on the fly.
    let mut b = bar();
    let mouse = Peripheral { kind: PeripheralKind::Mouse, name: "Souris".into(), percent: Some(80), low: false };
    b.apply(Msg::Battery(Some(PowerState { laptop: None, peripherals: vec![mouse] })));
    assert!(b.grid("HDMI-A-1", 100).text().contains('%'), "default [battery] peripherals = \"always\": shown");
    b.config.battery = BatteryConfig { peripherals: BatteryVisibility::Never, ..BatteryConfig::default() };
    assert!(!b.grid("HDMI-A-1", 100).text().contains('%'), "[battery] peripherals = \"never\": hidden");
}

#[test]
fn waiting_and_down() {
    let b = Bar::new(BarConfig::default());
    assert!(b.grid("eDP-1", 50).text().starts_with(" \u{f0349}  ·   …   ·  …"), "nothing received: \"…\"");
    let mut b = bar();
    b.apply(Msg::Compositor(None));
    let t = b.grid("eDP-1", 50).text();
    assert!(t.starts_with(" \u{f0349}  ·   –   ·  –"), "source down: \"–\" ({t})");
    assert!(t.contains("14:05"), "the rest of the bar carries on");
}

#[test]
fn launcher_button_on_every_output_and_dropped_last() {
    let b = bar();
    for output in ["eDP-1", "HDMI-A-1", "DVI-I-1", "HEADLESS-1"] {
        for w in [70u16, 100] {
            let g = b.grid(output, w);
            assert_eq!(g.cells()[1].ch, segments::LAUNCHER, "{output} at {w}: {}", g.text());
            assert_eq!(g.target_at(1), Some(&Target::Module(ModuleId::Launcher)), "{output} at {w}");
        }
    }
    let g = b.grid("HDMI-A-1", 50);
    assert_eq!(g.target_at(1), Some(&Target::Module(ModuleId::Launcher)), "50 columns: title gone, the button kept");
    let g = b.grid("HDMI-A-1", 49);
    assert!(!g.text().contains(segments::LAUNCHER), "49 columns: the button dropped, after every module but ⏻: {}", g.text());
    assert!((0..49).all(|x| g.target_at(x) != Some(&Target::Module(ModuleId::Launcher))), "and its click zone");
    let mut b = bar();
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Clock];
    assert!(!b.grid("HDMI-A-1", 60).text().contains(segments::LAUNCHER), "removed from bar.toml: gone");
}

/// The power button is its own block with no hue, the last module removed, and
/// absent when `[quick]`'s four session commands are all empty.
#[test]
fn the_power_button_is_its_own_block_with_no_hue_removed_last_and_hidden_without_commands() {
    assert_eq!(ModuleId::Power.spec().group, GroupKind::Power);
    assert_eq!(ModuleId::Power.spec().hue, None, "neutral, like the bell");
    assert_eq!(REMOVAL.last(), Some(&ModuleId::Power), "after every other removable module");
    let power = Target::Module(ModuleId::Power);
    let g = bar().grid("HDMI-A-1", 100);
    assert!(g.text().ends_with("  ·  \u{23fb} "), "{}", g.text());
    assert_eq!(g.styles().chars().nth(98), Some('.'), "the glyph in the bar's normal ink");
    let mut b = bar();
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Clock, ModuleId::Power];
    b.module_colors = ModuleColors::Pills;
    let g = styled(b.clone(), BarStyle::Islands).grid("HDMI-A-1", 60);
    assert_eq!(g.groups().last().map(|x| (x.kind, x.hue)), Some((GroupKind::Power, None)), "a block of its own, no hue");
    let q = &mut b.config.quick;
    (q.lock, q.reboot, q.poweroff) = (vec![], vec![], vec![]);
    let g = b.grid("HDMI-A-1", 60);
    assert!(!g.text().contains('\u{23fb}') && g.text().ends_with(" 14:05 "), "all four empty: no button: {}", g.text());
    assert!(g.zones().iter().all(|z| z.target != power), "nor its zone");
    b.config.quick.logout = vec!["fake-logout".into()];
    assert!(b.grid("HDMI-A-1", 60).text().ends_with("  ·  \u{23fb} "), "one command is enough");
}

#[test]
fn hostile_title() {
    let mut e = example();
    e.window = Some(Window { class: "Alacritty".into(), title: format!("a\tb\n日本語 — 🦀 \u{1b}[31m{}", "x".repeat(300)) });
    let mut b = bar();
    b.apply(Msg::Compositor(Some(e)));
    for w in [30u16, 47, 48, 49, 200] {
        let g = b.grid("HDMI-A-1", w);
        assert_eq!(g.width(), w);
        let t = g.text();
        assert!(!t.contains(['\t', '\n', '\u{1b}']), "{t:?}");
    }
}

/// The window module's icon is purple under icons and full; the title only under full; a bar
/// that has not heard from the theme yet (`Bar::new`) takes no hue at all.
#[test]
fn the_window_title_takes_the_purple_by_module_colours() {
    let styles = |mc| {
        let mut b = bar();
        b.module_colors = mc;
        b.grid("HDMI-A-1", 100).styles()
    };
    let none = styles(ModuleColors::None);
    assert_eq!(bar().module_colors, ModuleColors::None);
    let (icons, full) = (styles(ModuleColors::Icons), styles(ModuleColors::Full));
    assert_eq!(none.matches('p').count(), 0);
    assert_eq!(icons.matches('p').count(), 1, "the glyph alone: {icons}");
    assert!(full.matches('p').count() > 10, "the glyph, the space and the title: {full}");
    assert_eq!(none.replace('p', "."), none);
    assert_eq!(icons.replace('p', "."), none, "only the icon cell differs");
    assert_eq!(full.replace('p', "."), none, "only the tints differ, never a character");
}

#[test]
fn a_module_that_draws_nothing_takes_no_separator() {
    let mut b = bar();
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Volume, ModuleId::Tray, ModuleId::Clock];
    let t = b.grid("HDMI-A-1", 60).text();
    assert_eq!(t.matches("  ·  ").count(), 1, "no separator for a missing module: {t}");
}

#[test]
fn no_empty_segment_without_window_or_known_workspace() {
    // With no active window: the window module must leave no empty
    // segment (so no "· <empty> ·").
    let mut e = example();
    e.window = None;
    let mut b = bar();
    b.apply(Msg::Compositor(Some(e)));
    let t = b.grid("HDMI-A-1", 60).text();
    assert_eq!(t.matches("  ·  ").count(), 3, "launcher, workspaces, clock and ⏻ only: {t}");

    // With no workspace at all known to the compositor: same thing for
    // the workspaces module.
    let e2 = CompositorState {
        workspaces: Vec::new(),
        window: Some(Window { class: "firefox".into(), title: "Page".into() }),
        toplevels: Vec::new(),
        ..Default::default()
    };
    let mut b2 = bar();
    b2.apply(Msg::Compositor(Some(e2)));
    let t2 = b2.grid("HDMI-A-1", 60).text();
    assert_eq!(t2.matches("  ·  ").count(), 3, "launcher, window, clock and ⏻ only: {t2}");

    // While waiting or down, "…" / "–" stay shown for both modules
    // (covered by `waiting_and_down`), never removed.
}

/// Launcher, workspaces, window, volume, network, battery, clock — the
/// module set from a bug report: with no
/// active window, the only elastic segment vanishes, and the free
/// space it used to carry must keep tracking its slot in the order.
fn bar_with_right_modules() -> Bar {
    let mut b = bar();
    b.config.modules = vec![
        ModuleId::Launcher,
        ModuleId::Workspaces,
        ModuleId::Window,
        ModuleId::Volume,
        ModuleId::Network,
        ModuleId::Battery,
        ModuleId::Clock,
    ];
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 55, muted: false }), ..SoundState::default() })));
    b.apply(Msg::Network(Some(NetState::Online { link: Link::Wifi, vpns: 0, limited: false })));
    b.apply(Msg::Battery(Some(PowerState { laptop: Some(Battery { percent: 80, charge: Charge::Discharging }), peripherals: Vec::new() })));
    b
}

/// The column of `target`'s first cell, and the text from there to the
/// end of the grid (by cell, not by byte: a column does not line up
/// with a byte offset into `Grid::text()` once a wide character has
/// been written before it).
fn tail_from(g: &Grid, target: &Target) -> (u16, String) {
    let start = (0..g.width()).find(|&x| g.target_at(x) == Some(target)).expect("segment not found");
    let tail = g.cells()[start as usize..].iter().filter(|c| c.ch != crate::grid::CONT).map(|c| c.ch).collect();
    (start, tail)
}

#[test]
fn with_no_active_window_the_right_hand_modules_stay_right() {
    let b = bar_with_right_modules(); // bar() already sets an active window (example())
    let volume = Target::Module(ModuleId::Volume);

    let with_window = b.grid("HDMI-A-1", 100);
    let (start_with, tail_with) = tail_from(&with_window, &volume);

    let mut b2 = bar_with_right_modules();
    let mut e = example();
    e.window = None;
    b2.apply(Msg::Compositor(Some(e)));
    let without_window = b2.grid("HDMI-A-1", 100);
    let (start_without, tail_without) = tail_from(&without_window, &volume);

    assert_eq!(without_window.width(), with_window.width());
    assert_eq!(start_without, start_with, "volume starts at the same column either way: {}", without_window.text());
    assert_eq!(tail_without, tail_with, "volume…clock, right-aligned exactly as with a window");
    assert_eq!(
        without_window.text().matches("  ·  ").count(),
        with_window.text().matches("  ·  ").count() - 1,
        "one separator fewer: no empty window segment ({})",
        without_window.text()
    );
    insta::assert_snapshot!("no_active_window_right_modules", snapshot(&without_window));
}

#[test]
fn narrow_screen_keeps_the_gap_at_the_window_slot_as_modules_are_removed() {
    // Narrow screens drop modules by priority (REMOVAL: here, network
    // is the first of this module set to go) on top of the window
    // module already having no segment. The free space must keep
    // tracking the window's configured slot through that shrinking,
    // not fall back to "before the last segment" as soon as anything
    // else disappears.
    let mut b = bar_with_right_modules();
    let mut e = example();
    e.window = None;
    b.apply(Msg::Compositor(Some(e)));

    let narrow = (20..100)
        .rev()
        .find(|&w| {
            let t = b.grid("HDMI-A-1", w).text();
            !t.contains(segments::WIFI) && t.contains('%')
        })
        .expect("no width drops exactly the network module while keeping volume and battery");
    let got = b.grid("HDMI-A-1", narrow);
    assert_eq!(
        got.text().matches("  ·  ").count(),
        4,
        "launcher, workspaces, volume, battery, clock (network dropped, window absent): {}",
        got.text()
    );

    // Same surviving modules, built directly without the window module
    // in the configuration at all: the old "before the last segment"
    // fallback applies there. The fix's gap must land somewhere else
    // (right after workspaces), not degrade back to that fallback the
    // moment a second module is also missing.
    let mut reference = b.clone();
    reference.config.modules = vec![ModuleId::Launcher, ModuleId::Workspaces, ModuleId::Volume, ModuleId::Battery, ModuleId::Clock];
    let old_rule = reference.grid("HDMI-A-1", narrow);

    let volume = Target::Module(ModuleId::Volume);
    let (vol_at_got, _) = tail_from(&got, &volume);
    let (vol_at_old_rule, _) = tail_from(&old_rule, &volume);
    assert!(
        vol_at_got > vol_at_old_rule,
        "volume still pushed right by the gap at the window's slot, not stuck against workspaces like the old-rule fallback: got {} vs old-rule {} ({})",
        vol_at_got,
        vol_at_old_rule,
        got.text()
    );
}

// ---- groups and per-style spacing ----

const BLOCK_STYLES: [BarStyle; 4] = [BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Translucent];

fn styled(mut b: Bar, style: BarStyle) -> Bar {
    b.style = style;
    b
}

fn group(kind: GroupKind, start: u16, end: u16, hue: Option<Hue>) -> Group {
    Group { kind, start, end, hue }
}

/// `flat` records no block: its grid is exactly today's (the snapshots above), groups included.
#[test]
fn flat_records_no_group() {
    for w in [100u16, 60, 40] {
        let g = bar().grid("HDMI-A-1", w);
        assert_eq!(g.groups(), &[], "{w} columns");
    }
    assert_eq!(bar_with_right_modules().grid("HDMI-A-1", 100).groups(), &[]);
}

/// `underline` keeps flat's cells and zones to the cell; it only records the blocks, each from
/// its first module's first cell to its last module's last.
#[test]
fn underline_keeps_the_flat_cells_and_records_the_blocks() {
    for b in [bar(), bar_with_right_modules()] {
        for w in [100u16, 60, 40, 20] {
            let (flat, under) = (b.grid("HDMI-A-1", w), styled(b.clone(), BarStyle::Underline).grid("HDMI-A-1", w));
            assert_eq!((under.text(), under.styles(), under.zones()), (flat.text(), flat.styles(), flat.zones()), "{w}");
        }
    }
    let g = styled(bar_with_right_modules(), BarStyle::Underline).grid("HDMI-A-1", 100);
    assert_eq!(
        g.groups(),
        &[
            group(GroupKind::Launcher, 1, 2, None),
            group(GroupKind::Workspaces, 7, 23, None),
            group(GroupKind::Title, 28, 53, Some(Hue::Purple)),
            group(GroupKind::Status, 58, 79, Some(Hue::Blue)),
            group(GroupKind::Clock, 84, 99, None),
        ],
        "{}",
        g.text()
    );
}

/// The block styles: no dot separators, a pad cell inside each end of a block, one blank cell between
/// blocks, the title's block around its text only (the free space stays outside it).
#[test]
fn block_styles_drop_the_separators_and_pad_each_block() {
    let g = styled(bar(), BarStyle::Islands).grid("HDMI-A-1", 100);
    let t = g.text();
    assert!(!t.contains("  ·  "), "{t}");
    assert!(t.starts_with("  \u{f0349}    2·  4!····+  5    \u{f0239} Web page — Mozilla Firefox "), "{t}");
    assert!(t.ends_with(" Sun 4 Oct 14:05   \u{23fb}  "), "the clock's block, the power button's own, then the margin: {t}");
    assert_eq!(
        g.groups(),
        &[
            group(GroupKind::Launcher, 1, 4, None),
            group(GroupKind::Workspaces, 5, 23, None),
            group(GroupKind::Title, 24, 54, Some(Hue::Purple)),
            group(GroupKind::Clock, 78, 95, None),
            group(GroupKind::Power, 96, 99, None),
        ]
    );
    assert_eq!(g.split(), 77, "the title's free space and closing pad end where the right-hand side starts");
    assert_eq!(g.target_at(2), Some(&Target::Module(ModuleId::Launcher)), "the zones follow the cells");
    assert_eq!(g.target_at(6), Some(&Target::Workspace { reference: "2".into(), output: "HDMI-A-1".into() }));
    insta::assert_snapshot!("blocks_hdmi_100", snapshot(&g));
    for style in BLOCK_STYLES {
        let other = styled(bar(), style).grid("HDMI-A-1", 100);
        assert_eq!(other, g, "{style:?}: every block style lays out alike, only the painter differs");
    }
}

/// Consecutive modules of one kind share one block, two blank cells apart; its hue is its
/// first module's.
#[test]
fn a_block_holds_consecutive_modules_of_one_kind() {
    let g = styled(bar_with_right_modules(), BarStyle::Islands).grid("HDMI-A-1", 100);
    let status: Vec<&Group> = g.groups().iter().filter(|x| x.kind == GroupKind::Status).collect();
    assert_eq!(status, [&group(GroupKind::Status, 64, 81, Some(Hue::Blue))], "volume, network and battery: one block");
    let t: String = g.cells()[64..81].iter().filter(|c| c.ch != crate::grid::CONT).map(|c| c.ch).collect();
    assert_eq!(t, " \u{f0580} 55%  \u{f05a9}  \u{f0081} 80% ");
    insta::assert_snapshot!("blocks_right_modules_100", snapshot(&g));
    // Without volume, the block starts with the network: cyan.
    let mut b = styled(bar_with_right_modules(), BarStyle::Islands);
    b.config.modules.retain(|m| *m != ModuleId::Volume);
    let g = b.grid("HDMI-A-1", 100);
    assert_eq!(g.groups().iter().find(|x| x.kind == GroupKind::Status).map(|x| x.hue), Some(Some(Hue::Cyan)));
}

/// No block spans the free space: the window module's slot between two status modules splits
/// them into two blocks, and the right-hand side starts after it.
#[test]
fn the_free_space_splits_a_block() {
    let mut b = styled(bar_with_right_modules(), BarStyle::Islands);
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Volume, ModuleId::Window, ModuleId::Network, ModuleId::Clock];
    let mut e = example();
    e.window = None;
    b.apply(Msg::Compositor(Some(e)));
    let g = b.grid("HDMI-A-1", 100);
    let kinds: Vec<GroupKind> = g.groups().iter().map(|x| x.kind).collect();
    assert_eq!(kinds, [GroupKind::Workspaces, GroupKind::Status, GroupKind::Status, GroupKind::Clock], "{}", g.text());
    let (left, right) = (g.groups()[1], g.groups()[2]);
    assert!(left.end < g.split() && right.start >= g.split(), "{left:?} {right:?} split {}", g.split());
    assert_eq!((left.hue, right.hue), (Some(Hue::Blue), Some(Hue::Cyan)));
}

/// Every width, every style: the bar is exactly as wide as asked, the workspaces and the clock
/// stay whole (the block styles' spacing is counted before a module is removed: the clock's pad
/// and the margin end the bar), and the modules leave in REMOVAL's order.
#[test]
fn narrow_outputs_drop_modules_in_order_under_every_style() {
    // This bar's removable modules, in REMOVAL's order.
    let removable = [ModuleId::Window, ModuleId::Network, ModuleId::Volume, ModuleId::Battery, ModuleId::Launcher];
    assert!(removable.windows(2).all(|p| REMOVAL.iter().position(|m| *m == p[0]) < REMOVAL.iter().position(|m| *m == p[1])));
    for style in BarStyle::ALL {
        let b = styled(bar_with_right_modules(), style);
        let mut removed_at = Vec::new();
        for w in (45u16..=140).rev() {
            let g = b.grid("HDMI-A-1", w);
            let t = g.text();
            assert_eq!(g.width(), w, "{style:?}");
            let tail = if spacing(style) == Spacing::Blocks { " 14:05  " } else { " 14:05 " };
            assert!(t.ends_with(tail) && t.contains(" 4!····+ "), "{style:?} at {w}: {t}");
            let shown = |m: ModuleId| match m {
                ModuleId::Window => t.contains("Web"),
                m => g.zones().iter().any(|z| z.target == Target::Module(m)),
            };
            let gone = removable.iter().filter(|m| !shown(**m)).count();
            assert!(removable[gone..].iter().all(|m| shown(*m)), "{style:?} at {w}: out of REMOVAL's order: {t}");
            // A block always holds something; no lone border around blank cells.
            for x in g.groups() {
                let cells = &g.cells()[usize::from(x.start)..usize::from(x.end)];
                assert!(cells.iter().any(|c| c.ch != ' '), "{style:?} at {w}: an empty block {x:?}: {t}");
            }
            while removed_at.len() < gone {
                removed_at.push(w);
            }
        }
        assert_eq!(removed_at.len(), 4, "{style:?}: all but the launcher leave by 45 columns ({removed_at:?})");
    }
}

/// Left blocks end at or before the split and right blocks start at or after it, a long
/// (truncated) title included: powerline reads the side of a block from it.
#[test]
fn blocks_sit_on_one_side_of_the_split_even_with_a_truncated_title() {
    for style in BLOCK_STYLES {
        for title in ["Web page", &"very long title ".repeat(12)] {
            let mut b = styled(bar_with_right_modules(), style);
            let mut e = example();
            e.window.as_mut().unwrap().title = title.to_string();
            b.apply(Msg::Compositor(Some(e)));
            for w in [100u16, 80, 70] {
                let g = b.grid("HDMI-A-1", w);
                for x in g.groups() {
                    let left = matches!(x.kind, GroupKind::Launcher | GroupKind::Workspaces | GroupKind::Title);
                    assert!(
                        if left { x.end <= g.split() } else { x.start >= g.split() },
                        "{style:?} {w}: {x:?} split {} in {}",
                        g.split(),
                        g.text()
                    );
                }
            }
        }
    }
}

// ---- the privacy module ----

fn with_mic(mut b: Bar) -> Bar {
    let held = Indicator { names: vec!["firefox".into()], unnamed: false };
    b.privacy_at(
        &Msg::Sound(Some(SoundState { capture: Some(Capture { mic: held, ..Capture::default() }), ..SoundState::default() })),
        std::time::Instant::now(),
    );
    b
}

#[test]
fn privacy_is_its_own_block_with_no_hue_removed_just_before_power() {
    assert_eq!(ModuleId::Privacy.spec().group, GroupKind::Privacy);
    assert_eq!(ModuleId::Privacy.spec().hue, None, "its glyphs carry their own hues");
    assert_eq!(REMOVAL[REMOVAL.len() - 2..], [ModuleId::Privacy, ModuleId::Power]);
}

/// Its own block, always green, dropped on a narrow bar just before privacy.
#[test]
fn shield_is_its_own_block_dropped_just_before_privacy() {
    assert_eq!(ModuleId::Shield.spec().group, GroupKind::Shield);
    assert_eq!(ModuleId::Shield.spec().hue, Some(Hue::Green));
    assert_eq!(REMOVAL[REMOVAL.len() - 3..], [ModuleId::Shield, ModuleId::Privacy, ModuleId::Power]);
    let mut seen_without_shield = false;
    for style in BarStyle::ALL {
        let mut b = styled(bar_with_right_modules(), style);
        let at = b.config.modules.iter().position(|m| *m == ModuleId::Volume).unwrap();
        b.config.modules.splice(at..at, [ModuleId::Privacy, ModuleId::Shield]);
        b.shield =
            Some(crate::shield::ShieldView { state: Some(true), switching: false, failed_until: None, can_list: false, can_allow: false });
        let b = with_mic(b);
        for w in (45u16..=140).rev() {
            let g = b.grid("HDMI-A-1", w);
            let shown = |m: ModuleId| g.zones().iter().any(|z| z.target == Target::Module(m));
            if !shown(ModuleId::Shield) {
                seen_without_shield |= shown(ModuleId::Privacy);
            } else {
                assert!(shown(ModuleId::Privacy), "{style:?} at {w}: privacy outlives shield");
            }
        }
    }
    assert!(seen_without_shield, "a width where shield is gone and privacy stays");
}

/// On a narrow bar, whatever the style, a shown capture outlives the tray, the title and the
/// system indicators: only the launcher button and the power button leave after it.
#[test]
fn privacy_stays_on_a_narrow_bar_while_the_title_goes() {
    let mut seen_without_title = false;
    for style in BarStyle::ALL {
        let mut b = styled(bar_with_right_modules(), style);
        let at = b.config.modules.iter().position(|m| *m == ModuleId::Volume).unwrap();
        b.config.modules.insert(at, ModuleId::Privacy);
        let b = with_mic(b);
        for w in (45u16..=140).rev() {
            let g = b.grid("HDMI-A-1", w);
            let shown = |m: ModuleId| g.zones().iter().any(|z| z.target == Target::Module(m));
            let title = g.text().contains("Web");
            if !shown(ModuleId::Privacy) {
                assert!(!title && !shown(ModuleId::Volume) && !shown(ModuleId::Battery), "{style:?} at {w}: {}", g.text());
            }
            seen_without_title |= shown(ModuleId::Privacy) && !title;
        }
    }
    assert!(seen_without_title, "a width where the title is gone and privacy stays");
}

/// The failed count has a block of its own and leaves after the timer, before
/// the Shield glyph and the privacy indicators.
#[test]
fn failed_is_its_own_block_removed_after_the_timer_and_before_shield() {
    assert_eq!(ModuleId::Failed.spec().group, GroupKind::Failed);
    let at = |m| REMOVAL.iter().position(|x| *x == m).unwrap();
    assert_eq!(at(ModuleId::Failed), at(ModuleId::Timer) + 1);
    assert_eq!(at(ModuleId::Failed) + 1, at(ModuleId::Shield));
}

// ---- the media module ----

/// The media module between the title and the volume (its place in `ModuleId::ALL`).
fn bar_with_media(playing: bool) -> Bar {
    let mut b = bar();
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Media, ModuleId::Volume, ModuleId::Clock];
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    b.apply(Msg::Media(Some(MediaView { playing, title: "Bohemian Rhapsody".into() })));
    b
}

#[test]
fn media_sits_between_the_title_and_the_volume() {
    let g = bar_with_media(true).grid("HDMI-A-1", 100);
    let t = g.text();
    assert!(t.contains("  ·  \u{f040a} Bohemian Rhapsody  ·  \u{f057e} 84%  ·  Sun 4 Oct 14:05 "), "{t}");
    let media = Target::Module(ModuleId::Media);
    let (start, _) = tail_from(&g, &media);
    assert_eq!(g.target_at(start + 18), Some(&media), "its last cell: 19 cells, one zone");
    assert_eq!((g.target_at(start - 1), g.target_at(start + 19)), (None, None), "the separators around it");
    insta::assert_snapshot!("media_flat_100", snapshot(&g));
    let mut gone = bar_with_media(true);
    gone.apply(Msg::Media(None));
    let mut absent = bar_with_media(true);
    absent.config.modules.retain(|m| *m != ModuleId::Media);
    assert_eq!(gone.grid("HDMI-A-1", 100), absent.grid("HDMI-A-1", 100), "no player: exactly the bar without the module");
}

#[test]
fn media_is_a_block_of_its_own_in_purple() {
    for style in [BarStyle::Underline, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Translucent] {
        let g = styled(bar_with_media(true), style).grid("HDMI-A-1", 100);
        let kinds: Vec<GroupKind> = g.groups().iter().map(|x| x.kind).collect();
        assert_eq!(kinds, [GroupKind::Workspaces, GroupKind::Title, GroupKind::Media, GroupKind::Status, GroupKind::Clock], "{style:?}");
        let (media, status) = (g.groups()[2], g.groups()[3]);
        assert_eq!((media.hue, status.hue), (Some(Hue::Purple), Some(Hue::Blue)), "{style:?}: the volume opens a block of its own");
        let cells: String = g.cells()[usize::from(media.start)..usize::from(media.end)]
            .iter()
            .filter(|c| c.ch != crate::grid::CONT)
            .map(|c| c.ch)
            .collect();
        let want = if style == BarStyle::Underline { "\u{f040a} Bohemian Rhapsody" } else { " \u{f040a} Bohemian Rhapsody " };
        assert_eq!(cells, want, "{style:?}: the block wraps the mark and the title, its pads included");
    }
    let g = styled(bar_with_media(true), BarStyle::Islands).grid("HDMI-A-1", 100);
    insta::assert_snapshot!("media_islands_100", snapshot(&g));
}

#[test]
fn a_paused_media_block_takes_no_hue_even_with_pills() {
    for style in BLOCK_STYLES {
        for (playing, want) in [(true, Some(Hue::Purple)), (false, None)] {
            let mut b = styled(bar_with_media(playing), style);
            b.module_colors = ModuleColors::Pills;
            let g = b.grid("HDMI-A-1", 100);
            let media = g.groups().iter().find(|x| x.kind == GroupKind::Media).expect("a media block");
            assert_eq!(media.hue, want, "{style:?}, playing {playing}: a paused block stays neutral");
        }
    }
}

#[test]
fn media_leaves_after_the_tray_and_before_the_title() {
    let at = |m: ModuleId| REMOVAL.iter().position(|r| *r == m);
    assert!(at(ModuleId::Tray) < at(ModuleId::Media) && at(ModuleId::Media) < at(ModuleId::Window));
    let b = bar_with_media(true);
    let shown = |w: u16| {
        let t = b.grid("HDMI-A-1", w).text();
        (t.contains(segments::PLAY), t.contains("Web"))
    };
    let gone = (20u16..=100).rev().find(|&w| !shown(w).0).expect("narrow enough, the media leaves");
    assert_eq!(shown(gone), (false, true), "the title is still there at {gone} columns");
    for w in 20u16..=100 {
        let (media, title) = shown(w);
        assert!(!media || title, "at {w}: the media outlived the title");
    }
}

// ---- the bell ----

const THREE: NotifyView = NotifyView { unread: 3, dnd: false, serving: true };

/// The bell between the volume and the clock (its place in `ModuleId::ALL`).
fn bar_with_bell(view: NotifyView) -> Bar {
    let mut b = bar();
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Volume, ModuleId::Notifications, ModuleId::Clock];
    b.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    b.apply(Msg::Notifications(view));
    b
}

#[test]
fn the_bell_sits_between_the_volume_and_the_clock() {
    let g = bar_with_bell(THREE).grid("HDMI-A-1", 100);
    let t = g.text();
    assert!(t.contains("  ·  \u{f057e} 84%  ·  \u{f009a} 3  ·  Sun 4 Oct 14:05 "), "{t}");
    let bell = Target::Module(ModuleId::Notifications);
    let (start, _) = tail_from(&g, &bell);
    assert_eq!(g.target_at(start + 2), Some(&bell), "its last cell: 3 cells, one zone");
    assert_eq!((g.target_at(start - 1), g.target_at(start + 3)), (None, None), "the separators around it");
    insta::assert_snapshot!("bell_flat_100", snapshot(&g));
    let mut unheard = bar_with_bell(THREE);
    unheard.notifications = None;
    let mut absent = bar_with_bell(THREE);
    absent.config.modules.retain(|m| *m != ModuleId::Notifications);
    assert_eq!(unheard.grid("HDMI-A-1", 100), absent.grid("HDMI-A-1", 100), "nothing heard: exactly the bar without the module");
}

#[test]
fn the_bell_joins_the_status_block_and_takes_no_hue() {
    for style in [BarStyle::Underline, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Translucent] {
        let g = styled(bar_with_bell(THREE), style).grid("HDMI-A-1", 100);
        let kinds: Vec<GroupKind> = g.groups().iter().map(|x| x.kind).collect();
        assert_eq!(kinds, [GroupKind::Workspaces, GroupKind::Title, GroupKind::Status, GroupKind::Clock], "{style:?}");
        let status = g.groups()[2];
        assert_eq!(status.hue, Some(Hue::Blue), "{style:?}: the volume opens the block");
        let cells: String = g.cells()[usize::from(status.start)..usize::from(status.end)]
            .iter()
            .filter(|c| c.ch != crate::grid::CONT)
            .map(|c| c.ch)
            .collect();
        assert!(cells.contains('\u{f057e}') && (cells.ends_with("\u{f009a} 3") || cells.ends_with("\u{f009a} 3 ")), "{style:?}: {cells:?}");
    }
    for mc in [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills] {
        let mut b = styled(bar_with_bell(THREE), BarStyle::Islands);
        b.config.modules = vec![ModuleId::Workspaces, ModuleId::Window, ModuleId::Notifications, ModuleId::Volume, ModuleId::Clock];
        b.module_colors = mc;
        let g = b.grid("HDMI-A-1", 100);
        let status = g.groups().iter().find(|x| x.kind == GroupKind::Status).expect("a status block");
        assert_eq!(status.hue, None, "{mc:?}: the bell opens the block, which stays neutral");
    }
    insta::assert_snapshot!("bell_islands_100", snapshot(&styled(bar_with_bell(THREE), BarStyle::Islands).grid("HDMI-A-1", 100)));
}

#[test]
fn the_bell_is_never_dropped_on_a_narrow_output() {
    assert!(!REMOVAL.contains(&ModuleId::Notifications));
    let b = bar_with_bell(THREE);
    let volume_gone =
        (30u16..=100).rev().find(|&w| !b.grid("HDMI-A-1", w).text().contains('\u{f057e}')).expect("narrow enough, the volume leaves");
    assert!(volume_gone >= 50, "the volume leaves at {volume_gone} columns");
    // Down to the narrowest bar that still holds the whole clock: workspaces, bell and clock.
    let floor = (0u16..=100).find(|&w| b.grid("HDMI-A-1", w).text().contains("Sun 4 Oct 14:05")).expect("the clock fits somewhere");
    assert!(floor < 50, "the check goes below 50 columns: {floor}");
    let t = b.grid("HDMI-A-1", floor).text();
    assert!(!t.contains('\u{f057e}'), "at {floor} only the workspaces, the bell and the clock: {t}");
    for w in floor..=100 {
        let t = b.grid("HDMI-A-1", w).text();
        assert!(t.contains("\u{f009a} 3"), "{w}: the bell stays: {t}");
    }
}

/// Below the minimum width the clipped text may vanish: no block is left holding only blanks.
#[test]
fn no_block_holds_only_blanks_at_any_width() {
    for style in BLOCK_STYLES {
        let b = styled(bar_with_right_modules(), style);
        for w in 0u16..45 {
            let g = b.grid("HDMI-A-1", w);
            for x in g.groups() {
                let cells = &g.cells()[usize::from(x.start)..usize::from(x.end)];
                assert!(cells.iter().any(|c| c.ch != ' '), "{style:?} at {w}: {x:?} in {:?}", g.text());
            }
        }
    }
}

/// The OSD is a module without a segment: the bar is exactly the same without it.
#[test]
fn the_osd_module_draws_nothing_in_the_bar() {
    let mut with = bar();
    assert!(with.config.modules.contains(&ModuleId::Osd), "on by default");
    with.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    let mut without = with.clone();
    without.config.modules.retain(|m| *m != ModuleId::Osd);
    // Flat, and a block style: no empty block either.
    for style in [BarStyle::Flat, BarStyle::Islands] {
        let (with, without) = (styled(with.clone(), style), styled(without.clone(), style));
        for width in [100, 60, 40] {
            assert_eq!(
                with.grid("HDMI-A-1", width),
                without.grid("HDMI-A-1", width),
                "{style:?}, {width} cells: no segment, no separator, no gap"
            );
        }
    }
}

/// The quick panel is a popup of its own: its module draws nothing in the bar.
#[test]
fn the_quick_module_draws_nothing_in_the_bar() {
    let mut with = bar();
    assert!(with.config.modules.contains(&ModuleId::Quick), "on by default");
    with.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    let mut without = with.clone();
    without.config.modules.retain(|m| *m != ModuleId::Quick);
    for style in [BarStyle::Flat, BarStyle::Islands] {
        let (with, without) = (styled(with.clone(), style), styled(without.clone(), style));
        for width in [100, 60, 40] {
            assert_eq!(with.grid("HDMI-A-1", width), without.grid("HDMI-A-1", width), "{style:?}, {width} cells");
        }
    }
    assert_eq!(ModuleId::Quick.spec().hue, None);
}

/// The module reads and changes the screens; it draws nothing in the bar, has
/// no popup, no hue, no tab, and starts no source.
#[test]
fn the_displays_row_draws_nothing() {
    let spec = ModuleId::Displays.spec();
    assert!(spec.segment.is_none() && spec.popup.is_none() && spec.hue.is_none() && spec.tab.is_none());
    assert_eq!(spec.needs, crate::modules::Needs::NONE);
    let mut with = bar();
    assert!(with.config.modules.contains(&ModuleId::Displays), "on by default");
    with.apply(Msg::Sound(Some(SoundState { sink: Some(Volume { percent: 84, muted: false }), ..SoundState::default() })));
    let mut without = with.clone();
    without.config.modules.retain(|m| *m != ModuleId::Displays);
    for style in [BarStyle::Flat, BarStyle::Islands] {
        let (with, without) = (styled(with.clone(), style), styled(without.clone(), style));
        for width in [100, 60, 40] {
            assert_eq!(with.grid("HDMI-A-1", width), without.grid("HDMI-A-1", width), "{style:?}, {width} cells");
        }
    }
}

#[test]
fn flat_and_underline_separate_with_a_dim_dot() {
    for style in [BarStyle::Flat, BarStyle::Underline] {
        let g = styled(bar(), style).grid("HDMI-A-1", 100);
        let (t, st): (Vec<char>, Vec<char>) = (g.text().chars().collect(), g.styles().chars().collect());
        // The launcher button is one cell after the margin; the separator follows it.
        let at = 2;
        assert_eq!(t[at..at + 5].iter().collect::<String>(), "  \u{b7}  ", "{style:?}: {}", g.text());
        assert_eq!(st[at..at + 5].iter().collect::<String>(), "mmmmm", "{style:?}");
        assert!(!g.text().contains('\u{2502}'), "no full-height bar left");
    }
    let t = styled(bar(), BarStyle::Islands).grid("HDMI-A-1", 100).text();
    assert!(!t.contains("  \u{b7}  "), "no separator under islands: {t}");
}

// ---- the timer chip ----

fn with_timer(mut b: Bar) -> Bar {
    let chip = crate::timer::TimerChip { glyph: crate::timer::TIMER, text: Some("25m".into()), look: crate::timer::ChipLook::Running };
    b.apply(Msg::Timer(Some(chip)));
    b
}

#[test]
fn the_timer_is_a_block_of_its_own_before_the_clock() {
    assert_eq!(ModuleId::Timer.spec().group, GroupKind::Timer);
    assert_eq!(ModuleId::Timer.spec().hue, Some(Hue::Yellow));
    let at = |m| ModuleId::ALL.iter().position(|x| *x == m).unwrap();
    assert_eq!(at(ModuleId::Timer) + 1, at(ModuleId::Clock), "just before the clock");
    assert_eq!(
        REMOVAL[REMOVAL.len() - 6..],
        [ModuleId::Launcher, ModuleId::Timer, ModuleId::Failed, ModuleId::Shield, ModuleId::Privacy, ModuleId::Power]
    );
    // Its own yellow block under a block style.
    let mut b = with_timer(bar());
    b.config.modules = vec![ModuleId::Workspaces, ModuleId::Timer, ModuleId::Clock];
    b.module_colors = ModuleColors::Pills;
    let g = styled(b, BarStyle::Islands).grid("HDMI-A-1", 60);
    let kinds: Vec<(GroupKind, Option<Hue>)> = g.groups().iter().map(|x| (x.kind, x.hue)).collect();
    assert_eq!(kinds, [(GroupKind::Workspaces, None), (GroupKind::Timer, Some(Hue::Yellow)), (GroupKind::Clock, None)]);
    // An idle chip's block takes no hue; a running one does.
    let mut idle = bar();
    idle.config.modules = vec![ModuleId::Workspaces, ModuleId::Timer, ModuleId::Clock];
    idle.module_colors = ModuleColors::Pills;
    idle.apply(Msg::Timer(Some(crate::timer::TimerChip { glyph: crate::timer::TIMER, text: None, look: crate::timer::ChipLook::Idle })));
    let g = styled(idle, BarStyle::Islands).grid("HDMI-A-1", 60);
    assert_eq!(g.groups().iter().find(|x| x.kind == GroupKind::Timer).map(|x| x.hue), Some(None));
    // Nothing without a chip: exactly the bar without the module.
    let mut gone = bar();
    gone.config.modules = vec![ModuleId::Workspaces, ModuleId::Timer, ModuleId::Clock];
    let mut absent = bar();
    absent.config.modules = vec![ModuleId::Workspaces, ModuleId::Clock];
    assert_eq!(gone.grid("HDMI-A-1", 60), absent.grid("HDMI-A-1", 60));
}

/// On a narrow bar the timer leaves after the launcher and before the privacy indicators.
#[test]
fn the_timer_leaves_after_the_launcher_and_before_privacy() {
    for style in BarStyle::ALL {
        let mut b = styled(bar_with_right_modules(), style);
        let at = b.config.modules.iter().position(|m| *m == ModuleId::Clock).unwrap();
        b.config.modules.splice(at..at, [ModuleId::Timer, ModuleId::Privacy]);
        b.config.modules.push(ModuleId::Power);
        let b = with_timer(with_mic(b));
        let (mut timer_gone, mut privacy_gone) = (false, false);
        for w in (30u16..=140).rev() {
            let g = b.grid("HDMI-A-1", w);
            let shown = |m: ModuleId| g.zones().iter().any(|z| z.target == Target::Module(m));
            if !shown(ModuleId::Timer) {
                timer_gone = true;
                assert!(!shown(ModuleId::Launcher), "{style:?} at {w}: the launcher leaves first");
            }
            if !shown(ModuleId::Privacy) {
                privacy_gone = true;
            }
            if shown(ModuleId::Timer) {
                assert!(shown(ModuleId::Privacy), "{style:?} at {w}: the timer leaves before the privacy indicators");
            }
        }
        assert!(timer_gone && privacy_gone, "{style:?}: narrow enough to lose both");
    }
}

/// The keyboard module leaves a narrow bar right after the title: it says what the user set, not
/// what changed, so it goes before the status icons.
#[test]
fn the_keyboard_leaves_right_after_the_title() {
    assert_eq!(REMOVAL.len(), 15);
    let at = |m| REMOVAL.iter().position(|x| *x == m).unwrap();
    assert_eq!(at(ModuleId::Keyboard), at(ModuleId::Window) + 1);
    assert_eq!(at(ModuleId::Bluetooth), at(ModuleId::Keyboard) + 1, "then the system indicators");
}
