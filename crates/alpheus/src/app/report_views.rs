//! The boot report and the bug report: two read-only views of the Diag tab.
//!
//! The boot report shows what `systemd-analyze` says. The bug report is a redacted text, built once
//! on the worker, shown in full and held: what is copied is exactly the text on screen. It is copied
//! only on Enter or a click on `[ Copy ]` (Esc closes, nothing is copied).
//!
//! Never:
//! - run a tool or build the text: the worker does, and this file keeps the answer;
//! - copy on an Enter typed ahead of the text: Enter is ignored for `COPY_GRACE` after the text
//!   appears;
//! - copy when `[diagnostics] copy` is empty (`report_can_copy`);
//! - draw anything: `ui/report_views.rs` draws the two views from this state.
//!
//! See: docs/specs/2026-11-21-alpheus-diagnostics-design.md (§ 4, the boot report; § 5, the bug
//! report), docs/features/control-center.md.

use super::action::Action;
use super::diagnostics::View;
use super::worker::{DiagRead, DiagResult, Op, Request};
use super::{App, Tab};
use crate::backends::BResult;
use crate::diagnostics::analyze::Blame;
use alpheus_common::text::clean_line;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Span;
use std::cell::Cell;
use std::time::{Duration, Instant};

/// Enter on Copy is ignored for this long after the text appears: an Enter typed ahead must not
/// copy a report nobody has read yet.
const COPY_GRACE: Duration = Duration::from_millis(400);

/// Columns, until the view has been drawn once and knows its width.
const DEFAULT_WIDTH: usize = 80;

/// A page, until the view has been drawn once and knows its height.
const DEFAULT_PAGE: usize = 10;

/// The slowest units and the critical chain, each as the worker read them.
pub type BootRead = (BResult<Vec<Blame>>, BResult<Vec<String>>);

/// The boot report: what the worker read, and where the view is scrolled to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BootView {
    /// None while the worker reads.
    pub read: Option<BootRead>,
    pub scroll: usize,
    pub page: Cell<usize>,
}

/// The bug report: its text once built, when it arrived, and where the view is scrolled to.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportView {
    /// None while it is built.
    pub text: Option<String>,
    /// When the text arrived (Enter waits `COPY_GRACE` after it).
    pub shown_at: Option<Instant>,
    /// Rows, counted in wrapped rows.
    pub scroll: usize,
    pub page: Cell<usize>,
    /// Columns at the last draw: the width lines wrap at.
    pub width: Cell<usize>,
}

/// One text line as the rows it takes at `width` columns: cleaned, then cut where a character
/// would pass the edge (a wide character counts its cells). Every character is on some row, so
/// what is copied is all shown. An empty line is one empty row.
pub fn wrap_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = clean_line(line, usize::MAX);
        let (mut row, mut used) = (String::new(), 0);
        for c in line.chars() {
            let w = Span::raw(&*c.encode_utf8(&mut [0u8; 4])).width();
            if used + w > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(c);
            used += w;
        }
        rows.push(row);
    }
    rows
}

impl BootView {
    /// The lines the view takes: both lists (an error counts as one line each) and three more for
    /// the headings; none before the read has come.
    pub fn lines(&self) -> usize {
        let Some((blame, chain)) = &self.read else { return 0 };
        let blame_lines = match blame {
            Ok(units) => units.len(),
            Err(_) => 1,
        };
        let chain_lines = match chain {
            Ok(lines) => lines.len(),
            Err(_) => 1,
        };
        blame_lines + chain_lines + 3
    }
}

impl ReportView {
    /// The rows the text takes at the width of the last draw; none before the text has come.
    pub fn lines(&self) -> usize {
        match &self.text {
            Some(text) => wrap_rows(text, self.width.get()).len(),
            None => 0,
        }
    }
}

/// Moves a scroll position by `by` rows, never past the last full page of `lines` rows.
fn step(scroll: &mut usize, by: isize, lines: usize, page: usize) {
    let max = lines.saturating_sub(page.max(1));
    *scroll = scroll.saturating_add_signed(by).min(max);
}

impl App {
    /// Enter on the Boot row: the view opens at once and the read goes to the worker.
    pub(crate) fn boot_open(&mut self) {
        self.requests.push(Request::Diag(DiagRead::BootReport));
        self.open_diag_view(View::Boot(BootView { page: Cell::new(DEFAULT_PAGE), ..BootView::default() }));
    }

