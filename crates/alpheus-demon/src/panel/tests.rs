use super::*;
use crate::config::NotificationsConfig;
use crate::media::{Control, PositionSync, Value};
use crate::notify::{Action, Incoming, Urgency};
use std::time::Duration;

const LEN: i64 = 245_000_000; // 4:05
/// 14:30 on day 0, the entries' own day: their times show.
const TODAY: u32 = 14 * 60 + 30;

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
    let new =
        c.notify(incoming("Chat", "Message from Lina", &[("default", "Open")], Urgency::Normal), t0 + Duration::from_secs(31), 14 * 60 + 2);
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
    let b = rows(&c, Some(&players), &p, t0, TODAY);
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
    let b = rows(&c, None, &p, Instant::now(), TODAY);
    let dnd = format!(" Do not disturb{}○", " ".repeat(15));
    assert_eq!(texts(&b), [" No notifications".to_string(), String::new(), dnd, " Clear all".to_string()]);
    assert_eq!(b.separators, vec![1]);
    assert_eq!(&b.rows[0].styles()[1..17], "m".repeat(16), "muted");
    let none = Players::default();
    assert_eq!(rows(&c, Some(&none), &Panel::new(&c, Some(&none), 40), Instant::now(), TODAY).cells().1, 4, "no player shown: no card");
}

#[test]
fn do_not_disturb_shows_a_full_dot_when_on() {
    let mut c = Center::new(&NotificationsConfig::default(), false);
    c.set_dnd(true);
    let b = rows(&c, None, &Panel::new(&c, None, 40), Instant::now(), TODAY);
    assert_eq!(texts(&b)[2], format!(" Do not disturb{}●", " ".repeat(15)));
}

#[test]
fn keys_move_run_remove_toggle_and_clear() {
    let t0 = Instant::now();
    let (c, old, new) = center(t0);
    let mut p = Panel::new(&c, None, 40);
    assert_eq!(p.selected, 0, "the newest entry");
    let live = c.history().next().unwrap();
    assert_eq!(key(&mut p, &c, None, Key::Enter), PanelAction::Invoke(new, live.revision, "default".into()), "live, with a default action");
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
    let b = rows(&c, Some(&players), &p, t0, TODAY);
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
        let _ =
            c.notify(incoming(&format!("App {}", n % 5), &format!("Event {n}"), &[], Urgency::Critical), t0 + Duration::from_secs(n), 600);
    }
    assert_eq!(c.history().count(), 50);
    // Room 13: the footer takes 3, so 5 entries of two lines show.
    let mut p = Panel::new(&c, None, 13);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(rows(&c, None, &p, t0, TODAY).cells().1, 13, "within the room");
    for _ in 0..7 {
        assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Redraw);
    }
    scroll_into_view(&mut p, &c, None);
    assert_eq!(p.top, 3, "entry 7 is the last of the five shown");
    let b = rows(&c, None, &p, t0, TODAY);
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
    assert_eq!(rows(&c, None, &tiny, t0, TODAY).cells().1, 2 + 3, "at least one entry, whatever the room");
}

/// The selection is the entry, not its row. A notification arriving above it moves
/// the highlight down with it, so Enter and Delete still hit the entry the user chose, never the
/// newcomer (whose default action is another app's).
#[test]
fn the_selection_follows_its_entry_when_one_arrives_above() {
    let t0 = Instant::now();
    let (mut c, _, chat) = center(t0);
    let mut p = Panel::new(&c, None, 40);
    assert_eq!(items(&c, None)[p.selected], Item::Entry(0), "Chat, the newest, selected");
    let calendar = c.notify(
        incoming("Calendar", "Standup in 5 minutes", &[("default", "Join")], Urgency::Normal),
        t0 + Duration::from_secs(40),
        14 * 60 + 5,
    );
    assert_eq!(c.history().next().unwrap().id, calendar, "the newcomer is first");
    scroll_into_view(&mut p, &c, None);
    assert_eq!(p.selected, 1, "the highlight moved down with Chat");
    let b = rows(&c, None, &p, t0, TODAY);
    assert_eq!(b.rows[2].styles(), "S".repeat(32), "Chat's row on the band");
    assert!(!b.rows[0].styles().contains('S'), "not the newcomer's");
    let revision = c.history().nth(1).unwrap().revision;
    assert_eq!(key(&mut p, &c, None, Key::Enter), PanelAction::Invoke(chat, revision, "default".into()), "Chat's action, not Calendar's");
    // Without a refresh in between (a key right after the arrival), the same.
    let _ = c.notify(incoming("Mailer", "Invoice", &[("default", "Open")], Urgency::Normal), t0 + Duration::from_secs(41), 14 * 60 + 6);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(chat));
    // The footer rows keep theirs too.
    assert_eq!(key(&mut p, &c, None, Key::End), PanelAction::Redraw);
    let _ = c.notify(incoming("Mailer", "Receipt", &[], Urgency::Normal), t0 + Duration::from_secs(42), 14 * 60 + 7);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(items(&c, None)[p.selected], Item::Clear);
}

