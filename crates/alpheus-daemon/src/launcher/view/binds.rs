//! The Keybindings level drawn in the command panel's box, with the panel's pieces: the search
//! field, a blank row, the tab strip (`All`, then each group), a line, the list (each binding on
//! two lines: its combo in a capsule then its description, its command dim under it; on All under
//! its group's label) beside the selected binding's card, a line, the footer. Every number is in
//! the ring's coordinates, as in the rest of the view.
//!
//! See: docs/superpowers/specs/2026-11-27-session-binds-design.md ("Cheatsheet").

use super::panel::{self, Geometry};
use super::{BoxLayout, Click, Hint, TOOLS_INSET, blank, key_hints, ring_shape, tools_chip};
use crate::grid::{Grid, Style, Tint, text_width, truncate};
use crate::launcher::matching::Source;
use crate::launcher::model::Launcher;
use crate::launcher::tools::binds::Binds;
use crate::launcher::tools::nav::{RowId, ToolsState};
use crate::launcher::tools::panel::{Label, PanelRow, window};
use crate::list::{self, Boxed, Shape, ShapeKind, wrap};
use std::borrow::Cow;

const HINTS: [Hint; 3] = [(&["Tab"], "Group"), (&[], "type to search"), (&["Esc"], "Back")];
/// One group at most: Tab opens nothing, so it is not offered.
const HINTS_ONE_GROUP: [Hint; 2] = [(&[], "type to search"), (&["Esc"], "Back")];
/// The most lines a description takes in the card.
const CARD_DESCRIPTION_LINES: usize = 3;

/// Whether `l` draws this level: the Tools mode at its Keybindings level, in a panel's box.
pub(super) fn shown(l: &Launcher) -> bool {
    l.at_binds() && l.panel_size().is_some()
}

/// The list's rows: one per result (each binding's index into `Binds::binds`), in their order; on
/// the All tab under their group's label, each time the group changes.
fn list_rows(l: &Launcher, t: &ToolsState, binds: &Binds) -> Vec<PanelRow> {
    let all = t.bind_tab() == 0;
    let mut rows = Vec::with_capacity(l.results.len() + if all { binds.groups.len() } else { 0 });
    let mut last = None;
    // The level's results are bindings only (`ToolsState::rows` at `Level::Binds`).
    let shown = l.results.iter().filter_map(|h| match h.source {
        Source::Tool(i) => match l.tool_rows().get(i).map(|r| r.id) {
            Some(RowId::Bind(k)) => Some(k),
            _ => None,
        },
        _ => None,
    });
    for k in shown {
        let group = binds.binds.get(k).map(|b| b.group);
        if all && group.is_some() && group != last {
            rows.extend(group.map(|g| PanelRow::Section(Label::Group(g))));
            last = group;
        }
        rows.push(PanelRow::Tool(k));
    }
    rows
}

/// The tab names: `All`, then each group's, borrowed.
fn tab_names(binds: &Binds) -> Vec<Cow<'_, str>> {
    std::iter::once(Cow::Borrowed("All")).chain(binds.groups.iter().map(|g| Cow::Borrowed(g.as_str()))).collect()
}

/// The list pane's lines: the panel's, less the last when it says how many lines were not
/// understood.
fn list_lines(g: &Geometry, binds: &Binds) -> usize {
    if binds.not_understood > 0 { g.pane_rows.saturating_sub(1) } else { g.pane_rows }
}

/// The cells of the combos' column: the widest combo of `rows` in its capsule, at most two fifths
/// of the list pane, so every description starts at the same column.
fn combo_cells(rows: &[PanelRow], binds: &Binds, list: (u16, u16)) -> u16 {
    let most = list.1.saturating_sub(list.0) * 2 / 5;
    let widest = rows
        .iter()
        .filter_map(|r| if let PanelRow::Tool(k) = *r { binds.binds.get(k) } else { None })
        .map(|b| text_width(&b.combo).saturating_add(2))
        .max()
        .unwrap_or(0);
    widest.min(most)
}

