//! The rail layout's frame: a left rail with the brand and one entry per tab, a header in the tab's
//! hue with a one-line summary, and the shortcut hints drawn as keycaps on the last row.
//!
//! It is drawn when the terminal is at least `RAIL_MIN_W` × `RAIL_MIN_H`; the UI refresh spec sized
//! those for its 87×26 real window. Below them `ui::render` draws the classic layout. `render`
//! returns the body rectangle the tab draws into. The tab hues (`tab_hue`) and the tab summaries
//! (`subtitle`) live here because the rail and the header are what show them.
//!
//! Never:
//! - change the `App`;
//! - write outside its rectangles (`draw::put` clips);
//! - read anything: the summaries come from what the `App` already holds.
//!
//! See: docs/specs/2026-11-15-alpheus-ui-refresh-design.md, docs/features/control-center.md.

use super::draw::{self, put};
use super::shape::{self, Ink};
use super::{HitMap, Target, shortcut_pairs};
use crate::app::action::Tab;
use crate::app::{App, Loadable};
use crate::theme::{Shape, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// The rail's width: icon, name (the longest, "Displays") and number fit.
pub const RAIL_W: u16 = 16;
/// The smallest terminal that gets the rail, in columns and rows (the UI refresh plan's amendment
/// set 84 by 26).
pub const RAIL_MIN_W: u16 = 84;
pub const RAIL_MIN_H: u16 = 26;

/// Each tab has its own colour: the rail, the header and the tab's controls take it.
pub fn tab_hue(th: &Theme, t: Tab) -> Color {
    match t {
        Tab::Home => th.accent,
        Tab::Sound => th.blue,
        Tab::Network => th.cyan,
        Tab::Bluetooth => th.purple,
        Tab::Power => th.red,
        Tab::Displays => th.green,
        Tab::Appearance => th.orange,
        Tab::Diagnostics => th.yellow,
    }
}

/// The tab's Nerd Font glyph in the rail and the header.
pub fn icon(t: Tab) -> &'static str {
    match t {
        Tab::Home => "󰋜",
        Tab::Sound => "󰕾",
        Tab::Network => "󰖩",
        Tab::Bluetooth => "󰂯",
        Tab::Power => "󰐥",
        Tab::Displays => "󰍹",
        Tab::Appearance => "󰏘",
        Tab::Diagnostics => "󰃤",
    }
}

/// `n` and the matching word: `1 output`, `2 outputs`.
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One line of context under the tab's name.
pub fn subtitle(app: &App) -> String {
    match app.tab {
        Tab::Home => "The everyday switches".into(),
        Tab::Sound => match app.data.audio.ready() {
            Some(a) => {
                let def = match a.sinks.iter().find(|n| n.default) {
                    Some(n) => n.name.as_str(),
                    None => "none",
                };
                format!(
                    "{} · {} · playing on {def}",
                    plural(a.sinks.len(), "output", "outputs"),
                    plural(a.sources.len(), "input", "inputs")
                )
            }
            None => "Outputs and inputs".into(),
        },
        Tab::Network => match &app.data.network {
            Loadable::Ready(n) => {
                format!(
                    "Wi-Fi {} · {}",
                    if n.wifi_enabled { "on" } else { "off" },
                    plural(n.active.len(), "active connection", "active connections")
                )
            }
            _ => "Connections".into(),
        },
        Tab::Bluetooth => match &app.data.bluetooth {
            Loadable::Ready(b) if b.powered => format!("On · {} connected", b.connected.len()),
            Loadable::Ready(_) => "Off".into(),
            _ => "Devices".into(),
        },
        Tab::Power => "Lock, leave or restart the session".into(),
        Tab::Displays => match app.data.displays.ready() {
            Some(o) => plural(o.len(), "screen", "screens"),
            None => "Screens and profiles".into(),
        },
        Tab::Appearance => "Theme, mode, bar style and module colours".into(),
        Tab::Diagnostics => "Session health, failed units and the journal".into(),
    }
}

