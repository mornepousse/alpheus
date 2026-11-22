//! The overlays and the corner messages: help, the command palette, the power confirmation, and
//! the toasts. The display overlays (Keep, an output's panel, the save box) and the unit
//! confirmation are drawn by `displays.rs` and `diagnostics.rs`; this file only calls them.
//!
//! They are drawn last, so their regions win a click (`HitMap::at`).
//!
//! Never:
//! - change the `App`.
//!
//! See: docs/features/control-center.md, docs/specs/2026-10-02-alpheus-v1-design.md (§ 2, "Visual cues").

use super::draw::{centered, clear, put, truncate};
use super::hit::{HitMap, Target};
use super::{diagnostics, displays};
use crate::app::action::{Action, Tab};
use crate::app::{App, Confirm, Overlay, Palette};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Widget};

/// Visible result rows in the palette.
const PALETTE_ROWS: usize = 10;

/// Draws the toasts, then the open overlay if there is one.
pub fn render(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    toasts(buf, area, app);
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => help(buf, area, app, hits),
        Overlay::Palette(p) => palette(buf, area, app, p, hits),
        Overlay::Confirm(c) => confirm(buf, area, app, c, hits),
        Overlay::Keep(k) => displays::keep(buf, area, app, k, hits),
        Overlay::Panel(p) => displays::panel(buf, area, app, p, hits),
        Overlay::SaveProfile(d) => displays::save(buf, area, app, d, hits),
        Overlay::UnitConfirm { scope, unit, action, yes_focused } => {
            diagnostics::unit_confirm(buf, area, app, *scope, unit, *action, *yes_focused, hits)
        }
    }
}

/// Overlay frame: background cleared, border, title; returns the inside.
pub(super) fn frame(buf: &mut Buffer, r: Rect, title: &str, app: &App, hits: &mut HitMap) -> Rect {
    let th = &app.theme;
    let style = Style::default().fg(th.fg).bg(th.surface);
    clear(buf, r, style);
    let block = Block::bordered().title(title.to_string()).border_style(Style::default().fg(th.accent).bg(th.surface)).style(style);
    let inner = block.inner(r);
    block.render(r, buf);
    hits.push(r, Target::Overlay);
    inner
}

/// The corner messages, one per row from the top right, each with `✓` or `✗`.
fn toasts(buf: &mut Buffer, area: Rect, app: &App) {
    let th = &app.theme;
    for (n, t) in app.toasts.iter().enumerate() {
        let y = area.y + 1 + n as u16;
        let text = format!(" {} {} ", if t.error { "✗" } else { "✓" }, t.text);
        let w = (text.chars().count() as u16).min(area.width.saturating_sub(2));
        let x = area.right().saturating_sub(w + 1);
        let style = Style::default().fg(if t.error { th.error } else { th.ok }).bg(th.surface).add_modifier(Modifier::BOLD);
        put(buf, area, x, y, &truncate(&text, usize::from(w)), style);
    }
}

/// The help box: one line per action with its keys, and the mouse line.
fn help(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    let line = |label: &str, keys: &[String]| format!("{label:<24}{}", keys.join(", "));
    let mut lines: Vec<String> = Vec::new();
    // The "Tab X" entries (one per tab) fit on one line, in place of the first one:
    // the whole help screen thus fits in 80×26.
    let mut tab_keys: Vec<String> = Vec::new();
    let mut tab_line: Option<usize> = None;
    for (a, keys) in app.keymap.all() {
        let k: Vec<String> = keys.iter().map(|c| c.label()).collect();
        if matches!(a, Action::GoTab(_)) {
            // The tab line goes where the first tab entry was.
            if tab_line.is_none() {
                tab_line = Some(lines.len());
            }
            tab_keys.extend(k);
        } else if !k.is_empty() {
            lines.push(line(&a.label(), &k));
        }
    }
    // No tab line when every tab key is unbound.
    if let Some(i) = tab_line {
        if !tab_keys.is_empty() {
            lines.insert(i, line(&format!("Tabs 1 to {}", Tab::ALL.len()), &tab_keys));
        }
    }
    lines.push(line("Mouse", &["click, drag, scroll".to_string()]));
    let r = centered(area, 64, lines.len() as u16 + 2);
    let inner = frame(buf, r, " Help — Esc to close ", app, hits);
    for (n, l) in lines.iter().enumerate().take(usize::from(inner.height)) {
        put(buf, inner, inner.x + 1, inner.y + n as u16, l, Style::default().fg(app.theme.fg).bg(app.theme.surface));
    }
}

/// The command palette: the typed text, then the matching entries, scrolled to keep the selection
/// visible.
fn palette(buf: &mut Buffer, area: Rect, app: &App, p: &Palette, hits: &mut HitMap) {
    let th = &app.theme;
    let matches = app.palette_matches();
    let shown = matches.len().clamp(1, PALETTE_ROWS) as u16;
    let r = centered(area, 56, shown + 4);
    let inner = frame(buf, r, " Command palette ", app, hits);
    put(buf, inner, inner.x + 1, inner.y, &format!("> {}▏", p.query), Style::default().fg(th.accent).bg(th.surface));
    if matches.is_empty() {
        put(buf, inner, inner.x + 1, inner.y + 2, "no command", Style::default().fg(th.muted).bg(th.surface));
        return;
    }
    let visible = usize::from(inner.height).saturating_sub(2);
    let offset = if p.selected >= visible { p.selected + 1 - visible } else { 0 };
    for (n, (label, _)) in matches.iter().enumerate().skip(offset).take(visible) {
        let y = inner.y + 2 + (n - offset) as u16;
        let style = if n == p.selected { th.selected() } else { Style::default().fg(th.fg).bg(th.surface) };
        let row = Rect::new(inner.x, y, inner.width, 1);
        clear(buf, row, style);
        put(buf, row, inner.x + 1, y, &truncate(label, usize::from(inner.width.saturating_sub(2))), style);
        hits.push(row, Target::PaletteEntry(n));
    }
}

/// The power confirmation: the question and a Cancel and a Confirm button, the focused one filled.
fn confirm(buf: &mut Buffer, area: Rect, app: &App, c: &Confirm, hits: &mut HitMap) {
    let th = &app.theme;
    let r = centered(area, 46, 6);
    let inner = frame(buf, r, " Confirmation ", app, hits);
    put(buf, inner, inner.x + 1, inner.y, c.action.question(), Style::default().fg(th.fg).bg(th.surface).add_modifier(Modifier::BOLD));
    let y = inner.y + inner.height.saturating_sub(1);
    let on = th.filled();
    let off = Style::default().fg(th.fg).bg(th.surface);
    let mut x = inner.x + 1;
    let w = put(buf, inner, x, y, "[ Cancel ]", if c.yes_focused { off } else { on });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmNo);
    x = x.saturating_add(w + 2);
    let w = put(buf, inner, x, y, "[ Confirm ]", if c.yes_focused { on } else { off });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmYes);
}
