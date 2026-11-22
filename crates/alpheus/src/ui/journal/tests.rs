//! Tests of the journal view's drawing (`ui/journal.rs`): the header, the entries and the picker as
//! snapshots, the times, and the keys the status line offers.

use crate::app::diagnostics::DiagItem;
use crate::app::testing::diag_app;
use crate::app::worker::{DiagRead, DiagResult, Request, Update};
use crate::app::{App, Item};
use crate::backends::JournalRead;
use crate::diagnostics::journal::Entry;
use crate::diagnostics::query::{Preset, Scope};
use crate::ui::testing::{T, buffer_at, draw};
use std::time::Instant;

/// 2023-11-15 01:00:00 UTC: "today" for the tests' clock (15 November).
const BASE: u64 = 1_700_010_000_000_000;

fn entry(n: u64, priority: u8, ident: &str, message: &str) -> Entry {
    Entry {
        cursor: format!("s=0;i={n}"),
        time_us: BASE + n * 1_000_000,
        priority,
        ident: ident.into(),
        unit: String::new(),
        pid: Some(100 + n as u32),
        message: message.into(),
    }
}

fn rows(a: &App, w: u16, h: u16) -> String {
    draw(a, w, h).0.join("\n")
}

fn open(preset: Preset, entries: Vec<Entry>) -> App {
    let mut a = diag_app(false);
    let t = Instant::now();
    a.diag_activate(DiagItem::Preset(preset));
    let q = a
        .take_requests()
        .into_iter()
        .find_map(|r| match r {
            Request::Diag(DiagRead::Journal(q)) => Some(q),
            _ => None,
        })
        .expect("a read");
    a.on_update(Update::Diag(DiagRead::Journal(q), DiagResult::Journal(Ok(JournalRead { entries, capped: false, timed_out: false }))), t);
    a
}

fn sample() -> Vec<Entry> {
    vec![
        entry(1, 7, "kernel", "usb 3-2: new high-speed USB device number 4"),
        entry(2, 6, "example-app", "started"),
        entry(3, 5, "example-sync", "nothing to sync"),
        entry(4, 4, "NetworkManager", "device (wlan0): link is slow"),
        entry(5, 3, "example-app", "failed to open /mnt/data"),
        entry(6, 0, "kernel", "Kernel panic - not syncing"),
    ]
}

#[test]
fn journal_view_priorities() {
    let a = open(Preset::ErrorsThisBoot, sample());
    insta::assert_snapshot!("journal_view_priorities_87x26", rows(&a, 87, 26));
    insta::assert_snapshot!("journal_view_priorities_80x24", rows(&a, 80, 24));
    // Each priority has its colour: emerg and err the error role, warning warn, info dim.
    let buf = buffer_at(&a, 120, 40, Some(T));
    let th = &a.theme;
    let find = |needle: &str| -> ratatui::style::Color {
        for y in 0..40 {
            let line: String = (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect();
            if let Some(x) = line.chars().position(|_| false).or(line.find(needle).map(|b| line[..b].chars().count())) {
                return buf[(x as u16, y)].fg;
            }
        }
        panic!("{needle} not drawn");
    };
    assert_ne!(find("Kernel panic"), find("started"), "error and info differ");
    assert_eq!(find("Kernel panic"), find("failed to open"), "emerg and err share the error role");
    assert_ne!(find("link is slow"), find("nothing to sync"), "warning is not the plain foreground");
    let _ = th;
}

#[test]
fn journal_view_hostile_message() {
    let hostile = Entry {
        cursor: "s=0;i=1".into(),
        time_us: BASE,
        priority: 3,
        // What the parser cleans is cleaned again: the cells never see raw text, whatever got in.
        ident: "evil\u{1b}[2J".into(),
        unit: String::new(),
        pid: None,
        message: "\u{1b}]0;owned\u{7}tidy \u{202e}live\r\nreorder\u{85}d".into(),
    };
    let a = open(Preset::ErrorsThisBoot, vec![hostile]);
    insta::assert_snapshot!("journal_view_hostile_message", rows(&a, 87, 26));
    let buf = buffer_at(&a, 87, 26, Some(T));
    for y in 0..26 {
        for x in 0..87 {
            let sym = buf[(x, y)].symbol();
            assert!(
                !sym.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')),
                "a raw control or bidi character reached the cell ({x},{y}): {sym:?}"
            );
        }
    }
}

#[test]
fn journal_picker() {
    let mut a = open(Preset::ErrorsThisBoot, sample());
    let t = Instant::now();
    a.on_key_action(crate::app::action::Action::Search); // not the picker's key: nothing
    a.take_requests();
    // Opened by `u`, filled by the worker.
    let press = |a: &mut App, c: char| {
        a.on_key(
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char(c),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
            t,
        )
    };
    press(&mut a, 'u');
    a.on_update(
        Update::Diag(
            DiagRead::ServiceUnits,
            DiagResult::ServiceUnits(Ok(vec![
                (Scope::System, "NetworkManager.service".into()),
                (Scope::System, "example-sync.service".into()),
                (Scope::User, "example-app.service".into()),
            ])),
        ),
        t,
    );
    insta::assert_snapshot!("journal_picker", rows(&a, 87, 26));
    press(&mut a, 'e');
    press(&mut a, 'x');
    let filtered = rows(&a, 87, 26);
    assert!(filtered.contains("example-sync.service") && !filtered.contains("NetworkManager.service"), "{filtered}");
    let _ = Item::Profile;
}

#[test]
fn the_header_notices_and_the_footer() {
    let mut a = open(Preset::Live, (1..=40).map(|n| entry(n, 6, "example-app", &format!("line {n}"))).collect());
    let t = Instant::now();
    let all = rows(&a, 87, 26);
    assert!(all.contains("Journal · Live journal") && all.contains("follow: on") && all.contains("/ filter"), "{all}");
    assert!(!all.contains("your own messages only"), "unknown until checked");
    a.on_update(Update::Diag(DiagRead::JournalReadable, DiagResult::JournalReadable(false)), t);
    assert!(rows(&a, 87, 26).contains("your own messages only (not in systemd-journal, adm or wheel)"));
    // Paused, with entries arriving: the footer counts them.
    a.on_key(
        ratatui::crossterm::event::KeyEvent::new(ratatui::crossterm::event::KeyCode::PageUp, ratatui::crossterm::event::KeyModifiers::NONE),
        t,
    );
    a.on_tick(t + std::time::Duration::from_secs(1));
    let q = a
        .take_requests()
        .into_iter()
        .find_map(|r| match r {
            Request::Diag(DiagRead::Journal(q)) => Some(q),
            _ => None,
        })
        .unwrap();
    let burst = JournalRead { entries: vec![entry(41, 6, "example-app", "line 41")], capped: true, timed_out: false };
    a.on_update(Update::Diag(DiagRead::Journal(q), DiagResult::Journal(Ok(burst))), t + std::time::Duration::from_secs(1));
    let s = rows(&a, 87, 26);
    assert!(s.contains("▼ 1 new"), "{s}");
    assert!(s.contains("more lines are waiting"), "{s}");
    assert!(!s.contains("lines skipped"), "nothing is skipped: the read goes on from its last cursor");
}

#[test]
fn an_empty_and_a_filtered_view_say_so() {
    let mut a = open(Preset::ErrorsThisBoot, vec![]);
    assert!(rows(&a, 87, 26).contains("No entries"));
    a = open(Preset::ErrorsThisBoot, sample());
    let t = Instant::now();
    for c in "/zzz".chars() {
        a.on_key(
            ratatui::crossterm::event::KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char(c),
                ratatui::crossterm::event::KeyModifiers::NONE,
            ),
            t,
        );
    }
    let s = rows(&a, 87, 26);
    assert!(s.contains("Nothing matches the filter") && s.contains("/zzz▌"), "{s}");
}

