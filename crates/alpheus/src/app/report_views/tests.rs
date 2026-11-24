//! Tests of the boot and bug report views' logic (`app/report_views.rs`): the reads they ask for, the
//! text held, scrolling, and when Enter or a click copies.

use super::*;
use crate::app::diagnostics::DiagItem;
use crate::app::keymap::Keymap;
use crate::app::worker::Update;
use crate::backends::BackendError;
use crate::config::Config;
use crate::diagnostics::query::{Preset, Scope};
use crate::theme::Theme;
use ratatui::crossterm::event::{KeyEventKind, KeyEventState};
use std::time::Instant;

fn on_diag(config: Config) -> App {
    let mut a = App::with_tab(config, Theme::default(), Keymap::default(), Tab::Diagnostics, Instant::now());
    a.take_requests();
    a
}

fn key(a: &mut App, code: KeyCode) {
    a.on_key(KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }, Instant::now());
}

#[test]
fn boot_view_reads_on_open() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Boot);
    assert_eq!(a.take_requests(), vec![Request::Diag(DiagRead::BootReport)], "one read when the view opens, none before");
    assert!(matches!(&a.diag.view, View::Boot(b) if b.read.is_none()), "reading");
    let blame =
        vec![Blame { duration_ms: 12_345, unit: "example-sync.service".into() }, Blame { duration_ms: 812, unit: "mnt-data.mount".into() }];
    let chain = vec!["graphical.target @18.1s".to_string(), "└─multi-user.target @18.1s".to_string()];
    a.on_update(
        Update::Diag(
            DiagRead::BootReport,
            DiagResult::BootReport { blame: Ok(blame.clone()), chain: Err(BackendError::Timeout { tool: "systemd-analyze".into() }) },
        ),
        Instant::now(),
    );
    let View::Boot(b) = &a.diag.view else { panic!() };
    let (got_blame, got_chain) = b.read.as_ref().unwrap();
    assert_eq!(got_blame, &Ok(blame));
    assert!(got_chain.is_err(), "an error shows as it is, in its own section");
    let _ = chain;
    key(&mut a, KeyCode::Esc);
    assert_eq!(a.diag.view, View::Overview);
}

#[test]
fn an_answer_for_a_view_that_is_gone_is_dropped() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Report);
    a.take_requests();
    key(&mut a, KeyCode::Esc);
    a.on_update(
        Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report("late".into())),
        Instant::now(),
    );
    assert_eq!(a.diag.view, View::Overview);
}

#[test]
fn the_report_is_shown_before_copy() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Report);
    assert_eq!(a.take_requests(), vec![Request::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 })]);
    // Enter before the text is there copies nothing.
    key(&mut a, KeyCode::Enter);
    assert!(a.take_requests().is_empty());
    let text = "alpheus diagnostics report, 2026-11-21 21:40 UTC\nalpheus 0.1.0\n".to_string();
    let shown = Instant::now();
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text.clone())), shown);
    assert!(a.take_requests().is_empty(), "showing it copies nothing");
    key_at(&mut a, KeyCode::Enter, shown + Duration::from_secs(1));
    let argv = vec![Config::default().diagnostics.copy[0].clone()];
    assert_eq!(a.take_requests(), vec![Request::Do(Op::Copy { argv, text })], "exactly the text that is on screen, and only on Enter");
    // Esc closes: nothing was copied by it.
    key(&mut a, KeyCode::Esc);
    assert_eq!(a.diag.view, View::Overview);
    assert!(a.take_requests().is_empty());
}

#[test]
fn the_compositor_name_comes_from_the_health_read() {
    let mut a = crate::app::testing::diag_app(false);
    a.take_requests();
    a.diag_activate(DiagItem::Report);
    assert_eq!(a.take_requests(), vec![Request::Diag(DiagRead::Report { compositor: "Mango".into(), restart_loop: 3 })]);
}

#[test]
fn no_copy_program_means_no_copy() {
    let mut config = Config::default();
    config.diagnostics.copy = vec![];
    let mut a = on_diag(config);
    a.diag_activate(DiagItem::Report);
    a.take_requests();
    a.on_update(
        Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report("text\n".into())),
        Instant::now(),
    );
    assert!(!a.report_can_copy());
    key(&mut a, KeyCode::Enter);
    a.report_copy_click();
    assert!(a.take_requests().is_empty());
}

#[test]
fn enter_after_the_delay_copies_nothing_when_there_is_no_copy_program() {
    let mut config = Config::default();
    config.diagnostics.copy = vec![];
    let mut a = on_diag(config);
    a.diag_activate(DiagItem::Report);
    a.take_requests();
    let t = Instant::now();
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report("text\n".into())), t);
    key_at(&mut a, KeyCode::Enter, t + Duration::from_millis(500));
    assert!(a.take_requests().is_empty(), "the delay has passed, but there is no program to copy with");
}

