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
use std::time::Duration;

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
    /// When it was scheduled, as a wall-clock time. A shutdown saved by an earlier boot is dropped
    /// at load by comparing this with the boot time: the state directory outlives a reboot.
    pub set: u64,
}

/// The slack the warning's bookkeeping allows: a deadline that fires this late, or a wall clock
/// that moved this much against the monotonic one, is still the same warning.
pub const STALE_MS: u64 = 5_000;

/// How far the wall clock may run ahead of the monotonic one since a warning before it counts as a
/// sleep: a second. A sleep of 3 s just before the end must not power the machine off the moment it
/// wakes, so this is tighter than `STALE_MS`; a timer's jitter and a clock's slewing are far below it.
pub const SLEEP_MS: u64 = 1_000;

/// Whether the warning given at wall-clock time `warned` still counts at `now`.
///
/// `mono_elapsed` is the time since the warning on the monotonic clock, which does not run while
/// the machine sleeps; the wall clock does. So a sleep shows as more wall time than monotonic time,
/// and a warning the machine slept through, however briefly (a 3 s sleep at the end of its
/// minute, or 12 s), no longer counts: the minute was not seen. Nor does it when the clock went back before
/// the warning, or `end` passed by more than `STALE_MS`. A warning that no longer counts is given
/// again, and the power off waits a fresh minute.
pub fn warning_holds(s: &Shutdown, warned: Option<u64>, mono_elapsed: Duration, now: u64) -> bool {
    let Some(w) = warned else { return false };
    if now < w || now > s.end.saturating_add(STALE_MS) {
        return false;
    }
    let mono_ms = u64::try_from(mono_elapsed.as_millis()).unwrap_or(u64::MAX);
    now - w <= mono_ms.saturating_add(SLEEP_MS)
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
        ShutdownChoice::After(ms) => Some(Shutdown { end: now.saturating_add(ms), set: now }),
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
        Shutdown { end: self.end.max(now.saturating_add(WARN_MS)), set: self.set }
    }

    /// Whether the last minute has begun: the chip turns to the warn colour.
    pub fn in_warning(&self, now: u64) -> bool {
        self.left(now) <= WARN_MS
    }

    /// The chip's text: the time left in minutes, rounded up (`1h20`).
    pub fn chip_text(&self, now: u64) -> String {
        minutes_text(self.left(now).div_ceil(MINUTE_MS))
    }

    /// The state file's line: `shutdown <unix end ms> <unix scheduled-at ms>`.
    pub fn saved(&self) -> String {
        format!("shutdown {} {}", self.end, self.set)
    }

    /// A state line read back, treated as hostile like the timer's (docs/decisions.md, "The state
    /// file is hostile input"): digits only, the exact fields, an end at most 25 hours ahead. A past
    /// end is fine: the warning comes first. A line from before the scheduled-at time existed (one
    /// number) reads with `set` 0, which the boot check always drops: the safe side.
    pub fn parse_saved(line: &str, now: u64) -> Option<Shutdown> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (end, set) = match fields.as_slice() {
            ["shutdown", end] => (num(end)?, 0),
            ["shutdown", end, set] => (num(end)?, num(set)?),
            _ => return None,
        };
        (end <= now.saturating_add(SAVED_AHEAD_MS)).then_some(Shutdown { end, set })
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

/// How often the lock is looked at while the warning's minute runs: a lock that starts and ends
/// inside the minute would otherwise go unseen, and the user would be powered off with a few
/// seconds of warning at the end of it. Twelve looks a minute, only during that minute.
pub const LOCK_SAMPLE_MS: u64 = 5_000;

/// The next instant the wiring must look at the shutdown: a step, the chip's next minute, and, while
/// the warning's minute runs, the next look at the lock. None once the power off is due (nothing
/// left to wait for).
pub fn wake(s: &Shutdown, now: u64, warned: bool) -> Option<u64> {
    let (step, _) = next(s, now, warned);
    // The chip's text changes when the time left falls to one minute fewer. Past the end there is
    // nothing to show, and the step alone decides.
    let tick = (now < s.end).then(|| countdown_tick(s.end, now, MINUTE_MS));
    let sample = (warned && now < s.end).then(|| now.saturating_add(LOCK_SAMPLE_MS));
    [step, tick, sample].into_iter().flatten().min()
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

/// The notice that says the power off did not happen (the command could not start, or ended with
/// an error: a refusal, an inhibitor, a missing program). Critical, so it stays until it is read;
/// no action, as the shutdown is gone.
pub fn failure() -> Incoming {
    Incoming {
        app_name: "alpheus".into(),
        replaces_id: 0,
        summary: "Shutdown".into(),
        body: "Shutdown failed".into(),
        actions: Vec::new(),
        urgency: Urgency::Critical,
        // A negative value asks for the server's default; a critical notice has no timeout anyway.
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        // Its own tag: the next warning (`shutdown`) never replaces it.
        tag: Some("shutdown-failed".into()),
    }
}

#[cfg(test)]
mod tests;
