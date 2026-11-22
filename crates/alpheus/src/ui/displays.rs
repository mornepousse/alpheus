//! The Displays tab's drawing: the profile bar, the scaled map, one row per output, the shortcut
//! hints, and the three boxes that open over the tab (the settings panel, "Keep these settings?" and
//! saving a profile).
//!
//! `tabs::render` draws the rows this file lists (`rows`) with its shared item drawing, which calls
//! `draw_profiles` and `draw_output` for this tab's two kinds of item. The boxes are drawn by
//! `overlays.rs`, which calls `keep`, `panel` and `save` here and supplies their frame. The state
//! they show is `app/displays.rs`'s.
//!
//! Never:
//! - change the state: everything here reads the `App` and fills cells and the map of clickable
//!   regions;
//! - run a tool or read a clock: the countdown it shows is `Trial::remaining`, set by the tick.
//!
//! See: docs/specs/2026-10-03-alpheus-ecrans-design.md (§ 2, the tab; § 3, the "Keep these settings?"
//! box), docs/specs/2026-11-15-alpheus-ui-refresh-design.md (the rail and the tab hues),
//! docs/features/control-center.md.

use super::draw::{centered, put, truncate};
use super::hit::{HitMap, Target};
use super::overlays::frame;
use super::shape::{self, Ink};
use super::tabs::Row;
use crate::app::action::Action;
use crate::app::displays::{panel_fields, place_label, placement_labels};
use crate::app::keymap::Keymap;
use crate::app::{App, Item, Keep, Loadable, Panel, PanelField, PlaceChoice, SaveDialog};
use crate::displays::geometry::logical_size;
use crate::displays::{Output, active_count, rotation_label, short_from_description, short_names};
use crate::theme::Shape;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Adds the Displays tab's rows to `rows`; `map_rows` is the height of the map.
pub(super) fn rows(app: &App, map_rows: usize, rows: &mut Vec<Row>) {
    rows.push(Row::Item(0));
    rows.push(Row::Blank);
    match &app.data.displays {
        Loadable::Loading => rows.push(Row::Text("…".into())),
        Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
        Loadable::Ready(outs) => {
            rows.push(Row::Heading("Map"));
            rows.extend((0..map_rows).map(Row::Map));
            rows.push(Row::Blank);
            rows.push(Row::Heading("Displays"));
            if outs.is_empty() {
                rows.push(Row::Text("no output".into()));
            }
            rows.extend((1..=outs.len()).map(Row::Item));
        }
    }
}

/// The keys the shortcut bar (the keycaps row in the rail) offers on this tab.
pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, app: &App, km: &Keymap) {
    match app.focused() {
        Some(Item::DisplayProfiles) => {
            pairs.push((format!("{}/{}", km.label(Action::Left), km.label(Action::Right)), "Choose"));
            pairs.push((km.label(Action::Activate), "Apply"));
        }
        _ => {
            pairs.push((km.label(Action::Toggle), "Toggle"));
            pairs.push((km.label(Action::Activate), "Adjust"));
        }
    }
}

/// Maximum map height, in rows.
pub const MAP_ROWS: u16 = 8;

/// A map cell: character, and does it belong to the focused screen?
pub type MapCell = Option<(char, bool)>;

