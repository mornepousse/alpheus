use super::*;
use crate::app::diagnostics::DiagItem;
use crate::app::keymap::Keymap;
use crate::backends::BackendError;
use crate::config::Config;
use crate::diagnostics::query::Priority;
use crate::theme::Theme;
use ratatui::crossterm::event::{KeyEventKind, KeyEventState};

fn at(secs: u64, t0: Instant) -> Instant {
    t0 + Duration::from_secs(secs)
}

fn on_diag(t0: Instant) -> App {
    let mut a = App::with_tab(Config::default(), Theme::default(), Keymap::default(), Tab::Diagnostics, t0);
    a.take_requests();
    a
}

fn entry(n: u32, message: &str) -> Entry {
    Entry {
        cursor: format!("s=0;i={n}"),
        time_us: 1_700_000_000_000_000 + u64::from(n) * 1_000_000,
        priority: 6,
        ident: "example-app".into(),
        unit: "example-app.service".into(),
        pid: Some(100 + n),
        message: message.into(),
    }
}

fn read(entries: Vec<Entry>) -> DiagResult {
    DiagResult::Journal(Ok(JournalRead { entries, capped: false, timed_out: false }))
}

fn key(a: &mut App, code: KeyCode, now: Instant) {
    a.on_key(KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }, now);
}

fn typed(a: &mut App, text: &str, now: Instant) {
    for c in text.chars() {
        key(a, KeyCode::Char(c), now);
    }
}

fn view(a: &App) -> &JournalView {
    match &a.diag.view {
        View::Journal(j) => j,
        other => panic!("not the journal view: {other:?}"),
    }
}

/// The journal reads queued since the last call.
fn journal_reads(a: &mut App) -> Vec<JournalQuery> {
    a.take_requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::Diag(DiagRead::Journal(q)) => Some(q),
            _ => None,
        })
        .collect()
}

/// Opens a preset and answers its first read with `entries` at `t`.
fn opened(a: &mut App, preset: Preset, entries: Vec<Entry>, t: Instant) -> JournalQuery {
    a.diag_activate(DiagItem::Preset(preset));
    let q = journal_reads(a).pop().expect("the first read");
    a.on_update(Update::Diag(DiagRead::Journal(q.clone()), read(entries)), t);
    q
}

use crate::app::worker::Update;

#[test]
fn presets_open_with_their_query() {
    let t0 = Instant::now();
    for preset in Preset::ALL {
        let mut a = on_diag(t0);
        a.diag_activate(DiagItem::Preset(preset));
        let r = a.take_requests();
        assert!(r.contains(&Request::Diag(DiagRead::JournalReadable)), "{preset:?}: the access check, once");
        let reads: Vec<_> = r.iter().filter(|q| matches!(q, Request::Diag(DiagRead::Journal(_)))).collect();
        if preset == Preset::ServiceLogs {
            assert!(reads.is_empty(), "a service is chosen first");
            assert!(r.contains(&Request::Diag(DiagRead::ServiceUnits)));
            assert!(view(&a).picker.is_some());
        } else {
            assert_eq!(reads, [&Request::Diag(DiagRead::Journal(JournalQuery::new(preset, 500)))], "{preset:?}");
            assert_eq!(view(&a).follow, preset.follows(), "follow starts on for Live and USB only");
        }
    }
    // A unit from the overview reads its own journal, and a crash the minute around it.
    let mut a = on_diag(t0);
    a.diag_activate(DiagItem::Failed(Scope::User, "example-app.service".into()));
    let q = journal_reads(&mut a).pop().unwrap();
    assert_eq!(q.unit, Some((Scope::User, "example-app.service".to_string())));
    assert!(!view(&a).follow);
}

