//! Tests of the boot and bug report views' drawing (`ui/report_views.rs`): the two views, the Copy
//! button and the wrapping of a long line.

use super::*;
use crate::app::diagnostics::DiagItem;
use crate::app::testing::diag_app;
use crate::app::worker::{DiagRead, DiagResult, Update};
use crate::backends::BackendError;
use crate::diagnostics::analyze::Blame;
use crate::ui::testing::{buffer_at, draw};
use std::time::Instant;

fn rows(a: &App, w: u16, h: u16) -> String {
    draw(a, w, h).0.join("\n")
}

#[test]
fn durations_read_like_systemds() {
    assert_eq!(duration(812), "812ms");
    assert_eq!(duration(12_345), "12.345s");
    assert_eq!(duration(62_345), "1min 2.345s");
}

#[test]
fn the_boot_view() {
    let mut a = diag_app(false);
    a.diag_activate(DiagItem::Boot);
    assert!(rows(&a, 87, 26).contains("Reading…"));
    let blame =
        vec![Blame { duration_ms: 12_345, unit: "example-sync.service".into() }, Blame { duration_ms: 812, unit: "mnt-data.mount".into() }];
    let chain = vec!["graphical.target @18.1s".to_string(), "\u{1b}[2J└─multi-user.target @18.1s".to_string()];
    a.on_update(Update::Diag(DiagRead::BootReport, DiagResult::BootReport { blame: Ok(blame), chain: Ok(chain) }), Instant::now());
    insta::assert_snapshot!("boot_view_87x26", rows(&a, 87, 26));
    let s = rows(&a, 87, 26);
    assert!(!s.contains('\u{1b}') && s.contains("└─multi-user.target"), "{s}");
    // An error shows as it is, in its section.
    a.on_update(
        Update::Diag(
            DiagRead::BootReport,
            DiagResult::BootReport { blame: Err(BackendError::Timeout { tool: "systemd-analyze".into() }), chain: Ok(vec![]) },
        ),
        Instant::now(),
    );
    let s = rows(&a, 87, 26);
    assert!(s.contains("✗") && s.contains("Critical chain"), "{s}");
}

#[test]
fn the_report_view() {
    let mut a = diag_app(false);
    a.diag_activate(DiagItem::Report);
    let s = rows(&a, 87, 26);
    assert!(s.contains("Read before copying: logs can name things") && s.contains("Building the report…"), "{s}");
    let (_, hits) = draw(&a, 87, 26);
    assert!(hits.find(Target::ReportCopy).is_none(), "no button before there is a text");
    let text =
        "alpheus diagnostics report, 2026-11-21 21:40 UTC\nalpheus 0.1.0 · systemd 260 (260.5)\nFailed units: none\n\u{1b}[2Jhostile\n";
    a.on_update(
        Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text.into())),
        Instant::now(),
    );
    insta::assert_snapshot!("report_view_87x26", rows(&a, 87, 26));
    let (_, hits) = draw(&a, 87, 26);
    assert!(hits.find(Target::ReportCopy).is_some());
    let buf = buffer_at(&a, 87, 26, Some(crate::ui::testing::T));
    for y in 0..26 {
        for x in 0..87 {
            assert!(!buf[(x, y)].symbol().chars().any(|c| c.is_control()), "a raw control reached ({x},{y})");
        }
    }
    // A click on the button asks for the copy.
    a.take_requests();
    let r = hits.find(Target::ReportCopy).unwrap();
    a.on_mouse(
        ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left),
            column: r.x,
            row: r.y,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        },
        &hits,
        Instant::now(),
    );
    assert!(matches!(a.take_requests().as_slice(), [crate::app::worker::Request::Do(crate::app::worker::Op::Copy { .. })]));
}

#[test]
fn without_a_copy_program_the_button_is_not_drawn() {
    let mut a = diag_app(false);
    a.config.diagnostics.copy.clear();
    a.diag_activate(DiagItem::Report);
    a.on_update(
        Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report("x\n".into())),
        Instant::now(),
    );
    let s = rows(&a, 87, 26);
    assert!(!s.contains("[ Copy ]") && s.contains("No copy program configured"), "{s}");
}

#[test]
fn a_long_line_wraps_so_everything_that_is_copied_is_visible() {
    let mut a = diag_app(false);
    a.diag_activate(DiagItem::Report);
    // 250 characters with markers at both ends and in the middle.
    let long = format!("§S§{}§M§{}§E§", "a".repeat(121), "b".repeat(120));
    assert_eq!(long.chars().count(), 250);
    let text = format!("head\n {long}\ntail\n");
    a.on_update(Update::Diag(DiagRead::Report { compositor: String::new(), restart_loop: 3 }, DiagResult::Report(text)), Instant::now());
    let s = rows(&a, 87, 26);
    for marker in ["§S§", "§M§", "§E§", "tail"] {
        assert!(s.contains(marker), "{marker} is not on screen:\n{s}");
    }
    assert!(!s.contains('…'), "nothing is cut with an ellipsis:\n{s}");
}
