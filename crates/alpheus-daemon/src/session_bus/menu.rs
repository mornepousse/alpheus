//! A tray item's menu as the bar draws it (spec § 1 Menus, § 2): rows of
//! cells inside a pixel frame (the painter's), the launcher's list rendering and keys
//! (`crate::list`, `crate::keys`), one level at a time — a submenu replaces
//! the list in the same popup, its first row `◂ <parent>` (drill-down; see
//! docs/decisions.md, "A submenu opens in the same popup"). Pure: it reads the session's `Layout` and answers with a
//! `MenuAction`; the D-Bus calls and the popup are the wiring's.
//! Never selects a separator, a disabled or a hidden entry.

use crate::grid::{Grid, Style, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::session_bus::codec::{Layout, MenuNode, Toggle};

/// What a menu interaction asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Nothing,
    Redraw,
    Close,
    /// The user chose this entry: Event(id, "clicked"), then close.
    Click(i32),
    /// Entering this submenu: fetch it if its children are not known (MenuSession::fetch_submenu).
    Submenu(i32),
}

/// Most cells a menu takes across (labels truncated beyond).
pub const MENU_MAX_COLUMNS: u16 = 48;
/// Fewest cells across: a menu of one short label still reads as a menu.
const MENU_MIN_COLUMNS: u16 = 12;
/// The layout's root (dbusmenu id 0).
const ROOT: i32 = 0;
const BACK: char = '◂';
const SUBMENU: &str = "▸";
const MORE_ABOVE: char = '▴';
const MORE_BELOW: char = '▾';
const EMPTY: &str = "empty";

/// One level of the drill-down: the node whose children are listed, the selected entry (by id, so a
/// layout change keeps it) and the first entry shown.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Level {
    parent: i32,
    selected: Option<i32>,
    top: usize,
}

const ROOT_LEVEL: Level = Level { parent: ROOT, selected: None, top: 0 };

/// The open menu's view state: a stack of levels (drill-down, question 2), each with its selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    levels: Vec<Level>,
    /// Content rows (the header row included), from `set_rows`.
    rows: usize,
}

impl Default for Menu {
    fn default() -> Self {
        Menu::new()
    }
}

fn selectable(n: &MenuNode) -> bool {
    !n.is_separator() && n.is_enabled()
}

/// `parent`'s visible children, in order (empty when `parent` is unknown).
fn entries_of(parent: i32, layout: &Layout) -> Vec<&MenuNode> {
    layout.node(parent).map(|n| n.children.iter().filter(|c| c.is_visible()).collect()).unwrap_or_default()
}

/// Whether the level entered at `parent` is still alive: it has visible children to list, and its
/// own entry — among `host`, the entries of the level it was entered from — is still visible (by
/// construction of `host`) and selectable. The one predicate `enter` and `relayout` both use: the
/// first refuses to push a level that fails it, the second pops one that no longer passes it.
fn level_alive(parent: i32, host: &[&MenuNode], layout: &Layout) -> bool {
    let has_children = layout.node(parent).is_some_and(|n| n.children.iter().any(MenuNode::is_visible));
    has_children && host.iter().any(|e| e.id == parent && selectable(e))
}

impl Menu {
    pub fn new() -> Menu {
        Menu { levels: vec![ROOT_LEVEL], rows: usize::MAX }
    }

    /// The current level; the root one if the stack were ever empty (it never is: `back` and
    /// `relayout` keep the root — and a panic here would take the bar down).
    fn level(&self) -> &Level {
        self.levels.last().unwrap_or(&ROOT_LEVEL)
    }

    fn level_mut(&mut self) -> &mut Level {
        if self.levels.is_empty() {
            self.levels.push(ROOT_LEVEL);
        }
        let last = self.levels.len() - 1; // at least 1 entry here
        &mut self.levels[last]
    }

    /// The current level's selected entry, if any (the wiring checks it still
    /// matches a submenu fetch before grafting a reply that may have arrived late).
    pub fn selected(&self) -> Option<i32> {
        self.level().selected
    }

    /// 1 when the current level is a submenu: its `◂ parent` header row.
    fn header(&self) -> usize {
        usize::from(self.levels.len() > 1)
    }

    /// Entry rows visible under the header.
    fn entry_rows(&self) -> usize {
        self.rows.saturating_sub(self.header()).max(1)
    }