#[test]
fn the_text_filter_is_local() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::ErrorsThisBoot, vec![entry(1, "Café ouvert"), entry(2, "disk full"), entry(3, "CAFE fermé")], t0);
    assert_eq!(view(&a).visible().len(), 3);
    key(&mut a, KeyCode::Char('/'), t0);
    typed(&mut a, "cafe", t0);
    let shown: Vec<_> = view(&a).visible().iter().map(|e| e.message.clone()).collect();
    assert_eq!(shown, ["Café ouvert", "CAFE fermé"], "case and accents folded");
    assert!(a.take_requests().is_empty(), "no new read for a filter");
    key(&mut a, KeyCode::Backspace, t0);
    key(&mut a, KeyCode::Enter, t0);
    assert!(!view(&a).filter_editing && view(&a).filter == "caf");
    // q typed while editing is text, not a quit.
    key(&mut a, KeyCode::Char('/'), t0);
    key(&mut a, KeyCode::Char('q'), t0);
    assert!(!a.should_quit && view(&a).filter == "cafq");
    key(&mut a, KeyCode::Esc, t0);
    assert!(view(&a).filter.is_empty() && !view(&a).filter_editing, "Esc clears it");
    assert_eq!(view(&a).visible().len(), 3);
}

#[test]
fn priority_and_unit_changes_read_again_from_scratch() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    let first = opened(&mut a, Preset::Live, vec![entry(1, "old")], t0);
    key(&mut a, KeyCode::Char('p'), t0);
    let q = journal_reads(&mut a).pop().expect("a new read");
    assert_eq!((q.priority, q.after_cursor.clone()), (Priority::Err, None));
    assert!(view(&a).entries.is_empty(), "nothing of the old query stays");
    // The old query's answer, arriving late, is ignored.
    a.on_update(Update::Diag(DiagRead::Journal(first), read(vec![entry(9, "stale")])), at(1, t0));
    assert!(view(&a).entries.is_empty());
    a.on_update(Update::Diag(DiagRead::Journal(q), read(vec![entry(2, "fresh")])), at(1, t0));
    assert_eq!(view(&a).entries.len(), 1);
    // The cycle: err, warning, notice, info, any.
    for expected in [Priority::Warning, Priority::Notice, Priority::Info, Priority::Any] {
        key(&mut a, KeyCode::Char('p'), t0);
        assert_eq!(journal_reads(&mut a).pop().unwrap().priority, expected);
    }
    // A unit from the picker.
    key(&mut a, KeyCode::Char('u'), t0);
    assert!(a.take_requests().contains(&Request::Diag(DiagRead::ServiceUnits)));
    a.on_update(
        Update::Diag(
            DiagRead::ServiceUnits,
            DiagResult::ServiceUnits(Ok(vec![(Scope::System, "mnt-data.mount".into()), (Scope::User, "example-app.service".into())])),
        ),
        t0,
    );
    typed(&mut a, "app", t0);
    key(&mut a, KeyCode::Enter, t0);
    let q = journal_reads(&mut a).pop().unwrap();
    assert_eq!(q.unit, Some((Scope::User, "example-app.service".to_string())));
    assert!(view(&a).picker.is_none());
    // Backspace on an empty query clears the unit.
    key(&mut a, KeyCode::Char('u'), t0);
    a.take_requests();
    key(&mut a, KeyCode::Backspace, t0);
    assert_eq!(journal_reads(&mut a).pop().unwrap().unit, None);
}

#[test]
fn follow_reads_after_the_last_cursor_once_a_second() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a"), entry(2, "b")], t0);
    a.on_tick(at(0, t0) + Duration::from_millis(500));
    assert!(journal_reads(&mut a).is_empty(), "not before a second");
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().expect("a follow read");
    assert_eq!(q.after_cursor.as_deref(), Some("s=0;i=2"));
    a.on_update(Update::Diag(DiagRead::Journal(q), read(vec![entry(3, "c")])), at(1, t0));
    assert_eq!(view(&a).entries.len(), 3);
    a.on_tick(at(1, t0) + Duration::from_millis(900));
    assert!(journal_reads(&mut a).is_empty());
    a.on_tick(at(2, t0));
    assert_eq!(journal_reads(&mut a).pop().unwrap().after_cursor.as_deref(), Some("s=0;i=3"));
}

#[test]
fn an_empty_journal_follows_from_a_time_not_from_the_start_of_the_boot() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    let first = opened(&mut a, Preset::Live, vec![], t0);
    assert_eq!(first.since, None);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().expect("a follow read");
    assert_eq!(q.after_cursor, None);
    assert!(q.since.is_some_and(|s| s > 1_000_000_000), "from a wall-clock time: {q:?}");
    // It comes back empty: asked again from a time, still not a cursor.
    a.on_update(Update::Diag(DiagRead::Journal(q), read(vec![])), at(1, t0));
    a.on_tick(at(2, t0));
    assert!(journal_reads(&mut a).pop().unwrap().since.is_some());
}

