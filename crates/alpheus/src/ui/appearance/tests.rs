use super::super::testing::draw;
use super::*;
use crate::app::action::{Action, Tab};
use crate::app::appearance::tests::open;
use alpheus_commun::theme::Rgb;
use alpheus_commun::theme::contrast::ratio;
use alpheus_commun::theme::presets::preset;
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

const WALL: &str = "bg = \"#101820\"\nfg = \"#e8eef2\"\naccent = \"#ff8800\"\nmuted = \"#7a8894\"\nsurface = \"#1b2631\"\nerror = \"#ff5555\"\nok = \"#50fa7b\"\n";

fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// The rendered buffer, 100×24.
fn buffer(app: &crate::app::App) -> ratatui::buffer::Buffer {
    let mut t = Terminal::new(TestBackend::new(100, 24)).unwrap();
    t.draw(|f| {
        super::super::render(f, app);
    })
    .unwrap();
    t.backend().buffer().clone()
}

/// (fg, bg) of the first cell showing `sym` in row `y`, in the preview column (from x = 33).
fn cell(b: &ratatui::buffer::Buffer, y: u16, sym: &str) -> (Color, Color) {
    let x = (33..100).find(|x| b[(*x, y)].symbol() == sym).unwrap_or_else(|| panic!("{sym:?} not in row {y}"));
    (b[(x, y)].fg, b[(x, y)].bg)
}

fn goto(a: &mut crate::app::App, key: &str) {
    for _ in 0..12 {
        if a.appearance.cursor_entry().is_some_and(|e| e.key == key) {
            return;
        }
        a.on_key_action(Action::Down);
    }
    panic!("{key} not reached");
}

#[test]
fn dark_preset_under_the_cursor() {
    let (mut a, _dir) = open("snap-dark", &[("theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\n")]);
    goto(&mut a, "tokyo-night");
    insta::assert_snapshot!("appearance_dark", draw(&a, 100, 24).0.join("\n"));
}

#[test]
fn light_mode_and_the_focus_on_the_mode_row() {
    let (mut a, _dir) = open("snap-light", &[("theme.toml", "theme = \"gruvbox\"\nmode = \"light\"\n")]);
    crate::app::appearance::tests::to_row(&mut a, 1);
    insta::assert_snapshot!("appearance_light", draw(&a, 100, 24).0.join("\n"));
}

#[test]
fn the_wallpaper_entry_with_a_fixture_colors_toml() {
    let (a, _dir) = open("snap-wall", &[("colors.toml", WALL)]);
    insta::assert_snapshot!("appearance_wallpaper", draw(&a, 100, 24).0.join("\n"));
}

#[test]
fn dark_only_tags_and_the_light_mode_fallback() {
    let (mut a, _dir) = open("snap-nord", &[("theme.toml", "theme = \"nord\"\nmode = \"light\"\n")]);
    a.on_key_action(Action::Down);
    insta::assert_snapshot!("appearance_dark_only", draw(&a, 100, 24).0.join("\n"));
    let rows = draw(&a, 100, 24).0;
    assert!(rows[2].contains("Preview — Dracula, dark"), "the header says what is drawn: {:?}", rows[2]);
    assert_eq!(rows.iter().filter(|r| r.contains("dark only")).count(), 2, "Nord and Dracula only");
}

