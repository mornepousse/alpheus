//! The notifications panel (spec § 4), pure. It shows:
//! - the media card when a player is shown: the media popup's own rows, keys and press map;
//! - the history, newest first, two lines an entry;
//! - "Do not disturb ○/●" and "Clear all".
//!
//! It also holds the window over a history taller than the room, its keys and its mouse hit map.
//! The popup (`PopupBody::Panel`), the D-Bus side and the bubbles are the wiring's: nothing here
//! talks to a bus, starts a thread or runs a command.

use crate::actions::Button;
use crate::bubbles::{DEFAULT_ACTION, clock};
use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::media::Players;
use crate::media_popup::{self, MediaAction, MediaPopup};
use crate::notify::{Center, Entry};
use std::time::Instant;

/// Content width in cells: the media card's, since `media_popup::press` hit-tests its buttons at it.
pub const WIDTH: u16 = media_popup::WIDTH;
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
    /// Index in `items()`.
    pub selected: usize,
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
    players.map_or(0, |p| media_popup::lines(p).len())
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
        let selected =
            items.iter().position(|i| matches!(i, Item::Entry(_))).or_else(|| items.iter().position(|i| *i == Item::Card)).unwrap_or(0);
        let media = players.map_or(MediaPopup { selected: usize::MAX, button: 1, chosen: None }, MediaPopup::new);
        Panel { selected, top: 0, media, room, entries: center.history().count() }
    }
}

