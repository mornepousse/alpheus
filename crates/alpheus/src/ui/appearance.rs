//! The Theme tab's drawing: the theme list on the left, the preview of the theme under the cursor on
//! the right, and the mode, module-colours and style rows below.
//!
//! The preview is a mini bar in the chosen style and a mini menu, drawn in cells with the previewed
//! theme's own roles: `mini_bar` for the flat line, `mini_blocks` for the other five styles (the
//! blocks, rounded ends, powerline arrows and underline are approximated with cell backgrounds and
//! half-block characters), `mini_menu` for the menu. `tabs::render` calls `render` for this tab, and
//! `hints` gives the status line's keys.
//!
//! Never:
//! - change the state or read a file: everything here reads the `App` (the roles to preview are
//!   already resolved in `AppearanceTab::preview`);
//! - draw outside `body`: every `put` is clipped to it, and a region that is clickable is clipped
//!   to it too.
//!
//! See: docs/specs/2026-10-07-alpheus-themes-design.md (§ 9, module colours),
//! docs/specs/2026-10-07-alpheus-shapes-design.md (§ 4, the preview approximated in cells),
//! docs/features/control-center.md.

use super::draw::{put, truncate};
use super::hit::{HitMap, Target};
use crate::app::App;
use crate::app::action::Action;
use crate::app::appearance::{Focus, STYLES, colors_label, mode_label, style_label};
use crate::app::keymap::Keymap;
use crate::theme::color;
use alpheus_common::theme::contrast::ratio;
use alpheus_common::theme::settings::{BarStyle, ModeSetting, ModuleColors};
use alpheus_common::theme::{Mode, Rgb, Roles};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// The keys the status line offers on this tab: choose in the list or along the focused row, apply.
pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, app: &App, km: &Keymap) {
    if app.appearance.focus != Focus::List {
        pairs.push((format!("{}/{}", km.label(Action::Left), km.label(Action::Right)), "Choose"));
    } else {
        pairs.push((format!("{}/{}", km.label(Action::Up), km.label(Action::Down)), "Choose"));
    }
    pairs.push((km.label(Action::Activate), "Apply"));
}

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
/// The six style chips side by side with no gap: narrower than this, they wrap onto a second line.
const STYLES_GLUED_W: u16 = 60;
/// Column of the "dark only" tags in a list row.
const TAG_X: u16 = 18;
/// Inner width of the mini menu.
const MENU_W: u16 = 12;