#[test]
fn the_preview_wears_the_roles_of_the_theme_under_the_cursor() {
    let (mut a, _dir) = open("colors", &[("theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\n")]);
    goto(&mut a, "tokyo-night");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    // The bar's content row (body y = 2, frame at 3, content at 4).
    assert_eq!(cell(&b, 4, "1"), (rgb(p.on_accent), rgb(p.accent_container)), "active workspace");
    assert_eq!(cell(&b, 4, "2"), (rgb(p.fg), rgb(p.pill_bg)), "workspace visible on another output");
    assert_eq!(cell(&b, 4, "!").0, rgb(p.urgent), "urgent workspace");
    assert_eq!(cell(&b, 4, "n"), (rgb(p.fg), rgb(p.bar_bg)), "window title");
    assert_eq!(cell(&b, 4, "▮").0, rgb(p.warn), "low battery");
    assert_eq!(cell(&b, 4, "5").0, rgb(p.warn), "its number too");
    assert_eq!(cell(&b, 4, ":").0, rgb(p.accent), "clock");
    // The menu (frame at 7, rows 8..=10).
    assert_eq!(cell(&b, 8, "O"), (rgb(p.fg), rgb(p.selection)), "selected row");
    assert_eq!(cell(&b, 9, "✓").0, rgb(p.accent2), "check");
    assert_eq!(cell(&b, 10, "L"), (rgb(p.fg), rgb(p.popup_bg)), "plain row");
    assert_eq!(cell(&b, 7, "┌"), (rgb(p.border_over(p.popup_bg)), rgb(p.popup_bg)), "frame");
    // The TUI itself still wears the applied theme, not the previewed one.
    assert_eq!(a.theme.bg, rgb(preset("catppuccin").unwrap().dark.bar_bg));
    assert_ne!(rgb(p.bar_bg), a.theme.bg, "the test bites only if the two themes differ");
}

/// The mini bar's modules under each setting, on the previewed theme's hues.
#[test]
fn the_mini_bar_tints_its_modules_by_the_selected_module_colours() {
    let (mut a, _dir) = open("preview-colors", &[("theme.toml", "theme = \"tokyo-night\"\nmode = \"dark\"\n")]);
    let p = preset("tokyo-night").unwrap().dark;
    let (fg, bar) = (rgb(p.fg), rgb(p.bar_bg));
    let mut seen = Vec::new();
    for (mc, icons_tinted, text_tinted) in
        [(ModuleColors::None, false, false), (ModuleColors::Icons, true, false), (ModuleColors::Full, true, true)]
    {
        a.appearance.module_colors = mc;
        a.on_key_action(Action::Home);
        goto(&mut a, "tokyo-night");
        let b = buffer(&a);
        let pick = |sym, hue: Rgb, tinted: bool| {
            let want = if tinted { rgb(hue) } else { fg };
            assert_eq!(cell(&b, 4, sym), (want, bar), "{sym} under {mc:?}");
        };
        pick("♪", p.hues.blue, icons_tinted);
        pick("8", p.hues.blue, text_tinted);
        pick("≈", p.hues.cyan, icons_tinted);
        pick("▤", p.hues.yellow, icons_tinted);
        pick("6", p.hues.yellow, text_tinted);
        assert_eq!(cell(&b, 4, "▮").0, rgb(p.warn), "a state wins over the hue under {mc:?}");
        seen.push(buffer(&a));
    }
    assert_ne!(seen[0], seen[1]);
    assert_ne!(seen[1], seen[2]);
}

#[test]
fn the_module_colours_row_is_drawn_with_its_four_values_and_regions() {
    let (a, _dir) = open("colors-row", &[("theme.toml", "module_colors = \"full\"\n")]);
    let (rows, hits) = draw(&a, 100, 24);
    let row = rows.iter().find(|r| r.contains("Module colours")).expect("the row");
    assert!(row.contains("None") && row.contains("Icons") && row.contains("Full") && row.contains("Pills"), "{row:?}");
    for m in ModuleColors::ALL {
        assert!(hits.find(Target::AppearanceColors(m)).is_some(), "{m:?}");
    }
    let mode_y = hits.find(Target::AppearanceMode(ModeSetting::ALL[0])).unwrap().y;
    assert_eq!(hits.find(Target::AppearanceColors(ModuleColors::ALL[0])).unwrap().y, mode_y + 1, "right under Mode");
    // The loaded value is the highlighted one.
    let b = buffer(&a);
    let full = hits.find(Target::AppearanceColors(ModuleColors::Full)).unwrap();
    let none = hits.find(Target::AppearanceColors(ModuleColors::None)).unwrap();
    assert_ne!(b[(full.x + 1, full.y)].bg, b[(none.x + 1, none.y)].bg, "Full is highlighted, None is not");
    assert!(rows[23].contains("Enter Apply"), "{:?}", rows[23]);
}

