//! The timer popup's content: the rows it shows and what a key or the pointer asks of the timer.
//!
//! The popup is the small window that opens under the timer's chip. Its first row is the status of
//! the timer, then a separator, then the actions the timer's state offers (a running countdown
//! offers Pause, Add 1 min and Stop; an idle timer offers to start one). This file holds the
//! rows, which action is selected, and what each key, the pointer and the wheel do. It does not
//! change the timer itself: it answers with a `TimerAction` and the wiring (`app.rs`) acts on it.
//! The window itself (`PopupBody::Timer`) and the one-second tick are the wiring's too.
//!
//! Never:
//! - read a clock (the caller passes `now`);
//! - start a thread, a process or a command;
//! - change the timer: it only says what to change.
//!
//! See: docs/features/timer.md, and
//! docs/decisions.md ("The popup lists what the state allows").

use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::segments::{PAUSE, PLAY, SKIP_NEXT};
use crate::timer::{BREAK, Change, DONE, Lengths, MINUTE_MS, Phase, STOPWATCH, TIMER, Timer as Model, WORK, clock_text};

/// A row's width in terminal-style cells, the pad cell at each end included.
pub const WIDTH: u16 = 26;
/// Content row 0 is the status and row 1 a separator, so the actions start at row 2.
pub const FIRST_ITEM_ROW: u16 = 2;
pub const PLUS: char = '\u{f0415}'; // md-plus
pub const STOP: char = '\u{f04db}'; // md-stop

/// The longest countdown the popup's Timer row can be set to, in minutes (the shortest is 1).
const MINUTES_MAX: u32 = 180;

/// One action the popup can offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Countdown,
    Pomodoro,
    Stopwatch,
    Pause,
    Resume,
    Add,
    Skip,
    Stop,
    /// Cancels the scheduled shutdown (the timer is not touched).
    CancelShutdown,
}

/// The actions the state offers, top to bottom.
///
/// An idle timer offers the three ways to start one. A timer that runs or is paused offers
/// Pause (when running) or Resume (when paused), then what only its kind has (Add 1 min for a
/// countdown, Skip for a Pomodoro), then Stop.
pub fn items(t: &Model) -> Vec<Item> {
    items_with(t, false)
}

/// The actions the state offers, and a last row that cancels the scheduled shutdown while one is
/// pending (`shutdown`).
pub fn items_with(t: &Model, shutdown: bool) -> Vec<Item> {
    let toggle = if t.running() { Item::Pause } else { Item::Resume };
    let mut rows = match t {
        Model::Idle { .. } => vec![Item::Countdown, Item::Pomodoro, Item::Stopwatch],
        Model::Countdown { .. } => vec![toggle, Item::Add, Item::Stop],
        Model::Pomodoro { .. } => vec![toggle, Item::Skip, Item::Stop],
        Model::Stopwatch { .. } => vec![toggle, Item::Stop],
    };
    if shutdown {
        rows.push(Item::CancelShutdown);
    }
    rows
}

/// The countdown row's length, in minutes, moved by `delta` steps (negative goes down). A step is
/// one minute below 10 and five minutes from 10 up, within 1 to 180. From 12, up goes to 15 and
/// down goes to 10: above 10 the steps land on multiples of five.
pub fn step_minutes(m: u32, delta: i32) -> u32 {
    let mut m = m.clamp(1, MINUTES_MAX);
    for _ in 0..delta.unsigned_abs() {
        let stepped = if delta > 0 { step_up(m) } else { step_down(m) };
        m = stepped.clamp(1, MINUTES_MAX);
    }
    m
}

/// One step up: +1 below 10, else the next multiple of five.
fn step_up(m: u32) -> u32 {
    if m < 10 { m + 1 } else { (m / 5 + 1) * 5 }
}

/// One step down: -1 up to 10, else the multiple of five below.
fn step_down(m: u32) -> u32 {
    if m <= 10 { m - 1 } else { (m - 1) / 5 * 5 }
}

/// What a key, the pointer or the wheel asks of the wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerAction {
    /// Nothing happened.
    Nothing,
    /// Only what is shown changed (the selection, the length): draw the popup again.
    Redraw,
    Close,
    /// Make this change to the timer.
    Do(Change),
    /// Cancel the scheduled shutdown.
    CancelShutdown,
}

/// The open popup's own state: which action is selected and the length of the Timer row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerPopup {
    /// The selected action's index in the items.
    pub selected: usize,
    /// The countdown row's length, in minutes.
    pub minutes: u32,
    /// The actions on screen, kept to notice when the timer's state changes them (`follow`).
    shown: Vec<Item>,
    /// A shutdown is scheduled: the popup offers to cancel it (`set_shutdown`).
    shutdown: bool,
}