/// Draws the tab in `body`: the list (scrolled to keep the cursor in view), the three chip rows, and
/// the preview beside the list when there is room for it. Each theme, chip and style adds its
/// clickable region to `hits`.
pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let ap = &app.appearance;
    // The mode row sits under the list when it fits, else on the body's last row with the list
    // scrolled so the cursor stays visible. Nothing is drawn or made clickable outside `body`.
    let n = ap.entries.len();
    // A blank row sits between the list and the three chip rows when everything fits with it.
    // A body too narrow for the six style chips on one line gives them a second one.
    let wrap = body.width < CHIPS_X + STYLES_GLUED_W;
    let chip_rows = 3 + usize::from(wrap);
    // The "Theme" caption goes when its row is the one that keeps the whole list on screen (the
    // rail's header already names the tab).
    let head = usize::from(usize::from(body.height) >= n + 1 + chip_rows || usize::from(body.height) < n + chip_rows);
    if head == 1 {
        put(buf, body, body.x, body.y, "Theme", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    }
    let gap = usize::from(usize::from(body.height) >= n + head + 1 + chip_rows);
    // The cursor's row always shows: on the tiniest bodies the last chip rows are clipped instead.
    let room = usize::from(body.height).saturating_sub(head + chip_rows + gap).max(1);
    let shown = n.min(room);
    let offset = if ap.cursor >= shown { ap.cursor + 1 - shown } else { 0 };
    let mode_y = body.y + (head + shown + gap) as u16;
    for (i, e) in ap.entries.iter().enumerate().skip(offset).take(shown) {
        let y = body.y + (head + i - offset) as u16;
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
    for m in ModeSetting::ALL {
        let w = put(buf, body, x, mode_y, &format!(" {} ", mode_label(m)), chip(m == ap.mode, ap.focus == Focus::Mode, th));
        hits.push(Rect::new(x, mode_y, w, 1).intersection(body), Target::AppearanceMode(m));
        x = x.saturating_add(w + 1);
    }
    let colors_y = mode_y + 1;
    put(buf, body, body.x, colors_y, "Module colours", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    let mut x = body.x + CHIPS_X;
    for m in ModuleColors::ALL {
        let w = put(buf, body, x, colors_y, &format!(" {} ", colors_label(m)), chip(m == ap.module_colors, ap.focus == Focus::Colors, th));
        hits.push(Rect::new(x, colors_y, w, 1).intersection(body), Target::AppearanceColors(m));
        x = x.saturating_add(w + 1);
    }
    let style_y = colors_y + 1;
    put(buf, body, body.x, style_y, "Style", Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    // Six chips: the usual one-cell gap when they fit, chip against chip when only that fits (80
    // columns), else one-cell gaps over two lines, so every chip is whole and clickable.
    let full: u16 = STYLES.iter().map(|s| style_label(*s).len() as u16 + 3).sum::<u16>() - 1;
    let step = u16::from(wrap || body.width >= CHIPS_X + full);
    let (mut x, mut y) = (body.x + CHIPS_X, style_y);
    for s in STYLES {
        let w = style_label(s).len() as u16 + 2;
        if wrap && x + w > body.right() {
            (x, y) = (body.x + CHIPS_X, y + 1);
        }
        put(buf, body, x, y, &format!(" {} ", style_label(s)), chip(s == ap.style, ap.focus == Focus::Style, th));
        hits.push(Rect::new(x, y, w, 1).intersection(body), Target::AppearanceStyle(s));
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

/// The bar of the previewed theme: workspaces (one visible on another output), a title, four modules (sound, network, battery,
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
    // Visible on another output: the daemon's `Style::SURFACE`, fg on pill_bg.
    cx += put(buf, inner, cx, y, " 2 ", bg.bg(color(p.pill_bg)));
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
/// background (the active workspace), whether its colour is a state (warn, urgent), which wins
/// over `pills`' contrast-picked text, and whether it is a workspace visible on another output
/// (the bar's `Tint::Pill`: `pill_bg`, or `raised` on a `pill_bg` block).
#[derive(Clone, Copy)]
struct Run {
    text: &'static str,
    fg: Rgb,
    bg: Option<Rgb>,
    state: bool,
    visible: bool,
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
    let run = |text, fg| Run { text, fg, bg: None, state: false, visible: false };
    let state = |text, fg| Run { text, fg, bg: None, state: true, visible: false };
    let icon = |hue| if colors == ModuleColors::None { p.fg } else { hue };
    let text = |hue| if matches!(colors, ModuleColors::Full | ModuleColors::Pills) { hue } else { p.fg };
    let h = &p.hues;
    let active = Run { text: "1", fg: p.on_accent, bg: Some(p.accent_container), state: false, visible: false };
    let visible = Run { visible: true, ..run("2", p.fg) };
    let left = vec![
        Block { runs: vec![active, run(" ", p.fg), visible, run(" ", p.fg), state("3!", p.urgent)], hue: None },
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
/// odd one (ruling of 2026-10-08, CLAUDE.md, Themes delivery 2), so the workspaces are on `pill_bg` and neighbours differ.
fn fill(p: &Roles, style: BarStyle, colors: ModuleColors, b: &Block, i: usize) -> Option<Rgb> {
    let pill = if colors == ModuleColors::Pills { Some(b.hue.unwrap_or(p.pill_bg)) } else { None };
    match style {
        BarStyle::Flat => None,
        BarStyle::Underline => Some(p.pill_bg),
        BarStyle::Islands | BarStyle::Translucent => Some(pill.unwrap_or(p.pill_bg)),
        // One bar; under pills only the hued blocks are drawn inside it, as the daemon draws it.
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
    let title = match left.get(TITLE) {
        Some(block) => block.runs.first().map(|run| run.text),
        None => None,
    };
    if let Some(full) = title {
        // Cut at character boundaries (a block's width counts characters): never inside one.
        let cuts: Vec<usize> = full.char_indices().map(|(i, _)| i).skip(TITLE_MIN).collect();
        for &k in cuts.iter().rev() {
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
    let bar = if hole { None } else { Some(p.bar_bg) };
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
    // Underline: the active workspace has no fill, its text is accent (ruling of 2026-10-08, CLAUDE.md, Themes delivery 2).
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
            // A visible workspace, as the bar paints it: `raised` on a `pill_bg` block.
            let bg = if run.visible { Some(if f == Some(p.pill_bg) { p.raised } else { p.pill_bg }) } else { bg };
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
        if let Some((x, w)) = active_span(&left, area.x) {
            rule(buf, x, w, p.accent);
        }
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

/// The active workspace's run (the one with its own background) in the left side laid out from
/// `x` (each block: a lead cell, its runs, a trail cell, then one cell of gap or arrow): its x and
/// width; None without one.
fn active_span(left: &[Block], x: u16) -> Option<(u16, u16)> {
    let mut bx = x;
    for b in left {
        let mut cx = bx + 1;
        for r in &b.runs {
            let w = r.text.chars().count() as u16;
            if r.bg.is_some() {
                return Some((cx, w));
            }
            cx += w;
        }
        bx += b.width() + 3;
    }
    None
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
mod tests;