#[test]
fn the_footer_no_longer_sends_tab_inside_the_tab() {
    let (mut a, _dir) = open("footer", &[]);
    let last = |a: &crate::app::App| draw(a, 100, 24).0[23].clone();
    assert!(last(&a).contains("Enter Apply") && !last(&a).contains("Tab Mode"), "{:?}", last(&a));
    crate::app::appearance::tests::to_row(&mut a, 1);
    assert!(last(&a).contains("Choose") && !last(&a).contains("Tab Colours"), "{:?}", last(&a));
}

#[test]
fn the_header_names_the_theme_and_the_mode() {
    let (mut a, _dir) = open("header", &[("theme.toml", "mode = \"light\"\n")]);
    assert!(draw(&a, 100, 24).0[2].contains("Preview — Catppuccin, light"));
    a.on_key_action(Action::Down);
    assert!(draw(&a, 100, 24).0[2].contains("Preview — Gruvbox, light"));
}

#[test]
fn the_regions_and_the_tab_title() {
    let (a, _dir) = open("hits", &[]);
    let (rows, hits) = draw(&a, 100, 24);
    assert!(rows[0].contains(" 7 Theme "), "{:?}", rows[0]);
    assert!(hits.find(Target::Tab(Tab::Appearance)).is_some());
    assert_eq!(hits.find(Target::AppearanceTheme(3)).map(|r| (r.y, r.width)), Some((6, 30)));
    for m in ModeSetting::ALL {
        assert!(hits.find(Target::AppearanceMode(m)).is_some());
    }
    assert!(rows[23].contains("Enter Apply"), "{:?}", rows[23]);
}

#[test]
fn small_windows_never_panic_and_a_narrow_one_drops_the_preview() {
    let (a, _dir) = open("small", &[]);
    for (w, h) in [(30, 8), (40, 12), (60, 14), (80, 24), (31, 9)] {
        draw(&a, w, h);
    }
    let all = draw(&a, 50, 24).0.join("\n");
    assert!(!all.contains("Preview"), "{all}");
    assert!(all.contains("Mode"));
}

#[test]
fn short_terminals_scroll_the_list_and_never_expose_a_hidden_hit() {
    let (mut a, _dir) = open("short", &[]);
    for h in 8..=20u16 {
        let body = Rect::new(1, 2, 98, h - 4);
        for cursor in 0..a.appearance.entries.len() {
            a.appearance.cursor = cursor;
            let (rows, hits) = draw(&a, 100, h);
            for i in 0..a.appearance.entries.len() {
                if let Some(r) = hits.find(Target::AppearanceTheme(i)) {
                    assert_eq!(r.intersection(body), r, "theme {i} outside the body at height {h}");
                }
            }
            for m in ModeSetting::ALL {
                if let Some(r) = hits.find(Target::AppearanceMode(m)) {
                    assert_eq!(r.intersection(body), r, "mode outside the body at height {h}");
                }
            }
            for m in ModuleColors::ALL {
                if let Some(r) = hits.find(Target::AppearanceColors(m)) {
                    assert_eq!(r.intersection(body), r, "module colours outside the body at height {h}");
                }
            }
            for s in STYLES {
                if let Some(r) = hits.find(Target::AppearanceStyle(s)) {
                    assert_eq!(r.intersection(body), r, "style outside the body at height {h}");
                }
            }
            // What a chip's region covers is that chip: never the preview drawn over it.
            let chips = ModeSetting::ALL.iter().map(|m| (Target::AppearanceMode(*m), mode_label(*m)));
            let chips = chips.chain(ModuleColors::ALL.iter().map(|m| (Target::AppearanceColors(*m), colors_label(*m))));
            for (t, label) in chips.chain(STYLES.iter().map(|s| (Target::AppearanceStyle(*s), style_label(*s)))) {
                if let Some(r) = hits.find(t) {
                    let shown: String = rows[usize::from(r.y)].chars().skip(usize::from(r.x)).take(usize::from(r.width)).collect();
                    let want: String = format!(" {label} ").chars().take(usize::from(r.width)).collect();
                    assert_eq!(shown, want, "{t:?} at height {h}, cursor {cursor}");
                }
            }
            let row = hits.find(Target::AppearanceTheme(cursor));
            assert!(row.is_some(), "cursor {cursor} not drawn at height {h}");
            if let Some(r) = row {
                assert!(rows[usize::from(r.y)].contains('❯'), "{:?}", rows[usize::from(r.y)]);
            }
            if h >= 9 {
                assert!(hits.find(Target::AppearanceMode(ModeSetting::ALL[0])).is_some(), "mode row visible at height {h}");
                assert!(hits.find(Target::AppearanceColors(ModuleColors::ALL[0])).is_some(), "module colours row visible at height {h}");
                assert!(hits.find(Target::AppearanceStyle(STYLES[0])).is_some(), "style row visible at height {h}");
            }
        }
    }
}