/// Scaled map of the enabled screens, at most `width` columns. One terminal
/// row is worth roughly two columns. Each screen carries its number (rank
/// in the list) and its short name; the one focused in the list carries "●".
pub fn map_canvas(outs: &[Output], focused: Option<&str>, width: u16) -> Vec<Vec<MapCell>> {
    let labels = short_names(outs);
    let boxes: Vec<(usize, i32, i32, i32, i32)> = outs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| {
            o.active.as_ref().map(|a| {
                let (w, h) = logical_size(a.mode, a.scale, a.transform);
                (i, a.x, a.y, w, h)
            })
        })
        .collect();
    if boxes.is_empty() || width < 4 {
        return Vec::new();
    }
    let min_x = boxes.iter().map(|b| b.1).min().unwrap_or(0);
    let min_y = boxes.iter().map(|b| b.2).min().unwrap_or(0);
    let max_x = boxes.iter().map(|b| b.1 + b.3).max().unwrap_or(1);
    let max_y = boxes.iter().map(|b| b.2 + b.4).max().unwrap_or(1);
    let (bw, bh) = (f64::from((max_x - min_x).max(1)), f64::from((max_y - min_y).max(1)));
    let ky = (f64::from(MAP_ROWS) / bh).min(f64::from(width) / bw / 2.0);
    let kx = ky * 2.0;
    let cols = ((bw * kx).round() as usize).clamp(1, usize::from(width));
    let rows = ((bh * ky).round() as usize).clamp(1, usize::from(MAP_ROWS));
    let mut grid: Vec<Vec<MapCell>> = vec![vec![None; cols]; rows];
    for (i, x, y, w, h) in boxes {
        let cx = |v: i32| ((f64::from(v - min_x) * kx).round() as usize).min(cols);
        let cy = |v: i32| ((f64::from(v - min_y) * ky).round() as usize).min(rows);
        let (x0, y0) = (cx(x).min(cols - 1), cy(y).min(rows - 1));
        let x1 = cx(x + w).max(x0 + 1);
        let y1 = cy(y + h).max(y0 + 1);
        let foc = focused == Some(outs[i].name.as_str());
        let mut set = |c: usize, r: usize, ch: char| {
            if r < rows && c < cols {
                grid[r][c] = Some((ch, foc));
            }
        };
        let number = (i + 1).to_string();
        let (wc, hc) = (x1 - x0, y1 - y0);
        if hc == 1 || wc < 3 {
            // Too small for a frame: "[Alpha 3]" on one line.
            let text = truncate(&format!("[{} {number}]", labels[i]), wc);
            for (k, ch) in text.chars().enumerate() {
                set(x0 + k, y0, ch);
            }
            continue;
        }
        let (r0, r1) = (y0, y1 - 1);
        for c in x0..x1 {
            set(c, r0, '─');
            set(c, r1, '─');
        }
        for r in r0..=r1 {
            set(x0, r, '│');
            set(x1 - 1, r, '│');
        }
        set(x0, r0, '┌');
        set(x1 - 1, r0, '┐');
        set(x0, r1, '└');
        set(x1 - 1, r1, '┘');
        let name = truncate(&labels[i], wc.saturating_sub(2));
        let start = x0 + 1 + (wc - 2 - name.chars().count()) / 2;
        for (k, ch) in name.chars().enumerate() {
            set(start + k, r0, ch);
        }
        if hc >= 3 {
            let mid = r0 + (r1 - r0) / 2;
            let start = x0 + (wc - number.len()) / 2;
            for (k, ch) in number.chars().enumerate() {
                set(start + k, mid, ch);
            }
            if foc && wc >= 6 {
                set(x1 - 3, mid, '●');
            }
        }
    }
    grid
}

