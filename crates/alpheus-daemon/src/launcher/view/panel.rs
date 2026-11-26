//! The Super+X command panel drawn: the search field, the tab strip, a line, the list (the open
//! tab's section labels and tools on two lines, or the search's results under their category's
//! label, fixed choices as chips) beside the selected tool's detail card, a line, the footer. Every
//! number is in the ring's coordinates, as in the rest of the view.
//!
//! See: docs/superpowers/specs/2026-11-26-superx-panel-design.md ("Layout", "Search").

use super::{BoxLayout, Click, Hint, LETTER_CELLS, TOOLS_INSET, blank, capsule, category_hue, key_hints, ring_shape, tools_chip};
use crate::grid::{Grid, Style, Tint, text_width, truncate};
use crate::launcher::matching::Source;
use crate::launcher::model::Launcher;
use crate::launcher::tools::catalog::{Choices, Entry, EntryKind};
use crate::launcher::tools::nav::{RowId, ToolsState};
use crate::launcher::tools::panel::{Action, Label, Panel, PanelRow, Tab, actions};
use crate::list::{self, Boxed, Shape, ShapeKind, wrap};
use std::borrow::Cow;

/// The rows around the panes: the ring's two, the search field, the tabs, the line under them,
/// the line over the footer, the footer.
pub(super) const CHROME_ROWS: u16 = 7;
/// The panes' lines the box asks for: six two-line tools and five section labels between them.
pub(super) const PANE_ROWS: u16 = 23;
/// The narrowest list and card side by side: below, the list takes the box alone.
const SPLIT_COLUMNS: u16 = 80;
/// The widest a tab's name is drawn.
const TAB_MAX: u16 = 20;
/// The most lines a description takes in the card.
const CARD_DESCRIPTION_LINES: usize = 3;
/// The cells a switch takes at a tool's right, its gap included.
const SWITCH_CELLS: u16 = 3;

const HOME_HINTS: [Hint; 5] = [(&["⏎"], "Run"), (&["→"], "Options"), (&["Tab"], "Category"), (&["a-z"], "Quick key"), (&["Esc"], "Close")];
const HOME_HINTS_NO_LETTERS: [Hint; 4] = [(&["⏎"], "Run"), (&["→"], "Options"), (&["Tab"], "Category"), (&["Esc"], "Close")];
const SEARCH_HINTS: [Hint; 2] = [(&["⏎"], "Run"), (&["Esc"], "Close")];
const SEARCH_CHIP_HINTS: [Hint; 3] = [(&["⏎"], "Run"), (&["←", "→"], "Choose"), (&["Esc"], "Close")];
/// What a picked chip's and the open tab's text is drawn in: the surface's background kept under
/// it (a filled cell would square the pill).
const ON_ACCENT: Style = Style { fg: Tint::OnAccent, bg: Tint::Bg, bold: true };

/// Where the panel's parts lie in a box (ring coordinates, ends exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    tabs: usize,
    pane_top: usize,
    pane_rows: usize,
    /// The list pane's columns.
    list: (u16, u16),
    /// The detail card's columns, when the box has room for it.
    card: Option<(u16, u16)>,
}

fn geometry(b: &BoxLayout) -> Geometry {
    let rows = usize::from(b.rows);
    let (start, end) = (1 + TOOLS_INSET, b.columns.saturating_sub(1 + TOOLS_INSET));
    let inner = end.saturating_sub(start);
    // About 60 % for the list, the rest for the card, one blank column between.
    let (list, card) = if inner >= SPLIT_COLUMNS {
        let list_end = start + inner * 3 / 5;
        ((start, list_end), Some((list_end + 1, end)))
    } else {
        ((start, end), None)
    };
    Geometry { tabs: 2, pane_top: 4, pane_rows: rows.saturating_sub(usize::from(CHROME_ROWS)), list, card }
}

/// Whether `l` shows the panel: the Tools mode at an opened panel's root, its home or its search.
pub(super) fn shown(l: &Launcher) -> bool {
    l.tools().and_then(ToolsState::panel).is_some()
}