#[test]
fn the_first_entry_of_an_empty_view_switches_to_the_cursor() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![], t0);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    a.on_update(Update::Diag(DiagRead::Journal(q), read(vec![entry(5, "late")])), at(1, t0));
    a.on_tick(at(2, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    assert_eq!((q.after_cursor.as_deref(), q.since), (Some("s=0;i=5"), None));
}

#[test]
fn one_follow_read_in_flight() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a")], t0);
    let mut sent = 0;
    for s in 1..=10 {
        a.on_tick(at(s, t0));
        sent += journal_reads(&mut a).len();
    }
    assert_eq!(sent, 1, "a slow read delays the next, never stacks");
    // The first read is still out too: nothing is asked while it is.
    let mut b = on_diag(t0);
    b.diag_activate(DiagItem::Preset(Preset::Live));
    journal_reads(&mut b);
    for s in 1..=5 {
        b.on_tick(at(s, t0));
    }
    assert!(journal_reads(&mut b).is_empty());
}

#[test]
fn leaving_the_view_stops_the_follow() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a")], t0);
    key(&mut a, KeyCode::Esc, t0); // Back
    assert_eq!(a.diag.view, View::Overview);
    for s in 1..=5 {
        a.on_tick(at(s, t0));
    }
    assert!(journal_reads(&mut a).is_empty(), "no read once the view is gone");
    // Another tab ends it too, and a late answer is dropped.
    let mut b = on_diag(t0);
    let q = opened(&mut b, Preset::Live, vec![entry(1, "a")], t0);
    b.on_tick(at(1, t0));
    let follow = journal_reads(&mut b).pop().unwrap();
    b.go_tab(Tab::Power, at(1, t0));
    b.take_requests();
    b.on_update(Update::Diag(DiagRead::Journal(follow), read(vec![entry(2, "b")])), at(2, t0));
    for s in 2..=5 {
        b.on_tick(at(s, t0));
    }
    assert!(b.take_requests().is_empty());
    let _ = q;
    // Follow turned off.
    let mut c = on_diag(t0);
    opened(&mut c, Preset::Live, vec![entry(1, "a")], t0);
    key(&mut c, KeyCode::Char('f'), t0);
    assert!(!view(&c).follow);
    c.on_tick(at(5, t0));
    assert!(journal_reads(&mut c).is_empty());
}

#[test]
fn the_buffer_keeps_2000() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, (0..1500).map(|n| entry(n, "x")).collect(), t0);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    assert_eq!(q.after_cursor.as_deref(), Some("s=0;i=1499"));
    a.on_update(Update::Diag(DiagRead::Journal(q), read((1500..2300).map(|n| entry(n, "y")).collect())), at(1, t0));
    let v = view(&a);
    assert_eq!(v.entries.len(), MAX_ENTRIES);
    assert_eq!(v.entries.front().unwrap().entry.cursor, "s=0;i=300", "the oldest went first");
    assert_eq!(v.last_cursor(), Some("s=0;i=2299"));
}

#[test]
fn a_capped_follow_says_more_is_waiting_and_only_that() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a")], t0);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    // A burst: 1,000 kept and one more behind them (a 1,001 line read). Nothing is lost, the next
    // read goes on from the last kept cursor, so the view says "more waiting", not a count.
    let burst = JournalRead { entries: (2..1002).map(|n| entry(n, "b")).collect(), capped: true, timed_out: false };
    a.on_update(Update::Diag(DiagRead::Journal(q), DiagResult::Journal(Ok(burst))), at(1, t0));
    assert!(view(&a).more_waiting);
    a.on_tick(at(2, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    assert_eq!(q.after_cursor.as_deref(), Some("s=0;i=1001"), "continues from its last cursor");
    a.on_update(Update::Diag(DiagRead::Journal(q), read(vec![entry(1002, "c")])), at(2, t0));
    assert!(!view(&a).more_waiting, "caught up");
}

#[test]
fn a_failed_follow_stops_and_says_why() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a")], t0);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    let err = BackendError::Missing { tool: "journalctl".into() };
    a.on_update(Update::Diag(DiagRead::Journal(q), DiagResult::Journal(Err(err))), at(1, t0));
    assert!(matches!(view(&a).load, Load::Failed(_)) && !view(&a).follow);
    assert_eq!(view(&a).entries.len(), 1, "what was read stays");
    for s in 2..=5 {
        a.on_tick(at(s, t0));
    }
    assert!(journal_reads(&mut a).is_empty(), "a broken tool is not relaunched every second");
}

