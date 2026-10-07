//! The Appearance tab: the theme list on the left, the preview of the theme under the cursor on
//! the right (a mini bar and a mini menu, drawn with the previewed theme's own roles), and the mode
//! and module-colours rows below. Everything is cells; nothing here reads a file.

use super::draw::{put, truncate};
use super::hit::{HitMap, Target};
use crate::app::App;
use crate::app::appearance::{COLORS, MODES, colors_label, mode_label};
use crate::theme::color;
use alpheus_commun::theme::settings::ModuleColors;
use alpheus_commun::theme::{Mode, Roles};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Width of the list column, and the gap before the preview.
const LIST_W: u16 = 30;
const GAP: u16 = 2;
/// The preview's frames: widest, and narrowest worth drawing.
const PREVIEW_W: u16 = 46;
const PREVIEW_MIN: u16 = 22;
/// Column of the chips of the mode and module-colours rows, from the body's left edge.
const CHIPS_X: u16 = 16;
/// Column of the "dark only" tags in a list row.
const TAG_X: u16 = 18;
/// Inner width of the mini menu.
const MENU_W: u16 = 12;

pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let ap = &app.appearance;
    put(buf, body, body.x, body.y, "Theme", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    // The mode row sits under the list when it fits, else on the body's last row with the list
    // scrolled so the cursor stays visible. Nothing is drawn or made clickable outside `body`.
    let n = ap.entries.len();
    // A blank row sits between the list and the two chip rows when everything fits with it.
    let gap = usize::from(usize::from(body.height) >= n + 4);
    let room = usize::from(body.height).saturating_sub(3 + gap);
    let shown = n.min(room);
    let offset = if ap.cursor >= shown { ap.cursor + 1 - shown } else { 0 };
    let mode_y = body.y + 1 + (shown + gap) as u16;
    for (i, e) in ap.entries.iter().enumerate().skip(offset).take(shown) {
        let y = body.y + 1 + (i - offset) as u16;
        let row = Rect::new(body.x, y, LIST_W.min(body.width), 1);
        let on_cursor = i == ap.cursor;
        let base = if on_cursor { th.selected() } else { Style::default().fg(th.fg) };
        if on_cursor {
            put(buf, row, row.x, y, &" ".repeat(usize::from(row.width)), base);
        }
        put(buf, row, row.x, y, if on_cursor { "❯" } else { " " }, Style::default().fg(th.accent).bg(base.bg.unwrap_or(th.bg)));
        if ap.applied == Some(i) {
            put(buf, row, row.x + 2, y, "●", Style::default().fg(th.accent).bg(base.bg.unwrap_or(th.bg)));
        }
        put(buf, row, row.x + 4, y, &truncate(e.label, 20), base);
        if e.dark_only {
            // A column of its own, clear of the longest preset name.
            put(buf, row, row.x + TAG_X, y, "dark only", Style::default().fg(th.muted).bg(base.bg.unwrap_or(th.bg)));
        }
        hits.push(row.intersection(body), Target::AppearanceTheme(i));
    }
    let chip = |m_selected: bool, focused: bool, th: &crate::theme::Theme| {
        if !m_selected {
            Style::default().fg(th.muted)
        } else if focused {
            th.filled()
        } else {
            th.selected()
        }
    };
    put(buf, body, body.x, mode_y, "Mode", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    let mut x = body.x + CHIPS_X;
    for m in MODES {
        let w = put(buf, body, x, mode_y, &format!(" {} ", mode_label(m)), chip(m == ap.mode, ap.mode_focus, th));
        hits.push(Rect::new(x, mode_y, w, 1).intersection(body), Target::AppearanceMode(m));
        x = x.saturating_add(w + 1);
    }
    let colors_y = mode_y + 1;
    put(buf, body, body.x, colors_y, "Module colours", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    let mut x = body.x + CHIPS_X;
    for m in COLORS {
        let w = put(buf, body, x, colors_y, &format!(" {} ", colors_label(m)), chip(m == ap.module_colors, ap.colors_focus, th));
        hits.push(Rect::new(x, colors_y, w, 1).intersection(body), Target::AppearanceColors(m));
        x = x.saturating_add(w + 1);
    }
    if let (Some(roles), Some(entry)) = (&ap.preview, ap.cursor_entry()) {
        let px = body.x + LIST_W + GAP;
        let avail = body.right().saturating_sub(px);
        if avail >= PREVIEW_MIN {
            // What is drawn: a preset with no light variant shows its dark half.
            let mode = if ap.effective_mode() == Mode::Light && !entry.dark_only { "light" } else { "dark" };
            put(
                buf,
                body,
                px,
                body.y,
                &format!("Preview — {}, {mode}", entry.label),
                Style::default().fg(th.muted).add_modifier(Modifier::BOLD),
            );
            let w = avail.min(PREVIEW_W);
            mini_bar(buf, body, Rect::new(px, body.y + 1, w, 3), roles, ap.module_colors);
            mini_menu(buf, body, Rect::new(px, body.y + 5, (MENU_W + 2).min(w), 5), roles);
            if let Some(note) = &ap.preview_note {
                put(buf, body, px, body.y + 10, &truncate(note, usize::from(avail)), Style::default().fg(th.error));
            }
        }
    }
}

/// A frame of `r` in `border` over `bg`, the inside filled with `bg`.
fn frame(buf: &mut Buffer, clip: Rect, r: Rect, bg: Style, border: Style) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let inner = r.width - 2;
    let line = |l: &str, m: &str, rt: &str| format!("{l}{}{rt}", m.repeat(usize::from(inner)));
    put(buf, clip, r.x, r.y, &line("┌", "─", "┐"), border);
    for y in r.y + 1..r.bottom() - 1 {
        put(buf, clip, r.x, y, &line("│", " ", "│"), border);
        put(buf, clip, r.x + 1, y, &" ".repeat(usize::from(inner)), bg);
    }
    put(buf, clip, r.x, r.bottom() - 1, &line("└", "─", "┘"), border);
}

/// The bar of the previewed theme: workspaces, a title, four modules (sound, network, battery,
/// memory) tinted by `colors` with the theme's hues, and a clock. The battery is low, so it shows
/// the warn colour whatever the setting (a state wins over its hue).
fn mini_bar(buf: &mut Buffer, clip: Rect, r: Rect, p: &Roles, colors: ModuleColors) {
    let bg = Style::default().fg(color(p.fg)).bg(color(p.bar_bg));
    let border = Style::default().fg(color(p.border_over(p.bar_bg))).bg(color(p.bar_bg));
    frame(buf, clip, r, bg, border);
    let (x, y) = (r.x + 1, r.y + 1);
    let inner = Rect::new(x, y, r.width.saturating_sub(2), 1);
    let on = |c| bg.fg(color(c));
    let mut cx = x;
    cx += put(buf, inner, cx, y, " 1 ", Style::default().fg(color(p.on_accent)).bg(color(p.accent_container)).add_modifier(Modifier::BOLD));
    cx += put(buf, inner, cx, y, " 2 ", bg);
    cx += put(buf, inner, cx, y, " 3!", on(p.urgent));
    put(buf, inner, cx + 1, y, "notes.md", bg);
    // Right-aligned, only when it fits after the title.
    let modules = MODULES_W + 1 + CLOCK_W;
    let tx = inner.right().saturating_sub(modules + 1);
    if tx > cx + 9 {
        let icon = |hue: alpheus_commun::theme::Rgb| if colors == ModuleColors::None { bg } else { on(hue) };
        let text = |hue: alpheus_commun::theme::Rgb| if colors == ModuleColors::Full { on(hue) } else { bg };
        let h = &p.hues;
        let mut mx = tx;
        mx += put(buf, inner, mx, y, "♪", icon(h.blue));
        mx += put(buf, inner, mx, y, " 84% ", text(h.blue));
        mx += put(buf, inner, mx, y, "≈ ", icon(h.cyan));
        mx += put(buf, inner, mx, y, "▮", on(p.warn));
        mx += put(buf, inner, mx, y, " 15% ", on(p.warn));
        mx += put(buf, inner, mx, y, "▤", icon(h.yellow));
        put(buf, inner, mx, y, " 6G", text(h.yellow));
        put(buf, inner, tx + MODULES_W + 1, y, "14:05", on(p.accent));
    }
}

/// Cells of the mini bar's four modules ("♪ 84% ≈ ▮ 15% ▤ 6G") and of its clock.
const MODULES_W: u16 = 18;
const CLOCK_W: u16 = 5;

/// The menu of the previewed theme: a selected row, a checked row and a plain one.
fn mini_menu(buf: &mut Buffer, clip: Rect, r: Rect, p: &Roles) {
    let bg = Style::default().fg(color(p.fg)).bg(color(p.popup_bg));
    let border = Style::default().fg(color(p.border_over(p.popup_bg))).bg(color(p.popup_bg));
    frame(buf, clip, r, bg, border);
    let x = r.x + 1;
    let inner_w = usize::from(r.width.saturating_sub(2));
    let sel = Style::default().fg(color(p.fg)).bg(color(p.selection));
    put(buf, clip, x, r.y + 1, &format!(" {:<w$}", "Open", w = inner_w.saturating_sub(1)), sel);
    put(buf, clip, x + 1, r.y + 2, "✓", bg.fg(color(p.accent2)));
    put(buf, clip, x + 3, r.y + 2, "Mute", bg);
    put(buf, clip, x + 3, r.y + 3, "Low", bg);
}

#[cfg(test)]
mod tests {
    use super::super::testing::draw;
    use super::*;
    use crate::app::action::{Action, Tab};
    use crate::app::appearance::tests::open;
    use alpheus_commun::theme::Rgb;
    use alpheus_commun::theme::presets::preset;
    use alpheus_commun::theme::settings::ModuleColors;
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
        let (mut a, _) = open("snap-dark", &[("theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\n")]);
        goto(&mut a, "tokyo-night");
        insta::assert_snapshot!("appearance_dark", draw(&a, 100, 24).0.join("\n"));
    }

    #[test]
    fn light_mode_and_the_focus_on_the_mode_row() {
        let (mut a, _) = open("snap-light", &[("theme.toml", "theme = \"gruvbox\"\nmode = \"light\"\n")]);
        a.on_key_action(Action::NextTab);
        insta::assert_snapshot!("appearance_light", draw(&a, 100, 24).0.join("\n"));
    }

    #[test]
    fn the_wallpaper_entry_with_a_fixture_colors_toml() {
        let (a, _) = open("snap-wall", &[("colors.toml", WALL)]);
        insta::assert_snapshot!("appearance_wallpaper", draw(&a, 100, 24).0.join("\n"));
    }

    #[test]
    fn dark_only_tags_and_the_light_mode_fallback() {
        let (mut a, _) = open("snap-nord", &[("theme.toml", "theme = \"nord\"\nmode = \"light\"\n")]);
        a.on_key_action(Action::Down);
        insta::assert_snapshot!("appearance_dark_only", draw(&a, 100, 24).0.join("\n"));
        let rows = draw(&a, 100, 24).0;
        assert!(rows[2].contains("Preview — Dracula, dark"), "the header says what is drawn: {:?}", rows[2]);
        assert_eq!(rows.iter().filter(|r| r.contains("dark only")).count(), 2, "Nord and Dracula only");
    }

    #[test]
    fn the_preview_wears_the_roles_of_the_theme_under_the_cursor() {
        let (mut a, _) = open("colors", &[("theme.toml", "theme = \"catppuccin\"\nmode = \"dark\"\n")]);
        goto(&mut a, "tokyo-night");
        let p = preset("tokyo-night").unwrap().dark;
        let b = buffer(&a);
        // The bar's content row (body y = 2, frame at 3, content at 4).
        assert_eq!(cell(&b, 4, "1"), (rgb(p.on_accent), rgb(p.accent_container)), "active workspace");
        assert_eq!(cell(&b, 4, "2"), (rgb(p.fg), rgb(p.bar_bg)), "inactive workspace");
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
        let (mut a, _) = open("preview-colors", &[("theme.toml", "theme = \"tokyo-night\"\nmode = \"dark\"\n")]);
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
    fn the_module_colours_row_is_drawn_with_its_three_values_and_regions() {
        let (a, _) = open("colors-row", &[("theme.toml", "module_colors = \"full\"\n")]);
        let (rows, hits) = draw(&a, 100, 24);
        let row = rows.iter().find(|r| r.contains("Module colours")).expect("the row");
        assert!(row.contains("None") && row.contains("Icons") && row.contains("Full"), "{row:?}");
        for m in COLORS {
            assert!(hits.find(Target::AppearanceColors(m)).is_some(), "{m:?}");
        }
        let mode_y = hits.find(Target::AppearanceMode(MODES[0])).unwrap().y;
        assert_eq!(hits.find(Target::AppearanceColors(COLORS[0])).unwrap().y, mode_y + 1, "right under Mode");
        // The loaded value is the highlighted one.
        let b = buffer(&a);
        let full = hits.find(Target::AppearanceColors(ModuleColors::Full)).unwrap();
        let none = hits.find(Target::AppearanceColors(ModuleColors::None)).unwrap();
        assert_ne!(b[(full.x + 1, full.y)].bg, b[(none.x + 1, none.y)].bg, "Full is highlighted, None is not");
        assert!(rows[23].contains("Enter Apply"), "{:?}", rows[23]);
    }

    #[test]
    fn the_footer_names_the_next_stop_of_the_tab_key() {
        let (mut a, _) = open("footer", &[]);
        let last = |a: &crate::app::App| draw(a, 100, 24).0[23].clone();
        assert!(last(&a).contains("Tab Mode"), "{:?}", last(&a));
        a.on_key_action(Action::NextTab);
        assert!(last(&a).contains("Tab Colours"), "{:?}", last(&a));
        a.on_key_action(Action::NextTab);
        assert!(last(&a).contains("Tab Theme"), "{:?}", last(&a));
    }

    #[test]
    fn the_header_names_the_theme_and_the_mode() {
        let (mut a, _) = open("header", &[("theme.toml", "mode = \"light\"\n")]);
        assert!(draw(&a, 100, 24).0[2].contains("Preview — Catppuccin, light"));
        a.on_key_action(Action::Down);
        assert!(draw(&a, 100, 24).0[2].contains("Preview — Gruvbox, light"));
    }

    #[test]
    fn the_regions_and_the_tab_title() {
        let (a, _) = open("hits", &[]);
        let (rows, hits) = draw(&a, 100, 24);
        assert!(rows[0].contains(" 7 Theme "), "{:?}", rows[0]);
        assert!(hits.find(Target::Tab(Tab::Appearance)).is_some());
        assert_eq!(hits.find(Target::AppearanceTheme(3)).map(|r| (r.y, r.width)), Some((6, 30)));
        for m in MODES {
            assert!(hits.find(Target::AppearanceMode(m)).is_some());
        }
        assert!(rows[23].contains("Enter Apply") && rows[23].contains("Tab Mode"), "{:?}", rows[23]);
    }

    #[test]
    fn small_windows_never_panic_and_a_narrow_one_drops_the_preview() {
        let (a, _) = open("small", &[]);
        for (w, h) in [(30, 8), (40, 12), (60, 14), (80, 24), (31, 9)] {
            draw(&a, w, h);
        }
        let all = draw(&a, 50, 24).0.join("\n");
        assert!(!all.contains("Preview"), "{all}");
        assert!(all.contains("Mode"));
    }

    #[test]
    fn short_terminals_scroll_the_list_and_never_expose_a_hidden_hit() {
        let (mut a, _) = open("short", &[]);
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
                for m in MODES {
                    if let Some(r) = hits.find(Target::AppearanceMode(m)) {
                        assert_eq!(r.intersection(body), r, "mode outside the body at height {h}");
                    }
                }
                for m in COLORS {
                    if let Some(r) = hits.find(Target::AppearanceColors(m)) {
                        assert_eq!(r.intersection(body), r, "module colours outside the body at height {h}");
                    }
                }
                let row = hits.find(Target::AppearanceTheme(cursor));
                assert!(row.is_some(), "cursor {cursor} not drawn at height {h}");
                if let Some(r) = row {
                    assert!(rows[usize::from(r.y)].contains('❯'), "{:?}", rows[usize::from(r.y)]);
                }
                if h >= 9 {
                    assert!(hits.find(Target::AppearanceMode(MODES[0])).is_some(), "mode row visible at height {h}");
                    assert!(hits.find(Target::AppearanceColors(COLORS[0])).is_some(), "module colours row visible at height {h}");
                }
            }
        }
    }
}