#[test]
fn an_older_entry_shows_its_date() {
    let mut e = entry(1, 6, "example-app", "yesterday");
    e.time_us = BASE - 86_400 * 1_000_000;
    let a = open(Preset::ErrorsThisBoot, vec![e, entry(2, 6, "example-app", "today")]);
    let s = rows(&a, 87, 26);
    assert!(s.contains("Nov 14 01:00:00 example-app[101]: yesterday"), "{s}");
    assert!(s.contains("01:00:02 example-app[102]: today") && !s.contains("Nov 15"), "{s}");
}

#[test]
fn an_entry_shows_its_date_unless_it_is_from_today_by_both_day_and_month() {
    use crate::ui::journal::entry_time;
    use crate::ui::testing::T; // Sunday 15 November
    let (nov_15, oct_15) = (1_794_751_331_000_000, 1_792_072_931_000_000);
    assert_eq!(entry_time(nov_15, Some(T)), "14:02:11", "today: the clock alone");
    assert_eq!(entry_time(oct_15, Some(T)), "Oct 15 14:02:11", "the same day of the month, a month ago");
    assert_eq!(entry_time(nov_15, None), "Nov 15 14:02:11", "no local time to compare with: always the date");
}

#[test]
fn the_keycap_row_follows_the_journal_view() {
    use crate::ui::shortcut_pairs;
    let mut a = open(Preset::Live, sample());
    let keys = |a: &App| shortcut_pairs(a).into_iter().map(|(k, w)| format!("{k} {w}")).collect::<Vec<_>>();
    let k = keys(&a);
    for want in ["/ Filter", "u Unit", "p Level", "f Follow", "PgUp/Dn/End Scroll"] {
        assert!(k.contains(&want.to_string()), "{want} missing in {k:?}");
    }
    assert!(!k.iter().any(|p| p.ends_with("Refresh")), "the overview's keys are not offered here");
    let t = Instant::now();
    let press = |a: &mut App, c| a.on_key(ratatui::crossterm::event::KeyEvent::new(c, ratatui::crossterm::event::KeyModifiers::NONE), t);
    press(&mut a, ratatui::crossterm::event::KeyCode::Char('/'));
    let k = keys(&a);
    assert!(k.contains(&"Enter Keep".to_string()) && k.contains(&"Esc Clear".to_string()), "{k:?}");
}

#[test]
fn a_long_unit_name_is_cut_in_the_middle_in_the_header() {
    let mut a = open(Preset::Live, sample());
    let unit = format!("{}-tail.service", "head-of-a-very-long-unit-name".repeat(4));
    if let crate::app::diagnostics::View::Journal(j) = &mut a.diag.view {
        j.query.unit = Some((Scope::System, unit));
    }
    let all = rows(&a, 87, 26);
    assert!(all.contains("unit: head-of-") && all.contains("tail.service"), "{all}");
}

#[test]
fn the_journal_hints_fit_the_rail_layout_at_87_columns() {
    let a = open(Preset::Live, sample());
    let last = draw(&a, 87, 26).0.pop().unwrap();
    for hint in ["Filter", "Unit", "Level", "Follow", "Scroll"] {
        assert!(last.contains(hint), "{hint} dropped from the bar at 87 columns: {last:?}");
    }
}