impl TimerPopup {
    /// The popup at open: the first action selected.
    pub fn new(minutes: u32, t: &Model) -> TimerPopup {
        TimerPopup { selected: 0, minutes: minutes.clamp(1, MINUTES_MAX), shown: items(t), shutdown: false }
    }

    /// The timer changed under the popup: true when its actions did (the selection goes back to
    /// the first).
    pub fn follow(&mut self, t: &Model) -> bool {
        let now = items_with(t, self.shutdown);
        if now == self.shown {
            return false;
        }
        self.shown = now;
        self.selected = 0;
        true
    }

    /// Whether a shutdown is scheduled; the rows follow at the next `follow`.
    pub fn set_shutdown(&mut self, pending: bool) {
        self.shutdown = pending;
    }

    /// What an action asks of the wiring: its change to the timer, or the shutdown's cancel.
    fn act(&self, item: Item) -> TimerAction {
        match item {
            Item::CancelShutdown => TimerAction::CancelShutdown,
            other => TimerAction::Do(self.change_of(other)),
        }
    }

    /// The change an action makes. The Timer row uses the length set on its row.
    fn change_of(&self, item: Item) -> Change {
        match item {
            Item::Countdown => Change::Countdown(u64::from(self.minutes) * MINUTE_MS),
            Item::Pomodoro => Change::Pomodoro,
            Item::Stopwatch => Change::Stopwatch,
            Item::Pause => Change::Pause,
            Item::Resume => Change::Resume,
            Item::Add => Change::Add(MINUTE_MS),
            Item::Skip => Change::Skip,
            Item::Stop => Change::Stop,
            // Not a change to the timer: `act` answers it before this is asked.
            Item::CancelShutdown => Change::Seen,
        }
    }

    /// Selects action `to`; Redraw only if that is a change.
    fn select(&mut self, to: usize) -> TimerAction {
        if self.selected == to {
            return TimerAction::Nothing;
        }
        self.selected = to;
        TimerAction::Redraw
    }

    /// The countdown row's length stepped; Nothing when it did not move.
    fn adjust(&mut self, delta: i32) -> TimerAction {
        let to = step_minutes(self.minutes, delta);
        if to == self.minutes {
            return TimerAction::Nothing;
        }
        self.minutes = to;
        TimerAction::Redraw
    }

    /// Whether the selected action is the Timer row, the only one with a length to adjust.
    fn on_countdown(&self) -> bool {
        self.shown.get(self.selected) == Some(&Item::Countdown)
    }

    /// ↑ ↓ Tab Shift+Tab move the selection (it stops at the first and last row), ← → adjust the
    /// countdown's length, Enter and Space act, Escape closes. `repeat` is true for a key the
    /// keyboard repeats because it is held: a held Enter acts once, but a held arrow keeps moving.
    pub fn key(&mut self, _t: &Model, k: Key, repeat: bool) -> TimerAction {
        let len = self.shown.len();
        match k {
            Key::Escape => TimerAction::Close,
            Key::Up | Key::BackTab | Key::CtrlP => self.select(list::step(self.selected, -1, len, |_| true)),
            Key::Down | Key::Tab | Key::CtrlN => self.select(list::step(self.selected, 1, len, |_| true)),
            Key::Left if self.on_countdown() => self.adjust(-1),
            Key::Right if self.on_countdown() => self.adjust(1),
            Key::Enter | Key::Char(' ') if !repeat => match self.shown.get(self.selected) {
                Some(&item) => self.act(item),
                None => TimerAction::Nothing,
            },
            _ => TimerAction::Nothing,
        }
    }

    /// The action on content row `row`, if the row is one (not the status, the separator or below).
    fn item_at(&self, row: u16) -> Option<usize> {
        // `checked_sub` gives None for the status and separator rows, which sit above the actions.
        let i = usize::from(row.checked_sub(FIRST_ITEM_ROW)?);
        (i < self.shown.len()).then_some(i)
    }

    /// The pointer over content row `row`: selects the action there.
    pub fn hover(&mut self, _t: &Model, row: u16) -> TimerAction {
        match self.item_at(row) {
            Some(i) => self.select(i),
            None => TimerAction::Nothing,
        }
    }

    /// A left press on content row `row`: selects that action and runs it.
    pub fn press(&mut self, _t: &Model, row: u16) -> TimerAction {
        let Some(i) = self.item_at(row) else { return TimerAction::Nothing };
        self.selected = i;
        self.act(self.shown[i])
    }