/// Profile bar row: "Profiles  [ Bureau ]  [ + Save current ]".
pub fn draw_profiles(buf: &mut Buffer, body: Rect, y: u16, focused: bool, app: &App, hits: &mut HitMap, rail: bool) {
    let th = &app.theme;
    let lx = body.x + 2;
    // Under the rail the focused row's band carries the focus: `fg` on it (the accent does not read on every `selection`).
    let label = if focused && !rail { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
    put(buf, body, lx, y, "Profiles", label);
    let mut names: Vec<String> = app.displays.profiles.iter().map(|p| p.name.clone()).collect();
    names.push("+ Save current".to_string());
    let mut x = lx + 11;
    for (j, name) in names.iter().enumerate() {
        let chosen = focused && j == app.displays.profile_sel;
        let (w, gap) = if rail {
            // The rail's chips: the chosen one filled with the tab's hue, the others on `raised`.
            let hue = super::rail::tab_hue(th, app.tab);
            let (fg, fill) = if chosen { (th.on_accent, hue) } else { (th.fg, th.raised) };
            let fill = if th.shape == Shape::Line && !chosen { th.muted } else { fill };
            let behind = if focused && th.shape != Shape::Line { th.selection } else { th.bg };
            (shape::chip(buf, body, x, y, &format!(" {name} "), Ink { fg, fill, behind, bold: chosen }, th.shape), 1)
        } else {
            let style = if chosen { th.selected() } else { Style::default().fg(th.accent) };
            (put(buf, body, x, y, &format!("[ {name} ]"), style), 2)
        };
        hits.push(Rect::new(x, y, w, 1), Target::DisplayProfile(j));
        x = x.saturating_add(w + gap);
    }
}

/// An output's row: number and short name, connector (⚠ DisplayLink),
/// on/off switch, mode, scale, placement.
#[allow(clippy::too_many_arguments)]
pub fn draw_output(buf: &mut Buffer, body: Rect, y: u16, i: usize, name: &str, focused: bool, app: &App, hits: &mut HitMap, rail: bool) {
    let th = &app.theme;
    let outs = app.outputs();
    let Some(k) = outs.iter().position(|o| o.name == name) else { return };
    let o = &outs[k];
    let labels = short_names(outs);
    let places = placement_labels(outs);
    let lx = body.x + 2;
    let cols = Cols::fit(body.width.saturating_sub(2), places.iter().map(|p| p.chars().count()).max().unwrap_or(0) as u16);
    // Under the rail the focused row's band carries the focus: `fg` on it (the accent does not read on every `selection`).
    let label = if focused && !rail { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
    // What the row sits on: the focused row's band under the rail. A dim word or the accent only where
    // they read there, else `fg`.
    let under = if rail && focused && th.shape != Shape::Line { th.selection } else { th.bg };
    let dim = th.ink(th.muted, under, 3.0);
    let lit = th.ink(th.accent, under, 4.5);
    hits.push(Rect::new(body.x, y, body.width, 1), Target::Item(i));
    put(buf, body, lx, y, &truncate(&format!("{} {}", k + 1, labels[k]), 12), label);
    if let Some(c) = cols.conn {
        let conn = if o.displaylink { format!("{} ⚠", o.name) } else { o.name.clone() };
        put(buf, body, lx + c, y, &truncate(&conn, 10), Style::default().fg(dim));
    }
    let last = o.active.is_some() && active_count(outs) <= 1;
    let (text, style) = match (&o.active, last) {
        (Some(_), true) => ("[■] only on", Style::default().fg(dim)),
        (Some(_), false) => ("[■] on", Style::default().fg(lit)),
        (None, _) => ("[ ] off", Style::default().fg(dim)),
    };
    let w = put(buf, body, lx + cols.toggle, y, text, style);
    hits.push(Rect::new(lx + cols.toggle, y, w, 1), Target::OutputToggle(i));
    let Some(a) = &o.active else { return };
    put(buf, body, lx + cols.mode, y, &a.mode.label(), Style::default().fg(th.fg));
    if let Some(c) = cols.scale {
        put(buf, body, lx + c, y, &format!("scale {}", a.scale.label()), Style::default().fg(th.fg));
    }
    let px = lx + cols.pos;
    let avail = usize::from(body.right().saturating_sub(px));
    put(buf, body, px, y, &truncate(&places[k], avail), Style::default().fg(dim));
}

/// Where an output row's columns start, from its left edge. The placement is what the tab is for, so
/// it is never the one to lose room: the scale goes first, then the connector (the map and the
/// name still say which screen it is).
struct Cols {
    conn: Option<u16>,
    toggle: u16,
    mode: u16,
    scale: Option<u16>,
    pos: u16,
}

impl Cols {
    /// The richest layout whose placement (`pos_w` cells at most) fits in `avail` cells.
    fn fit(avail: u16, pos_w: u16) -> Cols {
        let full = Cols { conn: Some(13), toggle: 23, mode: 37, scale: Some(51), pos: 64 };
        let no_scale = Cols { scale: None, pos: 51, ..full };
        let compact = Cols { conn: None, toggle: 13, mode: 27, scale: None, pos: 41 };
        if full.pos + pos_w <= avail {
            full
        } else if no_scale.pos + pos_w <= avail {
            no_scale
        } else {
            compact
        }
    }
}

// ── Overlays ─────────────────────────────────────────────────────────

/// The two buttons of a small box on row `y`: `no` (Cancel) then `yes`, the focused one filled.
/// `yes_enabled` false greys `yes` out (the Keep box's countdown has not started).
#[allow(clippy::too_many_arguments)]
fn buttons(buf: &mut Buffer, inner: Rect, y: u16, no: &str, yes: &str, yes_focused: bool, yes_enabled: bool, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let on = th.filled();
    let off = Style::default().fg(th.fg).bg(th.surface);
    let disabled = Style::default().fg(th.muted).bg(th.surface);
    let mut x = inner.x + 1;
    let w = put(buf, inner, x, y, &format!("[ {no} ]"), if yes_focused { off } else { on });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmNo);
    x = x.saturating_add(w + 2);
    let style = match (yes_enabled, yes_focused) {
        (false, _) => disabled,
        (true, true) => on,
        (true, false) => off,
    };
    let w = put(buf, inner, x, y, &format!("[ {yes} ]"), style);
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmYes);
}

