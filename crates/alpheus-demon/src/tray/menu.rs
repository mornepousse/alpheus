//! A tray item's menu as the bar draws it (spec § 1 Menus, § 2): rows of
//! cells inside a thin frame, the launcher's list rendering and keys
//! (`crate::list`, `crate::keys`), one level at a time — a submenu replaces
//! the list in the same popup, its first row `◂ <parent>` (drill-down, Mae
//! 2026-10-05). Pure: it reads the session's `Layout` and answers with a
//! `MenuAction`; the D-Bus calls and the popup are the wiring's (task 17).
//! Never selects a separator, a disabled or a hidden entry.

use crate::grid::{Grid, Style, text_width};
use crate::keys::Key;
use crate::list::{self, Row};
use crate::tray::codec::{Layout, MenuNode, Toggle};

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

/// The open menu's view state: a stack of levels (drill-down, question 2), each with its selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    levels: Vec<Level>,
    /// Content rows between the frames (the header row included), from `set_rows`.
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

impl Menu {
    pub fn new() -> Menu {
        Menu { levels: vec![Level { parent: ROOT, selected: None, top: 0 }], rows: usize::MAX }
    }

    fn level(&self) -> &Level {
        self.levels.last().expect("the root level is never popped")
    }

    fn level_mut(&mut self) -> &mut Level {
        self.levels.last_mut().expect("the root level is never popped")
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
        layout.node(self.level().parent).map(|n| n.children.iter().filter(|c| c.is_visible()).collect()).unwrap_or_default()
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
            Key::Left => {
                if self.levels.len() > 1 {
                    self.levels.pop();
                    MenuAction::Redraw
                } else {
                    MenuAction::Nothing
                }
            }
            Key::Escape => MenuAction::Close,
            _ => MenuAction::Nothing,
        }
    }

    /// The entry under content row `row` (frame excluded; the header is no entry).
    fn entry_at(&self, row: u16, layout: &Layout) -> Option<usize> {
        let row = usize::from(row).checked_sub(self.header())?;
        if row >= self.entry_rows() {
            return None;
        }
        let entries = self.entries(layout);
        let i = self.top(&entries) + row;
        (i < entries.len()).then_some(i)
    }

    /// Pointer over content row `row` (frame excluded): selects it if selectable.
    pub fn hover(&mut self, row: u16, layout: &Layout) -> MenuAction {
        let entries = self.entries(layout);
        match self.entry_at(row, layout) {
            Some(i) if selectable(entries[i]) && self.selected_index(&entries) != Some(i) => {
                self.select(&entries, i);
                MenuAction::Redraw
            }
            _ => MenuAction::Nothing,
        }
    }

    /// Left press on content row `row`: Click, or Submenu, or Nothing (separator, disabled, header).
    pub fn press(&mut self, row: u16, layout: &Layout) -> MenuAction {
        let entries = self.entries(layout);
        match self.entry_at(row, layout) {
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
    pub fn enter(&mut self, id: i32, layout: &Layout) -> bool {
        let known = layout.node(id).is_some_and(|n| n.children.iter().any(MenuNode::is_visible));
        if !known || self.levels.iter().any(|l| l.parent == id) {
            return false;
        }
        self.levels.push(Level { parent: id, selected: None, top: 0 });
        self.relayout(layout);
        true
    }

    /// After a layout change: selection kept by entry id, levels whose parent vanished popped.
    pub fn relayout(&mut self, layout: &Layout) {
        // A level stays while its parent is still in the tree with something to list.
        let alive = self.levels.iter().skip(1).position(|l| layout.node(l.parent).is_none_or(|n| n.children.is_empty()));
        if let Some(gone) = alive {
            self.levels.truncate(gone + 1);
        }
        for depth in 0..self.levels.len() {
            let parent = self.levels[depth].parent;
            let entries: Vec<&MenuNode> =
                layout.node(parent).map(|n| n.children.iter().filter(|c| c.is_visible()).collect()).unwrap_or_default();
            let kept = self.levels[depth].selected.and_then(|id| entries.iter().position(|e| e.id == id && selectable(e)));
            let index = kept.or_else(|| list::first_selectable(entries.len(), |i| selectable(entries[i])));
            let rows = self.rows.saturating_sub(usize::from(depth > 0)).max(1);
            let level = &mut self.levels[depth];
            level.selected = index.map(|i| entries[i].id);
            level.top = list::keep_visible(index.unwrap_or(0), level.top, rows, entries.len());
        }
    }

    /// Box size in cells for the current level, frame included, within the output's cells.
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
        let columns = entry_width.max(header_width).clamp(MENU_MIN_COLUMNS, MENU_MAX_COLUMNS).min(max_columns);
        let content = (self.header() + entries.len().max(1)).min(usize::from(u16::MAX - 2)) as u16;
        let rows = (content + 2).min(max_rows.max(3));
        (columns, rows)
    }

    /// `rows`: the content rows between the frames, header included (size's height − 2).
    pub fn set_rows(&mut self, rows: usize) {
        self.rows = rows.max(1);
    }

    /// The first entry shown: the level's own, moved just enough to keep the selection visible
    /// with no blank row at the bottom (the rows may have shrunk since it was set).
    fn top(&self, entries: &[&MenuNode]) -> usize {
        let l = self.level();
        list::keep_visible(self.selected_index(entries).unwrap_or(l.top), l.top, self.entry_rows(), entries.len())
    }

    /// One Grid per row: frame, `◂ parent` header row below a submenu's top frame (MUTED), entries
    /// (toggle column ✓ / ● when any entry of the level has a toggle and its state is 1; label NORMAL or
    /// MUTED when disabled; "▸" right for submenus; separators as list::separator; selection INVERTED), frame.
    pub fn render(&self, layout: &Layout, columns: u16, rows: u16) -> Vec<Grid> {
        let entries = self.entries(layout);
        let selected = self.selected_index(&entries);
        let toggles = entries.iter().any(|e| e.props.toggle.is_some());
        let header = self.header();
        let shown = usize::from(rows.saturating_sub(2)).saturating_sub(header);
        let top = self.top(&entries);
        let mut out = Vec::with_capacity(usize::from(rows));
        let mut first = list::frame(columns, '╭', '╮', None);
        if top > 0 {
            first.write(columns.saturating_sub(3), columns.saturating_sub(1), &MORE_ABOVE.to_string(), Style::MUTED);
        }
        out.push(first);
        if header == 1 {
            let mut g = list::interior(columns, None);
            let parent = layout.node(self.level().parent).map_or("", MenuNode::label);
            let row = Row { icon: Some(BACK), text: parent, right: "", style: Style::MUTED, selected: false };
            list::row(&mut g, 1, columns.saturating_sub(1), &row);
            // The arrow is part of the muted header, not an accent icon.
            g.write(2, columns.saturating_sub(1), &BACK.to_string(), Style::MUTED);
            out.push(g);
        }
        for r in 0..shown {
            let i = top + r;
            let Some(e) = entries.get(i) else {
                let mut g = list::interior(columns, None);
                if entries.is_empty() && r == 0 {
                    g.write(2, columns.saturating_sub(1), EMPTY, Style::MUTED);
                }
                out.push(g);
                continue;
            };
            if e.is_separator() {
                out.push(list::separator(columns));
                continue;
            }
            let mark = match (e.props.toggle, e.props.toggle_state) {
                (Some(Toggle::Checkmark), Some(1)) => '✓',
                (Some(Toggle::Radio), Some(1)) => '●',
                _ => ' ',
            };
            let mut g = list::interior(columns, None);
            let row = Row {
                icon: toggles.then_some(mark),
                text: e.label(),
                right: if e.has_submenu() { SUBMENU } else { "" },
                style: if e.is_enabled() { Style::NORMAL } else { Style::MUTED },
                selected: selected == Some(i),
            };
            list::row(&mut g, 1, columns.saturating_sub(1), &row);
            out.push(g);
        }
        let mut last = list::frame(columns, '╰', '╯', None);
        if top + shown < entries.len() {
            last.write(columns.saturating_sub(3), columns.saturating_sub(1), &MORE_BELOW.to_string(), Style::MUTED);
        }
        out.push(last);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::codec::MenuProps;

    fn node(id: i32, label: &str) -> MenuNode {
        MenuNode { id, props: MenuProps { label: Some(label.into()), ..MenuProps::default() }, children: Vec::new() }
    }

    fn with(mut n: MenuNode, f: impl FnOnce(&mut MenuProps)) -> MenuNode {
        f(&mut n.props);
        n
    }

    fn root(children: Vec<MenuNode>) -> Layout {
        Layout { revision: 1, root: MenuNode { id: 0, props: MenuProps::default(), children } }
    }

    /// FakeMenu::sample's tree (task 3), built in memory: "Open", "Mute" (checkmark on), "Low"/"High"
    /// (radios, High on), a separator, "Disabled", a hidden entry, "More" (a submenu not fetched yet).
    fn sample() -> Layout {
        root(vec![
            node(1, "Open"),
            with(node(2, "Mute"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Checkmark), Some(1))),
            with(node(3, "Low"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Radio), Some(0))),
            with(node(4, "High"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Radio), Some(1))),
            with(node(5, ""), |p| p.separator = Some(true)),
            with(node(6, "Disabled"), |p| p.enabled = Some(false)),
            with(node(7, "Hidden"), |p| p.visible = Some(false)),
            with(node(8, "More"), |p| p.submenu = Some(true)),
        ])
    }

    /// "More" fetched: its two children grafted (MenuSession, task 6).
    fn fetched() -> Layout {
        let mut l = sample();
        let mut more = l.node(8).unwrap().clone();
        more.children = vec![node(9, "Settings"), node(10, "About")];
        assert!(l.graft(more));
        l
    }

    /// As the wiring opens it (task 17): relayout selects the first selectable entry.
    fn open(l: &Layout) -> Menu {
        let mut m = Menu::new();
        m.relayout(l);
        m
    }

    /// Sized and rendered like the wiring does, on an output of `columns` × `rows` cells.
    fn draw(m: &mut Menu, l: &Layout, columns: u16, rows: u16) -> Vec<Grid> {
        let (c, r) = m.size(l, columns, rows);
        m.set_rows(usize::from(r - 2));
        let out = m.render(l, c, r);
        assert_eq!(out.len(), usize::from(r));
        for g in &out {
            assert_eq!(g.width(), c, "{}", snapshot(&out));
        }
        out
    }

    fn snapshot(rows: &[Grid]) -> String {
        let texts: Vec<String> = rows.iter().map(Grid::text).collect();
        let styles: Vec<String> = rows.iter().map(Grid::styles).collect();
        format!("{}\n\n{}", texts.join("\n"), styles.join("\n"))
    }

    fn selected_id(m: &Menu, l: &Layout) -> Option<i32> {
        let e = m.entries(l);
        m.selected_index(&e).map(|i| e[i].id)
    }

    #[test]
    fn menu_checks_radios_separators_disabled_hidden() {
        let l = sample();
        let mut m = open(&l);
        let rows = draw(&mut m, &l, 200, 40);
        insta::assert_snapshot!("menu_sample", snapshot(&rows));
        assert_eq!(m.entries(&l).len(), 7, "the hidden entry is not listed");
    }

    #[test]
    fn submenu_level_with_its_header() {
        let l = fetched();
        let mut m = open(&l);
        m.key(Key::End, &l);
        assert_eq!(m.key(Key::Enter, &l), MenuAction::Submenu(8));
        assert!(m.enter(8, &l));
        let rows = draw(&mut m, &l, 200, 40);
        insta::assert_snapshot!("menu_submenu", snapshot(&rows));
    }

    #[test]
    fn long_labels_truncated_and_width_bounded() {
        let l = root(vec![node(1, &"A very long label ".repeat(10)), with(node(2, "Short"), |p| p.submenu = Some(true))]);
        let mut m = open(&l);
        let rows = draw(&mut m, &l, 200, 40);
        assert_eq!(rows[0].width(), MENU_MAX_COLUMNS);
        insta::assert_snapshot!("menu_long_labels", snapshot(&rows));
        let rows = draw(&mut m, &l, 30, 40);
        assert_eq!(rows[0].width(), 30, "never wider than the output");
    }

    #[test]
    fn a_tall_menu_scrolls() {
        let l = root((1..=30).map(|i| node(i, &format!("Entry {i}"))).collect());
        let mut m = open(&l);
        draw(&mut m, &l, 200, 8);
        for _ in 0..9 {
            m.key(Key::Down, &l);
        }
        assert_eq!(selected_id(&m, &l), Some(10));
        let rows = draw(&mut m, &l, 200, 8);
        insta::assert_snapshot!("menu_scrolled", snapshot(&rows));
        assert_eq!(m.hover(5, &l), MenuAction::Nothing, "the last visible row is the selection");
        assert_eq!(m.press(0, &l), MenuAction::Click(5), "row 0 is entry 5 once scrolled");
    }

    #[test]
    fn hostile_labels_never_misalign() {
        let hostile = [
            "x".repeat(300),
            "日本語のメニュー項目がとても長い場合".to_string(),
            "\u{202e}reversed\u{200e} bidi \u{2066}isolate\u{2069}".to_string(),
            "🦀🦀🦀 crab 🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀".to_string(),
            "e\u{301}cole\u{0300}".to_string(),
        ];
        let l = root(
            hostile
                .iter()
                .enumerate()
                .map(|(i, h)| with(node(i as i32 + 1, h), |p| p.submenu = Some(i % 2 == 0)))
                .chain(std::iter::once(with(node(99, "t"), |p| (p.toggle, p.toggle_state) = (Some(Toggle::Radio), Some(1)))))
                .collect(),
        );
        for columns in [3u16, 12, 20, 33, 48, 200] {
            let mut m = open(&l);
            for _ in 0..l.root.children.len() {
                let rows = draw(&mut m, &l, columns, 40);
                for r in &rows {
                    let t = r.text();
                    assert!(t.starts_with(['│', '╭', '╰', '├']) && t.ends_with(['│', '╮', '╯', '┤']), "frame broken at {columns}: {t:?}");
                    assert!(!t.contains(['\u{202e}', '\u{200e}', '\u{2066}']), "{t:?}");
                }
                m.key(Key::Down, &l);
            }
        }
    }

    #[test]
    fn arrows_skip_separators_and_disabled() {
        let l = sample();
        let mut m = open(&l);
        assert_eq!(selected_id(&m, &l), Some(1), "the first selectable entry at open");
        let mut seen = Vec::new();
        for _ in 0..6 {
            m.key(Key::Down, &l);
            seen.push(selected_id(&m, &l).unwrap());
        }
        assert_eq!(seen, [2, 3, 4, 8, 8, 8], "over the separator and Disabled, no wrap");
        assert_eq!(m.key(Key::Down, &l), MenuAction::Nothing, "at the end: nothing to redraw");
        assert_eq!(m.key(Key::Up, &l), MenuAction::Redraw);
        assert_eq!(selected_id(&m, &l), Some(4), "back over them");
        m.key(Key::Home, &l);
        assert_eq!(selected_id(&m, &l), Some(1));
        m.key(Key::CtrlN, &l);
        assert_eq!(selected_id(&m, &l), Some(2), "Ctrl+N like the launcher");
        m.key(Key::PageDown, &l);
        assert_eq!(selected_id(&m, &l), Some(8));
    }

    #[test]
    fn enter_on_an_item_clicks() {
        let l = sample();
        let mut m = open(&l);
        m.key(Key::Down, &l);
        assert_eq!(m.key(Key::Enter, &l), MenuAction::Click(2));
        assert_eq!(m.key(Key::Right, &l), MenuAction::Nothing, "→ on an item that is no submenu");
    }

    #[test]
    fn enter_or_right_on_a_submenu_asks_for_it() {
        let l = sample();
        let mut m = open(&l);
        m.key(Key::End, &l);
        assert_eq!(m.key(Key::Enter, &l), MenuAction::Submenu(8));
        assert_eq!(m.key(Key::Right, &l), MenuAction::Submenu(8));
        assert!(!m.enter(8, &l), "its children are not known yet: stays at the root");
        assert_eq!(m.entries(&l).len(), 7);
        let l = fetched();
        assert!(m.enter(8, &l), "grafted: entered");
        assert_eq!(m.entries(&l).iter().map(|e| e.id).collect::<Vec<_>>(), [9, 10]);
        assert_eq!(selected_id(&m, &l), Some(9));
    }

    #[test]
    fn left_goes_back_one_level_escape_closes() {
        let l = fetched();
        let mut m = open(&l);
        m.key(Key::End, &l);
        m.enter(8, &l);
        assert_eq!(m.key(Key::Left, &l), MenuAction::Redraw);
        assert_eq!(m.entries(&l).len(), 7, "back at the root");
        assert_eq!(selected_id(&m, &l), Some(8), "on the submenu it came from");
        assert_eq!(m.key(Key::Left, &l), MenuAction::Nothing, "at the root, ← does nothing");
        assert_eq!(m.key(Key::Escape, &l), MenuAction::Close);
        assert_eq!(m.key(Key::Char('x'), &l), MenuAction::Nothing);
    }

    #[test]
    fn relayout_keeps_selection_by_id() {
        let l = sample();
        let mut m = open(&l);
        m.key(Key::Down, &l);
        m.key(Key::Down, &l);
        assert_eq!(selected_id(&m, &l), Some(3));
        let mut changed = sample();
        changed.root.children.insert(0, node(20, "New first"));
        m.relayout(&changed);
        assert_eq!(selected_id(&m, &changed), Some(3), "still Low, one row lower");
        let mut gone = sample();
        gone.root.children.retain(|n| n.id != 3);
        m.relayout(&gone);
        assert_eq!(selected_id(&m, &gone), Some(1), "Low gone: the first selectable");
    }

    #[test]
    fn relayout_pops_a_vanished_submenu() {
        let l = fetched();
        let mut m = open(&l);
        m.key(Key::End, &l);
        m.enter(8, &l);
        let mut gone = sample();
        gone.root.children.retain(|n| n.id != 8);
        m.relayout(&gone);
        assert_eq!(m.entries(&gone).len(), 6, "back at the root, without More");
        assert_eq!(m.key(Key::Left, &gone), MenuAction::Nothing, "no level left to pop");
    }

    /// LayoutUpdated(parent 0): the wiring re-reads the root at depth 1, which announces More
    /// again as a submenu but without its children; task 2's `graft` carries the fetched ones
    /// over, so the open submenu is not popped and keeps its selection.
    #[test]
    fn relayout_keeps_a_submenu_across_a_root_reread() {
        let mut l = fetched();
        let mut m = open(&l);
        m.key(Key::End, &l);
        assert!(m.enter(8, &l));
        m.key(Key::Down, &l);
        assert_eq!(selected_id(&m, &l), Some(10));
        let reread = sample().root;
        assert!(reread.children.iter().any(|n| n.id == 8 && n.children.is_empty()), "depth 1: no grandchildren");
        assert!(l.graft(reread));
        m.relayout(&l);
        assert_eq!(m.entries(&l).iter().map(|e| e.id).collect::<Vec<_>>(), [9, 10], "still inside More");
        assert_eq!(selected_id(&m, &l), Some(10), "the selection kept by id");
    }

    #[test]
    fn hover_selects_press_runs() {
        let l = fetched();
        let mut m = open(&l);
        draw(&mut m, &l, 200, 40);
        assert_eq!(m.hover(2, &l), MenuAction::Redraw, "row 2: Low");
        assert_eq!(selected_id(&m, &l), Some(3));
        assert_eq!(m.hover(2, &l), MenuAction::Nothing, "the same row: nothing to redraw");
        assert_eq!(m.hover(4, &l), MenuAction::Nothing, "the separator is never selected");
        assert_eq!(m.hover(5, &l), MenuAction::Nothing, "nor the disabled entry");
        assert_eq!(selected_id(&m, &l), Some(3));
        assert_eq!(m.press(4, &l), MenuAction::Nothing);
        assert_eq!(m.press(1, &l), MenuAction::Click(2));
        assert_eq!(m.press(6, &l), MenuAction::Submenu(8));
        assert_eq!(m.press(30, &l), MenuAction::Nothing, "below the entries");
        m.enter(8, &l);
        assert_eq!(m.press(0, &l), MenuAction::Nothing, "the header is no entry");
        assert_eq!(m.press(2, &l), MenuAction::Click(10));
        assert_eq!(m.scroll(-1, &l), MenuAction::Redraw);
        assert_eq!(selected_id(&m, &l), Some(9), "the wheel moves the selection");
    }

    /// Controller note (carried over from the task 13 review): `list::step` leaves a raw
    /// `selected >= len` index unchanged when nothing is selectable that way, which would be a
    /// stale, out-of-range index if `Menu` ever stored one. It never does — the selection is an
    /// id, re-looked-up in the *current* entries on every call (`selected_index`) — but `relayout`
    /// must still clamp both the selection and the scroll window when a level shrinks under a
    /// selection far down the list, never leaving `top` past the new, shorter list either.
    #[test]
    fn relayout_clamps_selection_and_scroll_window_when_entries_shrink() {
        let l = root((1..=10).map(|i| node(i, &format!("Entry {i}"))).collect());
        let mut m = open(&l);
        for _ in 0..7 {
            m.key(Key::Down, &l);
        }
        assert_eq!(selected_id(&m, &l), Some(8));
        let shrunk = root(vec![node(1, "Entry 1")]);
        m.relayout(&shrunk);
        assert_eq!(selected_id(&m, &shrunk), Some(1), "clamped to the only entry left, never out of range");
        assert_eq!(m.key(Key::Down, &shrunk), MenuAction::Nothing, "nowhere further to go");
        assert_eq!(selected_id(&m, &shrunk), Some(1));
        // Rendering must not panic either: a stale `top` would index past the shrunk entries.
        let rows = draw(&mut m, &shrunk, 200, 40);
        assert_eq!(rows.len(), 3, "frame, one entry, frame");
    }

    #[test]
    fn a_menu_with_nothing_selectable_selects_nothing() {
        let l = root(vec![with(node(1, ""), |p| p.separator = Some(true)), with(node(2, "Off"), |p| p.enabled = Some(false))]);
        let mut m = open(&l);
        assert_eq!(selected_id(&m, &l), None);
        for k in [Key::Down, Key::Up, Key::End, Key::Enter, Key::Right] {
            assert_eq!(m.key(k, &l), MenuAction::Nothing, "{k:?}");
        }
        let empty = root(Vec::new());
        let mut m = open(&empty);
        let rows = draw(&mut m, &empty, 200, 40);
        assert!(rows[1].text().contains("empty"), "{}", snapshot(&rows));
        assert_eq!(m.key(Key::Enter, &empty), MenuAction::Nothing);
    }
}