#[test]
fn the_boot_view_counts_a_line_for_each_failed_list_and_three_for_its_headings() {
    let unit = |name: &str| Blame { duration_ms: 1, unit: name.into() };
    let timeout = || BackendError::Timeout { tool: "systemd-analyze".into() };
    let view = |blame, chain| BootView { read: Some((blame, chain)), ..BootView::default() };
    assert_eq!(BootView::default().lines(), 0, "nothing before the read has come");
    assert_eq!(view(Ok(vec![unit("a.service"), unit("b.mount")]), Ok(vec!["graphical.target @1s".into()])).lines(), 2 + 1 + 3);
    assert_eq!(view(Err(timeout()), Ok(vec![])).lines(), 4, "one line for the error, none for the empty chain, three for the headings");
    assert_eq!(view(Ok(vec![]), Err(timeout())).lines(), 4);
}

#[test]
fn the_copy_answer_says_so() {
    let mut a = on_diag(Config::default());
    let op = Op::Copy { argv: vec!["x".into()], text: "t".into() };
    let t = Instant::now();
    a.on_update(Update::Done(op.clone(), Ok(())), t);
    assert!(a.toasts.iter().any(|t| t.text == "Copied to the clipboard" && !t.error));
    a.on_update(Update::Done(op, Err(BackendError::Missing { tool: "wl-copy".into() })), t);
    assert!(a.toasts.iter().any(|t| t.error && t.text.starts_with("Copy:")), "{:?}", a.toasts);
}

#[test]
fn the_report_scrolls_and_stops_at_its_ends() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Report);
    let text: String = (0..50).map(|n| format!("line {n}\n")).collect();
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text)), Instant::now());
    let scroll = |a: &App| match &a.diag.view {
        View::Report(r) => r.scroll,
        _ => panic!(),
    };
    key(&mut a, KeyCode::Up);
    assert_eq!(scroll(&a), 0);
    key(&mut a, KeyCode::PageDown);
    assert_eq!(scroll(&a), 9);
    key(&mut a, KeyCode::End);
    assert_eq!(scroll(&a), 40, "the last page");
    assert!(a.report_views_wheel(1));
    assert_eq!(scroll(&a), 37);
    key(&mut a, KeyCode::Home);
    assert_eq!(scroll(&a), 0);
}

#[test]
fn a_crash_opens_the_minute_before_it() {
    let mut a = crate::app::testing::diag_app(true);
    a.take_requests();
    a.diag_activate(DiagItem::Crash(0));
    let q = a
        .take_requests()
        .into_iter()
        .find_map(|r| match r {
            Request::Diag(DiagRead::Journal(q)) => Some(q),
            _ => None,
        })
        .expect("a journal read");
    let secs = 1_795_183_320u64;
    assert_eq!(q.since_until, Some((secs - 60, secs + 5)));
    assert_eq!((q.preset, q.unit), (Preset::ServiceLogs, None::<(Scope, String)>));
}

#[test]
fn the_report_scrolls_by_the_key_table() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Report);
    let text: String = (0..50).map(|n| format!("line {n}\n")).collect();
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text)), Instant::now());
    let scroll = |a: &App| match &a.diag.view {
        View::Report(r) => r.scroll,
        _ => panic!(),
    };
    key(&mut a, KeyCode::Down);
    assert_eq!(scroll(&a), 1);
    key(&mut a, KeyCode::Up);
    assert_eq!(scroll(&a), 0);
}

fn key_at(a: &mut App, code: KeyCode, now: Instant) {
    a.on_key(KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }, now);
}

#[test]
fn wrapped_rows_are_what_scrolls() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Report);
    // A 250-character line is seven rows at 40 columns; five short lines follow.
    let text = format!("{}\n{}", "x".repeat(250), "y\n".repeat(5));
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text)), Instant::now());
    let View::Report(r) = &a.diag.view else { panic!() };
    r.width.set(40);
    r.page.set(4);
    assert_eq!(r.lines(), 7 + 5);
    key(&mut a, KeyCode::End);
    let View::Report(r) = &a.diag.view else { panic!() };
    assert_eq!(r.scroll, 12 - 4, "the last page of wrapped rows");
    assert_eq!(wrap_rows("é".repeat(5).as_str(), 2), vec!["éé", "éé", "é"]);
    assert_eq!(wrap_rows("a\n\nb", 10), vec!["a", "", "b"], "an empty line is a row");
    assert_eq!(wrap_rows("日本語", 4), vec!["日本", "語"], "wide characters count two cells");
}

#[test]
fn enter_just_after_the_text_arrives_copies_nothing() {
    let mut a = on_diag(Config::default());
    a.diag_activate(DiagItem::Report);
    a.take_requests();
    let t = Instant::now();
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report("text\n".into())), t);
    // An Enter typed ahead, before the person could read the warning.
    key_at(&mut a, KeyCode::Enter, t + Duration::from_millis(100));
    assert!(a.take_requests().is_empty(), "too soon");
    key_at(&mut a, KeyCode::Enter, t + Duration::from_millis(500));
    assert!(matches!(a.take_requests().as_slice(), [Request::Do(Op::Copy { .. })]), "after the delay it copies");
}