/// "Keep these settings?": summary, missing screens, DisplayLink warning,
/// countdown; focus starts on Cancel.
pub fn keep(buf: &mut Buffer, area: Rect, app: &App, k: &Keep, hits: &mut HitMap) {
    let th = &app.theme;
    let Some(t) = &app.displays.trial else { return };
    let text = Style::default().fg(th.fg).bg(th.surface);
    // Head: title, summary, missing screens — cut from the bottom if the
    // window is short. Foot: DisplayLink warning, blank line (sacrificed
    // first) and countdown, always visible.
    let mut head: Vec<(String, Style)> = vec![(t.title.clone(), text.add_modifier(Modifier::BOLD))];
    head.extend(t.summary.iter().map(|s| (s.clone(), text)));
    head.extend(t.notes.iter().map(|s| (s.clone(), Style::default().fg(th.muted).bg(th.surface))));
    let mut foot: Vec<(String, Style)> = Vec::new();
    if t.displaylink {
        foot.push(("⚠ DisplayLink output: known freeze risk on this machine".into(), Style::default().fg(th.error).bg(th.surface)));
    }
    let wait = match t.deadline {
        None => "Applying…".to_string(),
        Some(_) => format!("Automatic rollback in {} s", t.remaining),
    };
    foot.push((String::new(), text));
    foot.push((wait, Style::default().fg(th.accent).bg(th.surface)));
    let r = centered(area, 66, (head.len() + foot.len()) as u16 + 3);
    let inner = frame(buf, r, " Keep these settings? ", app, hits);
    let room = usize::from(inner.height.saturating_sub(1));
    if head.len() + foot.len() > room {
        foot.remove(foot.len() - 2); // the blank line
    }
    head.truncate(room.saturating_sub(foot.len()));
    let skip = foot.len().saturating_sub(room); // extreme case: the countdown first
    let lines: Vec<_> = head.iter().chain(foot.iter().skip(skip)).collect();
    for (n, (l, style)) in lines.iter().enumerate() {
        put(buf, inner, inner.x + 1, inner.y + n as u16, &truncate(l, usize::from(inner.width.saturating_sub(2))), *style);
    }
    let y = inner.y + inner.height.saturating_sub(1);
    buttons(buf, inner, y, "Cancel", "Keep", k.keep_focused, t.deadline.is_some(), app, hits);
}

/// The label of a panel field (the two buttons have none: they are drawn apart).
fn field_label(f: PanelField) -> &'static str {
    match f {
        PanelField::Mode => "Mode",
        PanelField::Scale => "Scale",
        PanelField::Rotation => "Rotation",
        PanelField::Place => "Placement",
        PanelField::Reference => "Reference",
        PanelField::Align => "Alignment",
        PanelField::Cancel | PanelField::Apply => "",
    }
}

/// Displayed value of a panel field.
fn field_value(app: &App, p: &Panel, f: PanelField) -> String {
    match f {
        PanelField::Mode => {
            let modes = app.panel_modes(p);
            // The mode's place in the list, counted from 1; 0 if it is not in it.
            let k = match modes.iter().position(|m| *m == p.mode) {
                Some(k) => k + 1,
                None => 0,
            };
            format!("{}  {k}/{}", p.mode.label(), modes.len())
        }
        PanelField::Scale => p.scale.label(),
        PanelField::Rotation => rotation_label(p.transform),
        PanelField::Place => place_label(p.place).to_string(),
        PanelField::Reference => {
            let outs = app.outputs();
            let labels = short_names(outs);
            match &p.reference {
                None => "—".into(),
                Some(d) => match outs.iter().position(|o| o.description == *d) {
                    Some(i) => labels[i].clone(),
                    None => short_from_description(d),
                },
            }
        }
        PanelField::Align => match p.place {
            PlaceChoice::Rel(r) => p.align.label(r).to_string(),
            _ => String::new(),
        },
        PanelField::Cancel | PanelField::Apply => String::new(),
    }
}