#[test]
fn scrolling_up_pauses_and_counts_new() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, (0..100).map(|n| entry(n, "x")).collect(), t0);
    assert_eq!(view(&a).scroll, Scroll::Tail);
    key(&mut a, KeyCode::PageUp, t0);
    let Scroll::At(k) = view(&a).scroll else { panic!("paused") };
    assert!(k > 0);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    a.on_update(Update::Diag(DiagRead::Journal(q), read((100..103).map(|n| entry(n, "y")).collect())), at(1, t0));
    assert_eq!(view(&a).scroll, Scroll::At(k + 3), "what is on screen stays where it is");
    assert_eq!(view(&a).new_below, 3);
    key(&mut a, KeyCode::End, t0);
    assert_eq!((view(&a).scroll, view(&a).new_below), (Scroll::Tail, 0));
    // At the tail, new entries are not "new below".
    a.on_tick(at(2, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    a.on_update(Update::Diag(DiagRead::Journal(q), read(vec![entry(103, "z")])), at(2, t0));
    assert_eq!((view(&a).scroll, view(&a).new_below), (Scroll::Tail, 0));
    // The wheel and Home reach the oldest entry and no further.
    key(&mut a, KeyCode::Home, t0);
    let top = view(&a).visible().len() - view(&a).page.get();
    assert_eq!(view(&a).scroll, Scroll::At(top), "the oldest entry is the top row, not scrolled off");
    key(&mut a, KeyCode::PageUp, t0);
    assert_eq!(view(&a).scroll, Scroll::At(top));
}

#[test]
fn an_unreadable_system_journal_is_known_after_one_check() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    a.diag_activate(DiagItem::Preset(Preset::ErrorsThisBoot));
    let checks = a.take_requests().iter().filter(|r| **r == Request::Diag(DiagRead::JournalReadable)).count();
    assert_eq!(checks, 1);
    assert_eq!(view(&a).system_readable, None);
    a.on_update(Update::Diag(DiagRead::JournalReadable, DiagResult::JournalReadable(false)), t0);
    assert_eq!(view(&a).system_readable, Some(false));
    // Priority changes do not check again.
    key(&mut a, KeyCode::Char('p'), t0);
    assert!(!a.take_requests().contains(&Request::Diag(DiagRead::JournalReadable)));
}

#[test]
fn the_wheel_scrolls_the_view() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, (0..50).map(|n| entry(n, "x")).collect(), t0);
    assert!(a.journal_wheel(1));
    assert_eq!(view(&a).scroll, Scroll::At(3));
    assert!(a.journal_wheel(-1));
    assert_eq!(view(&a).scroll, Scroll::Tail);
}

fn with_keys(keys: &[(&str, &[&str])], t0: Instant) -> App {
    let mut cfg = crate::config::KeysConfig::default();
    for (action, chords) in keys {
        cfg.bindings.insert((*action).to_string(), chords.iter().map(|c| (*c).to_string()).collect());
    }
    let mut a = App::with_tab(Config::default(), Theme::default(), Keymap::from_config(&cfg).unwrap(), Tab::Diagnostics, t0);
    a.take_requests();
    a
}