    /// Enter on "Bug report…": the text is built on the worker.
    pub(crate) fn report_open(&mut self) {
        let compositor = match self.diag.health.as_ref().and_then(|h| h.compositor.as_ref()) {
            Some(c) => c.name.to_string(),
            None => String::new(),
        };
        let restart_loop = self.config.diagnostics.restart_loop;
        self.requests.push(Request::Diag(DiagRead::Report { compositor, restart_loop }));
        self.open_diag_view(View::Report(ReportView {
            page: Cell::new(DEFAULT_PAGE),
            width: Cell::new(DEFAULT_WIDTH),
            ..ReportView::default()
        }));
    }

    /// Whether the report can be copied at all (`copy = []` turns it off).
    pub fn report_can_copy(&self) -> bool {
        !self.config.diagnostics.copy.is_empty()
    }

    /// The answers of the boot and report reads; anything else is handed back.
    pub(crate) fn report_views_try(&mut self, result: DiagResult, now: Instant) -> Option<DiagResult> {
        match (&mut self.diag.view, result) {
            (View::Boot(b), DiagResult::BootReport { blame, chain }) => b.read = Some((blame, chain)),
            (View::Report(r), DiagResult::Report(text)) => {
                r.text = Some(text);
                r.shown_at = Some(now);
            }
            // An answer for a view that is not (or no longer) open is dropped.
            (_, DiagResult::BootReport { .. } | DiagResult::Report(_)) => {}
            (_, other) => return Some(other),
        }
        None
    }

    /// Scrolling in the two views, and Enter (copy) in the report. False for anything else (Esc goes back).
    pub(crate) fn report_views_key(&mut self, e: KeyEvent, now: Instant) -> bool {
        if self.tab != Tab::Diagnostics || e.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        let can_copy = self.report_can_copy();
        let argv = self.config.diagnostics.copy.clone();
        // Scrolling goes through the key table, so the Dvorak keys and a user's own keys scroll.
        let action = self.keymap.lookup(&e);
        let by = |page: usize| match action {
            Some(Action::Up) => Some(-1),
            Some(Action::Down) => Some(1),
            Some(Action::PageUp) => Some(-(page.max(2) as isize - 1)),
            Some(Action::PageDown) => Some(page.max(2) as isize - 1),
            Some(Action::Home) => Some(isize::MIN / 2),
            Some(Action::End) => Some(isize::MAX / 2),
            _ => None,
        };
        match &mut self.diag.view {
            View::Boot(b) => {
                let Some(d) = by(b.page.get()) else { return false };
                let (n, page) = (b.lines(), b.page.get());
                step(&mut b.scroll, d, n, page);
                true
            }
            View::Report(r) => {
                if e.code == KeyCode::Enter {
                    // Nothing to copy until the text is there (and has been on screen a moment),
                    // and nothing at all without a program.
                    let read = match r.shown_at {
                        Some(shown_at) => now.saturating_duration_since(shown_at) >= COPY_GRACE,
                        None => false,
                    };
                    if can_copy
                        && read
                        && let Some(text) = r.text.clone()
                    {
                        self.send_op(Op::Copy { argv, text });
                    }
                    return true;
                }
                let Some(d) = by(r.page.get()) else { return false };
                let (n, page) = (r.lines(), r.page.get());
                step(&mut r.scroll, d, n, page);
                true
            }
            _ => false,
        }
    }

    /// The wheel over the boot or report view; true when it was theirs.
    pub(crate) fn report_views_wheel(&mut self, dir: i32) -> bool {
        let by = if dir > 0 { -3 } else { 3 };
        match &mut self.diag.view {
            View::Boot(b) => {
                let (n, page) = (b.lines(), b.page.get());
                step(&mut b.scroll, by, n, page);
            }
            View::Report(r) => {
                let (n, page) = (r.lines(), r.page.get());
                step(&mut r.scroll, by, n, page);
            }
            _ => return false,
        }
        true
    }

    /// A click on `[ Copy ]`. Deliberately not gated by `COPY_GRACE`: the grace only guards an
    /// Enter typed ahead of the text, and a click is a pointing act at a button that is on screen
    /// (the button is drawn only once the text is there).
    pub(crate) fn report_copy_click(&mut self) {
        let argv = self.config.diagnostics.copy.clone();
        if argv.is_empty() {
            return;
        }
        if let View::Report(r) = &self.diag.view
            && let Some(text) = r.text.clone()
        {
            self.send_op(Op::Copy { argv, text });
        }
    }
}

#[cfg(test)]
mod tests;
