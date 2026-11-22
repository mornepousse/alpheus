//! The journal view of the Diag tab: what was asked of the journal, the entries held (at most
//! 2,000), the local text filter, the unit picker and the follow. Nothing here runs a tool: a
//! read is a `Request::Diag(DiagRead::Journal(..))` for the worker, one at a time, and the view
//! never asks again while one is in flight. Leaving the view (or the tab) ends the follow: there
//! is no child that outlives it, only a bounded read each second while the view is open.

use super::diagnostics::View;
use super::worker::{DiagRead, DiagResult, Request};
use super::{App, Tab};
use crate::backends::{BResult, JournalRead};
use crate::diagnostics::journal::Entry;
use crate::diagnostics::query::{JournalQuery, Preset, Scope};
use crate::fuzzy;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Entries held; the oldest are dropped first.
pub const MAX_ENTRIES: usize = 2000;
/// A follow read is asked this long after the previous one ended.
pub const POLL: Duration = Duration::from_secs(1);
/// How far the wheel moves, in entries.
const WHEEL: usize = 3;
/// A page, until the view has been drawn once and knows its height.
const DEFAULT_PAGE: usize = 10;

/// Where the entries are shown from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// The newest entry is at the bottom, and stays there as entries come.
    Tail,
    /// Paused: this many of the shown entries lie below the screen.
    At(usize),
}

/// How the last read ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    /// Nothing asked yet (the picker is open and no unit chosen).
    Idle,
    /// The first read of this query is out.
    Reading,
    Ready,
    Failed(String),
}

/// One entry held, with its text folded once for the local filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub entry: Entry,
    haystack: String,
}

impl Held {
    fn new(entry: Entry) -> Held {
        let haystack = fuzzy::fold_str(&format!("{} {} {}", entry.message, entry.ident, entry.unit));
        Held { entry, haystack }
    }
}

/// The unit picker: the loaded services of both managers, filtered as one types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// None until the list has been read.
    pub units: Option<Vec<(Scope, String)>>,
    pub error: Option<String>,
    pub query: String,
    pub selected: usize,
}

impl Picker {
    fn new() -> Picker {
        Picker { units: None, error: None, query: String::new(), selected: 0 }
    }

    /// The units matching the query, best first.
    pub fn matches(&self) -> Vec<&(Scope, String)> {
        let Some(units) = &self.units else { return Vec::new() };
        let labels: Vec<&str> = units.iter().map(|(_, u)| u.as_str()).collect();
        fuzzy::rank(&self.query, &labels).into_iter().map(|i| &units[i]).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalView {
    /// What the person asked for; never carries a cursor (a follow read adds one to a copy).
    pub query: JournalQuery,
    pub entries: VecDeque<Held>,
    pub follow: bool,
    pub filter: String,
    pub filter_editing: bool,
    pub picker: Option<Picker>,
    pub scroll: Scroll,
    /// Entries that came while paused.
    pub new_below: usize,
    pub load: Load,
    /// The read that is out, if any: one at a time, and an answer to anything else is stale.
    pub pending: Option<JournalQuery>,
    /// When the last read ended (a follow read is due `POLL` after).
    pub last_poll: Option<Instant>,
    /// None until the once-per-view check answers.
    pub system_readable: Option<bool>,
    /// The last follow read was cut short: more entries are waiting, read on the next turn.
    pub more_waiting: bool,
    /// The first read stopped at its size or time limit.
    pub first_cut: bool,
    /// Rows the entries had at the last draw (what a page is).
    pub page: Cell<usize>,
}

impl JournalView {
    pub fn new(query: JournalQuery) -> JournalView {
        JournalView {
            follow: query.preset.follows() && query.since_until.is_none(),
            query,
            entries: VecDeque::new(),
            filter: String::new(),
            filter_editing: false,
            picker: None,
            scroll: Scroll::Tail,
            new_below: 0,
            load: Load::Idle,
            pending: None,
            last_poll: None,
            system_readable: None,
            more_waiting: false,
            first_cut: false,
            page: Cell::new(DEFAULT_PAGE),
        }
    }

    /// Whether the entry passes the text filter (a substring of message, identifier or unit, case
    /// and accents folded).
    fn passes(&self, h: &Held) -> bool {
        self.filter.is_empty() || h.haystack.contains(&fuzzy::fold_str(&self.filter))
    }

    /// The entries that pass the filter, oldest first.
    pub fn visible(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|h| self.passes(h)).map(|h| &h.entry).collect()
    }

    /// The cursor a follow read goes on from.
    pub fn last_cursor(&self) -> Option<&str> {
        self.entries.back().map(|h| h.entry.cursor.as_str())
    }

    fn push(&mut self, entries: Vec<Entry>) {
        let mut shown = 0;
        for e in entries {
            let h = Held::new(e);
            if self.passes(&h) {
                shown += 1;
            }
            self.entries.push_back(h);
        }
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
        if let Scroll::At(k) = self.scroll {
            // Paused: what is on screen stays where it is.
            self.scroll = Scroll::At(k + shown);
            self.new_below += shown;
        }
    }

    /// Moves the window `by` entries towards the past (negative: the present), clamped.
    pub fn scroll_by(&mut self, by: isize, shown: usize) {
        let now = match self.scroll {
            Scroll::Tail => 0,
            Scroll::At(k) => k,
        };
        let top = shown.saturating_sub(1);
        let next = now.saturating_add_signed(by).min(top);
        self.set_offset(next);
    }

    fn set_offset(&mut self, k: usize) {
        if k == 0 {
            self.scroll = Scroll::Tail;
            self.new_below = 0;
        } else {
            self.scroll = Scroll::At(k);
        }
    }
}

impl App {
    fn journal_view(&mut self) -> Option<&mut JournalView> {
        match &mut self.diag.view {
            View::Journal(j) => Some(j.as_mut()),
            _ => None,
        }
    }