/// Draws the rail, the header and the keycaps on `area` (at least `RAIL_MIN_W` × `RAIL_MIN_H`); returns
/// the body the tab draws into.
pub fn render(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) -> Rect {
    let rail = Rect::new(area.x, area.y, RAIL_W, area.height.saturating_sub(1));
    nav(buf, rail, app, hits);
    let content = Rect::new(rail.right() + 2, area.y + 1, area.width.saturating_sub(RAIL_W + 4), area.height.saturating_sub(2));
    header(buf, Rect::new(content.x, content.y, content.width, 3), app);
    keycaps(buf, Rect::new(area.x, area.bottom() - 1, area.width, 1), app);
    Rect::new(content.x, content.y + 4, content.width, content.height.saturating_sub(4))
}

/// The brand, then one two-row entry per tab, the active one filled with its hue; the last action at the foot,
/// after a dot that is red while there is an error or a startup warning.
fn nav(buf: &mut Buffer, r: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    draw::clear(buf, r, Style::default().bg(th.surface).fg(th.fg));
    put(buf, r, r.x + 2, r.y + 1, "◆", Style::default().fg(th.ink(th.accent, th.surface, 3.0)).bg(th.surface));
    put(buf, r, r.x + 4, r.y + 1, "alpheus", Style::default().fg(th.fg).bg(th.surface).add_modifier(Modifier::BOLD));
    // Under the diamond, not the name: "control center" (14 cells) must fit in the 16-column rail.
    put(buf, r, r.x + 1, r.y + 2, "control center", Style::default().fg(th.muted).bg(th.surface));
    for (i, t) in Tab::ALL.iter().enumerate() {
        let y = r.y + 5 + 2 * i as u16;
        if y >= r.bottom() {
            break;
        }
        let row = Rect::new(r.x + 1, y, r.width - 2, 1);
        let hue = tab_hue(th, *t);
        let active = *t == app.tab;
        let line = th.shape == Shape::Line;
        // The active entry is `on_accent` on `accent_container` (the presets' pair, 4.5:1), the tab's hue
        // left to its icon when it reads there; under the line style it has no fill: hue text, underlined,
        // `fg` where the hue does not read on the rail. A hue is only the ink of a glyph (3:1).
        let (bg, fg, ic, edge) = match (active, line) {
            (false, _) => (th.surface, th.fg, th.ink(hue, th.surface, 3.0), th.on_accent),
            (true, false) => (th.accent_container, th.on_accent, th.ink_or(hue, th.accent_container, 3.0, th.on_accent), th.on_accent),
            (true, true) => (th.surface, th.ink(hue, th.surface, 4.5), th.ink(hue, th.surface, 3.0), th.ink(hue, th.surface, 3.0)),
        };
        if active {
            shape::band(buf, row, bg, edge, th.surface, th.shape);
        }
        put(buf, row, row.x + 1, y, icon(*t), Style::default().fg(ic).bg(bg));
        let mut label = Style::default().fg(fg).bg(bg);
        if active {
            label = label.add_modifier(Modifier::BOLD);
            if line {
                label = label.add_modifier(Modifier::UNDERLINED);
            }
        }
        // One cell after the icon, so "Displays" (8) leaves a gap before its number.
        put(buf, row, row.x + 3, y, t.title(), label);
        put(
            buf,
            row,
            row.right() - 2,
            y,
            &(i + 1).to_string(),
            Style::default().fg(if active { fg } else { th.ink(th.muted, th.surface, 3.0) }).bg(bg),
        );
        hits.push(row, Target::Tab(*t));
    }
    let last = app.status.last.as_deref().unwrap_or("ready");
    let fy = r.bottom().saturating_sub(2);
    let dot = if app.status.error.is_some() || app.status.warning.is_some() { th.error } else { th.ok };
    put(buf, r, r.x + 2, fy, "●", Style::default().fg(th.ink(dot, th.surface, 3.0)).bg(th.surface));
    let dim = th.ink(th.muted, th.surface, 3.0);
    put(buf, r, r.x + 4, fy, &draw::truncate(last, usize::from(r.width.saturating_sub(5))), Style::default().fg(dim).bg(th.surface));
}