/// The list's rows: the open tab's at home; in the search one tool per result, in their order,
/// under their category's label (a root entry's group under Pinned's, where home lists it).
fn list_rows<'a>(l: &Launcher, t: &ToolsState, panel: &'a Panel) -> Cow<'a, [PanelRow]> {
    if !l.panel_search() {
        return Cow::Borrowed(&panel.rows);
    }
    let mut rows = Vec::with_capacity(l.results.len() * 2);
    let mut last = None;
    // The search's results are entries only (`matching::group_by_category`).
    let entries = l.results.iter().filter_map(|h| match h.source {
        Source::Tool(i) => match l.tool_rows().get(i).map(|r| r.id) {
            Some(RowId::Entry(e)) => Some(e),
            _ => None,
        },
        _ => None,
    });
    for e in entries {
        let label = t.catalog.entries.get(e).and_then(|x| x.category).map_or(Label::Pinned, Label::Category);
        if last != Some(label) {
            rows.push(PanelRow::Section(label));
            last = Some(label);
        }
        rows.push(PanelRow::Tool(e));
    }
    Cow::Owned(rows)
}

/// The selectable rows' entries, in row order.
fn row_tools(rows: &[PanelRow]) -> impl Iterator<Item = usize> + '_ {
    rows.iter().filter_map(|r| if let PanelRow::Tool(e) = *r { Some(e) } else { None })
}

/// The model's page at the panel: one tool per two lines of its pane (`Launcher::set_list_rows`);
/// any other box's list rows.
pub fn model_rows(l: &Launcher, b: &BoxLayout) -> usize {
    if l.panel_size().is_some() { (usize::from(b.rows.saturating_sub(CHROME_ROWS)) / 2).max(1) } else { usize::from(b.list_rows) }
}

/// A line of the list: a section label, or the first of a tool's two (`tool` indexes `Panel::tools`).
#[derive(Debug, Clone, Copy)]
struct Line {
    at: usize,
    row: PanelRow,
    tool: usize,
}

/// The list's lines, then the first line shown: the model's top tool (with its label right above
/// it), moved down only as far as the selected tool needs to show whole.
fn window(l: &Launcher, rows: &[PanelRow], pane_rows: usize) -> (Vec<Line>, usize) {
    let mut lines = Vec::with_capacity(rows.len());
    let (mut at, mut tool) = (0, 0);
    for &row in rows {
        lines.push(Line { at, row, tool });
        match row {
            PanelRow::Section(_) => at += 1,
            PanelRow::Tool(_) => {
                at += 2;
                tool += 1;
            }
        }
    }
    let total = at;
    // The first line of tool `k`, its label's when the label is right above it.
    let begin = |k: usize| {
        let i = lines.iter().position(|x| matches!(x.row, PanelRow::Tool(_)) && x.tool == k);
        i.map_or(0, |i| match i.checked_sub(1).map(|p| lines[p].row) {
            Some(PanelRow::Section(_)) => lines[i - 1].at,
            _ => lines[i].at,
        })
    };
    let mut start = begin(l.top);
    if !l.results.is_empty() {
        let first = begin(l.selected);
        let end = lines.iter().find(|x| matches!(x.row, PanelRow::Tool(_)) && x.tool == l.selected).map_or(0, |x| x.at + 2);
        if end > start + pane_rows {
            start = end - pane_rows;
        }
        start = start.min(first);
    }
    (lines, start.min(total.saturating_sub(pane_rows)))
}