/// Before drawing: the selection on an item that exists, the card's own selection re-fitted (a
/// player gone, a capability lost), the window over the history following the selected entry.
pub fn scroll_into_view(p: &mut Panel, center: &Center, players: Option<&Players>) {
    let items = items(center, players);
    p.selected = p.selected.min(items.len().saturating_sub(1));
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
pub fn rows(center: &Center, players: Option<&Players>, p: &Panel, now: Instant) -> Boxed {
    let items = items(center, players);
    let selected = items.get(p.selected).copied();
    let history: Vec<&Entry> = center.history().collect();
    let card = match players {
        Some(pl) if card_rows(players) > 0 => {
            // The card's band only while the card holds the selection.
            let focus = if selected == Some(Item::Card) { p.media.clone() } else { MediaPopup { selected: usize::MAX, ..p.media.clone() } };
            media_popup::rows(pl, &focus, now, WIDTH)
        }
        _ => Boxed::default(),
    };
    let columns = WIDTH + 2;
    // The card comes first: its own separators keep their row numbers.
    let mut out = Boxed { rows: Vec::new(), separators: card.separators.clone(), divider: None };
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
                    entry_row(&mut g, e, second, selected == Some(Item::Entry(i)));
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
fn entry_row(g: &mut Grid, e: &Entry, second: bool, selected: bool) {
    let end = g.width();
    if second {
        let text = format!("  {}", e.summary);
        list::row(g, 0, end, &Row { icon: None, icon_style: Style::NORMAL, text: &text, right: "", style: Style::NORMAL, selected });
    } else {
        let time = clock(e.at_minute);
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
    let last = items.len().saturating_sub(1);
    p.selected = p.selected.min(last);
    if let (Some(Item::Card), Some(pl)) = (items.get(p.selected), players)
        && matches!(k, Key::Up | Key::Down | Key::CtrlP | Key::CtrlN | Key::Left | Key::Right | Key::Enter)
    {
        let a = media_popup::key(&mut p.media, pl, k);
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

/// A press at `cell` (Boxed coordinates: column 0 is the pad). On the card a left press is the media
/// popup's; on an entry a left press selects it and acts like Enter, a right press removes it; on the
/// footer a left press acts like Enter. The press selects what it ran.
pub fn press(p: &mut Panel, center: &Center, players: Option<&Players>, cell: (u16, u16), button: Button) -> PanelAction {
    let items = items(center, players);
    let select = |p: &mut Panel, item: Item| -> bool {
        let Some(i) = items.iter().position(|x| *x == item) else { return false };
        let moved = p.selected != i;
        p.selected = i;
        moved
    };
    let line = layout(center, players, p).get(usize::from(cell.1)).copied();
    match (line, button) {
        (Some(Line::Card(_)), Button::Left) => {
            let Some(pl) = players else { return PanelAction::Nothing };
            match media_popup::press(&mut p.media, pl, cell, WIDTH) {
                MediaAction::Nothing => PanelAction::Nothing,
                a => {
                    select(p, Item::Card);
                    PanelAction::Media(a)
                }
            }
        }
        (Some(Line::Entry(i, _)), Button::Right) => center.history().nth(i).map_or(PanelAction::Nothing, |e| PanelAction::Remove(e.id)),
        (Some(Line::Entry(i, _)), Button::Left) => {
            let moved = select(p, Item::Entry(i));
            match activate(Item::Entry(i), center) {
                PanelAction::Nothing if moved => PanelAction::Redraw,
                a => a,
            }
        }
        (Some(Line::Dnd), Button::Left) => {
            select(p, Item::Dnd);
            PanelAction::ToggleDnd
        }
        (Some(Line::Clear), Button::Left) => {
            select(p, Item::Clear);
            PanelAction::Clear
        }
        _ => PanelAction::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationsConfig;
    use crate::media::{Control, PositionSync, Value};
    use crate::notify::{Action, Incoming, Urgency};
    use std::time::Duration;

    const LEN: i64 = 245_000_000; // 4:05

    /// What GetAll returns for a fictional track.
    fn props(status: &str, title: &str, artist: &str) -> Vec<(String, Value)> {
        let meta = vec![
            ("mpris:trackid".to_string(), Value::Str(format!("/fake/track/{}", title.len()))),
            ("xesam:title".to_string(), Value::Str(title.to_string())),
            ("xesam:artist".to_string(), Value::StrList(vec![artist.to_string()])),
            ("mpris:length".to_string(), Value::Int(LEN)),
        ];
        let can = |k: &str| (k.to_string(), Value::Bool(true));
        vec![
            ("PlaybackStatus".to_string(), Value::Str(status.to_string())),
            ("Metadata".to_string(), Value::Map(meta)),
            ("Rate".to_string(), Value::Float(1.0)),
            can("CanPlay"),
            can("CanPause"),
            can("CanGoNext"),
            can("CanGoPrevious"),
        ]
    }

    /// One player playing "Paper Lanterns" by "The Quiet Hours", at 1:42 at `t0`: a 4-row card.
    fn one(t0: Instant) -> Players {
        let mut players = Players::default();
        assert!(players.add("org.mpris.MediaPlayer2.lumen".to_string(), ":1.7".to_string()));
        let _ = players.apply(":1.7", &props("Playing", "Paper Lanterns", "The Quiet Hours"), &[]);
        players.set_identity(":1.7", "Lumen Player");
        players.set_position(":1.7", PositionSync { at: t0, us: 102_000_000 });
        players
    }

    fn incoming(app: &str, summary: &str, actions: &[(&str, &str)], urgency: Urgency) -> Incoming {
        Incoming {
            app_name: app.to_string(),
            replaces_id: 0,
            summary: summary.to_string(),
            body: String::new(),
            actions: actions.iter().map(|(k, l)| Action { key: k.to_string(), label: l.to_string() }).collect(),
            urgency,
            expire_ms: -1,
            desktop_entry: None,
            category: None,
            transient: false,
            resident: false,
            tag: None,
        }
    }

    /// "Weekly report ready" (Mailer, 13:58, expired: history only), then "Message from Lina" (Chat,
    /// 14:02, live, with a default action): `history()` gives the Chat one first.
    fn center(t0: Instant) -> (Center, u32, u32) {
        let mut c = Center::new(&NotificationsConfig::default(), false);
        let old = c.notify(incoming("Mailer", "Weekly report ready", &[], Urgency::Normal), t0, 13 * 60 + 58);
        let _ = c.expire(t0 + Duration::from_secs(30));
        let new = c.notify(
            incoming("Chat", "Message from Lina", &[("default", "Open")], Urgency::Normal),
            t0 + Duration::from_secs(31),
            14 * 60 + 2,
        );
        (c, old, new)
    }

    fn texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    #[test]
    fn the_card_then_the_history_newest_first_then_dnd_and_clear_all() {
        let t0 = Instant::now();
        let (c, _, _) = center(t0);
        let players = one(t0);
        let p = Panel::new(&c, Some(&players), 40);
        assert_eq!(p.selected, 1, "the newest entry, after the card");
        let b = rows(&c, Some(&players), &p, t0);
        assert_eq!(b.cells(), (WIDTH + 2, 4 + 1 + 4 + 3), "card, separator, two entries of two lines, separator, two rows");
        assert_eq!(b.separators, vec![4, 9]);
        let t = texts(&b);
        assert_eq!(t[0], " Paper Lanterns", "the media popup's own rows");
        let chat = c.history().next().unwrap();
        let mailer = c.history().nth(1).unwrap();
        // Pad, glyph, space, the app from cell 3, the time at cells 26 to 30.
        assert_eq!(t[5], format!(" {} Chat{}14:02", chat.glyph, " ".repeat(19)));
        assert_eq!(t[6], "   Message from Lina");
        assert_eq!(t[7], format!(" {} Mailer{}13:58", mailer.glyph, " ".repeat(17)));
        assert_eq!(t[8], "   Weekly report ready");
        assert_eq!(t[10], format!(" Do not disturb{}○", " ".repeat(15)));
        assert_eq!(t[11], " Clear all");
        assert_eq!(b.rows[5].styles(), "S".repeat(32), "the selected entry's two lines on the band");
        assert_eq!(b.rows[6].styles(), "S".repeat(32));
        assert_eq!(&b.rows[7].styles()[1..2], "a", "an entry's glyph in the accent");
        assert!(!b.rows[3].styles().contains('S'), "the card shows no band while the selection is elsewhere");
    }

    #[test]
    fn without_a_player_no_card_and_an_empty_history_says_so() {
        let c = Center::new(&NotificationsConfig::default(), false);
        let p = Panel::new(&c, None, 40);
        assert_eq!(items(&c, None)[p.selected], Item::Dnd, "nothing else to select");
        let b = rows(&c, None, &p, Instant::now());
        let dnd = format!(" Do not disturb{}○", " ".repeat(15));
        assert_eq!(texts(&b), [" No notifications".to_string(), String::new(), dnd, " Clear all".to_string()]);
        assert_eq!(b.separators, vec![1]);
        assert_eq!(&b.rows[0].styles()[1..17], "m".repeat(16), "muted");
        let none = Players::default();
        assert_eq!(rows(&c, Some(&none), &Panel::new(&c, Some(&none), 40), Instant::now()).cells().1, 4, "no player shown: no card");
    }

    #[test]
    fn do_not_disturb_shows_a_full_dot_when_on() {
        let mut c = Center::new(&NotificationsConfig::default(), false);
        c.set_dnd(true);
        let b = rows(&c, None, &Panel::new(&c, None, 40), Instant::now());
        assert_eq!(texts(&b)[2], format!(" Do not disturb{}●", " ".repeat(15)));
    }

    #[test]
    fn keys_move_run_remove_toggle_and_clear() {
        let t0 = Instant::now();
        let (c, old, new) = center(t0);
        let mut p = Panel::new(&c, None, 40);
        assert_eq!(p.selected, 0, "the newest entry");
        let live = c.history().next().unwrap();
        assert_eq!(
            key(&mut p, &c, None, Key::Enter),
            PanelAction::Invoke(new, live.revision, "default".into()),
            "live, with a default action"
        );
        assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Redraw);
        assert_eq!(key(&mut p, &c, None, Key::Enter), PanelAction::Nothing, "from the history only: Enter does nothing");
        assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(old));
        assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Redraw);
        assert_eq!(key(&mut p, &c, None, Key::Enter), PanelAction::ToggleDnd);
        assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Nothing, "not an entry");
        assert_eq!(key(&mut p, &c, None, Key::End), PanelAction::Redraw);
        assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Nothing, "clamped, never wrapping");
        assert_eq!(key(&mut p, &c, None, Key::Enter), PanelAction::Clear);
        assert_eq!(key(&mut p, &c, None, Key::Home), PanelAction::Redraw);
        assert_eq!(key(&mut p, &c, None, Key::Up), PanelAction::Nothing);
        assert_eq!(key(&mut p, &c, None, Key::Escape), PanelAction::Close);
    }

    /// An expired notification keeps its actions in the history, but nothing serves them any more:
    /// Enter (or a left press) on it only selects.
    #[test]
    fn an_expired_entry_with_a_default_action_runs_nothing() {
        let t0 = Instant::now();
        let mut c = Center::new(&NotificationsConfig::default(), false);
        let _ = c.notify(incoming("Chat", "Message from Lina", &[("default", "Open")], Urgency::Normal), t0, 14 * 60 + 2);
        let _ = c.expire(t0 + Duration::from_secs(30));
        assert!(!c.history().next().unwrap().live && !c.history().next().unwrap().actions.is_empty());
        let mut p = Panel::new(&c, None, 40);
        assert_eq!(key(&mut p, &c, None, Key::Enter), PanelAction::Nothing);
        assert_eq!(press(&mut p, &c, None, (5, 0), Button::Left), PanelAction::Nothing, "already selected: nothing at all");
    }

    #[test]
    fn the_card_keeps_the_media_popups_keys() {
        let t0 = Instant::now();
        let (c, _, _) = center(t0);
        let players = one(t0);
        let mut p = Panel::new(&c, Some(&players), 40);
        assert_eq!(key(&mut p, &c, Some(&players), Key::Up), PanelAction::Redraw);
        assert_eq!(p.selected, 0, "on the card");
        let b = rows(&c, Some(&players), &p, t0);
        assert_eq!(&b.rows[3].styles()[15..18], "SSS", "now the card shows its band, on play/pause");
        assert_eq!(key(&mut p, &c, Some(&players), Key::Right), PanelAction::Media(MediaAction::Redraw));
        assert_eq!(key(&mut p, &c, Some(&players), Key::Enter), PanelAction::Media(MediaAction::Control(Control::Next)));
        assert_eq!(key(&mut p, &c, Some(&players), Key::Up), PanelAction::Nothing, "nothing selectable above the buttons");
        assert_eq!(key(&mut p, &c, Some(&players), Key::Delete), PanelAction::Nothing);
        assert_eq!(key(&mut p, &c, Some(&players), Key::Down), PanelAction::Redraw, "past the card: the history");
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn a_press_maps_rows_to_the_card_the_entries_and_the_footer() {
        let t0 = Instant::now();
        let (c, old, new) = center(t0);
        let players = one(t0);
        let mut p = Panel::new(&c, Some(&players), 40);
        // Rows: 0-3 the card, 4 a separator, 5-6 Chat (live), 7-8 Mailer, 9 a separator, 10 DND, 11 Clear all.
        assert_eq!(press(&mut p, &c, Some(&players), (21, 3), Button::Left), PanelAction::Media(MediaAction::Control(Control::Next)), "⏭");
        assert_eq!(p.selected, 0);
        let live = c.history().next().unwrap();
        assert_eq!(
            press(&mut p, &c, Some(&players), (5, 6), Button::Left),
            PanelAction::Invoke(new, live.revision, "default".into()),
            "either line"
        );
        assert_eq!(p.selected, 1);
        assert_eq!(press(&mut p, &c, Some(&players), (5, 7), Button::Left), PanelAction::Redraw, "from the history: selected only");
        assert_eq!(p.selected, 2);
        assert_eq!(press(&mut p, &c, Some(&players), (5, 8), Button::Right), PanelAction::Remove(old));
        assert_eq!(press(&mut p, &c, Some(&players), (5, 10), Button::Left), PanelAction::ToggleDnd);
        assert_eq!(press(&mut p, &c, Some(&players), (5, 11), Button::Left), PanelAction::Clear);
        for row in [4, 9, 12, 40] {
            assert_eq!(press(&mut p, &c, Some(&players), (5, row), Button::Left), PanelAction::Nothing, "row {row}");
        }
        assert_eq!(press(&mut p, &c, Some(&players), (5, 10), Button::Right), PanelAction::Nothing, "a right click removes entries only");
    }

    #[test]
    fn a_long_history_scrolls_with_the_selection() {
        let t0 = Instant::now();
        let mut c = Center::new(&NotificationsConfig::default(), false);
        for n in 0..50u64 {
            let _ = c.notify(
                incoming(&format!("App {}", n % 5), &format!("Event {n}"), &[], Urgency::Critical),
                t0 + Duration::from_secs(n),
                600,
            );
        }
        assert_eq!(c.history().count(), 50);
        // Room 13: the footer takes 3, so 5 entries of two lines show.
        let mut p = Panel::new(&c, None, 13);
        scroll_into_view(&mut p, &c, None);
        assert_eq!(rows(&c, None, &p, t0).cells().1, 13, "within the room");
        for _ in 0..7 {
            assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Redraw);
        }
        scroll_into_view(&mut p, &c, None);
        assert_eq!(p.top, 3, "entry 7 is the last of the five shown");
        let b = rows(&c, None, &p, t0);
        assert_eq!(texts(&b)[9], "   Event 42", "newest first: entry 7 is Event 42");
        assert_eq!(b.rows[8].styles(), "S".repeat(32));
        assert_eq!(key(&mut p, &c, None, Key::End), PanelAction::Redraw);
        scroll_into_view(&mut p, &c, None);
        assert_eq!(p.top, 3, "on the footer, which always shows: the window stays");
        assert_eq!(key(&mut p, &c, None, Key::Home), PanelAction::Redraw);
        scroll_into_view(&mut p, &c, None);
        assert_eq!(p.top, 0);
        let mut tiny = Panel::new(&c, None, 0);
        scroll_into_view(&mut tiny, &c, None);
        assert_eq!(rows(&c, None, &tiny, t0).cells().1, 2 + 3, "at least one entry, whatever the room");
    }

    /// Review Focus: a notification arriving while the panel is open shows first, and the selection
    /// stays on a row that exists.
    #[test]
    fn a_notification_arriving_while_open_shows_first() {
        let t0 = Instant::now();
        let (mut c, _, _) = center(t0);
        let mut p = Panel::new(&c, None, 40);
        p.selected = 1; // the older entry
        let before = rows(&c, None, &p, t0);
        let _ = c.notify(incoming("Calendar", "Standup in 5 minutes", &[], Urgency::Normal), t0 + Duration::from_secs(40), 14 * 60 + 5);
        scroll_into_view(&mut p, &c, None);
        let after = rows(&c, None, &p, t0);
        assert_eq!(after.cells().1, before.cells().1 + 2);
        assert_eq!(texts(&after)[1], "   Standup in 5 minutes");
        assert!(p.selected < items(&c, None).len());
    }
}
