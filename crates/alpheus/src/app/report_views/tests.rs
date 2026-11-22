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
    a.on_update(
        Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text.clone())),
        Instant::now(),
    );
    assert!(a.take_requests().is_empty(), "showing it copies nothing");
    key(&mut a, KeyCode::Enter);
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