/// What a press selected is followed as a key's choice is.
#[test]
fn a_pressed_entry_is_followed_when_one_arrives_above() {
    let t0 = Instant::now();
    let (mut c, old, _) = center(t0);
    let mut p = Panel::new(&c, None, 40);
    // Rows: 0-1 Chat, 2-3 Mailer.
    assert_eq!(press(&mut p, &c, None, (5, 2), Button::Left), PanelAction::Redraw, "Mailer selected");
    let _ = c.notify(incoming("Calendar", "Standup in 5 minutes", &[], Urgency::Normal), t0 + Duration::from_secs(40), 14 * 60 + 5);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(old));
}

/// The wheel: every notch applied, one action for them all (the wiring repaints once), Redraw
/// when any moved; at most 10 notches.
#[test]
fn the_wheel_moves_by_its_notches_in_one_action() {
    let t0 = Instant::now();
    let (c, _, _) = center(t0);
    let mut p = Panel::new(&c, None, 40);
    assert_eq!(scroll(&mut p, &c, None, 2), PanelAction::Redraw);
    assert_eq!(items(&c, None)[p.selected], Item::Dnd, "two rows down: Mailer, then DND");
    assert_eq!(scroll(&mut p, &c, None, 5), PanelAction::Redraw, "clamped at Clear all, still moved");
    assert_eq!(items(&c, None)[p.selected], Item::Clear);
    assert_eq!(scroll(&mut p, &c, None, 1), PanelAction::Nothing, "already at the end");
    assert_eq!(scroll(&mut p, &c, None, -1), PanelAction::Redraw);
    assert_eq!(items(&c, None)[p.selected], Item::Dnd);
    let mut long = Center::new(&NotificationsConfig::default(), false);
    for n in 0..30u64 {
        let _ = long.notify(incoming("App", &format!("Event {n}"), &[], Urgency::Critical), t0 + Duration::from_secs(n), 600);
    }
    let mut q = Panel::new(&long, None, 40);
    assert_eq!(scroll(&mut q, &long, None, 25), PanelAction::Redraw);
    assert_eq!(q.selected, 10, "ten notches at most");
}

/// The selected entry gone (removed, cleared): the selection stays at its row, on what now
/// shows there, and follows that one from then on.
#[test]
fn a_removed_selection_falls_to_the_row_that_takes_its_place() {
    let t0 = Instant::now();
    let (mut c, old, new) = center(t0);
    let mut p = Panel::new(&c, None, 40);
    c.remove(new);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(p.selected, 0, "Mailer, now first");
    let _ = c.notify(incoming("Calendar", "Standup in 5 minutes", &[], Urgency::Normal), t0 + Duration::from_secs(40), 14 * 60 + 5);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(old), "Mailer followed");
}

/// An entry with no summary shows its body's first line in its place.
#[test]
fn an_entry_without_a_summary_shows_its_first_body_line() {
    let t0 = Instant::now();
    let mut c = Center::new(&NotificationsConfig::default(), false);
    let n = Incoming { body: "\n  Lina sent a photo\nsecond line".into(), ..incoming("Chat", "", &[], Urgency::Normal) };
    let _ = c.notify(n, t0, 14 * 60 + 2);
    let b = rows(&c, None, &Panel::new(&c, None, 40), t0, TODAY);
    assert_eq!(texts(&b)[1], "   Lina sent a photo");
}

