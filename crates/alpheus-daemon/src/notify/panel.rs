//! The notification panel: what it shows, how its selection moves, and what a key or a press on it
//! asks for.
//!
//! This is the pure half of the panel. It shows:
//! - the media card when a player is shown: the media popup's own rows, keys and press map;
//! - the history, newest first, two lines an entry;
//! - "Do not disturb ○/●" and "Clear all".
//!
//! It also holds the window over a history taller than the room, its keys and its mouse hit map.
//! The other half, `panel_app.rs`, opens the popup (`PopupBody::Panel`), reads the server and the
//! players, and carries out the action this file answers with.
//!
//! Never:
//! - talk to a bus, start a thread or run a command;
//! - panic (a guardrail, `the_notification_server_has_no_panicking_path`, covers this file).
//!
//! See: docs/specs/2026-10-09-alpheus-notifications-design.md § 4 and docs/features/notifications.md.

use crate::actions::Button;
use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::media::popup::{MediaAction, MediaPopup};
use crate::media::{self, Players};
use crate::notify::bubbles::{DEFAULT_ACTION, clock};
use crate::notify::{Center, Entry};
use std::time::Instant;

/// Content width in cells: the media card's, since `media::popup::press` hit-tests its buttons at it.
pub const WIDTH: u16 = media::popup::WIDTH;
/// Rows outside the history and the card: a separator, "Do not disturb", "Clear all".
const FOOTER: usize = 3;

/// What can be selected, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Card,
    /// The n-th of `Center::history()`.
    Entry(usize),
    Dnd,
    Clear,
}

/// What is selected, by what it is rather than by its row: arrivals and removals above it move
/// its row, never the selection off it (as `media::popup::Chosen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chosen {
    Card,
    /// A history entry, by notification id.
    Entry(u32),
    Dnd,
    Clear,
}

/// One row of the panel as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    /// The card's n-th row.
    Card(usize),
    CardSeparator,
    /// An entry's first (false) or second (true) line.
    Entry(usize, bool),
    Empty,
    Separator,
    Dnd,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    /// Index in `items()`, found again from `chosen` before each use.
    pub selected: usize,
    /// What `selected` points at, kept across changes to the history.
    pub chosen: Option<Chosen>,
    /// The first history entry shown.
    pub top: usize,
    /// The card's own selection (its buttons, its players).
    pub media: MediaPopup,
    /// Rows the popup may take under its bar: the history scrolls past it.
    pub room: u16,
    /// The history length last shown (the wiring logs a change).
    pub entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelAction {
    Nothing,
    Redraw,
    Close,
    /// The card's: the media popup's actions, run by the wiring as the media popup's are.
    Media(MediaAction),
    /// (id, revision, action key): a live entry's default action.
    Invoke(u32, u32, String),
    Remove(u32),
    ToggleDnd,
    Clear,
}

/// The card's rows: the media popup's lines, none without a shown player.
fn card_rows(players: Option<&Players>) -> usize {
    match players {
        Some(p) => media::popup::lines(p).len(),
        None => 0,
    }
}

/// What can be selected, top to bottom: the card when it shows, every entry, then the footer's two.
pub fn items(center: &Center, players: Option<&Players>) -> Vec<Item> {
    let mut v = Vec::new();
    if card_rows(players) > 0 {
        v.push(Item::Card);
    }
    v.extend((0..center.history().count()).map(Item::Entry));
    v.extend([Item::Dnd, Item::Clear]);
    v
}

/// Entries shown at once in `room` rows: what the card and the footer leave, two rows each; one at least.
fn visible(card: usize, room: u16) -> usize {
    let fixed = card + usize::from(card > 0) + FOOTER;
    (usize::from(room).saturating_sub(fixed) / 2).max(1)
}

/// The panel's rows top to bottom as `Line`s: the card, its separator, the window over the history
/// (two lines an entry, or `Empty`), then the footer.
fn layout(center: &Center, players: Option<&Players>, p: &Panel) -> Vec<Line> {
    let card = card_rows(players);
    let mut v: Vec<Line> = (0..card).map(Line::Card).collect();
    if card > 0 {
        v.push(Line::CardSeparator);
    }
    let len = center.history().count();
    if len == 0 {
        v.push(Line::Empty);
    } else {
        let shown = visible(card, p.room);
        let top = p.top.min(len.saturating_sub(shown));
        for i in top..len.min(top + shown) {
            v.push(Line::Entry(i, false));
            v.push(Line::Entry(i, true));
        }
    }
    v.extend([Line::Separator, Line::Dnd, Line::Clear]);
    v
}