/// The level drawn in `b`.
pub(super) fn render(l: &Launcher, b: &BoxLayout) -> Boxed {
    let rows = usize::from(b.rows);
    let Some((t, binds)) = l.tools().and_then(|t| Some((t, t.binds()?))) else { return Boxed::default() };
    let g = panel::geometry(b);
    let mut shapes = Vec::new();
    let mut grid: Vec<Grid> = (0..rows).map(|_| blank(b)).collect();
    if let Some(row) = grid.get_mut(1) {
        tools_chip(l, b, row, &mut shapes, 1);
    }
    // One group or none: All is the only tab, so no strip.
    if let Some(row) = grid.get_mut(g.tabs).filter(|_| binds.groups.len() > 1) {
        panel::tab_strip(&tab_names(binds), t.bind_tab(), b, row, &mut shapes, g.tabs);
    }
    let list = list_rows(l, t, binds);
    let pane = list_lines(&g, binds);
    let (lines, start) = window(&list, l.top, (!l.results.is_empty()).then_some(l.selected), pane);
    let selected = (!l.results.is_empty() && !l.selection_lost()).then_some(l.selected);
    let cells = combo_cells(&list, binds, g.list);
    let mut chosen = None;
    for line in &lines {
        let Some(offset) = line.at.checked_sub(start) else { continue };
        // Only what shows whole, as at the panel's home: a label always has a binding under it.
        let height = if matches!(line.row, PanelRow::Tool(_)) { 2 } else { 3 };
        if offset + height > pane {
            break;
        }
        let y = g.pane_top + offset;
        match line.row {
            PanelRow::Section(label) => panel::section(t, label, g.list, &mut grid[y]),
            PanelRow::Tool(k) => {
                let on = selected == Some(line.tool);
                if on {
                    chosen = Some(k);
                }
                bind_row(binds, k, on, (g.list, cells), y, &mut grid, &mut shapes);
            }
        }
    }
    if pane > 0 && lines.iter().all(|x| !matches!(x.row, PanelRow::Tool(_))) {
        let text = if l.query.trim().is_empty() { "No keybindings found" } else { "no match" };
        grid[g.pane_top].write(g.list.0 + 1, g.list.1, text, Style::MUTED);
    }
    if binds.not_understood > 0 && g.pane_rows > 1 {
        let n = binds.not_understood;
        let note = format!("{n} line{} not understood", if n == 1 { "" } else { "s" });
        grid[g.pane_top + g.pane_rows - 1].write(
            g.list.0 + 1,
            g.list.1,
            &truncate(&note, g.list.1.saturating_sub(g.list.0 + 1)),
            Style::MUTED,
        );
    }
    // The selected binding's card; one scrolled out of the pane keeps its card too.
    let chosen = chosen
        .or_else(|| selected.and_then(|k| list.iter().filter_map(|r| if let PanelRow::Tool(b) = *r { Some(b) } else { None }).nth(k)));
    if let (Some(card), Some(k)) = (g.card, chosen) {
        bind_card(binds, k, card, (g.pane_top, g.pane_rows), &mut grid, &mut shapes);
    }
    let footer = rows.saturating_sub(2);
    if let Some(row) = grid.get_mut(footer) {
        let (start, end) = (1 + TOOLS_INSET, b.columns.saturating_sub(1 + TOOLS_INSET));
        match &l.message {
            Some(m) => {
                row.write(start + 1, end, &truncate(m, end.saturating_sub(start + 1)), Style::ERROR);
            }
            None => {
                let hints: &[Hint] = if binds.groups.len() > 1 { &HINTS } else { &HINTS_ONE_GROUP };
                key_hints(row, &mut shapes, footer, start, end, hints);
            }
        }
    }
    let mut boxed = list::boxed(&grid, &[g.tabs + 1, rows.saturating_sub(3)], None);
    boxed.shapes = shapes;
    boxed
}

/// Binding `k` on rows `y` and `y + 1` of the list pane `list`: its combo in a capsule (cut to the
/// combos' column, `cells`), its description after the column, its command dim under it; the band
/// under both lines when selected.
fn bind_row(
    binds: &Binds,
    k: usize,
    selected: bool,
    (list, cells): ((u16, u16), u16),
    y: usize,
    grid: &mut [Grid],
    shapes: &mut Vec<Shape>,
) {
    let Some(bind) = binds.binds.get(k) else { return };
    if selected {
        // Under the capsule, listed before it.
        shapes.push(ring_shape(y, list.0, list.1.saturating_sub(list.0), 2, ShapeKind::Band));
    }
    let x = list.0 + 1;
    let combo = truncate(&bind.combo, cells.saturating_sub(2));
    let w = text_width(&combo);
    if w > 0 {
        shapes.push(ring_shape(y, x, w + 2, 1, ShapeKind::Chip(Tint::Pill)));
        grid[y].write(x + 1, x + 1 + w, &combo, Style::NORMAL);
    }
    let (x, stop) = (x + cells + 1, list.1.saturating_sub(1));
    let room = stop.saturating_sub(x);
    grid[y].write(x, stop, &truncate(&bind.description, room), Style::NORMAL);
    grid[y + 1].write(x, stop, &truncate(&bind.command, room), Style::MUTED);
}