/// The tab's name in its hue, one line of context, a rule; the last action's error and the startup warning
/// right-aligned on the first line.
fn header(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let hue = tab_hue(th, app.tab);
    let w = put(buf, r, r.x, r.y, icon(app.tab), Style::default().fg(hue));
    put(buf, r, r.x + w + 2, r.y, app.tab.title(), Style::default().fg(hue).add_modifier(Modifier::BOLD));
    put(buf, r, r.x, r.y + 1, &subtitle(app), Style::default().fg(th.muted));
    // The faults share the room right of the title, each cut with `…` keeping its start; stacked from the
    // right, so two never overwrite each other nor the title.
    let msgs: Vec<String> = [&app.status.error, &app.status.warning].into_iter().flatten().map(|m| format!("✗ {m}")).collect();
    let room = usize::from(r.right().saturating_sub(r.x + w + 2 + app.tab.title().chars().count() as u16 + 2));
    let mut right = r.right();
    for (m, len) in msgs.iter().zip(fault_widths(&msgs, room)) {
        let t = draw::truncate(m, len);
        let x = right.saturating_sub(t.chars().count() as u16);
        put(buf, r, x, r.y, &t, Style::default().fg(th.error));
        right = x.saturating_sub(2);
    }
    for k in 0..r.width {
        let (glyph, c) = if k < 12 { ("━", hue) } else { ("─", th.raised) };
        put(buf, r, r.x + k, r.y + 2, glyph, Style::default().fg(c));
    }
}

/// The width each fault gets in `room` cells (two apart): its own when everything fits; else an equal share,
/// what a short one leaves going to the first one that is cut. A fault that would get under 4 cells is dropped (0).
fn fault_widths(msgs: &[String], room: usize) -> Vec<usize> {
    let full: Vec<usize> = msgs.iter().map(|m| m.chars().count()).collect();
    let n = full.len();
    if n == 0 {
        return Vec::new();
    }
    let avail = room.saturating_sub(2 * (n - 1));
    if full.iter().sum::<usize>() <= avail {
        return full;
    }
    let share = avail / n;
    let mut len: Vec<usize> = full.iter().map(|f| (*f).min(share)).collect();
    let mut left = avail - len.iter().sum::<usize>();
    for (l, f) in len.iter_mut().zip(&full) {
        let add = (f - *l).min(left);
        *l += add;
        left -= add;
    }
    len.iter().map(|l| if *l < 4 { 0 } else { *l }).collect()
}

/// The bottom row: keys drawn as keycaps, from the content's left edge, from the same list as the classic
/// shortcut bar (`shortcut_pairs`).
fn keycaps(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    draw::clear(buf, r, Style::default().bg(th.bg));
    let fill = if th.shape == Shape::Line { tab_hue(th, app.tab) } else { th.raised };
    let ink = Ink { fg: th.fg, fill, behind: th.bg, bold: true };
    let caps = match th.shape {
        Shape::Round => 2,
        Shape::Arrow => 1,
        _ => 0,
    };
    let mut x = r.x + RAIL_W + 2;
    for (key, what) in shortcut_pairs(app) {
        if key.is_empty() {
            continue; // action with no key (unassigned in config.toml)
        }
        // The cap and the label, without the trailing padding: a pair that does not fit whole is not drawn.
        let w = key.chars().count() as u16 + 2 + caps + 1 + what.chars().count() as u16;
        if x.saturating_add(w) > r.right() {
            break;
        }
        x = x.saturating_add(shape::chip(buf, r, x, r.y, &format!(" {key} "), ink, th.shape));
        x = x.saturating_add(put(buf, r, x, r.y, &format!(" {what}   "), Style::default().fg(th.muted).bg(th.bg)));
    }
}

#[cfg(test)]
mod tests;
