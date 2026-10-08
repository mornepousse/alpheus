//! The Appearance tab: the theme list on the left, the preview of the theme under the cursor on
//! the right (a mini bar in the chosen style and a mini menu, drawn with the previewed theme's own
//! roles), and the mode, module-colours and style rows below. Everything is cells; nothing here
//! reads a file.

use super::draw::{put, truncate};
use super::hit::{HitMap, Target};
use crate::app::App;
use crate::app::appearance::{COLORS, MODES, STYLES, colors_label, mode_label, style_label};
use crate::theme::color;
use alpheus_commun::theme::contrast::ratio;
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
use alpheus_commun::theme::{Mode, Rgb, Roles};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Width of the list column, and the gap before the preview.
const LIST_W: u16 = 30;
const GAP: u16 = 2;
/// The preview's frames: widest, and narrowest worth drawing.
const PREVIEW_W: u16 = 46;
const PREVIEW_MIN: u16 = 22;
/// The preview's frame under a style other than `flat`: the blocks need more room than the flat line.
const PREVIEW_BLOCKS_W: u16 = 54;
/// Column of the chips of the mode, module-colours and style rows, from the body's left edge.
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
    // A blank row sits between the list and the three chip rows when everything fits with it.
    let gap = usize::from(usize::from(body.height) >= n + 5);
    // The cursor's row always shows: on the tiniest bodies the last chip rows are clipped instead.
    let room = usize::from(body.height).saturating_sub(4 + gap).max(1);
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
    let style_y = colors_y + 1;
    put(buf, body, body.x, style_y, "Style", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    // Six chips: the usual one-cell gap when they fit, chip against chip otherwise (80 columns).
    let full: u16 = STYLES.iter().map(|s| style_label(*s).len() as u16 + 3).sum::<u16>() - 1;
    let step = u16::from(body.width >= CHIPS_X + full);
    let mut x = body.x + CHIPS_X;
    for s in STYLES {
        let w = put(buf, body, x, style_y, &format!(" {} ", style_label(s)), chip(s == ap.style, ap.style_focus, th));
        hits.push(Rect::new(x, style_y, w, 1).intersection(body), Target::AppearanceStyle(s));
        x = x.saturating_add(w + step);
    }
    if let (Some(roles), Some(entry)) = (&ap.preview, ap.cursor_entry()) {
        // The chip rows reach into the preview's column: on short bodies the preview stops above
        // them, so a chip's region always shows that chip.
        let body = Rect { height: mode_y.saturating_sub(body.y).min(body.height), ..body };
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
            if ap.style == BarStyle::Flat {
                mini_bar(buf, body, Rect::new(px, body.y + 1, avail.min(PREVIEW_W), 3), roles, ap.module_colors);
            } else {
                mini_blocks(buf, body, Rect::new(px, body.y + 1, avail.min(PREVIEW_BLOCKS_W), 3), roles, ap.module_colors, ap.style);
            }
            let w = avail.min(PREVIEW_W);
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
    let inner = Rect::new(x, y, r.width.saturating_sub(2), 1).intersection(clip);
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
        let icon = |hue: Rgb| if colors == ModuleColors::None { bg } else { on(hue) };
        // Pills under flat is full (shapes spec § 1).
        let text = |hue: Rgb| if matches!(colors, ModuleColors::Full | ModuleColors::Pills) { on(hue) } else { bg };
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

/// A run of the mini bar under a block style: its text (one cell per char), its colour, its own
/// background (the active workspace), and whether its colour is a state (warn, urgent), which wins
/// over `pills`' contrast-picked text.
#[derive(Clone, Copy)]
struct Run {
    text: &'static str,
    fg: Rgb,
    bg: Option<Rgb>,
    state: bool,
}

/// A block of the mini bar, as the real bar groups its modules (shapes spec § 2), and the hue
/// `pills` fills it with (its first module's).
struct Block {
    runs: Vec<Run>,
    hue: Option<Rgb>,
}

impl Block {
    fn width(&self) -> u16 {
        self.runs.iter().map(|r| r.text.chars().count() as u16).sum()
    }
}

/// The mini bar's blocks, left then right: the flat line's content, grouped as workspaces, title |
/// status (sound, network, battery), resources (memory), clock.
fn blocks(p: &Roles, colors: ModuleColors) -> (Vec<Block>, Vec<Block>) {
    let run = |text, fg| Run { text, fg, bg: None, state: false };
    let state = |text, fg| Run { text, fg, bg: None, state: true };
    let icon = |hue| if colors == ModuleColors::None { p.fg } else { hue };
    let text = |hue| if matches!(colors, ModuleColors::Full | ModuleColors::Pills) { hue } else { p.fg };
    let h = &p.hues;
    let active = Run { text: "1", fg: p.on_accent, bg: Some(p.accent_container), state: false };
    let left = vec![
        Block { runs: vec![active, run(" 2 ", p.fg), state("3!", p.urgent)], hue: None },
        Block { runs: vec![run("notes.md", p.fg)], hue: None },
    ];
    let right = vec![
        Block {
            runs: vec![
                run("♪", icon(h.blue)),
                run(" 84% ", text(h.blue)),
                run("≈ ", icon(h.cyan)),
                state("▮", p.warn),
                state(" 15%", p.warn),
            ],
            hue: Some(h.blue),
        },
        Block { runs: vec![run("▤", icon(h.yellow)), run(" 6G", text(h.yellow))], hue: Some(h.yellow) },
        Block { runs: vec![run("14:05", p.accent)], hue: None },
    ];
    (left, right)
}

/// Text on a `pills` block: whichever of `bar_bg` and `fg` contrasts more with the hue (a tie: bar_bg).
fn on_hue(p: &Roles, hue: Rgb) -> Rgb {
    if ratio(p.bar_bg, hue) >= ratio(p.fg, hue) { p.bar_bg } else { p.fg }
}

/// A block's background under `style`; None where the bar's own background shows. `i` is the
/// block's index over the blocks shown on both sides (0: the workspaces), for powerline's
/// alternation: as the daemon's, `pill_bg` at an even distance from the workspaces, `raised` at an
/// odd one (Mae, 2026-10-08), so the workspaces are on `pill_bg` and neighbours differ.
fn fill(p: &Roles, style: BarStyle, colors: ModuleColors, b: &Block, i: usize) -> Option<Rgb> {
    let pill = (colors == ModuleColors::Pills).then(|| b.hue.unwrap_or(p.pill_bg));
    match style {
        BarStyle::Flat => None,
        BarStyle::Underline => Some(p.pill_bg),
        BarStyle::Islands | BarStyle::Translucent => Some(pill.unwrap_or(p.pill_bg)),
        // One bar; under pills only the hued blocks are drawn inside it (task 6).
        BarStyle::Floating => {
            if colors == ModuleColors::Pills {
                b.hue
            } else {
                None
            }
        }
        BarStyle::Powerline => match (colors, b.hue) {
            (ModuleColors::Pills, Some(hue)) => Some(hue),
            _ if i % 2 == 0 => Some(p.pill_bg),
            _ => Some(p.raised),
        },
    }
}

/// `style` with `c` as its background when there is one: None leaves the cell's own background
/// (the tab's), which is how the preview shows a transparent bar.
fn on(style: Style, c: Option<Rgb>) -> Style {
    match c {
        Some(c) => style.bg(color(c)),
        None => style,
    }
}

/// Cells the left side takes up to its last cell (powerline: its last arrow included).
fn left_end(left: &[Block], powerline: bool) -> u16 {
    let w: u16 = left.iter().map(|b| b.width() + 3).sum();
    if powerline { w } else { w.saturating_sub(1) }
}

/// Cells the right side takes: blocks with a gap between them (powerline: an arrow before each).
fn right_width(right: &[Block], powerline: bool) -> u16 {
    let w: u16 = right.iter().map(|b| b.width() + 3).sum();
    if powerline { w } else { w.saturating_sub(1) }
}

/// The title's index in the left side (after the workspaces).
const TITLE: usize = 1;
/// The shortest title worth showing; shorter, it is dropped.
const TITLE_MIN: usize = 3;

/// Makes both sides fit `width` cells with a blank between them. What gives way, in order: the
/// title is shortened, then dropped; then the clock; then the modules. The workspaces stay.
fn fit(left: &mut Vec<Block>, right: &mut Vec<Block>, width: u16, powerline: bool) {
    let fits = |left: &[Block], right: &[Block]| {
        right.is_empty() || width.saturating_sub(right_width(right, powerline)) > left_end(left, powerline)
    };
    if fits(left, right) {
        return;
    }
    if let Some(full) = left.get(TITLE).and_then(|b| b.runs.first()).map(|r| r.text) {
        for k in (TITLE_MIN..full.len()).rev() {
            left[TITLE].runs[0].text = &full[..k];
            if fits(left, right) {
                return;
            }
        }
        left.remove(TITLE);
    }
    if !fits(left, right) {
        right.pop();
    }
    if !fits(left, right) {
        right.clear();
    }
}

/// The mini bar under a style other than `flat`, approximated in cells (shapes spec § 4).
/// - Blocks are cell backgrounds.
/// - Rounded ends are half-blocks in the block's colour.
/// - Powerline arrows are U+E0B0 / U+E0B2.
/// - The underline is `━` on the frame's bottom row.
/// - `islands` and `floating` leave the bar itself transparent, so the tab's background shows
///   there. `translucent` cannot blend in cells: its bar is drawn on `bar_bg`.
fn mini_blocks(buf: &mut Buffer, clip: Rect, r: Rect, p: &Roles, colors: ModuleColors, style: BarStyle) {
    let hole = matches!(style, BarStyle::Islands | BarStyle::Floating);
    let bar = (!hole).then_some(p.bar_bg);
    frame(buf, clip, r, on(Style::default().fg(color(p.fg)), bar), on(Style::default().fg(color(p.border_over(p.bar_bg))), bar));
    if r.width < 4 || r.height < 3 {
        return;
    }
    let y = r.y + 1;
    let inner = Rect::new(r.x + 1, y, r.width - 2, 1).intersection(clip);
    // Floating: one rounded bar on bar_bg across the row, its ends as half-blocks.
    let (area, row) = if style == BarStyle::Floating {
        put(buf, inner, inner.x, y, "▐", Style::default().fg(color(p.bar_bg)));
        put(buf, inner, inner.x + 1, y, &" ".repeat(usize::from(inner.width.saturating_sub(2))), on(Style::default(), Some(p.bar_bg)));
        put(buf, inner, inner.right().saturating_sub(1), y, "▌", Style::default().fg(color(p.bar_bg)));
        (Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1), Some(p.bar_bg))
    } else {
        (inner, bar)
    };
    let (mut left, mut right) = blocks(p, colors);
    fit(&mut left, &mut right, area.width, style == BarStyle::Powerline);
    let fills: Vec<Option<Rgb>> = left.iter().chain(&right).enumerate().map(|(i, b)| fill(p, style, colors, b, i)).collect();
    let (lfill, rfill) = fills.split_at(left.len());
    let rounded = matches!(style, BarStyle::Islands | BarStyle::Translucent | BarStyle::Floating);
    let powerline = style == BarStyle::Powerline;
    // Pills fills and picks the text under the block styles only; under underline it is `full`
    // (spec § 1), which `blocks` already gives its runs.
    let pills = colors == ModuleColors::Pills && style != BarStyle::Underline;
    // One block: lead cell, runs, trail cell; returns the x after it.
    // Underline: the active workspace has no fill, its text is accent (Mae, 2026-10-08).
    let underline = style == BarStyle::Underline;
    let block = |buf: &mut Buffer, x: u16, b: &Block, f: Option<Rgb>| -> u16 {
        let end = |glyph: &'static str| match (rounded, f) {
            (true, Some(f)) => (glyph, on(Style::default().fg(color(f)), row)),
            _ => (" ", on(Style::default(), f.or(row))),
        };
        let mut cx = x;
        let (g, s) = end("▐");
        cx += put(buf, area, cx, y, g, s);
        for run in &b.runs {
            let (fg, bg) = if underline && run.bg.is_some() { (p.accent, None) } else { (run.fg, run.bg) };
            let ink = match f {
                Some(f) if pills && b.hue.is_some() && !run.state && bg.is_none() => on_hue(p, f),
                _ => fg,
            };
            cx += put(buf, area, cx, y, run.text, on(Style::default().fg(color(ink)), bg.or(f).or(row)));
        }
        let (g, s) = end("▌");
        cx + put(buf, area, cx, y, g, s)
    };
    let arrow = |buf: &mut Buffer, x: u16, glyph: &str, from: Option<Rgb>, into: Option<Rgb>| -> u16 {
        put(buf, area, x, y, glyph, on(Style::default().fg(color(from.unwrap_or(p.bar_bg))), into.or(row)))
    };
    // Left: blocks with one blank cell between them (powerline: an arrow out of each block).
    let mut x = area.x;
    for (i, b) in left.iter().enumerate() {
        x = block(buf, x, b, lfill[i]);
        x += if powerline { arrow(buf, x, "\u{e0b0}", lfill[i], lfill.get(i + 1).copied().flatten()) } else { 1 };
    }
    let left_end = if powerline { x } else { x.saturating_sub(1) };
    // Right, right-aligned, only when it fits after the left side.
    let start = area.right().saturating_sub(right_width(&right, powerline));
    if start > left_end {
        let mut x = start;
        for (j, b) in right.iter().enumerate() {
            if powerline {
                let behind = if j == 0 { None } else { rfill[j - 1] };
                x += arrow(buf, x, "\u{e0b2}", rfill[j], behind);
            } else if j > 0 {
                x += 1;
            }
            x = block(buf, x, b, rfill[j]);
        }
    }
    // Underline: a rule on the frame's bottom row, in accent under the active workspace and, with
    // module colours icons, full or pills, in each hued block's hue under that block (reconciliation 9).
    if style == BarStyle::Underline {
        let bottom = r.bottom() - 1;
        let rule = |buf: &mut Buffer, x: u16, w: u16, c: Rgb| {
            put(buf, clip, x, bottom, &"━".repeat(usize::from(w)), on(Style::default().fg(color(c)), Some(p.bar_bg)));
        };
        rule(buf, area.x + 1, 1, p.accent); // the "1" sits right after the workspaces block's lead cell
        if colors != ModuleColors::None && start > left_end {
            let mut x = start;
            for b in &right {
                if let Some(hue) = b.hue {
                    rule(buf, x, b.width() + 2, hue);
                }
                x += b.width() + 3;
            }
        }
    }
}

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
    fn the_module_colours_row_is_drawn_with_its_four_values_and_regions() {
        let (a, _) = open("colors-row", &[("theme.toml", "module_colors = \"full\"\n")]);
        let (rows, hits) = draw(&a, 100, 24);
        let row = rows.iter().find(|r| r.contains("Module colours")).expect("the row");
        assert!(row.contains("None") && row.contains("Icons") && row.contains("Full") && row.contains("Pills"), "{row:?}");
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
        assert!(last(&a).contains("Tab Style"), "{:?}", last(&a));
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
                for s in STYLES {
                    if let Some(r) = hits.find(Target::AppearanceStyle(s)) {
                        assert_eq!(r.intersection(body), r, "style outside the body at height {h}");
                    }
                }
                // What a chip's region covers is that chip: never the preview drawn over it.
                let chips = MODES.iter().map(|m| (Target::AppearanceMode(*m), mode_label(*m)));
                let chips = chips.chain(COLORS.iter().map(|m| (Target::AppearanceColors(*m), colors_label(*m))));
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
                    assert!(hits.find(Target::AppearanceMode(MODES[0])).is_some(), "mode row visible at height {h}");
                    assert!(hits.find(Target::AppearanceColors(COLORS[0])).is_some(), "module colours row visible at height {h}");
                    assert!(hits.find(Target::AppearanceStyle(STYLES[0])).is_some(), "style row visible at height {h}");
                }
            }
        }
    }

    #[test]
    fn the_style_row_is_drawn_under_the_colours_with_six_values_and_regions() {
        let (a, _) = open("style-row", &[("theme.toml", "style = \"powerline\"\n")]);
        let (rows, hits) = draw(&a, 100, 24);
        let row = rows.iter().find(|r| r.starts_with(" Style")).expect("the row");
        for label in ["Flat", "Islands", "Powerline", "Floating", "Underline", "Translucent"] {
            assert!(row.contains(label), "{label} in {row:?}");
        }
        for s in STYLES {
            assert!(hits.find(Target::AppearanceStyle(s)).is_some(), "{s:?}");
        }
        let colors_y = hits.find(Target::AppearanceColors(COLORS[0])).unwrap().y;
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
        let (mut a, _) = open(name, &[("theme.toml", toml.as_str())]);
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
        assert_eq!(cell(&b, 4, "2"), (rgb(p.fg), rgb(p.pill_bg)), "inactive workspaces inside the block, in their colours");
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
        assert_eq!(cell(&b, 4, "2").1, rgb(p.pill_bg), "the workspaces");
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

    #[test]
    fn each_block_style_snapshot_at_80_columns() {
        for style in ["islands", "powerline"] {
            let a = styled(&format!("snap80-{style}"), style, "icons");
            let rows = draw(&a, 80, 24).0;
            let preview: Vec<String> = rows[3..=5].iter().map(|r| r.chars().skip(33).take(46).collect()).collect();
            insta::assert_snapshot!(format!("appearance_style_{style}_80"), preview.join("\n"));
        }
    }
}