    /// The visible entries of the current level (hidden ones skipped), in order.
    pub fn entries<'a>(&self, layout: &'a Layout) -> Vec<&'a MenuNode> {
        entries_of(self.level().parent, layout)
    }

    /// Index of the selected entry in `entries`, if it is still there and selectable.
    fn selected_index(&self, entries: &[&MenuNode]) -> Option<usize> {
        let id = self.level().selected?;
        entries.iter().position(|e| e.id == id && selectable(e))
    }

    /// Selects `entries[i]` and keeps it visible.
    fn select(&mut self, entries: &[&MenuNode], i: usize) {
        let rows = self.entry_rows();
        let level = self.level_mut();
        level.selected = Some(entries[i].id);
        level.top = list::keep_visible(i, level.top, rows, entries.len());
    }

    /// The selection moved by `delta` over selectable entries (none selected yet: the first or
    /// last one, by direction); Redraw when it changed.
    fn step(&mut self, delta: i32, layout: &Layout) -> MenuAction {
        let entries = self.entries(layout);
        let ok = |i: usize| selectable(entries[i]);
        let to = match self.selected_index(&entries) {
            Some(i) => list::step(i, delta, entries.len(), ok),
            None if delta > 0 => match list::first_selectable(entries.len(), ok) {
                Some(i) => i,
                None => return MenuAction::Nothing,
            },
            None => match list::last_selectable(entries.len(), ok) {
                Some(i) => i,
                None => return MenuAction::Nothing,
            },
        };
        if self.selected_index(&entries) == Some(to) {
            return MenuAction::Nothing;
        }
        self.select(&entries, to);
        MenuAction::Redraw
    }

    /// What choosing `entries[i]` does: enter its submenu, or click it.
    fn choose(entry: &MenuNode) -> MenuAction {
        if entry.has_submenu() { MenuAction::Submenu(entry.id) } else { MenuAction::Click(entry.id) }
    }

    /// ↑ ↓ (and Ctrl+P/N, PageUp/PageDown, Home/End) move over enabled non-separator entries; Enter runs
    /// (Click) or enters a submenu (Submenu); → enters a submenu; ← goes back one level (at the root:
    /// Nothing); Escape → Close; anything else → Nothing.
    pub fn key(&mut self, k: Key, layout: &Layout) -> MenuAction {
        let page = i32::try_from(self.entry_rows()).unwrap_or(i32::MAX);
        match k {
            Key::Up | Key::CtrlP => self.step(-1, layout),
            Key::Down | Key::CtrlN => self.step(1, layout),
            Key::PageUp => self.step(-page, layout),
            Key::PageDown => self.step(page, layout),
            Key::Home => self.step(i32::MIN, layout),
            Key::End => self.step(i32::MAX, layout),
            Key::Enter => {
                let entries = self.entries(layout);
                self.selected_index(&entries).map_or(MenuAction::Nothing, |i| Self::choose(entries[i]))
            }
            Key::Right => {
                let entries = self.entries(layout);
                match self.selected_index(&entries).map(|i| entries[i]) {
                    Some(e) if e.has_submenu() => MenuAction::Submenu(e.id),
                    _ => MenuAction::Nothing,
                }
            }
            Key::Left => self.back(),
            Key::Escape => MenuAction::Close,
            _ => MenuAction::Nothing,
        }
    }

    /// Goes back one level, like `←`; a press on the `◂ parent` header row does the same
    /// (reason not recorded). `Nothing` at the root: there is no level left to pop.
    fn back(&mut self) -> MenuAction {
        if self.levels.len() > 1 {
            self.levels.pop();
            MenuAction::Redraw
        } else {
            MenuAction::Nothing
        }
    }

    /// The entry under content row `row` (frame excluded; the header is no entry).
    fn entry_at(&self, row: u16, entries: &[&MenuNode]) -> Option<usize> {
        let row = usize::from(row).checked_sub(self.header())?;
        if row >= self.entry_rows() {
            return None;
        }
        let i = self.top(entries) + row;
        (i < entries.len()).then_some(i)
    }

    /// Pointer over content row `row` (frame excluded): selects it if selectable. Hovering the
    /// header (a submenu's `◂ parent` row) selects nothing.
    pub fn hover(&mut self, row: u16, layout: &Layout) -> MenuAction {
        let entries = self.entries(layout);
        match self.entry_at(row, &entries) {
            Some(i) if selectable(entries[i]) && self.selected_index(&entries) != Some(i) => {
                self.select(&entries, i);
                MenuAction::Redraw
            }
            _ => MenuAction::Nothing,
        }
    }

    /// Left press on content row `row`: Click, Submenu, back a level (the header row; there is none at the root, so row 0 there is the first entry), or Nothing
    /// (separator, disabled).
    pub fn press(&mut self, row: u16, layout: &Layout) -> MenuAction {
        if row == 0 && self.header() == 1 {
            return self.back();
        }
        let entries = self.entries(layout);
        match self.entry_at(row, &entries) {
            Some(i) if selectable(entries[i]) => {
                self.select(&entries, i);
                Self::choose(entries[i])
            }
            _ => MenuAction::Nothing,
        }
    }

    /// Wheel inside the menu: moves the selection.
    pub fn scroll(&mut self, steps: i32, layout: &Layout) -> MenuAction {
        self.step(steps, layout)
    }

    /// Enters `id`'s level once its children are known (after Submenu, when the layout has them).
    /// Refused (never pushed) when `id` is not a selectable entry with a submenu of the *current*
    /// level (`level_alive`): a stale asynchronous `enter` reaching the menu after the user went
    /// back (or into a different submenu) must not push a level for an id that is no longer, or
    /// never was, an entry here.
    pub fn enter(&mut self, id: i32, layout: &Layout) -> bool {
        if self.levels.iter().any(|l| l.parent == id) {
            return false;
        }
        let host = self.entries(layout);
        if !level_alive(id, &host, layout) {
            return false;
        }
        self.levels.push(Level { parent: id, selected: None, top: 0 });
        self.relayout(layout);
        true
    }

    /// After a layout change: selection kept by entry id, levels whose parent vanished popped.
    /// A level stays alive by the same `level_alive` predicate `enter` uses to push one, checked
    /// outward from the root so a level is never kept once the one above it was already popped.
    pub fn relayout(&mut self, layout: &Layout) {
        let mut alive = 1; // the root level is never popped
        while alive < self.levels.len() {
            let host = entries_of(self.levels[alive - 1].parent, layout);
            if !level_alive(self.levels[alive].parent, &host, layout) {
                break;
            }
            alive += 1;
        }
        self.levels.truncate(alive);

        for depth in 0..self.levels.len() {
            let parent = self.levels[depth].parent;
            let entries = entries_of(parent, layout);
            let kept = self.levels[depth].selected.and_then(|id| entries.iter().position(|e| e.id == id && selectable(e)));
            let index = kept.or_else(|| list::first_selectable(entries.len(), |i| selectable(entries[i])));
            let rows = self.rows.saturating_sub(usize::from(depth > 0)).max(1);
            let level = &mut self.levels[depth];
            level.selected = index.map(|i| entries[i].id);
            level.top = list::keep_visible(index.unwrap_or(0), level.top, rows, entries.len());
        }
    }

    /// Box size in cells for the current level, within the output's cells, counting the one-cell ring
    /// (a row each side, a column each side) that `render` hands to `list::boxed` and the painter's pixel
    /// frame replaces.
    pub fn size(&self, layout: &Layout, max_columns: u16, max_rows: u16) -> (u16, u16) {
        let entries = self.entries(layout);
        let toggles = if entries.iter().any(|e| e.props.toggle.is_some()) { 2 } else { 0 };
        let entry_width = entries
            .iter()
            .filter(|e| !e.is_separator())
            .map(|e| 4 + toggles + text_width(e.label()) + if e.has_submenu() { 2 } else { 0 })
            .max()
            .unwrap_or(4 + text_width(EMPTY));
        let header_width = match self.header() {
            0 => 0,
            _ => 6 + layout.node(self.level().parent).map_or(0, |n| text_width(n.label())),
        };
        let content = (self.header() + entries.len().max(1)).min(usize::from(u16::MAX - 2)) as u16;
        let wanted = content + 2;
        // A menu that scrolls carries ▴/▾ marks after the text, with a blank cell before them.
        let mark_room = u16::from(wanted > max_rows);
        let columns = (entry_width.max(header_width) + mark_room).clamp(MENU_MIN_COLUMNS, MENU_MAX_COLUMNS).min(max_columns);
        // Never more than the output allows, even when it offers fewer than 3 rows (there is no
        // sane menu box below that, but returning more than `max_rows` would be worse: the caller
        // asked for a hard bound). Inside a submenu, aim for header + one entry + the ring's two rows (4)
        // when the output allows it, so the selection (never on the header) has somewhere to show;
        // at the root, aim for the usual 3 (one entry + the ring's two rows).
        let floor = (u16::from(self.header() == 1) + 1 + 2).min(max_rows);
        let rows = wanted.clamp(floor, max_rows);
        (columns, rows)
    }

    /// `rows`: the content rows, header included (size's height − the ring's 2).
    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
    }

    /// The first entry shown: the level's own, moved just enough to keep the selection visible
    /// with no blank row at the bottom (the rows may have shrunk since it was set).
    fn top(&self, entries: &[&MenuNode]) -> usize {
        let l = self.level();
        list::keep_visible(self.selected_index(entries).unwrap_or(l.top), l.top, self.entry_rows(), entries.len())
    }

    /// The menu as a boxed surface of `columns` × `rows` cells (`size`'s, a one-cell ring included,
    /// which `list::boxed` drops: the painter draws the frame in pixels). In it: a `◂ parent` header
    /// row below a submenu's top (MUTED), entries (toggle column ✓ / ● when any entry of the level has
    /// a toggle and its state is 1; label NORMAL or MUTED when disabled; "▸" right for submenus;
    /// separators drawn as lines; selection SELECTED, toggles ACCENT2), and at the right end of the first and last
    /// entry rows a ▴ / ▾ when more lie above / below.
    pub fn render(&self, layout: &Layout, columns: u16, rows: u16) -> Boxed {
        let entries = self.entries(layout);
        let selected = self.selected_index(&entries);
        let toggles = entries.iter().any(|e| e.props.toggle.is_some());
        let header = self.header();
        let shown = usize::from(rows.saturating_sub(2)).saturating_sub(header);
        let top = self.top(&entries);
        let mut out = Vec::with_capacity(usize::from(rows));
        let mut separators = Vec::new();
        out.push(Grid::empty(columns));
        if header == 1 {
            let mut g = Grid::empty(columns);
            let parent = layout.node(self.level().parent).map_or("", MenuNode::label);
            let row = Row { icon: Some(BACK), icon_style: Style::MUTED, text: parent, right: "", style: Style::MUTED, selected: false };
            list::row(&mut g, 1, columns.saturating_sub(1), &row);
            out.push(g);
        }
        // The rows carrying the scroll marks: the first / last entry row shown (a separator line
        // never carries one: the nearest entry row does).
        let is_entry = |r: &usize| entries.get(top + r).is_some_and(|e| !e.is_separator());
        let above = (top > 0).then(|| (0..shown).find(is_entry)).flatten();
        let below = (top + shown < entries.len()).then(|| (0..shown).rev().find(is_entry)).flatten();
        let mut marks = Vec::new();
        for r in 0..shown {
            let i = top + r;
            let Some(e) = entries.get(i) else {
                let mut g = Grid::empty(columns);
                if entries.is_empty() && r == 0 {
                    g.write(2, columns.saturating_sub(1), EMPTY, Style::MUTED);
                }
                out.push(g);
                continue;
            };
            if e.is_separator() {
                separators.push(out.len());
                out.push(Grid::empty(columns));
                continue;
            }
            let mark = match (e.props.toggle, e.props.toggle_state) {
                (Some(Toggle::Checkmark), Some(1)) => '✓',
                (Some(Toggle::Radio), Some(1)) => '●',
                _ => ' ',
            };
            let mut g = Grid::empty(columns);
            let row = Row {
                icon: toggles.then_some(mark),
                icon_style: Style::ACCENT2,
                text: e.label(),
                right: if e.has_submenu() { SUBMENU } else { "" },
                style: if e.is_enabled() { Style::NORMAL } else { Style::MUTED },
                selected: selected == Some(i),
            };
            // A marked row ends one cell earlier, so a blank cell separates its text from the mark.
            let mark = if below == Some(r) {
                Some(MORE_BELOW)
            } else if above == Some(r) {
                Some(MORE_ABOVE)
            } else {
                None
            };
            let end = columns.saturating_sub(1 + u16::from(mark.is_some()));
            list::row(&mut g, 1, end, &row);
            if let Some(m) = mark {
                marks.push((out.len(), m));
            }
            out.push(g);
        }
        // Where the frame's top and bottom strokes used to carry them: in the pad cell at the end of
        // the first and last entry rows.
        for (row, mark) in marks {
            put_mark(&mut out, row, columns, mark);
        }
        out.push(Grid::empty(columns));
        list::boxed(&out, &separators, None)
    }
}

/// Writes `mark` in the pad cell at the end of row `row` (ring coordinates, `columns` wide), in
/// the selection's SELECTED style when the row is selected and MUTED otherwise.
fn put_mark(rows: &mut [Grid], row: usize, columns: u16, mark: char) {
    let Some(g) = rows.get_mut(row) else { return };
    let column = columns.saturating_sub(2);
    // The blank cell before the mark is SELECTED on a selected row: the band is that style all along.
    let style = match g.cells().get(usize::from(column).saturating_sub(1)) {
        Some(c) if c.style == Style::SELECTED => Style::SELECTED,
        _ => Style::MUTED,
    };
    g.write(column, column + 1, &mark.to_string(), style);
}

#[cfg(test)]
mod tests;