#[test]
fn the_style_row_is_drawn_under_the_colours_with_six_values_and_regions() {
    let (a, _dir) = open("style-row", &[("theme.toml", "style = \"powerline\"\n")]);
    let (rows, hits) = draw(&a, 100, 24);
    let row = rows.iter().find(|r| r.starts_with(" Style")).expect("the row");
    for label in ["Flat", "Islands", "Powerline", "Floating", "Underline", "Translucent"] {
        assert!(row.contains(label), "{label} in {row:?}");
    }
    for s in STYLES {
        assert!(hits.find(Target::AppearanceStyle(s)).is_some(), "{s:?}");
    }
    let colors_y = hits.find(Target::AppearanceColors(ModuleColors::ALL[0])).unwrap().y;
    assert_eq!(hits.find(Target::AppearanceStyle(STYLES[0])).unwrap().y, colors_y + 1, "right under Module colours");
    let b = buffer(&a);
    let on = hits.find(Target::AppearanceStyle(BarStyle::Powerline)).unwrap();
    let off = hits.find(Target::AppearanceStyle(BarStyle::Flat)).unwrap();
    assert_ne!(b[(on.x + 1, on.y)].bg, b[(off.x + 1, off.y)].bg, "the loaded value is highlighted");
    // 80 columns: chip against chip, all six inside the body.
    let (rows, hits) = draw(&a, 80, 24);
    assert!(rows.iter().any(|r| r.contains("Translucent")), "not clipped at 80 columns");
    let last = hits.find(Target::AppearanceStyle(BarStyle::Translucent)).unwrap();
    assert!(last.right() <= 79, "{last:?}");
}

/// x of the first cell showing `sym` in row `y` of the preview column (from x = 33).
fn find(b: &ratatui::buffer::Buffer, y: u16, sym: &str) -> u16 {
    (33..100).find(|x| b[(*x, y)].symbol() == sym).unwrap_or_else(|| panic!("{sym:?} not in row {y}"))
}

/// The Appearance tab with catppuccin APPLIED (the tab's own background) and Tokyo Night under
/// the cursor (the previewed roles), in `style` and `colors`: the two backgrounds differ, so a
/// transparent cell is told apart from the bar's.
fn styled(name: &str, style: &str, colors: &str) -> crate::app::App {
    let toml = format!("theme = \"catppuccin\"\nmode = \"dark\"\nstyle = \"{style}\"\nmodule_colors = \"{colors}\"\n");
    let (mut a, _dir) = open(name, &[("theme.toml", toml.as_str())]);
    goto(&mut a, "tokyo-night");
    a
}