#[test]
fn scrolling_follows_the_key_table() {
    let t0 = Instant::now();
    // The Dvorak navigation keys (on by default) scroll: t up, h down.
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, (0..100).map(|n| entry(n, "x")).collect(), t0);
    key(&mut a, KeyCode::Char('t'), t0);
    assert_eq!(view(&a).scroll, Scroll::At(1));
    key(&mut a, KeyCode::Char('h'), t0);
    assert_eq!(view(&a).scroll, Scroll::Tail);
    // A user's own keys for the actions do too, and the arrow they replaced does not.
    let mut a = with_keys(&[("up", &["k"]), ("down", &["j"])], t0);
    opened(&mut a, Preset::Live, (0..100).map(|n| entry(n, "x")).collect(), t0);
    key(&mut a, KeyCode::Char('k'), t0);
    assert_eq!(view(&a).scroll, Scroll::At(1));
    key(&mut a, KeyCode::Up, t0);
    assert_eq!(view(&a).scroll, Scroll::At(1), "the arrow is no longer bound");
    key(&mut a, KeyCode::Char('j'), t0);
    assert_eq!(view(&a).scroll, Scroll::Tail);
}

#[test]
fn the_views_letters_never_shadow_a_users_binding() {
    let t0 = Instant::now();
    // `p` is the priority key; a user who bound it to Refresh gets Refresh, not a re-read.
    let mut a = with_keys(&[("refresh", &["p"])], t0);
    let q = opened(&mut a, Preset::Live, vec![entry(1, "x")], t0);
    key(&mut a, KeyCode::Char('p'), t0);
    assert_eq!(view(&a).query.priority, q.priority, "the priority did not change");
    // Unbound, `p` is the view's own.
    let mut a = on_diag(t0);
    let q = opened(&mut a, Preset::Live, vec![entry(1, "x")], t0);
    key(&mut a, KeyCode::Char('p'), t0);
    assert_ne!(view(&a).query.priority, q.priority);
}

fn held_view(n: u32) -> JournalView {
    let mut j = JournalView::new(JournalQuery::new(Preset::Live, 500));
    j.push((0..n).map(|i| entry(i, "x")).collect());
    j
}

#[test]
fn a_paused_view_counts_only_the_entries_it_kept() {
    let mut j = held_view(10);
    j.scroll = Scroll::At(1);
    j.push((0..(MAX_ENTRIES as u32 + 500)).map(|i| entry(100 + i, "y")).collect());
    assert_eq!(j.entries.len(), MAX_ENTRIES);
    assert!(j.new_below <= MAX_ENTRIES, "{}", j.new_below);
    assert!(matches!(j.scroll, Scroll::At(k) if k <= MAX_ENTRIES), "{:?}", j.scroll);
}

#[test]
fn a_failed_read_restarts_when_follow_is_asked_again() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a")], t0);
    a.on_tick(at(1, t0));
    let q = journal_reads(&mut a).pop().unwrap();
    a.on_update(
        Update::Diag(DiagRead::Journal(q), DiagResult::Journal(Err(BackendError::Missing { tool: "journalctl".into() }))),
        at(1, t0),
    );
    assert!(matches!(view(&a).load, Load::Failed(_)) && !view(&a).follow);
    key(&mut a, KeyCode::Char('f'), at(2, t0));
    assert!(view(&a).follow && view(&a).load == Load::Reading);
    assert_eq!(journal_reads(&mut a).len(), 1, "one new read");
}

#[test]
fn esc_in_the_service_picker_goes_back_to_the_overview() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    a.diag_activate(DiagItem::Preset(Preset::ServiceLogs));
    assert!(view(&a).picker.is_some());
    key(&mut a, KeyCode::Esc, t0);
    assert_eq!(a.diag.view, View::Overview);
    // From `u` inside a journal that has a unit, Esc only closes the picker.
    let mut b = on_diag(t0);
    opened(&mut b, Preset::Live, vec![entry(1, "a")], t0);
    key(&mut b, KeyCode::Char('u'), t0);
    key(&mut b, KeyCode::Esc, t0);
    assert!(matches!(b.diag.view, View::Journal(_)) && view(&b).picker.is_none());
}

#[test]
fn a_tab_key_on_the_journal_view_queues_nothing() {
    let t0 = Instant::now();
    let mut a = on_diag(t0);
    opened(&mut a, Preset::Live, vec![entry(1, "a")], t0);
    a.take_requests();
    a.go_tab(Tab::Diagnostics, at(5, t0));
    assert!(a.take_requests().is_empty(), "the view reads for itself");
}
