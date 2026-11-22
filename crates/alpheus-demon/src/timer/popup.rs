//! The timer popup (timer spec § 3, plan decision 11), pure: a status row, then the actions the
//! timer's state offers, the selection, the keys, the pointer and the rows drawn. The popup itself
//! (`PopupBody::Timer`), the one-second tick and every change made are the wiring's
//! (`timer_app.rs`): nothing here reads a clock, starts a thread or runs a command.

use crate::grid::{Grid, Style};
use crate::keys::Key;
use crate::list::{self, Boxed, Row};
use crate::segments::{PAUSE, PLAY, SKIP_NEXT};
use crate::timer::{BREAK, Change, DONE, Lengths, MINUTE_MS, Phase, Run, STOPWATCH, TIMER, Timer as Model, WORK, clock_text};

/// A row's cells, its pad cell at each end included.
pub const WIDTH: u16 = 26;
/// Content row 0 is the status, row 1 a separator, the actions follow.
pub const FIRST_ITEM_ROW: u16 = 2;
pub const PLUS: char = '\u{f0415}'; // md-plus
pub const STOP: char = '\u{f04db}'; // md-stop

/// The countdown row's length range, in minutes.
const MINUTES_MAX: u32 = 180;

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
}

/// The actions the state offers, top to bottom (plan decision 11).
pub fn items(t: &Model) -> Vec<Item> {
    let toggle = if t.running() { Item::Pause } else { Item::Resume };
    match t {
        Model::Idle { .. } => vec![Item::Countdown, Item::Pomodoro, Item::Stopwatch],
        Model::Countdown { .. } => vec![toggle, Item::Add, Item::Stop],
        Model::Pomodoro { .. } => vec![toggle, Item::Skip, Item::Stop],
        Model::Stopwatch { .. } => vec![toggle, Item::Stop],
    }
}

/// The countdown row's length moved by `delta` steps: one minute below 10, five from 10, within
/// 1 to 180 (from 12, up goes to 15 and down to 10).
pub fn step_minutes(m: u32, delta: i32) -> u32 {
    let mut m = m.clamp(1, MINUTES_MAX);
    for _ in 0..delta.unsigned_abs() {
        m = if delta > 0 {
            if m < 10 { m + 1 } else { (m / 5 + 1) * 5 }
        } else if m <= 10 {
            m - 1
        } else {
            (m - 1) / 5 * 5
        }
        .clamp(1, MINUTES_MAX);
    }
    m
}

/// What a key, the pointer or the wheel asks of the wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerAction {
    Nothing,
    Redraw,
    Close,
    Do(Change),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerPopup {
    /// The selected action's index in the items.
    pub selected: usize,
    /// The countdown row's length, in minutes.
    pub minutes: u32,
    shown: Vec<Item>,
}

impl TimerPopup {
    /// The popup at open: the first action selected.
    pub fn new(minutes: u32, t: &Model) -> TimerPopup {
        TimerPopup { selected: 0, minutes: minutes.clamp(1, MINUTES_MAX), shown: items(t) }
    }

    /// The timer changed under the popup: true when its actions did (the selection goes back to
    /// the first).
    pub fn follow(&mut self, t: &Model) -> bool {
        let now = items(t);
        if now == self.shown {
            return false;
        }
        self.shown = now;
        self.selected = 0;
        true
    }

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
        }
    }

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

    fn on_countdown(&self) -> bool {
        self.shown.get(self.selected) == Some(&Item::Countdown)
    }

    /// ↑ ↓ Tab Shift+Tab move (clamped), ← → adjust the countdown's length, Enter and Space act,
    /// Escape closes. `repeat`: the keyboard's auto-repeat, which only moves and adjusts follow.
    pub fn key(&mut self, _t: &Model, k: Key, repeat: bool) -> TimerAction {
        let len = self.shown.len();
        match k {
            Key::Escape => TimerAction::Close,
            Key::Up | Key::BackTab | Key::CtrlP => self.select(list::step(self.selected, -1, len, |_| true)),
            Key::Down | Key::Tab | Key::CtrlN => self.select(list::step(self.selected, 1, len, |_| true)),
            Key::Left if self.on_countdown() => self.adjust(-1),
            Key::Right if self.on_countdown() => self.adjust(1),
            Key::Enter | Key::Char(' ') if !repeat => match self.shown.get(self.selected) {
                Some(&item) => TimerAction::Do(self.change_of(item)),
                None => TimerAction::Nothing,
            },
            _ => TimerAction::Nothing,
        }
    }

    fn item_at(&self, row: u16) -> Option<usize> {
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
        TimerAction::Do(self.change_of(self.shown[i]))
    }

    /// The wheel over the countdown row: up lengthens, down shortens.
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
        let icon_style = if style == Style::NORMAL { Style::ACCENT } else { style };
        list::row(&mut status, 1, columns - 1, &Row { icon: Some(glyph), icon_style, text: &text, right: &right, style, selected: false });
        out.push(status);
        out.push(Grid::empty(columns));
        for (i, &item) in self.shown.iter().enumerate() {
            let mut g = Grid::empty(columns);
            let (glyph, text) = item_face(item);
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
    }
}

/// The status row: glyph, text, right caption, style (paused all muted, done warn).
fn status_of(t: &Model, l: &Lengths, now: u64) -> (char, String, String, Style) {
    let paused = matches!(
        t,
        Model::Countdown { run: Run::Paused(_), .. }
            | Model::Pomodoro { run: Run::Paused(_), .. }
            | Model::Stopwatch { run: Run::Paused(_) }
    );
    let style = if paused { Style::MUTED } else { Style::NORMAL };
    let glyph = |g| if paused { PAUSE } else { g };
    let left = |t: &Model| clock_text(t.left(now).unwrap_or(0).div_ceil(1000));
    match t {
        Model::Idle { done: true } => (DONE, "Done".into(), String::new(), Style::WARN),
        Model::Idle { done: false } => (TIMER, "Ready".into(), String::new(), Style::NORMAL),
        Model::Countdown { total, .. } => (glyph(TIMER), left(t), format!("of {}", clock_text(total.div_ceil(1000))), style),
        Model::Pomodoro { phase, round, .. } => {
            let (g, caption) = match phase {
                Phase::Work => (WORK, format!("work {round}/{}", l.rounds)),
                Phase::ShortBreak => (BREAK, format!("break {round}/{}", l.rounds)),
                Phase::LongBreak => (BREAK, "long break".to_string()),
            };
            (glyph(g), left(t), caption, style)
        }
        Model::Stopwatch { .. } => (glyph(STOPWATCH), clock_text(t.elapsed(now).unwrap_or(0) / 1000), String::new(), style),
    }
}

#[cfg(test)]
mod tests;