#[test]
fn islands_previews_rounded_blocks_with_transparent_gaps() {
    let a = styled("prev-islands", "islands", "icons");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    let tab_bg = a.theme.bg;
    assert_ne!(tab_bg, rgb(p.bar_bg), "the test bites only if the two backgrounds differ");
    let n = find(&b, 4, "n"); // notes.md: the title's block
    assert_eq!(b[(n, 4)].bg, rgb(p.pill_bg), "a block is pill_bg");
    assert_eq!((b[(n - 1, 4)].symbol(), b[(n - 1, 4)].fg, b[(n - 1, 4)].bg), ("▐", rgb(p.pill_bg), tab_bg), "its rounded end");
    assert_eq!(b[(n - 2, 4)].bg, tab_bg, "the gap shows what is behind the bar");
    assert_eq!(cell(&b, 4, "1"), (rgb(p.on_accent), rgb(p.accent_container)), "the active workspace keeps its colours");
    assert_eq!(b[(find(&b, 4, "▌"), 4)].fg, rgb(p.pill_bg), "the closing end");
}

#[test]
fn translucent_previews_the_blocks_on_the_bar_colour() {
    let a = styled("prev-translucent", "translucent", "icons");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    let n = find(&b, 4, "n");
    assert_eq!(b[(n, 4)].bg, rgb(p.pill_bg));
    assert_eq!((b[(n - 1, 4)].fg, b[(n - 1, 4)].bg), (rgb(p.pill_bg), rgb(p.bar_bg)), "the end over bar_bg");
    assert_eq!(b[(n - 2, 4)].bg, rgb(p.bar_bg), "the gap is the bar (cells cannot blend)");
}

#[test]
fn powerline_previews_full_blocks_and_arrows() {
    let a = styled("prev-powerline", "powerline", "icons");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    // Left (Mae, 2026-10-08): workspaces on pill_bg, an arrow into the title's block (raised),
    // and one out of it into the bar.
    assert_eq!(cell(&b, 4, "\u{e0b0}"), (rgb(p.pill_bg), rgb(p.raised)), "workspaces -> title");
    let second = (find(&b, 4, "\u{e0b0}") + 1..100).find(|x| b[(*x, 4)].symbol() == "\u{e0b0}").expect("a second arrow");
    assert_eq!((b[(second, 4)].fg, b[(second, 4)].bg), (rgb(p.raised), rgb(p.bar_bg)), "title -> bar");
    assert_eq!(cell(&b, 4, "3"), (rgb(p.urgent), rgb(p.pill_bg)), "inactive workspaces inside the block, in their colours");
    assert_eq!(cell(&b, 4, "1"), (rgb(p.on_accent), rgb(p.accent_container)), "the active workspace keeps its accent fill");
    assert_ne!(p.accent_container, p.pill_bg, "and stands out on its block");
    // Right: the first arrow comes out of the bar into the status block (pill_bg, index 2).
    assert_eq!(cell(&b, 4, "\u{e0b2}"), (rgb(p.pill_bg), rgb(p.bar_bg)), "bar -> status");
    assert_eq!(cell(&b, 4, "♪").1, rgb(p.pill_bg));
    assert_eq!(cell(&b, 4, "▤").1, rgb(p.raised), "resources");
    assert_eq!(cell(&b, 4, ":").1, rgb(p.pill_bg), "the clock");
}

/// The daemon's rule with the title dropped (80 columns): the alternation counts the blocks
/// shown, from the workspaces, so the status block right after them is raised.
#[test]
fn powerline_alternation_follows_the_blocks_shown_when_the_title_is_dropped() {
    let a = styled("prev-powerline-80", "powerline", "icons");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer_w(&a, 80);
    let row: String = (33..80).map(|x| b[(x, 4)].symbol().to_string()).collect();
    assert!(!row.contains("not"), "the title is dropped: {row:?}");
    assert_eq!(cell(&b, 4, "3").1, rgb(p.pill_bg), "the workspaces");
    assert_eq!(cell(&b, 4, "♪").1, rgb(p.raised), "the block after them differs");
    assert_eq!(cell(&b, 4, "▤").1, rgb(p.pill_bg));
    assert_eq!(cell(&b, 4, ":").1, rgb(p.raised));
}