/// The panel's home, drawn in `b`.
pub(super) fn render(l: &Launcher, b: &BoxLayout) -> Boxed {
    let (Some(t), rows) = (l.tools(), usize::from(b.rows)) else { return Boxed::default() };
    let Some(panel) = t.panel() else { return Boxed::default() };
    let g = geometry(b);
    let mut shapes = Vec::new();
    let mut grid: Vec<Grid> = (0..rows).map(|_| blank(b)).collect();
    if let Some(row) = grid.get_mut(1) {
        tools_chip(l, b, row, &mut shapes, 1);
    }
    if let Some(row) = grid.get_mut(g.tabs) {
        tab_strip(t, panel, b, row, &mut shapes, g.tabs);
    }
    let list = list_rows(l, t, panel);
    let search = l.panel_search();
    let (lines, start) = window(l, &list, g.pane_rows);
    let selected = (!l.results.is_empty() && !l.selection_lost()).then_some(l.selected);
    for line in &lines {
        let Some(offset) = line.at.checked_sub(start) else { continue };
        let height = if matches!(line.row, PanelRow::Tool(_)) { 2 } else { 1 };
        // Only what shows whole: a tool cut by the pane's edge is left out.
        if offset + height > g.pane_rows {
            break;
        }
        let y = g.pane_top + offset;
        match line.row {
            PanelRow::Section(label) => section(t, label, g.list, &mut grid[y]),
            PanelRow::Tool(e) => {
                let row = ToolLine { e, k: line.tool, selected: selected == Some(line.tool), search, y };
                tool_row(l, t, &row, g.list, &mut grid, &mut shapes);
            }
        }
    }
    if g.pane_rows > 0 && lines.iter().all(|x| !matches!(x.row, PanelRow::Tool(_))) {
        grid[g.pane_top].write(g.list.0 + 1, g.list.1, if search { "no match" } else { "nothing here" }, Style::MUTED);
    }
    let chosen = selected.and_then(|k| row_tools(&list).nth(k));
    if let (Some(card), Some(e)) = (g.card, chosen) {
        detail_card(t, panel, e, search, card, (g.pane_top, g.pane_rows), &mut grid, &mut shapes);
    }
    let footer = rows.saturating_sub(2);
    if let Some(row) = grid.get_mut(footer) {
        let (start, end) = (1 + TOOLS_INSET, b.columns.saturating_sub(1 + TOOLS_INSET));
        match &l.message {
            Some(m) => {
                row.write(start + 1, end, &truncate(m, end.saturating_sub(start + 1)), Style::ERROR);
            }
            None => {
                let hints: &[Hint] = match chosen {
                    Some(e) if search && t.chips(e).is_some() => &SEARCH_CHIP_HINTS,
                    _ if search => &SEARCH_HINTS,
                    _ if l.letters_live() && t.root_letters() => &HOME_HINTS,
                    _ => &HOME_HINTS_NO_LETTERS,
                };
                key_hints(row, &mut shapes, footer, start, end, hints);
            }
        }
    }
    let mut boxed = list::boxed(&grid, &[3, rows.saturating_sub(3)], None);
    boxed.shapes = shapes;
    boxed
}

/// The tabs a strip shows: each drawn tab's index, first column, width and name; whether tabs are
/// cut off at the left and at the right.
struct Strip {
    tabs: Vec<(usize, u16, u16, String)>,
    cut_left: bool,
    cut_right: bool,
}

/// The strip in box `b`: a pill per tab from the left; when they do not fit, it starts as far
/// right as the open tab needs, two cells kept at each cut end for its mark.
fn strip(t: &ToolsState, panel: &Panel, b: &BoxLayout) -> Strip {
    let names: Vec<String> = panel
        .tabs
        .iter()
        .map(|tab| match *tab {
            Tab::All => "All".to_string(),
            Tab::Category(k) => truncate(t.catalog.categories.get(k).map_or("", |c| c.name.as_str()), TAB_MAX),
        })
        .collect();
    let widths: Vec<u16> = names.iter().map(|n| text_width(n) + 2).collect();
    let (start, end) = (1 + TOOLS_INSET, b.columns.saturating_sub(1 + TOOLS_INSET));
    let span = |from: usize, to: usize| widths[from..=to].iter().map(|w| w + 1).sum::<u16>().saturating_sub(1);
    let last = widths.len().saturating_sub(1);
    let cut = !widths.is_empty() && span(0, last) > end.saturating_sub(start);
    let (from, to) = if cut { (start + 2, end.saturating_sub(2)) } else { (start, end) };
    let mut first = 0;
    while cut && first < panel.active && span(first, panel.active) > to.saturating_sub(from) {
        first += 1;
    }
    let mut x = from;
    let mut tabs = Vec::new();
    for (i, (name, w)) in names.into_iter().zip(widths).enumerate().skip(first) {
        if x + w > to {
            break;
        }
        tabs.push((i, x, w, name));
        x += w + 1;
    }
    let drawn = tabs.last().map_or(first, |(i, ..)| i + 1);
    Strip { tabs, cut_left: first > 0, cut_right: drawn < panel.tabs.len() }
}

/// The tab strip on row `y`: a pill per tab, the open one filled, `‹` and `›` at the cut ends.
fn tab_strip(t: &ToolsState, panel: &Panel, b: &BoxLayout, g: &mut Grid, shapes: &mut Vec<Shape>, y: usize) {
    let s = strip(t, panel, b);
    for (i, x, w, name) in &s.tabs {
        let active = *i == panel.active;
        let tint = if active { Tint::AccentContainer } else { Tint::Pill };
        shapes.push(ring_shape(y, *x, *w, 1, ShapeKind::Chip(tint)));
        g.write(x + 1, x + w - 1, name, if active { ON_ACCENT } else { Style::NORMAL });
    }
    let (start, end) = (1 + TOOLS_INSET, b.columns.saturating_sub(1 + TOOLS_INSET));
    if s.cut_left {
        g.write(start, start + 1, "‹", Style::MUTED);
    }
    if s.cut_right {
        g.write(end.saturating_sub(1), end, "›", Style::MUTED);
    }
}