impl Panel {
    /// At open: the newest entry selected, else the card, else "Do not disturb".
    pub fn new(center: &Center, players: Option<&Players>, room: u16) -> Panel {
        let items = items(center, players);
        let first_entry = items.iter().position(|i| matches!(i, Item::Entry(_)));
        let card = items.iter().position(|i| *i == Item::Card);
        let selected = match (first_entry, card) {
            (Some(i), _) => i,
            (None, Some(i)) => i,
            (None, None) => 0,
        };
        // Without a player the card's own selection points at no row (`usize::MAX`).
        let media = match players {
            Some(pl) => MediaPopup::new(pl),
            None => MediaPopup { selected: usize::MAX, button: 1, chosen: None },
        };
        let mut p = Panel { selected, chosen: None, top: 0, media, room, entries: center.history().count() };
        p.remember(center, &items);
        p
    }

    /// `chosen` from `selected` (after a fit, a key, a press).
    fn remember(&mut self, center: &Center, items: &[Item]) {
        self.chosen = match items.get(self.selected) {
            Some(Item::Card) => Some(Chosen::Card),
            Some(Item::Entry(i)) => center.history().nth(*i).map(|e| Chosen::Entry(e.id)),
            Some(Item::Dnd) => Some(Chosen::Dnd),
            Some(Item::Clear) => Some(Chosen::Clear),
            None => None,
        };
    }

    /// The selection on `chosen`'s row where it now is; gone, on the row it had (clamped), and
    /// that row remembered anew.
    fn fit(&mut self, center: &Center, items: &[Item]) {
        let found = match self.chosen {
            Some(Chosen::Card) => items.iter().position(|i| *i == Item::Card),
            Some(Chosen::Entry(id)) => match center.history().position(|e| e.id == id) {
                Some(n) => items.iter().position(|i| *i == Item::Entry(n)),
                None => None,
            },
            Some(Chosen::Dnd) => items.iter().position(|i| *i == Item::Dnd),
            Some(Chosen::Clear) => items.iter().position(|i| *i == Item::Clear),
            None => None,
        };
        match found {
            Some(i) => self.selected = i,
            None => {
                self.selected = match self.chosen {
                    Some(Chosen::Entry(_)) => neighbour(items, self.selected),
                    _ => self.selected.min(items.len().saturating_sub(1)),
                };
                self.remember(center, items);
            }
        }
    }
}

/// Where the selection goes when its entry, at `row` of `items` before it went, is gone: the entry
/// above it, else the one that took its row; with no entry left, the first row after them
/// ("Do not disturb").
fn neighbour(items: &[Item], row: usize) -> usize {
    let entries: Vec<usize> = items.iter().enumerate().filter(|(_, i)| matches!(i, Item::Entry(_))).map(|(r, _)| r).collect();
    match (entries.first(), entries.last()) {
        (Some(&first), Some(&last)) => row.saturating_sub(1).clamp(first, last),
        _ => items.iter().position(|i| *i == Item::Dnd).unwrap_or(0),
    }
}

/// Before drawing: the selection found again on what it chose (an arrival above an entry moves
/// its row, not the selection), the card's own selection re-fitted (a player gone, a capability
/// lost), the window over the history following the selected entry.
pub fn scroll_into_view(p: &mut Panel, center: &Center, players: Option<&Players>) {
    let items = items(center, players);
    p.fit(center, &items);
    if let Some(pl) = players {
        p.media.fit(pl);
    }
    let len = center.history().count();
    let shown = visible(card_rows(players), p.room);
    let at = match items.get(p.selected) {
        Some(Item::Entry(i)) => *i,
        _ => p.top,
    };
    p.top = list::keep_visible(at, p.top, shown, len);
}