#[test]
fn floating_previews_one_rounded_bar_over_what_is_behind() {
    let a = styled("prev-floating", "floating", "icons");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    let end = find(&b, 4, "▐");
    assert_eq!((b[(end, 4)].fg, b[(end, 4)].bg), (rgb(p.bar_bg), a.theme.bg), "the bar's rounded left end");
    assert_eq!(b[(end + 1, 4)].bg, rgb(p.bar_bg));
    assert_eq!(cell(&b, 4, "n").1, rgb(p.bar_bg), "blocks are not filled without pills");
    assert_eq!(b[(end, 3)].bg, a.theme.bg, "the margin above the bar shows what is behind");
    // Under pills, only the hued blocks are drawn inside the bar (task 6).
    let a = styled("prev-floating-pills", "floating", "pills");
    let b = buffer(&a);
    assert_eq!(cell(&b, 4, "♪").1, rgb(p.hues.blue), "pills: the status block in its hue");
    assert_eq!(cell(&b, 4, "n").1, rgb(p.bar_bg), "pills: the title has no hue, no block");
}

#[test]
fn underline_previews_a_rule_under_the_active_workspace_and_the_hued_blocks() {
    let a = styled("prev-underline", "underline", "icons");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    let one = find(&b, 4, "1");
    assert_eq!((b[(one, 5)].symbol(), b[(one, 5)].fg), ("━", rgb(p.accent)), "under the active workspace");
    // Mae, 2026-10-08: no accent fill, its text in accent on the block.
    assert_eq!(cell(&b, 4, "1"), (rgb(p.accent), rgb(p.pill_bg)), "the active workspace: accent text, no fill");
    assert_ne!(p.accent, p.pill_bg, "the test bites only if the two differ");
    assert_eq!(b[(find(&b, 4, "♪"), 5)].fg, rgb(p.hues.blue), "under the sound block, its hue");
    assert_eq!(cell(&b, 4, "n").1, rgb(p.pill_bg), "blocks on pill_bg");
    let a = styled("prev-underline-none", "underline", "none");
    let b = buffer(&a);
    assert_eq!((33..100).filter(|x| b[(*x, 5)].symbol() == "━").count(), 1, "module colours none: the workspace only");
    // Pills under underline is full (spec § 1): the hue rules stay, the text takes its hue on pill_bg.
    let a = styled("prev-underline-pills", "underline", "pills");
    let b = buffer(&a);
    assert_eq!(b[(find(&b, 4, "♪"), 5)].fg, rgb(p.hues.blue), "pills: the sound block's rule in its hue");
    assert_eq!(cell(&b, 4, "8"), (rgb(p.hues.blue), rgb(p.pill_bg)), "pills: the text in its hue, as under full");
}

/// A workspace visible on another output ("2"), as the bar paints it (fix wave B): `raised` on
/// a `pill_bg` block (islands, translucent, powerline, underline, and islands under pills: the
/// workspaces have no hue), `pill_bg` on the bar itself (floating); its padding stays the block's.
#[test]
fn a_visible_workspace_is_raised_on_a_pill_bg_block() {
    let p = preset("tokyo-night").unwrap().dark;
    assert_ne!(p.raised, p.pill_bg, "the test bites only if the two differ");
    let cases = [
        ("islands", "icons", p.raised, p.pill_bg),
        ("translucent", "icons", p.raised, p.pill_bg),
        ("powerline", "icons", p.raised, p.pill_bg),
        ("underline", "icons", p.raised, p.pill_bg),
        ("islands", "pills", p.raised, p.pill_bg),
        ("floating", "icons", p.pill_bg, p.bar_bg),
    ];
    for (style, colors, want, block) in cases {
        let a = styled(&format!("prev-visible-{style}-{colors}"), style, colors);
        let b = buffer(&a);
        assert_eq!(cell(&b, 4, "2"), (rgb(p.fg), rgb(want)), "{style} {colors}: the visible workspace");
        let two = find(&b, 4, "2");
        assert_eq!(b[(two + 1, 4)].bg, rgb(block), "{style} {colors}: the cell after it, the block's");
    }
}

