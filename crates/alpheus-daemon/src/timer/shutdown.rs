//! The scheduled shutdown's model: a wall-clock end, its warning, and everything it shows.
//!
//! The shutdown is a second slot beside the timer (`mod.rs`): it runs alongside any timer, never
//! replaces one and is never replaced by one. This file holds its state (`Shutdown`), the steps it
//! goes through (`next`: a warning a minute before the end, then the power off), the choices the
//! Tools menu offers, the chip text and the one line kept in the state file. The wiring (`app.rs`)
//! does the talking to the outside: the clock, the notification and the `[quick] poweroff` command.
//!
//! Never:
//! - read a clock: the caller always passes `now` in Unix milliseconds;
//! - read or write a file, run a command or start anything (a thread, a process, a timer);
//! - power off without the warning first, however late the end is noticed.
//!
//! See: docs/features/timer.md, and
//! docs/decisions.md ("A scheduled shutdown always warns a minute before, even when its time has
//! passed").

use super::{MINUTE_MS, SAVED_AHEAD_MS, countdown_tick, minutes_text, num};
use crate::notify::{Incoming, Urgency, action};

/// The warning comes this long before the end, and the power off is never sooner than this after
/// a warning.
pub const WARN_MS: u64 = MINUTE_MS;

/// The durations the Tools menu offers: the label, then the length in minutes.
const DURATIONS: [(&str, u64); 5] = [("30 min", 30), ("1 h", 60), ("1 h 30", 90), ("2 h", 120), ("3 h", 180)];

/// What the cancel choice starts with: `choice_of` matches it, so the label may carry the time left.
const CANCEL: &str = "Cancel shutdown";

/// The shutdown scheduled: the moment the machine powers off, as a wall-clock time. A wall-clock
/// end, never a monotonic one, so the time a laptop spends asleep counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shutdown {
    pub end: u64,
}

/// What is due: the warning (given at `at`), or the power off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownStep {
    Warn { at: u64 },
    PowerOff,
}

/// What the Tools menu, a verb or the popup asks of the shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownChoice {
    Cancel,
    /// Power off after this many milliseconds, replacing any shutdown pending.
    After(u64),
}

/// The shutdown after `choice` at `now`.
pub fn apply(_pending: Option<Shutdown>, choice: ShutdownChoice, now: u64) -> Option<Shutdown> {
    match choice {
        ShutdownChoice::Cancel => None,
        ShutdownChoice::After(ms) => Some(Shutdown { end: now.saturating_add(ms) }),
    }
}

impl Shutdown {
    /// The time left, in milliseconds; zero once the end has passed.
    pub fn left(&self, now: u64) -> u64 {
        self.end.saturating_sub(now)
    }

    /// The shutdown once its warning is given at `now`: the end is never less than a minute away,
    /// so a warning always has its minute, however late it was noticed.
    pub fn warned_at(self, now: u64) -> Shutdown {
        Shutdown { end: self.end.max(now.saturating_add(WARN_MS)) }
    }

    /// Whether the last minute has begun: the chip turns to the warn colour.
    pub fn in_warning(&self, now: u64) -> bool {
        self.left(now) <= WARN_MS
    }

    /// The chip's text: the time left in minutes, rounded up (`1h20`).
    pub fn chip_text(&self, now: u64) -> String {
        minutes_text(self.left(now).div_ceil(MINUTE_MS))
    }

    /// The state file's line: `shutdown <unix end in milliseconds>`.
    pub fn saved(&self) -> String {
        format!("shutdown {}", self.end)
    }

    /// A state line read back, treated as hostile like the timer's (docs/decisions.md, "The state
    /// file is hostile input"): digits only, the exact fields, an end at most 25 hours ahead. A past
    /// end is fine: the warning comes first.
    pub fn parse_saved(line: &str, now: u64) -> Option<Shutdown> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let ["shutdown", end] = fields.as_slice() else { return None };
        let end = num(end)?;
        (end <= now.saturating_add(SAVED_AHEAD_MS)).then_some(Shutdown { end })
    }
}

/// The next moment the shutdown has a step, and the step due now.
///
/// Not warned: the warning is due from a minute before the end, and it is due at once, even when
/// the end is long past (the caller then moves the end with `warned_at`). Warned: the power off is
/// due from the end. Never a power off before the warning.
pub fn next(s: &Shutdown, now: u64, warned: bool) -> (Option<u64>, Option<ShutdownStep>) {
    if warned {
        if now >= s.end {
            return (None, Some(ShutdownStep::PowerOff));
        }
        return (Some(s.end), None);
    }
    let warn_at = s.end.saturating_sub(WARN_MS);
    if now >= warn_at {
        // The warning is due now. What comes after it is the end `warned_at` gives.
        return (Some(s.warned_at(now).end), Some(ShutdownStep::Warn { at: now }));
    }
    (Some(warn_at), None)
}

/// The next instant the wiring must look at the shutdown: a step, or the chip's next minute. None
/// once the power off is due (nothing left to wait for).
pub fn wake(s: &Shutdown, now: u64, warned: bool) -> Option<u64> {
    let (step, _) = next(s, now, warned);
    // The chip's text changes when the time left falls to one minute fewer. Past the end there is
    // nothing to show, and the step alone decides.
    let tick = (now < s.end).then(|| countdown_tick(s.end, now, MINUTE_MS));
    match (step, tick) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The Tools menu's choices, in order: while a shutdown is pending, a first choice that cancels it
/// and says when it was due (`Cancel shutdown (in 1h20)`), then the five durations.
pub fn labels(pending_left: Option<u64>) -> Vec<String> {
    let mut out = Vec::with_capacity(DURATIONS.len() + 1);
    if let Some(left) = pending_left {
        out.push(format!("{CANCEL} (in {})", minutes_text(left.div_ceil(MINUTE_MS))));
    }
    out.extend(DURATIONS.iter().map(|(label, _)| (*label).to_string()));
    out
}

/// What a chosen label asks for: the cancel (whatever time it carries) or a duration; None for any
/// other text.
pub fn choice_of(label: &str) -> Option<ShutdownChoice> {
    if label.starts_with(CANCEL) {
        return Some(ShutdownChoice::Cancel);
    }
    DURATIONS.iter().find(|(l, _)| *l == label).map(|(_, minutes)| ShutdownChoice::After(minutes * MINUTE_MS))
}

/// The notice that comes a minute before the power off: critical, so it stays until it is
/// answered, with one action, which is the `default` one (a click on the notice): Cancel.
pub fn warning() -> Incoming {
    Incoming {
        app_name: "alpheus".into(),
        replaces_id: 0,
        summary: "Powering off in 1 min".into(),
        body: "Click to cancel".into(),
        actions: action("default", "Cancel").into_iter().collect(),
        urgency: Urgency::Critical,
        // 0 asks for no timeout at all.
        expire_ms: 0,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: Some("shutdown".into()),
    }
}

#[cfg(test)]
mod tests;