    /// The wheel over the countdown row: up lengthens, down shortens. `steps` is negative for up.
    pub fn scroll(&mut self, _t: &Model, row: u16, steps: i32) -> TimerAction {
        match self.item_at(row) {
            Some(i) if self.shown[i] == Item::Countdown => self.adjust(-steps),
            _ => TimerAction::Nothing,
        }
    }

    /// The rows: the status (glyph, time, a caption on the right), a separator, the actions.
    pub fn render(&self, t: &Model, l: &Lengths, now: u64) -> Boxed {
        let columns = WIDTH + 2;
        let mut out = vec![Grid::empty(columns)];
        let mut status = Grid::empty(columns);
        let (glyph, text, right, style) = status_of(t, l, now);
        // An ordinary status gets the accent colour on its icon; paused and done keep their own.
        let icon_style = if style == Style::NORMAL { Style::ACCENT } else { style };
        list::row(&mut status, 1, columns - 1, &Row { icon: Some(glyph), icon_style, text: &text, right: &right, style, selected: false });
        out.push(status);
        out.push(Grid::empty(columns));
        for (i, &item) in self.shown.iter().enumerate() {
            let mut g = Grid::empty(columns);
            let (glyph, text) = item_face(item);
            // The right-hand text: the length on the Timer row (‹ 10 min ›), the Pomodoro's
            // lengths (`25 · 5 min`: work and short break), nothing on the others.
            let right = match item {
                Item::Countdown => format!("\u{2039} {} min \u{203a}", self.minutes),
                Item::Pomodoro => format!("{} \u{b7} {} min", l.work / MINUTE_MS, l.short / MINUTE_MS),
                _ => String::new(),
            };
            let row = Row {
                icon: Some(glyph),
                icon_style: Style::ACCENT,
                text,
                right: &right,
                style: Style::NORMAL,
                selected: i == self.selected,
            };
            list::row(&mut g, 1, columns - 1, &row);
            out.push(g);
        }
        out.push(Grid::empty(columns));
        list::boxed(&out, &[2], None)
    }
}

/// An action's glyph and label.
fn item_face(item: Item) -> (char, &'static str) {
    match item {
        Item::Countdown => (TIMER, "Timer"),
        Item::Pomodoro => (WORK, "Pomodoro"),
        Item::Stopwatch => (STOPWATCH, "Stopwatch"),
        Item::Pause => (PAUSE, "Pause"),
        Item::Resume => (PLAY, "Resume"),
        Item::Add => (PLUS, "Add 1 min"),
        Item::Skip => (SKIP_NEXT, "Skip"),
        Item::Stop => (STOP, "Stop"),
        Item::CancelShutdown => (STOP, "Cancel shutdown"),
    }
}

/// The status row: glyph, text, right-hand caption and style. Paused is all muted, done is warn.
fn status_of(t: &Model, l: &Lengths, now: u64) -> (char, String, String, Style) {
    // The time left as a clock, rounded up to a whole second (a countdown that has 0.4 s left
    // still reads `0:01`, not `0:00`). `unwrap_or(0)` is for the timers with no time left to show,
    // which never reach this.
    let left = |t: &Model| clock_text(t.left(now).unwrap_or(0).div_ceil(1000));
    let (mode_glyph, text, caption) = match t {
        Model::Idle { done: true } => return (DONE, "Done".into(), String::new(), Style::WARN),
        Model::Idle { done: false } => return (TIMER, "Ready".into(), String::new(), Style::NORMAL),
        Model::Countdown { total, .. } => (TIMER, left(t), format!("of {}", clock_text(total.div_ceil(1000)))),
        Model::Pomodoro { phase, round, .. } => {
            let (glyph, caption) = match phase {
                Phase::Work => (WORK, format!("work {round}/{}", l.rounds)),
                Phase::ShortBreak => (BREAK, format!("break {round}/{}", l.rounds)),
                Phase::LongBreak => (BREAK, "long break".to_string()),
            };
            (glyph, left(t), caption)
        }
        Model::Stopwatch { .. } => (STOPWATCH, clock_text(t.elapsed(now).unwrap_or(0) / 1000), String::new()),
    };
    // A paused timer shows the pause glyph and is muted (dim) all along the row.
    if t.paused_now() { (PAUSE, text, caption, Style::MUTED) } else { (mode_glyph, text, caption, Style::NORMAL) }
}

#[cfg(test)]
mod tests;