/// A section label: dim capitals; a category's after its hue's dot.
fn section(t: &ToolsState, label: Label, list: (u16, u16), g: &mut Grid) {
    let x = list.0 + 1;
    match label {
        Label::Pinned => g.write(x, list.1, "PINNED", Style::MUTED),
        Label::Recent => g.write(x, list.1, "RECENT", Style::MUTED),
        Label::Category(k) => {
            let x = g.write(x, list.1, "●", category_hue(t, Some(k)));
            let name = t.catalog.categories.get(k).map_or_else(String::new, |c| c.name.to_uppercase());
            g.write(x + 1, list.1, &truncate(&name, list.1.saturating_sub(x + 2)), Style::MUTED)
        }
    };
}

/// A tool of the list: its entry, its index among the list's tools (the result's), whether it is
/// selected, whether the list is the search's, and its first line.
struct ToolLine {
    e: usize,
    k: usize,
    selected: bool,
    search: bool,
    y: usize,
}

/// A tool on rows `y` and `y + 1`: its tile with the glyph in its category's hue, its name (or its
/// pending question; in the search the matched characters in the accent), its description dim
/// below, or in the search its fixed choices as chips, the picked one filled; at the right of the
/// first line its switch (the slot kept from the first frame for a tool with a state), `›` when it
/// opens a level, its letter.
fn tool_row(l: &Launcher, t: &ToolsState, row: &ToolLine, list: (u16, u16), grid: &mut [Grid], shapes: &mut Vec<Shape>) {
    let &ToolLine { e, k, selected, search, y } = row;
    let Some(entry) = t.catalog.entries.get(e) else { return };
    if selected {
        // Under the tile and the capsule, listed before them.
        shapes.push(ring_shape(y, list.0, list.1.saturating_sub(list.0), 2, ShapeKind::Band));
    }
    let tile = list.0 + 1;
    shapes.push(ring_shape(y, tile, 3, 2, ShapeKind::Chip(Tint::Pill)));
    if let Some(icon) = entry.icon {
        grid[y].write(tile + 1, tile + 2, &icon.to_string(), category_hue(t, entry.category));
    }
    // From the right: the letter's capsule (its cells kept whether it shows), the switch, `›`.
    let mut right = list.1.saturating_sub(1 + LETTER_CELLS);
    let letter = entry.letter.filter(|_| l.letters_live());
    if let Some(c) = letter {
        capsule(&mut grid[y], shapes, y, right, right + LETTER_CELLS, &c.to_string(), Tint::Pill);
    }
    if entry.has_state {
        right = right.saturating_sub(SWITCH_CELLS);
        if let Some(on) = t.state_of(e) {
            shapes.push(ring_shape(y, right, 2, 1, ShapeKind::Switch(on)));
        }
    }
    // Its chips: in the search, for an entry with fixed choices, and the one picked if any.
    let chips = t.chips(e).filter(|_| search).map(|list| (list, t.chip(e)));
    if opens_level(entry) && chips.is_none() {
        right = right.saturating_sub(2);
        grid[y].write(right, right + 1, "›", Style::MUTED);
    }
    let (x, stop) = (tile + 4, right.saturating_sub(1));
    let room = stop.saturating_sub(x);
    let asked = match chips {
        Some((list, Some(picked))) if t.asking(RowId::Choice(e, picked)) => list.get(picked).map(|c| format!("{} · {c}? ⏎", entry.name)),
        _ if t.asking(RowId::Entry(e)) => l.tool_rows().get(k).filter(|r| r.id == RowId::Entry(e)).map(|r| t.question_text(r)),
        _ => None,
    };
    match asked {
        Some(q) => grid[y].write(x, stop, &truncate(&q, room), Style::URGENT),
        None if search => marked(&mut grid[y], x, stop, &entry.name, l.tool_marks(k)),
        None => grid[y].write(x, stop, &truncate(&entry.name, room), Style::NORMAL),
    };
    if let Some((list, picked)) = chips {
        chip_line(&mut grid[y + 1], shapes, y + 1, (x, stop), list, picked);
        return;
    }
    // No description: the category's name, dim, says where it lives.
    let second = entry.description.as_deref().or_else(|| entry.category.and_then(|c| t.catalog.categories.get(c)).map(|c| c.name.as_str()));
    if let Some(text) = second {
        grid[y + 1].write(x, stop, &truncate(text, room), Style::MUTED);
    }
}