    /// Opens the journal view on `query`: the first read and the once-per-view check go to the
    /// worker, or the unit picker opens when a service is still to be chosen.
    pub(crate) fn journal_open(&mut self, query: JournalQuery) {
        let choose = query.preset == Preset::ServiceLogs && query.unit.is_none() && query.since_until.is_none();
        let mut view = JournalView::new(query);
        self.requests.push(Request::Diag(DiagRead::JournalReadable));
        if choose {
            view.picker = Some(Picker::new());
            self.requests.push(Request::Diag(DiagRead::ServiceUnits));
        } else {
            let first = JournalQuery { lines: self.config.diagnostics.journal_lines, ..view.query.clone() };
            view.load = Load::Reading;
            view.pending = Some(first.clone());
            self.requests.push(Request::Diag(DiagRead::Journal(first)));
        }
        self.open_diag_view(View::Journal(Box::new(view)));
    }

    /// A unit or priority change: everything is read again from scratch.
    fn journal_reread(&mut self) {
        let lines = self.config.diagnostics.journal_lines;
        let Some(j) = self.journal_view() else { return };
        j.entries.clear();
        j.set_offset(0);
        j.more_waiting = false;
        j.first_cut = false;
        j.load = Load::Reading;
        let q = JournalQuery { lines, after_cursor: None, ..j.query.clone() };
        j.pending = Some(q.clone());
        self.requests.push(Request::Diag(DiagRead::Journal(q)));
    }

    /// The tick: one follow read when the view is open, follow is on, none is out and the last
    /// one ended a second ago. A slow read delays the next, never stacks.
    pub(crate) fn journal_tick(&mut self, now: Instant) {
        if self.tab != Tab::Diagnostics {
            return;
        }
        let lines = self.config.diagnostics.journal_lines;
        let Some(j) = self.journal_view() else { return };
        let due = j.last_poll.is_some_and(|t| now.duration_since(t) >= POLL);
        if !j.follow || j.pending.is_some() || j.picker.is_some() || !due || !matches!(j.load, Load::Ready) {
            return;
        }
        // Nothing held yet: no cursor to go on from, so the first read is made again.
        let q = JournalQuery { lines, after_cursor: j.last_cursor().map(str::to_string), ..j.query.clone() };
        j.pending = Some(q.clone());
        self.requests.push(Request::Diag(DiagRead::Journal(q)));
    }

    /// An answer of the worker that concerns the journal view is taken here; anything else is
    /// handed back.
    pub(crate) fn journal_try(&mut self, read: &DiagRead, result: DiagResult, now: Instant) -> Option<DiagResult> {
        match (read, result) {
            (DiagRead::Journal(q), DiagResult::Journal(r)) => self.journal_read_done(q, r, now),
            (DiagRead::ServiceUnits, DiagResult::ServiceUnits(r)) => {
                if let Some(p) = self.journal_view().and_then(|j| j.picker.as_mut()) {
                    match r {
                        Ok(units) => p.units = Some(units),
                        Err(e) => p.error = Some(e.to_string()),
                    }
                }
            }
            (DiagRead::JournalReadable, DiagResult::JournalReadable(ok)) => {
                if let Some(j) = self.journal_view() {
                    j.system_readable = Some(ok);
                }
            }
            (_, other) => return Some(other),
        }
        None
    }