/// Settings panel: one field per row (◀ value ▶), the reason for a refusal,
/// then Cancel and Apply (greyed out if the geometry refuses).
pub fn panel(buf: &mut Buffer, area: Rect, app: &App, p: &Panel, hits: &mut HitMap) {
    let th = &app.theme;
    let outs = app.outputs();
    let labels = short_names(outs);
    let title = match outs.iter().position(|o| o.name == p.output) {
        Some(i) => format!(" Settings — {} ({}) ", labels[i], p.output),
        None => format!(" Settings — {} ", p.output),
    };
    let fields: Vec<PanelField> = panel_fields(p).into_iter().filter(|f| !matches!(f, PanelField::Cancel | PanelField::Apply)).collect();
    let preview = app.panel_preview(p);
    let extra = u16::from(p.query.is_some()) + 1; // search, reason, or blank line
    let r = centered(area, 60, fields.len() as u16 + extra + 4);
    let inner = frame(buf, r, &title, app, hits);
    let text = Style::default().fg(th.fg).bg(th.surface);
    for (n, f) in fields.iter().enumerate() {
        let y = inner.y + n as u16;
        if y >= inner.bottom().saturating_sub(1) {
            break;
        }
        let focused = p.field == *f;
        let style = if focused { Style::default().fg(th.accent).bg(th.surface).add_modifier(Modifier::BOLD) } else { text };
        hits.push(Rect::new(inner.x, y, inner.width, 1), Target::PanelField(*f));
        put(buf, inner, inner.x + 1, y, if focused { "▶" } else { " " }, style);
        put(buf, inner, inner.x + 3, y, field_label(*f), style);
        let vx = inner.x + 16;
        put(buf, inner, vx, y, "◀", Style::default().fg(th.accent).bg(th.surface));
        hits.push(Rect::new(vx, y, 1, 1), Target::PanelStep { field: *f, dir: -1 });
        let w = put(buf, inner, vx + 2, y, &field_value(app, p, *f), text);
        put(buf, inner, vx + 3 + w, y, "▶", Style::default().fg(th.accent).bg(th.surface));
        hits.push(Rect::new(vx + 3 + w, y, 1, 1), Target::PanelStep { field: *f, dir: 1 });
    }
    let mut y = inner.y + fields.len() as u16;
    if let Some(q) = &p.query {
        put(buf, inner, inner.x + 3, y, &format!("Search: {q}▏"), Style::default().fg(th.accent).bg(th.surface));
        y += 1;
    }
    if let Err(e) = &preview {
        put(
            buf,
            inner,
            inner.x + 3,
            y,
            &truncate(&format!("✗ {e}"), usize::from(inner.width.saturating_sub(4))),
            Style::default().fg(th.error).bg(th.surface),
        );
    }
    let by = inner.y + inner.height.saturating_sub(1);
    let on = th.filled();
    let mut x = inner.x + 1;
    let cancel_style = if p.field == PanelField::Cancel { on } else { text };
    let w = put(buf, inner, x, by, "[ Cancel ]", cancel_style);
    hits.push(Rect::new(x, by, w, 1), Target::PanelCancel);
    x = x.saturating_add(w + 2);
    let apply_style = match (preview.is_ok(), p.field == PanelField::Apply) {
        (false, _) => Style::default().fg(th.muted).bg(th.surface),
        (true, true) => on,
        (true, false) => text,
    };
    let w = put(buf, inner, x, by, "[ Apply ]", apply_style);
    hits.push(Rect::new(x, by, w, 1), Target::PanelApply);
}

/// "Save current": name, then "Replace the profile?" if it already exists.
pub fn save(buf: &mut Buffer, area: Rect, app: &App, d: &SaveDialog, hits: &mut HitMap) {
    let th = &app.theme;
    let r = centered(area, 52, 7);
    let inner = frame(buf, r, " Save current ", app, hits);
    let text = Style::default().fg(th.fg).bg(th.surface);
    if d.replace {
        put(buf, inner, inner.x + 1, inner.y, &format!("Replace profile \"{}\"?", d.name), text.add_modifier(Modifier::BOLD));
        let y = inner.y + inner.height.saturating_sub(1);
        buttons(buf, inner, y, "Cancel", "Replace", d.yes_focused, true, app, hits);
        return;
    }
    put(buf, inner, inner.x + 1, inner.y, "Profile name:", text);
    put(buf, inner, inner.x + 1, inner.y + 1, &format!("> {}▏", d.name), Style::default().fg(th.accent).bg(th.surface));
    let y = inner.y + inner.height.saturating_sub(1);
    put(buf, inner, inner.x + 1, y, "Enter Save · Esc Cancel", Style::default().fg(th.muted).bg(th.surface));
}

/// Row `n` of the map, drawn starting at `x`.
pub fn draw_map_row(buf: &mut Buffer, body: Rect, x: u16, y: u16, row: &[MapCell], app: &App) {
    let th = &app.theme;
    for (c, cell) in row.iter().enumerate() {
        if let Some((ch, foc)) = cell {
            let style = if *foc { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
            put(buf, body, x + c as u16, y, &ch.to_string(), style);
        }
    }
}

/// Name of the output focused in the list, if any.
pub fn focused_output(app: &App) -> Option<String> {
    match app.focused() {
        Some(Item::Output(name)) => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
