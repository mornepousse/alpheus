use super::*;
use crate::app::action::Tab;
use crate::app::diagnostics::{DiagItem, View};
use crate::app::testing::{diag_app, ready_app};
use crate::app::worker::Update;
use crate::app::{Item, Overlay};
use crate::backends::UnitAction;
use crate::diagnostics::query::Scope;
use crate::diagnostics::units::FailedUnit;
use crate::ui::testing::{buffer_at, draw};
use std::time::Instant;

fn rows(a: &App, w: u16, h: u16) -> String {
    draw(a, w, h).0.join("\n")
}

#[test]
fn diag_overview_all_good() {
    let a = diag_app(false);
    insta::assert_snapshot!("diag_overview_all_good_87x26", rows(&a, 87, 26));
    insta::assert_snapshot!("diag_overview_all_good_80x24", rows(&a, 80, 24));
    let all = rows(&a, 87, 26);
    assert!(all.contains("✓ all good") && all.contains("no failed unit") && all.contains("no crash in 7 days"), "{all}");
}

#[test]
fn diag_overview_problems() {
    let a = diag_app(true);
    insta::assert_snapshot!("diag_overview_problems_87x26", rows(&a, 87, 26));
    insta::assert_snapshot!("diag_overview_problems_80x24", rows(&a, 80, 24));
    let tall = rows(&a, 120, 60);
    for expected in [
        "✗ 2 problems",
        "1 failed user unit",
        "systemd --user has WAYLAND_DISPLAY=wayland-0 (this session: wayland-1)",
        "fix: systemctl --user import-environment WAYLAND_DISPLAY",
        "Failed units (3)",
        "mnt-data.mount",
        "system",
        "example-app.service",
        "user",
        "7 restarts, activating",
        "Nov 20 14:02",
        "SIGSEGV",
        "core present",
        "Errors this boot",
        "Logs of a service…",
        "Bug report…",
    ] {
        // The rail upper-cases its headings.
        assert!(tall.to_lowercase().contains(&expected.to_lowercase()), "{expected:?} missing:\n{tall}");
    }
}