/// Under flat the visible workspace's label is the daemon's `Style::SURFACE`: fg on `pill_bg`,
/// its padding included, on the bar's background.
#[test]
fn under_flat_a_visible_workspace_is_on_pill_bg() {
    let p = preset("tokyo-night").unwrap().dark;
    assert_ne!(p.pill_bg, p.bar_bg, "the test bites only if the two differ");
    let a = styled("prev-visible-flat", "flat", "icons");
    let b = buffer(&a);
    assert_eq!(cell(&b, 4, "2"), (rgb(p.fg), rgb(p.pill_bg)), "the visible workspace");
    let two = find(&b, 4, "2");
    assert_eq!((b[(two - 1, 4)].bg, b[(two + 1, 4)].bg), (rgb(p.pill_bg), rgb(p.pill_bg)), "its padding");
    assert_eq!(b[(two + 2, 4)].bg, rgb(p.bar_bg), "the next label, on the bar");
    assert!(!b[(two, 4)].modifier.contains(Modifier::BOLD), "not bold, as SURFACE");
}

#[test]
fn pills_fill_each_block_with_its_hue_and_pick_the_text_by_contrast() {
    let a = styled("prev-pills", "islands", "pills");
    let p = preset("tokyo-night").unwrap().dark;
    let b = buffer(&a);
    let blue = p.hues.blue;
    let want = if ratio(p.bar_bg, blue) >= ratio(p.fg, blue) { p.bar_bg } else { p.fg };
    assert_ne!(want, p.fg, "on Tokyo Night's blue, bar_bg contrasts more: the pick bites");
    assert_eq!(cell(&b, 4, "♪"), (rgb(want), rgb(blue)), "the status block on its first module's hue");
    assert_eq!(cell(&b, 4, "≈").0, rgb(want), "every plain run of the block");
    assert_eq!(cell(&b, 4, "▮"), (rgb(p.warn), rgb(blue)), "a state wins over the pick");
    assert_eq!(cell(&b, 4, ":"), (rgb(p.accent), rgb(p.pill_bg)), "no hue: pill_bg, colours kept");
}

#[test]
fn under_flat_pills_previews_like_full() {
    let a = styled("prev-flat-pills", "flat", "pills");
    let p = preset("tokyo-night").unwrap().dark;
    assert_eq!(cell(&buffer(&a), 4, "8"), (rgb(p.hues.blue), rgb(p.bar_bg)), "the text takes its hue");
}

#[test]
fn each_style_snapshot() {
    for style in ["flat", "islands", "powerline", "floating", "underline", "translucent"] {
        let a = styled(&format!("snap-{style}"), style, "icons");
        let rows = draw(&a, 100, 24).0;
        let preview: Vec<String> = rows[3..=5].iter().map(|r| r.chars().skip(33).take(54).collect()).collect();
        insta::assert_snapshot!(format!("appearance_style_{style}"), preview.join("\n"));
    }
}

/// The rendered buffer, `w`×24.
fn buffer_w(app: &crate::app::App, w: u16) -> ratatui::buffer::Buffer {
    let mut t = Terminal::new(TestBackend::new(w, 24)).unwrap();
    t.draw(|f| {
        super::super::render(f, app);
    })
    .unwrap();
    t.backend().buffer().clone()
}