/// Past midnight, yesterday's entries show their date, today's their time.
#[test]
fn entries_from_another_day_show_their_date() {
    use crate::clock::{DAY_MINUTES, days_from_civil};
    let day = |d| u32::try_from(days_from_civil(2026, 10, d)).unwrap() * DAY_MINUTES;
    let t0 = Instant::now();
    let mut c = Center::new(&NotificationsConfig::default(), false);
    let _ = c.notify(incoming("Mailer", "Weekly report ready", &[], Urgency::Normal), t0, day(8) + 23 * 60 + 59);
    let _ = c.notify(incoming("Chat", "Message from Lina", &[], Urgency::Normal), t0, day(9) + 1);
    let t = texts(&rows(&c, None, &Panel::new(&c, None, 40), t0, day(9) + 2));
    assert!(t[0].ends_with("00:01"), "{}", t[0]);
    assert!(t[2].ends_with("Oct 8"), "{}", t[2]);
}

/// The selected entry deleted: the selection goes to the entry above it, or the one below when it
/// was the first; never to "Do not disturb" or "Clear all" while an entry is left.
#[test]
fn deleting_the_selected_entry_selects_its_neighbour() {
    let t0 = Instant::now();
    let mut c = Center::new(&NotificationsConfig::default(), false);
    let ids: Vec<u32> = ["a", "b", "c"].iter().map(|s| c.notify(incoming(s, s, &[], Urgency::Normal), t0, 600)).collect();
    // History, newest first: c, b, a.
    let mut p = Panel::new(&c, None, 40);
    assert_eq!(key(&mut p, &c, None, Key::End), PanelAction::Redraw);
    assert_eq!(key(&mut p, &c, None, Key::Up), PanelAction::Redraw);
    assert_eq!(key(&mut p, &c, None, Key::Up), PanelAction::Redraw);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(ids[0]), "the last entry, a");
    c.remove(ids[0]);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(items(&c, None)[p.selected], Item::Entry(1), "b, above it, not Do not disturb");
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(ids[1]));
    assert_eq!(key(&mut p, &c, None, Key::Up), PanelAction::Redraw);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(ids[2]), "c, the first");
    c.remove(ids[2]);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(ids[1]), "the first gone: the one below it");
    c.remove(ids[1]);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(items(&c, None)[p.selected], Item::Dnd, "no entry left: Do not disturb");
    // One in the middle: the entry above it, not the one that slid into its row.
    let ids: Vec<u32> = ["d", "e", "f"].iter().map(|s| c.notify(incoming(s, s, &[], Urgency::Normal), t0, 600)).collect();
    let mut p = Panel::new(&c, None, 40);
    assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Redraw);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(ids[1]), "e, between f and d");
    c.remove(ids[1]);
    scroll_into_view(&mut p, &c, None);
    assert_eq!(key(&mut p, &c, None, Key::Delete), PanelAction::Remove(ids[2]), "f, above it");
}

/// A notification arriving while the panel is open shows first, and the selection
/// stays on a row that exists.
#[test]
fn a_notification_arriving_while_open_shows_first() {
    let t0 = Instant::now();
    let (mut c, _, _) = center(t0);
    let mut p = Panel::new(&c, None, 40);
    assert_eq!(key(&mut p, &c, None, Key::Down), PanelAction::Redraw); // the older entry
    let before = rows(&c, None, &p, t0, TODAY);
    let _ = c.notify(incoming("Calendar", "Standup in 5 minutes", &[], Urgency::Normal), t0 + Duration::from_secs(40), 14 * 60 + 5);
    scroll_into_view(&mut p, &c, None);
    let after = rows(&c, None, &p, t0, TODAY);
    assert_eq!(after.cells().1, before.cells().1 + 2);
    assert_eq!(texts(&after)[1], "   Standup in 5 minutes");
    assert!(p.selected < items(&c, None).len());
}