#[test]
fn diag_overview_hostile_description() {
    let mut a = diag_app(false);
    let t = Instant::now();
    let hostile = FailedUnit {
        unit: "evil\u{1b}[2J.service".into(),
        load: "loaded".into(),
        active: "failed".into(),
        sub: "failed".into(),
        description: "\u{1b}]0;owned\u{7}tidy \u{202e}live\r\nreorder\u{85}d".into(),
    };
    a.on_update(Update::Failed(Scope::System, Ok(vec![hostile])), t);
    insta::assert_snapshot!("diag_overview_hostile_description", rows(&a, 87, 26));
    let buf = buffer_at(&a, 87, 26, Some(crate::ui::testing::T));
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
fn diag_rail_shows_eight_tabs() {
    let a = diag_app(true);
    let (rows, hits) = draw(&a, 87, 26);
    let text = rows.join("\n");
    for title in ["Home", "Sound", "Network", "BT", "Power", "Displays", "Theme", "Diag"] {
        assert!(text.contains(title), "{title} missing:\n{text}");
    }
    for t in Tab::ALL {
        assert!(hits.find(Target::Tab(t)).is_some(), "{t:?} has no entry on the rail");
    }
    let diag = hits.find(Target::Tab(Tab::Diagnostics)).unwrap();
    assert!(diag.bottom() < 25, "the eighth entry stays above the status line");
}

#[test]
fn every_focusable_row_is_drawn_and_clickable() {
    let a = diag_app(true);
    // Tall enough to show everything: each item has one region, in order.
    let (_, hits) = draw(&a, 120, 70);
    let items = a.items();
    for (i, item) in items.iter().enumerate() {
        assert!(hits.find(Target::Item(i)).is_some(), "{item:?} (item {i}) has no clickable row");
    }
    assert_eq!(items.len(), 2 + 3 + 1 + 1 + 1 + 8 + 1, "health ✗ ×2, failed, loop, crash, boot, presets, report");
}

#[test]
fn the_cursor_scrolls_into_view() {
    let mut a = diag_app(true);
    // The Bug report button is the last item: at 80×24 it only shows once the view follows it.
    a.focus = a.items().len() - 1;
    let all = rows(&a, 80, 24);
    assert!(all.contains("Bug report…"), "{all}");
    a.focus = 0;
    assert!(rows(&a, 80, 24).contains("Session health"));
}

#[test]
fn a_missing_tool_says_so_and_has_no_selectable_row() {
    use crate::app::worker::{DiagRead, DiagResult};
    use crate::backends::BackendError;
    let mut a = diag_app(true);
    let t = Instant::now();
    a.on_update(Update::Diag(DiagRead::Crashes, DiagResult::Crashes(Err(BackendError::Missing { tool: "coredumpctl".into() }))), t);
    let all = rows(&a, 120, 60);
    assert!(all.contains("coredumpctl not found"), "{all}");
    assert!(!all.contains("SIGSEGV"), "the section's rows are gone with its tool");
    assert!(a.items().iter().all(|i| !matches!(i, Item::Diag(DiagItem::Crash(_)))));
}

#[test]
fn a_failed_read_dims_the_kept_list_under_its_error() {
    use crate::backends::BackendError;
    let mut a = diag_app(true);
    let t = Instant::now();
    a.on_update(Update::Failed(Scope::System, Err(BackendError::Timeout { tool: "systemctl".into() })), t);
    let tall = rows(&a, 120, 60);
    assert!(tall.contains("✗ system: systemctl is not responding"), "{tall}");
    assert!(tall.contains("mnt-data.mount"), "the last list stays");
    let buf = buffer_at(&a, 120, 60, Some(crate::ui::testing::T));
    let (rows_, _) = draw(&a, 120, 60);
    let y = rows_.iter().position(|r| r.contains("mnt-data.mount")).unwrap() as u16;
    let x = rows_[usize::from(y)].find("mnt-data.mount").map(|b| rows_[usize::from(y)][..b].chars().count()).unwrap() as u16;
    assert_eq!(buf[(x, y)].fg, a.theme.muted, "dimmed while its read is failing");
}

#[test]
fn the_unit_confirmation_names_the_unit_and_starts_on_cancel() {
    let mut a = diag_app(true);
    a.overlay =
        Overlay::UnitConfirm { scope: Scope::System, unit: "mnt-data.mount".into(), action: UnitAction::Restart, yes_focused: false };
    let (rows_, hits) = draw(&a, 87, 26);
    let all = rows_.join("\n");
    assert!(all.contains("Restart mnt-data.mount (system)?"), "{all}");
    assert!(all.contains("your password in this window."), "the question wraps, whole: {all}");
    assert!(hits.find(Target::ConfirmYes).is_some() && hits.find(Target::ConfirmNo).is_some());
    insta::assert_snapshot!("diag_unit_confirm_system", all);
    a.overlay =
        Overlay::UnitConfirm { scope: Scope::User, unit: "example-app.service".into(), action: UnitAction::ResetFailed, yes_focused: true };
    let all = rows(&a, 87, 26);
    assert!(all.contains("Clear the failed state of example-app.service (user)?"), "{all}");
    assert!(!all.contains("password"), "a user unit needs none");
    // A tiny window never panics.
    for (w, h) in [(30, 8), (40, 10)] {
        let _ = rows(&a, w, h);
    }
}

#[test]
fn the_hints_follow_the_row_and_the_config() {
    let mut a = diag_app(true);
    let pairs = |a: &App| -> Vec<(String, &'static str)> { crate::ui::shortcut_pairs(a) };
    let has = |a: &App, what: &str| pairs(a).iter().any(|(_, w)| *w == what);
    // On a health row (the first item): no unit keys.
    a.focus = 0;
    assert!(!has(&a, "Restart") && has(&a, "Refresh"));
    // On a system failed unit: the keys show.
    a.focus = a.items().iter().position(|i| matches!(i, Item::Diag(DiagItem::Failed(Scope::System, _)))).unwrap();
    assert!(has(&a, "Restart") && has(&a, "Clear"));
    // system_actions off: hidden on a system unit, shown on a user one.
    a.config.diagnostics.system_actions = false;
    assert!(!has(&a, "Restart") && !has(&a, "Clear"));
    a.focus = a.items().iter().position(|i| matches!(i, Item::Diag(DiagItem::Failed(Scope::User, _)))).unwrap();
    assert!(has(&a, "Restart"));
}

#[test]
fn a_view_covers_the_overview_with_its_way_back() {
    let mut a = ready_app();
    a.go_tab(Tab::Diagnostics, Instant::now());
    a.diag.view = View::Report(Default::default());
    let all = rows(&a, 87, 26);
    assert!(all.contains("Bug report") && all.contains("Esc back"), "{all}");
}

#[test]
fn the_rail_heading_keeps_one_space_between_its_rule_and_the_summary() {
    for (w, h) in [(87, 26), (120, 40)] {
        let all = rows(&diag_app(false), w, h);
        let line = all.lines().find(|l| l.contains("SESSION HEALTH")).unwrap();
        assert!(line.contains("─ ✓ all good"), "{line}");
        assert!(!line.contains("─  ✓ all good"), "two spaces: {line}");
    }
}

/// Final review I1: two long unit names sharing their first 50 characters stay apart in the
/// confirm dialog and in the failed-unit rows (the end is what differs).
fn long_unit(tail: &str) -> String {
    format!("{}-{tail}.service", "very-long-shared-prefix-of-a-generated-unit-name-0123456789".repeat(2))
}

#[test]
fn long_unit_names_stay_apart_in_the_confirm_dialog() {
    let mut a = diag_app(false);
    let mut shown = Vec::new();
    for tail in ["alpha", "bravo"] {
        a.overlay = Overlay::UnitConfirm { scope: Scope::System, unit: long_unit(tail), action: UnitAction::Restart, yes_focused: false };
        let all = rows(&a, 87, 26);
        assert!(all.contains(&format!("{tail}.service")), "the end of the name is drawn: {all}");
        assert!(all.contains('…'), "{all}");
        shown.push(all);
    }
    assert_ne!(shown[0], shown[1]);
    // A narrow dialog still shows the end.
    a.overlay = Overlay::UnitConfirm { scope: Scope::User, unit: long_unit("alpha"), action: UnitAction::Restart, yes_focused: false };
    let all = rows(&a, 40, 12);
    assert!(all.contains("alpha.service"), "{all}");
}

#[test]
fn long_unit_names_stay_apart_in_the_rows() {
    let mut a = diag_app(false);
    let t = Instant::now();
    let mk = |tail: &str| FailedUnit {
        unit: long_unit(tail),
        load: "loaded".into(),
        active: "failed".into(),
        sub: "failed".into(),
        description: "d".into(),
    };
    a.on_update(Update::Failed(Scope::System, Ok(vec![mk("alpha"), mk("bravo")])), t);
    let all = rows(&a, 120, 60);
    assert!(all.contains("alpha.service") && all.contains("bravo.service"), "{all}");
}

#[test]
fn the_diagnostics_tab_at_80_by_24_matches_its_snapshot() {
    let mut a = ready_app();
    a.go_tab(Tab::Diagnostics, Instant::now());
    insta::assert_snapshot!("diagnostics_tab", draw(&a, 80, 24).0.join("\n"));
}