/// The panel's cells: `WIDTH` wide plus the pad cells, the separator rows recorded for the painter.
/// `now` times the media card; `today` (local minutes since 1970) the entries: "14:02" or "Oct 8".
pub fn rows(center: &Center, players: Option<&Players>, p: &Panel, now: Instant, today: u32) -> Boxed {
    let items = items(center, players);
    let selected = items.get(p.selected).copied();
    let history: Vec<&Entry> = center.history().collect();
    let card = match players {
        Some(pl) if card_rows(players) > 0 => {
            // The card's band only while the card holds the selection: otherwise its own selection
            // is moved off every row (`usize::MAX`), and the rest of `p.media` is kept.
            let focus = if selected == Some(Item::Card) { p.media.clone() } else { MediaPopup { selected: usize::MAX, ..p.media.clone() } };
            media::popup::rows(pl, &focus, now, WIDTH)
        }
        _ => Boxed::default(),
    };
    let columns = WIDTH + 2;
    // The card comes first: its own separators keep their row numbers.
    let mut out = Boxed { rows: Vec::new(), separators: card.separators.clone(), divider: None, graphs: Vec::new() };
    for (r, line) in layout(center, players, p).into_iter().enumerate() {
        let mut g = Grid::empty(columns);
        match line {
            Line::Card(i) => {
                if let Some(c) = card.rows.get(i) {
                    g = c.clone();
                }
            }
            Line::CardSeparator | Line::Separator => out.separators.push(r),
            Line::Entry(i, second) => {
                if let Some(e) = history.get(i) {
                    entry_row(&mut g, e, second, selected == Some(Item::Entry(i)), today);
                }
            }
            Line::Empty => plain(&mut g, "No notifications", "", Style::MUTED, false),
            Line::Dnd => plain(&mut g, "Do not disturb", if center.dnd() { "●" } else { "○" }, Style::NORMAL, selected == Some(Item::Dnd)),
            Line::Clear => plain(&mut g, "Clear all", "", Style::NORMAL, selected == Some(Item::Clear)),
        }
        out.rows.push(g);
    }
    out
}

/// A row of text from the first content cell, `right` right-aligned, as a menu row draws it.
fn plain(g: &mut Grid, text: &str, right: &str, style: Style, selected: bool) {
    let end = g.width();
    list::row(g, 0, end, &Row { icon: None, icon_style: style, text, right, style, selected });
}

/// An entry: the glyph, the app and the time, then the summary under the app.
fn entry_row(g: &mut Grid, e: &Entry, second: bool, selected: bool, today: u32) {
    let end = g.width();
    if second {
        // No summary: the body's first line stands in for it.
        let line =
            if e.summary.is_empty() { e.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default() } else { &e.summary };
        let text = format!("  {line}");
        list::row(g, 0, end, &Row { icon: None, icon_style: Style::NORMAL, text: &text, right: "", style: Style::NORMAL, selected });
    } else {
        let time = clock(e.at, today);
        list::row(
            g,
            0,
            end,
            &Row { icon: Some(e.glyph), icon_style: Style::ACCENT, text: &e.app, right: &time, style: Style::NORMAL, selected },
        );
    }
}

/// What Enter (or a left click) does on `item`.
fn activate(item: Item, center: &Center) -> PanelAction {
    match item {
        Item::Entry(i) => match center.history().nth(i) {
            Some(e) if e.live && e.actions.iter().any(|a| a.key == DEFAULT_ACTION) => {
                PanelAction::Invoke(e.id, e.revision, DEFAULT_ACTION.to_string())
            }
            _ => PanelAction::Nothing,
        },
        Item::Dnd => PanelAction::ToggleDnd,
        Item::Clear => PanelAction::Clear,
        Item::Card => PanelAction::Nothing,
    }
}

/// ↑ ↓ (Ctrl+P, Ctrl+N), Home and End move, clamped; on the card, ↑ ↓ ← → and Enter are the media
/// popup's (↓ past its last row goes on to the history). Enter activates, Delete removes an entry,
/// Escape closes.
pub fn key(p: &mut Panel, center: &Center, players: Option<&Players>, k: Key) -> PanelAction {
    if k == Key::Escape {
        return PanelAction::Close;
    }
    let items = items(center, players);
    p.fit(center, &items);
    let action = key_on(p, center, players, &items, k);
    p.remember(center, &items);
    action
}