/// `name` from column `x`, cut before `stop`, the characters at `marks` (character positions) in
/// the accent. The column after it.
fn marked(g: &mut Grid, x: u16, stop: u16, name: &str, marks: &[u32]) -> u16 {
    let shown = truncate(name, stop.saturating_sub(x));
    // A cut name ends with `…`, which matched nothing.
    let whole = shown.chars().count().saturating_sub(usize::from(shown != name));
    let mut x = x;
    let mut buf = [0; 4];
    for (i, c) in shown.chars().enumerate() {
        let hit = i < whole && u32::try_from(i).is_ok_and(|i| marks.binary_search(&i).is_ok());
        x = g.write(x, stop, c.encode_utf8(&mut buf), if hit { Style::ACCENT } else { Style::NORMAL });
    }
    x
}

/// A row's fixed choices as chips on row `y` within `columns`, one blank cell apart, the picked one
/// filled: the line starts at the first chip, or as far right as the picked one needs to show;
/// when they do not all fit, `‹` and `›` mark the cut ends, as on the tab strip.
fn chip_line(g: &mut Grid, shapes: &mut Vec<Shape>, y: usize, (start, stop): (u16, u16), list: &[String], picked: Option<usize>) {
    let room = stop.saturating_sub(start);
    // A chip never wider than the line, its cut marks kept.
    let words: Vec<String> = list.iter().map(|c| truncate(c, room.saturating_sub(6))).collect();
    let widths: Vec<u16> = words.iter().map(|w| text_width(w) + 2).collect();
    let span = |from: usize, to: usize| widths.get(from..=to).map_or(0, |w| w.iter().map(|w| w + 1).sum::<u16>().saturating_sub(1));
    let cut = span(0, widths.len().saturating_sub(1)) > room;
    // Two cells kept at each cut end for its mark: at the left only once the line starts further right.
    let to = if cut { stop.saturating_sub(2) } else { stop };
    let anchor = picked.unwrap_or(0);
    let mut first = 0;
    while first < anchor && span(first, anchor) > to.saturating_sub(start + 2) {
        first += 1;
    }
    let mut x = if first > 0 { start + 2 } else { start };
    let mut drawn = first;
    for (i, (word, w)) in words.iter().zip(&widths).enumerate().skip(first) {
        if x + w > to {
            break;
        }
        let on = Some(i) == picked;
        shapes.push(ring_shape(y, x, *w, 1, ShapeKind::Chip(if on { Tint::AccentContainer } else { Tint::Pill })));
        g.write(x + 1, x + w - 1, word, if on { ON_ACCENT } else { Style::NORMAL });
        x += w + 1;
        drawn = i + 1;
    }
    if first > 0 {
        g.write(start, start + 1, "‹", Style::MUTED);
    }
    if drawn < words.len() {
        g.write(stop.saturating_sub(1), stop, "›", Style::MUTED);
    }
}

/// Whether Enter on the entry opens a level (its choices, or the text it asks for).
fn opens_level(e: &Entry) -> bool {
    !matches!(e.choices, Choices::None) || e.ask.is_some()
}