    fn journal_read_done(&mut self, q: &JournalQuery, r: BResult<JournalRead>, now: Instant) {
        let Some(j) = self.journal_view() else { return };
        if j.pending.as_ref() != Some(q) {
            return; // an answer to a question that is no longer asked
        }
        j.pending = None;
        j.last_poll = Some(now);
        match r {
            Err(e) => {
                j.load = Load::Failed(e.to_string());
                // A tool that fails is not asked again every second.
                j.follow = false;
            }
            Ok(read) => {
                if q.after_cursor.is_none() {
                    j.entries.clear();
                    j.set_offset(0);
                    j.push(read.entries);
                    j.first_cut = read.capped || read.timed_out;
                    j.more_waiting = false;
                } else {
                    j.push(read.entries);
                    j.more_waiting = read.capped;
                }
                j.load = Load::Ready;
            }
        }
    }

    /// Keys of the journal view that need more than an action of the key table: the filter and
    /// the picker take text, and `u`, `p`, `f`, `/` and the scrolling keys are the view's own.
    /// False for anything else, which goes on to the key table (Esc back, `q`, the tabs…).
    pub(crate) fn journal_key(&mut self, e: KeyEvent) -> bool {
        if self.tab != Tab::Diagnostics || e.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        let Some(j) = self.journal_view() else { return false };
        if j.picker.is_some() {
            self.picker_key(e);
            return true;
        }
        if j.filter_editing {
            match e.code {
                KeyCode::Esc => {
                    j.filter.clear();
                    j.filter_editing = false;
                }
                KeyCode::Enter => j.filter_editing = false,
                KeyCode::Backspace => {
                    j.filter.pop();
                }
                KeyCode::Char(c) => j.filter.push(c),
                _ => return true,
            }
            j.set_offset(0);
            return true;
        }
        let shown = j.visible().len();
        let page = j.page.get().max(2) - 1;
        match e.code {
            KeyCode::Esc if !j.filter.is_empty() => {
                j.filter.clear();
                j.set_offset(0);
            }
            KeyCode::Char('/') => j.filter_editing = true,
            KeyCode::Char('u') => {
                j.picker = Some(Picker::new());
                self.requests.push(Request::Diag(DiagRead::ServiceUnits));
            }
            KeyCode::Char('p') => {
                j.query.priority = j.query.priority.next();
                self.journal_reread();
            }
            KeyCode::Char('f') => {
                if j.query.since_until.is_none() {
                    j.follow = !j.follow;
                }
            }
            KeyCode::Up => j.scroll_by(1, shown),
            KeyCode::Down => j.scroll_by(-1, shown),
            KeyCode::PageUp => j.scroll_by(page as isize, shown),
            KeyCode::PageDown => j.scroll_by(-(page as isize), shown),
            KeyCode::Home => j.scroll_by(isize::MAX / 2, shown),
            KeyCode::End => j.set_offset(0),
            _ => return false,
        }
        true
    }

    fn picker_key(&mut self, e: KeyEvent) {
        let Some(j) = self.journal_view() else { return };
        let Some(p) = j.picker.as_mut() else { return };
        let count = p.matches().len();
        match e.code {
            KeyCode::Esc => j.picker = None,
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => p.selected = (p.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Enter => {
                let chosen = p.matches().get(p.selected).map(|&(s, u)| (*s, u.clone()));
                if let Some(unit) = chosen {
                    j.query.unit = Some(unit);
                    j.picker = None;
                    self.journal_reread();
                }
            }
            KeyCode::Backspace if p.query.is_empty() => {
                // Backspace on nothing clears the unit.
                j.query.unit = None;
                j.picker = None;
                self.journal_reread();
            }
            KeyCode::Backspace => {
                p.query.pop();
                p.selected = 0;
            }
            KeyCode::Char(c) => {
                p.query.push(c);
                p.selected = 0;
            }
            _ => {}
        }
    }

    /// The wheel over the journal view: up goes towards the past. True when it was the view's.
    pub(crate) fn journal_wheel(&mut self, dir: i32) -> bool {
        let Some(j) = self.journal_view() else { return false };
        let shown = j.visible().len();
        j.scroll_by(if dir > 0 { WHEEL as isize } else { -(WHEEL as isize) }, shown);
        true
    }
}

#[cfg(test)]
mod tests;