/// The wheel: `steps` notches (negative up), at most 10, each a row as ↑ ↓ moves; one action for
/// them all, so the wiring repaints once. Redraw when any notch moved.
pub fn scroll(p: &mut Panel, center: &Center, players: Option<&Players>, steps: i32) -> PanelAction {
    let k = if steps < 0 { Key::Up } else { Key::Down };
    let mut moved = false;
    for _ in 0..steps.unsigned_abs().min(10) {
        // `|=` sets `moved` once any notch moved the selection, and keeps it set.
        moved |= key(p, center, players, k) != PanelAction::Nothing;
    }
    if moved { PanelAction::Redraw } else { PanelAction::Nothing }
}

/// `key` on the selection fitted, before it is remembered.
fn key_on(p: &mut Panel, center: &Center, players: Option<&Players>, items: &[Item], k: Key) -> PanelAction {
    let last = items.len().saturating_sub(1);
    if let (Some(Item::Card), Some(pl)) = (items.get(p.selected), players)
        && matches!(k, Key::Up | Key::Down | Key::CtrlP | Key::CtrlN | Key::Left | Key::Right | Key::Enter)
    {
        let a = media::popup::key(&mut p.media, pl, k);
        if a != MediaAction::Nothing {
            return PanelAction::Media(a);
        }
        if matches!(k, Key::Down | Key::CtrlN) && p.selected < last {
            p.selected += 1;
            return PanelAction::Redraw;
        }
        return PanelAction::Nothing;
    }
    let before = p.selected;
    match k {
        Key::Up | Key::CtrlP => p.selected = p.selected.saturating_sub(1),
        Key::Down | Key::CtrlN => p.selected = (p.selected + 1).min(last),
        Key::Home => p.selected = 0,
        Key::End => p.selected = last,
        Key::Enter => return items.get(p.selected).map_or(PanelAction::Nothing, |i| activate(*i, center)),
        Key::Delete => {
            return match items.get(p.selected) {
                Some(Item::Entry(i)) => center.history().nth(*i).map_or(PanelAction::Nothing, |e| PanelAction::Remove(e.id)),
                _ => PanelAction::Nothing,
            };
        }
        _ => {}
    }
    if p.selected == before { PanelAction::Nothing } else { PanelAction::Redraw }
}

/// Puts the selection on `item` and remembers it; false when `item` is not among `items`, or when
/// the selection was already there.
fn select(p: &mut Panel, center: &Center, items: &[Item], item: Item) -> bool {
    let Some(i) = items.iter().position(|x| *x == item) else { return false };
    let moved = p.selected != i;
    p.selected = i;
    p.remember(center, items);
    moved
}

/// A press at `cell` (Boxed coordinates: column 0 is the pad). On the card a left press is the media
/// popup's; on an entry a left press selects it and acts like Enter, a right press removes it; on the
/// footer a left press acts like Enter. The press selects what it ran.
pub fn press(p: &mut Panel, center: &Center, players: Option<&Players>, cell: (u16, u16), button: Button) -> PanelAction {
    let items = items(center, players);
    p.fit(center, &items);
    let line = layout(center, players, p).get(usize::from(cell.1)).copied();
    match (line, button) {
        (Some(Line::Card(_)), Button::Left) => {
            let Some(pl) = players else { return PanelAction::Nothing };
            match media::popup::press(&mut p.media, pl, cell, WIDTH) {
                MediaAction::Nothing => PanelAction::Nothing,
                a => {
                    select(p, center, &items, Item::Card);
                    PanelAction::Media(a)
                }
            }
        }
        (Some(Line::Entry(i, _)), Button::Right) => center.history().nth(i).map_or(PanelAction::Nothing, |e| PanelAction::Remove(e.id)),
        (Some(Line::Entry(i, _)), Button::Left) => {
            let moved = select(p, center, &items, Item::Entry(i));
            match activate(Item::Entry(i), center) {
                PanelAction::Nothing if moved => PanelAction::Redraw,
                a => a,
            }
        }
        (Some(Line::Dnd), Button::Left) => {
            select(p, center, &items, Item::Dnd);
            PanelAction::ToggleDnd
        }
        (Some(Line::Clear), Button::Left) => {
            select(p, center, &items, Item::Clear);
            PanelAction::Clear
        }
        _ => PanelAction::Nothing,
    }
}

#[cfg(test)]
mod tests;