/// The selected tool's card over `columns` and the pane's rows: its tile, its name in bold, its
/// category and kind dim, its description wrapped, a thin line, its actions with their keys at
/// the right, and `Pinned` for a pinned tool. A line that does not fit is left out.
#[allow(clippy::too_many_arguments)]
fn detail_card(
    t: &ToolsState,
    panel: &Panel,
    e: usize,
    search: bool,
    columns: (u16, u16),
    (top, rows): (usize, usize),
    grid: &mut [Grid],
    shapes: &mut Vec<Shape>,
) {
    let Some(entry) = t.catalog.entries.get(e) else { return };
    shapes.push(ring_shape(top, columns.0, columns.1.saturating_sub(columns.0), u16::try_from(rows).unwrap_or(u16::MAX), ShapeKind::Card));
    let (x, end) = (columns.0 + 2, columns.1.saturating_sub(2));
    let width = end.saturating_sub(x);
    // The last line inside the card, one blank line kept under it.
    let bottom = (top + rows).saturating_sub(1);
    let mut y = top + 1;
    if y + 2 <= bottom {
        shapes.push(ring_shape(y, x, 3, 2, ShapeKind::Chip(Tint::Pill)));
        if let Some(icon) = entry.icon {
            grid[y].write(x + 1, x + 2, &icon.to_string(), category_hue(t, entry.category));
        }
        y += 3;
    }
    // A line of the card at `y`, when it fits: `y` then moves past it.
    let put = |grid: &mut [Grid], y: &mut usize, text: &str, style: Style| {
        if *y < bottom {
            grid[*y].write(x, end, &truncate(text, width), style);
            *y += 1;
        }
    };
    put(grid, &mut y, &entry.name, Style::BOLD);
    let kind = match entry.kind {
        EntryKind::Builtin(_) => "built-in",
        EntryKind::User(_) => "yours",
    };
    let meta = match entry.category.and_then(|c| t.catalog.categories.get(c)) {
        Some(c) => format!("{} · {kind}", c.name),
        None => kind.to_string(),
    };
    put(grid, &mut y, &meta, Style::MUTED);
    if let Some(d) = &entry.description {
        y += 1;
        let mut lines = wrap(d, width);
        lines.truncate(CARD_DESCRIPTION_LINES);
        for line in &lines {
            put(grid, &mut y, line, Style::NORMAL);
        }
    }
    put(grid, &mut y, &"─".repeat(usize::from(width)), Style::MUTED);
    for action in actions(entry) {
        if y >= bottom {
            return;
        }
        let (label, keys) = action_line(t, e, &action, search && t.chips(e).is_some());
        let mut kx = end;
        for key in keys.iter().rev() {
            kx = kx.saturating_sub(text_width(key) + 2);
            capsule(&mut grid[y], shapes, y, kx, kx + text_width(key) + 2, key, Tint::Selection);
            kx = kx.saturating_sub(1);
        }
        grid[y].write(x, kx, &truncate(&label, kx.saturating_sub(x + 1)), Style::NORMAL);
        y += 1;
    }
    if panel.rows.iter().position(|r| *r == PanelRow::Section(Label::Pinned)).is_some_and(|p| pinned_holds(panel, p, e)) {
        y += 1;
        put(grid, &mut y, "Pinned", Style::MUTED);
    }
}

/// Whether the Pinned section starting at row `p` lists entry `e`.
fn pinned_holds(panel: &Panel, p: usize, e: usize) -> bool {
    panel.rows[p + 1..].iter().take_while(|r| matches!(r, PanelRow::Tool(_))).any(|r| *r == PanelRow::Tool(e))
}

/// An action's words and its keys (`chips`: the choices are the row's chips, picked by ← and →).
fn action_line(t: &ToolsState, e: usize, action: &Action, chips: bool) -> (String, Vec<String>) {
    let enter = || vec!["⏎".to_string()];
    match action {
        Action::Run => ("Run".into(), enter()),
        Action::Open => ("Open".into(), enter()),
        Action::Choose if chips => ("Choose".into(), vec!["←".into(), "→".into()]),
        Action::Choose => ("Choose".into(), vec!["→".into()]),
        Action::Type(label) => (format!("Type {label}"), enter()),
        Action::Confirm => ("Confirm".into(), vec!["⏎".into(), "⏎".into()]),
        Action::Toggle => {
            let words = match t.state_of(e) {
                Some(true) => "Turn off",
                Some(false) => "Turn on",
                None => "Turn on/off",
            };
            (words.into(), enter())
        }
        Action::QuickKey(c) => ("Quick key".into(), vec![c.to_string()]),
    }
}

/// Cell (column, row) of the panel → what it hits: a tab of the strip, or a tool (its two lines
/// are its result).
pub(super) fn hit(l: &Launcher, b: &BoxLayout, column: u16, row: u16) -> Click {
    let (Some(t), g) = (l.tools(), geometry(b)) else { return Click::Inside };
    let Some(panel) = t.panel() else { return Click::Inside };
    let row = usize::from(row);
    if row == g.tabs {
        return strip(t, panel, b)
            .tabs
            .iter()
            .find(|(_, x, w, _)| (*x..x + w).contains(&column))
            .map_or(Click::Inside, |(i, ..)| Click::Tab(*i));
    }
    if !(g.list.0..g.list.1).contains(&column) || row < g.pane_top || row >= g.pane_top + g.pane_rows {
        return Click::Inside;
    }
    let list = list_rows(l, t, panel);
    let (lines, start) = window(l, &list, g.pane_rows);
    let line = start + row - g.pane_top;
    lines
        .iter()
        .find(|x| {
            matches!(x.row, PanelRow::Tool(_)) && (x.at..x.at + 2).contains(&line) && x.at >= start && x.at + 2 <= start + g.pane_rows
        })
        .map_or(Click::Inside, |x| Click::Result(x.tool))
}