/// 80 columns, the reference width: the frame is 46 wide, too narrow for the whole line under a
/// block style. The title gives way, never the modules the preview is there to show.
#[test]
fn at_80_columns_every_block_style_keeps_its_modules_and_clock() {
    for style in ["islands", "powerline", "floating", "underline", "translucent"] {
        let a = styled(&format!("80-{style}"), style, "pills");
        let b = buffer_w(&a, 80);
        let row: String = (33..80).map(|x| b[(x, 4)].symbol().to_string()).collect();
        for glyph in ["♪", "▤", "14:05", "1 2 3!"] {
            assert!(row.contains(glyph), "{glyph} under {style}: {row:?}");
        }
    }
}

#[test]
fn the_title_gives_way_first_then_the_clock_then_the_modules() {
    let a = styled("80-title", "islands", "icons");
    let row = |w: u16| -> String {
        let b = buffer_w(&a, w);
        (33..w).map(|x| b[(x, 4)].symbol().to_string()).collect()
    };
    assert!(row(100).contains("▐notes.md▌"), "{:?}", row(100));
    assert!(row(84).contains("▐notes.▌") && !row(84).contains("notes.md"), "{:?}", row(84));
    assert!(!row(80).contains("not"), "{:?}", row(80));
    // Narrower: the clock goes next, then the modules; the workspaces stay.
    assert!(row(70).contains("♪") && !row(70).contains("14:05"), "{:?}", row(70));
    assert!(row(60).contains("1 2 3!") && !row(60).contains("♪"), "{:?}", row(60));
}

/// A title is shortened by characters, never by bytes: a multibyte one never panics and keeps
/// whole characters, as many as fit.
#[test]
fn a_multibyte_title_is_shortened_by_characters() {
    let run = |text| Run { text, fg: Rgb(0, 0, 0), bg: None, state: false, visible: false };
    let block = |text| Block { runs: vec![run(text)], hue: None };
    for width in 10..40 {
        let mut left = vec![block("1 2"), block("café–über–ñ✓")];
        let mut right = vec![block("♪ 84%")];
        fit(&mut left, &mut right, width, false);
        if let Some(title) = left.get(TITLE) {
            let t = title.runs[0].text;
            assert!("café–über–ñ✓".starts_with(t) && t.chars().count() >= TITLE_MIN, "{width}: {t:?}");
        }
    }
    let mut left = vec![block("1 2"), block("café–über–ñ✓")];
    fit(&mut left, &mut vec![block("♪ 84%")], 24, false);
    assert_eq!(left[TITLE].runs[0].text, "café–übe", "24 columns: eight characters fit");
}

/// Underline's accent rule sits under the active workspace's run wherever it is: the run with
/// its own background, found through the blocks' lead cells and gaps, never a fixed column.
#[test]
fn the_underline_rule_follows_the_active_workspace_run() {
    let run = |text, bg| Run { text, fg: Rgb(0, 0, 0), bg, state: false, visible: false };
    let lit = Some(Rgb(1, 2, 3));
    let left = vec![
        Block { runs: vec![run("◆", None)], hue: None },
        Block { runs: vec![run("1 ", None), run("2", lit), run(" 3", None)], hue: None },
    ];
    // The launcher block (lead, "◆", trail) and its gap take 4 cells; then the lead, "1 ".
    assert_eq!(active_span(&left, 10), Some((10 + 4 + 1 + 2, 1)));
    let none = vec![Block { runs: vec![run("1 2", None)], hue: None }];
    assert_eq!(active_span(&none, 10), None, "no active workspace, no rule");
}

#[test]
fn each_block_style_snapshot_at_80_columns() {
    for style in ["islands", "powerline"] {
        let a = styled(&format!("snap80-{style}"), style, "icons");
        let rows = draw(&a, 80, 24).0;
        let preview: Vec<String> = rows[3..=5].iter().map(|r| r.chars().skip(33).take(46).collect()).collect();
        insta::assert_snapshot!(format!("appearance_style_{style}_80"), preview.join("\n"));
    }
}