/// `text`'s words in lines of at most `width` cells, a word wider than a line broken across lines:
/// nothing is cut.
fn wrap_whole(text: &str, width: u16) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut pieces = String::with_capacity(text.len() + 8);
    for word in text.split_whitespace() {
        let mut cells = 0;
        for c in word.chars() {
            let mut buf = [0; 4];
            let w = text_width(c.encode_utf8(&mut buf));
            if cells + w > width {
                pieces.push(' ');
                cells = 0;
            }
            pieces.push(c);
            cells += w;
        }
        pieces.push(' ');
    }
    wrap(&pieces, width)
}

/// Binding `k`'s card over `columns` and the pane's rows: its combo in a capsule, its group dim,
/// its description wrapped, a thin line, `Command` and the whole command wrapped, then the file it
/// came from, by name. A line that does not fit is left out.
pub(super) fn bind_card(
    binds: &Binds,
    k: usize,
    columns: (u16, u16),
    (top, rows): (usize, usize),
    grid: &mut [Grid],
    shapes: &mut Vec<Shape>,
) {
    let Some(bind) = binds.binds.get(k) else { return };
    shapes.push(ring_shape(top, columns.0, columns.1.saturating_sub(columns.0), u16::try_from(rows).unwrap_or(u16::MAX), ShapeKind::Card));
    let (x, end) = (columns.0 + 2, columns.1.saturating_sub(2));
    let width = end.saturating_sub(x);
    // The last line inside the card, one blank line kept under it.
    let bottom = (top + rows).saturating_sub(1);
    let mut y = top + 1;
    if y < bottom {
        let combo = truncate(&bind.combo, width.saturating_sub(2));
        let w = text_width(&combo);
        if w > 0 {
            shapes.push(ring_shape(y, x, w + 2, 1, ShapeKind::Chip(Tint::Selection)));
            grid[y].write(x + 1, x + 1 + w, &combo, Style::BOLD);
        }
        y += 1;
    }
    let put = |grid: &mut [Grid], y: &mut usize, text: &str, style: Style| {
        if *y < bottom {
            grid[*y].write(x, end, &truncate(text, width), style);
            *y += 1;
        }
    };
    put(grid, &mut y, binds.groups.get(bind.group).map_or("", String::as_str), Style::MUTED);
    let mut lines = wrap(&bind.description, width);
    lines.truncate(CARD_DESCRIPTION_LINES);
    for line in &lines {
        put(grid, &mut y, line, Style::NORMAL);
    }
    put(grid, &mut y, &"─".repeat(usize::from(width)), Style::MUTED);
    // `Command` and the command whole, its file's line kept under them; past the card's foot the
    // last line kept says it goes on. No room for one line of it: no label either.
    let from = binds.files.get(bind.file).map(|f| format!("From {f}"));
    let room = bottom.saturating_sub(y + 1 + usize::from(from.is_some()));
    if room > 0 {
        put(grid, &mut y, "Command", Style::MUTED);
        let mut command = wrap_whole(&bind.command, width);
        if command.len() > room {
            command.truncate(room);
            if let Some(last) = command.last_mut() {
                *last = truncate(&format!("{last}…"), width);
            }
        }
        for line in &command {
            put(grid, &mut y, line, Style::NORMAL);
        }
    }
    if let Some(from) = from {
        put(grid, &mut y, &from, Style::MUTED);
    }
}

/// Cell (column, row) of the level → what it hits: a tab of the strip, or a binding (its two lines
/// are its result).
pub(super) fn hit(l: &Launcher, b: &BoxLayout, column: u16, row: u16) -> Click {
    let Some((t, binds)) = l.tools().and_then(|t| Some((t, t.binds()?))) else { return Click::Inside };
    let g = panel::geometry(b);
    let row = usize::from(row);
    if row == g.tabs && binds.groups.len() > 1 {
        return panel::strip(&tab_names(binds), t.bind_tab(), b)
            .tabs
            .iter()
            .find(|(_, x, w, _)| (*x..x + w).contains(&column))
            .map_or(Click::Inside, |(i, ..)| Click::Tab(*i));
    }
    let pane = list_lines(&g, binds);
    if !(g.list.0..g.list.1).contains(&column) || row < g.pane_top || row >= g.pane_top + pane {
        return Click::Inside;
    }
    let list = list_rows(l, t, binds);
    let (lines, start) = window(&list, l.top, (!l.results.is_empty()).then_some(l.selected), pane);
    let line = start + row - g.pane_top;
    lines.iter().find(|x| x.drawn(start, pane) && (x.at..x.at + 2).contains(&line)).map_or(Click::Inside, |x| Click::Result(x.tool))
}
