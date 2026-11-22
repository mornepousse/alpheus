//! The focus timer's model: a countdown, a Pomodoro or a stopwatch, and everything it shows.
//!
//! This is the pure half of the timer. It holds the timer's state (`Timer`), every change that can
//! be made to it (`Change`), the end it can come to (`End`), the chip the bar draws (`TimerChip`),
//! the next moment that chip changes, the one line kept in the state file, and the notification an
//! end becomes. The other half, `app.rs`, does the talking to the outside: it reads the clock, the
//! keys and the file, and hands this file every time it needs as Unix milliseconds (wall-clock
//! time counted from 1 January 1970).
//!
//! Never:
//! - read a clock: the caller always passes `now`, so a test can pick any time it likes;
//! - read or write a file, or start anything (a thread, a process, a timer).
//!
//! See: docs/specs/2026-11-15-alpheus-timer-design.md, docs/features/timer.md, and
//! docs/decisions.md ("The timer counts wall-clock time", "A late end is applied once").

use crate::config::{TimerConfig, TimerIdle};
use crate::notify::{Incoming, Urgency};
use crate::segments::PAUSE;

/// One second, in milliseconds. Every time and length in this file is in milliseconds.
pub const SECOND_MS: u64 = 1_000;
pub const MINUTE_MS: u64 = 60_000;
/// The longest countdown, and the stopwatch's cap: one day.
pub const MAX_MS: u64 = 86_400_000;
/// A running end read back from the state file is not believed when it is further than this from
/// now. The bound is the timer plan's decision 13: a running end at most 25 hours after now (the
/// longest timer is one day, plus an hour).
const SAVED_AHEAD_MS: u64 = 25 * 3_600_000;
/// The most Pomodoro rounds (`[timer] rounds`); a saved round above it is refused.
const ROUNDS_MAX: u8 = 12;

// The chip's glyphs: Material Design icons of the Nerd Font (the bar's icon font). The comment on
// each line is the icon's name.
pub const TIMER: char = '\u{f051b}'; // md-timer_outline
pub const WORK: char = '\u{f051f}'; // md-timer_sand
pub const BREAK: char = '\u{f0176}'; // md-coffee
pub const STOPWATCH: char = '\u{f13ab}'; // md-timer
pub const DONE: char = '\u{f009e}'; // md-bell_ring

/// The parts of a Pomodoro: a work session, a short break, and the long break that closes a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Work,
    ShortBreak,
    LongBreak,
}

impl Phase {
    /// The word the state file keeps for this phase.
    pub fn word(self) -> &'static str {
        match self {
            Phase::Work => "work",
            Phase::ShortBreak => "short",
            Phase::LongBreak => "long",
        }
    }

    /// The phase a state-file word names; None for any other word. It tries each phase's own
    /// `word()`, so the two functions cannot disagree.
    fn of_word(w: &str) -> Option<Phase> {
        [Phase::Work, Phase::ShortBreak, Phase::LongBreak].into_iter().find(|p| p.word() == w)
    }
}

/// Whether a timer runs or is paused, and the one number that goes with each.
///
/// While running, a countdown or a Pomodoro phase keeps the moment it ends, and a stopwatch the
/// moment it started. Keeping a moment, not a length, means time passing needs no update: the time
/// left is always "end minus now". When paused the moment is meaningless, so what is kept is a
/// length instead: the time left, or for a stopwatch the time elapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// The end (countdown, Pomodoro) or the start (stopwatch), in Unix milliseconds.
    Running(u64),
    /// The time left, or the time elapsed (stopwatch), in milliseconds.
    Paused(u64),
}

/// The timer's whole state. Only one timer exists at a time.
///
/// A `Timer` is a small plain value (`Copy`): a change never edits it in place, `apply` returns the
/// new timer and the caller keeps that one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Nothing runs. `done` is true from the end of a countdown or of a Pomodoro set until someone
    /// has looked at it: the chip then reads `done`.
    Idle {
        done: bool,
    },
    Countdown {
        run: Run,
        /// The length the countdown has in all (what `Add` lengthens).
        total: u64,
    },
    Pomodoro {
        run: Run,
        phase: Phase,
        /// The work round this phase belongs to, counted from 1.
        round: u8,
    },
    Stopwatch {
        run: Run,
    },
}

/// The Pomodoro's lengths, in milliseconds, and its rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lengths {
    pub work: u64,
    pub short: u64,
    pub long: u64,
    /// How many work sessions come before the long break.
    pub rounds: u8,
}

impl Lengths {
    /// The lengths of the `[timer]` section, which counts them in minutes.
    pub fn of(c: &TimerConfig) -> Lengths {
        // `saturating_mul` stops at the largest number instead of wrapping round on overflow.
        let ms = |minutes: u32| u64::from(minutes).saturating_mul(MINUTE_MS);
        Lengths { work: ms(c.work), short: ms(c.short_break), long: ms(c.long_break), rounds: c.rounds }
    }

    /// How long phase `p` lasts.
    pub fn phase(&self, p: Phase) -> u64 {
        match p {
            Phase::Work => self.work,
            Phase::ShortBreak => self.short,
            Phase::LongBreak => self.long,
        }
    }
}

/// What the user, a verb or the popup asks of the timer. (A verb is a word sent with
/// `alpheus-launch --timer`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Start a countdown of this many milliseconds (clamped to between one second and a day).
    Countdown(u64),
    Pomodoro,
    Stopwatch,
    Pause,
    Resume,
    /// Pause what runs, resume what is paused.
    Toggle,
    /// Lengthen a countdown by this many milliseconds, up to a day in all.
    Add(u64),
    /// A Pomodoro's next phase now.
    Skip,
    Stop,
    /// The end was seen: `done` clears.
    Seen,
}

/// What came to its end, for the notification, the sound and the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Countdown {
        total: u64,
    },
    /// A work session ended; `next` is the break that starts.
    WorkDone {
        next: Phase,
    },
    /// A break ended; `round` is the work round that starts.
    BreakDone {
        round: u8,
    },
    /// The long break ended: the whole set is over after `rounds` rounds.
    SetDone {
        rounds: u8,
    },
    /// The stopwatch reached its one-day cap and stopped itself.
    StopwatchCap,
}

/// How the chip is drawn, which the bar turns into colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipLook {
    Idle,
    Running,
    Paused,
    Done,
}

/// The chip is the timer's small block in the bar. It shows a glyph and, except at rest, a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerChip {
    pub glyph: char,
    pub text: Option<String>,
    pub look: ChipLook,
}

/// The phase after `phase` of `round`, and the round it belongs to. A work phase on the last round
/// goes to the long break, which closes the set. None after the long break: the set is over.
fn next_phase(phase: Phase, round: u8, rounds: u8) -> Option<(Phase, u8)> {
    match phase {
        Phase::Work if round >= rounds => Some((Phase::LongBreak, round)),
        Phase::Work => Some((Phase::ShortBreak, round)),
        Phase::ShortBreak => Some((Phase::Work, round.saturating_add(1))),
        Phase::LongBreak => None,
    }
}

/// The next instant a countdown's shown figure changes.
///
/// A countdown shows its time left rounded up to a whole `step` (a minute on the chip, a second in
/// the popup). That figure drops when the time left falls to one step fewer. Example: with `step`
/// one minute and 25 minutes left, the chip reads `25m`; it reads `24m` from the moment 24 minutes
/// are left, which is `end - 24 min`, and that is what this returns.
fn countdown_tick(end: u64, now: u64, step: u64) -> u64 {
    let left = end.saturating_sub(now);
    let steps_shown = left.div_ceil(step);
    let left_at_next_figure = steps_shown.saturating_sub(1).saturating_mul(step);
    end.saturating_sub(left_at_next_figure)
}

impl Timer {
    /// The timer after change `c`, at `now`. A change that has nothing to act on (a pause when
    /// idle, a skip outside a Pomodoro, an add outside a countdown) returns the timer unchanged.
    pub fn apply(self, c: Change, l: &Lengths, now: u64) -> Self {
        match c {
            // Starting a timer replaces whatever ran, and asks nothing first.
            Change::Countdown(ms) => {
                let total = ms.clamp(SECOND_MS, MAX_MS);
                Self::Countdown { run: Run::Running(now.saturating_add(total)), total }
            }
            Change::Pomodoro => Self::Pomodoro { run: Run::Running(now.saturating_add(l.work)), phase: Phase::Work, round: 1 },
            Change::Stopwatch => Self::Stopwatch { run: Run::Running(now) },
            Change::Pause => self.paused(now),
            Change::Resume => self.resumed(now),
            Change::Toggle if self.running() => self.paused(now),
            Change::Toggle => self.resumed(now),
            Change::Add(ms) => match self {
                Self::Countdown { run, total } => {
                    // The total never goes past a day: only the room left is added.
                    let add = ms.min(MAX_MS.saturating_sub(total));
                    let run = match run {
                        Run::Running(end) => Run::Running(end.saturating_add(add)),
                        Run::Paused(left) => Run::Paused(left.saturating_add(add)),
                    };
                    Self::Countdown { run, total: total.saturating_add(add) }
                }
                // Any other timer has nothing to lengthen: it comes back unchanged.
                other => other,
            },
            Change::Skip => match self {
                Self::Pomodoro { phase, round, .. } => match next_phase(phase, round, l.rounds) {
                    // The next phase starts now, running, even if the one skipped was paused.
                    Some((phase, round)) => Self::Pomodoro { run: Run::Running(now.saturating_add(l.phase(phase))), phase, round },
                    // Skipping the long break ends the set, with no `done` (a skip is silent).
                    None => Self::Idle { done: false },
                },
                other => other,
            },
            Change::Stop => Self::Idle { done: false },
            Change::Seen => match self {
                Self::Idle { done: true } => Self::Idle { done: false },
                other => other,
            },
        }
    }

    /// The timer paused at `now`: a running end (a moment) becomes the time left (a length).
    fn paused(self, now: u64) -> Self {
        match self {
            // The time left is cut to the total: a wall clock set backwards would otherwise make
            // it look longer than the countdown itself.
            Self::Countdown { run: Run::Running(end), total } => {
                Self::Countdown { run: Run::Paused(end.saturating_sub(now).min(total)), total }
            }
            Self::Pomodoro { run: Run::Running(end), phase, round } => {
                Self::Pomodoro { run: Run::Paused(end.saturating_sub(now).min(MAX_MS)), phase, round }
            }
            Self::Stopwatch { run: Run::Running(start) } => Self::Stopwatch { run: Run::Paused(now.saturating_sub(start).min(MAX_MS)) },
            // Already paused, or idle: nothing to pause.
            other => other,
        }
    }

    /// The timer resumed at `now`: the time left (a length) becomes a running end (a moment) again.
    fn resumed(self, now: u64) -> Self {
        match self {
            Self::Countdown { run: Run::Paused(left), total } => Self::Countdown { run: Run::Running(now.saturating_add(left)), total },
            Self::Pomodoro { run: Run::Paused(left), phase, round } => {
                Self::Pomodoro { run: Run::Running(now.saturating_add(left)), phase, round }
            }
            // A stopwatch resumes as if it had started `elapsed` ago, so it carries on from there.
            Self::Stopwatch { run: Run::Paused(elapsed) } => Self::Stopwatch { run: Run::Running(now.saturating_sub(elapsed)) },
            // Already running, or idle: nothing to resume.
            other => other,
        }
    }

    /// The timer once its end has passed at `now`, and what ended; unchanged when it has not.
    ///
    /// A late end (a laptop that slept through it) is applied once, and the next Pomodoro phase
    /// starts at `now`, not at the old end: otherwise a long sleep would make several phases end
    /// one after the other.
    pub fn due(self, l: &Lengths, now: u64) -> (Self, Option<End>) {
        match self {
            Self::Countdown { run: Run::Running(end), total } if now >= end => (Self::Idle { done: true }, Some(End::Countdown { total })),
            Self::Pomodoro { run: Run::Running(end), phase, round } if now >= end => match next_phase(phase, round, l.rounds) {
                Some((next, round)) => {
                    let t = Self::Pomodoro { run: Run::Running(now.saturating_add(l.phase(next))), phase: next, round };
                    let end = if phase == Phase::Work { End::WorkDone { next } } else { End::BreakDone { round } };
                    (t, Some(end))
                }
                // The long break ended: the set is over.
                None => (Self::Idle { done: true }, Some(End::SetDone { rounds: round })),
            },
            // The stopwatch stops itself, paused, a day after it started.
            Self::Stopwatch { run: Run::Running(start) } if now >= start.saturating_add(MAX_MS) => {
                (Self::Stopwatch { run: Run::Paused(MAX_MS) }, Some(End::StopwatchCap))
            }
            // A paused timer never ends, and nothing else has an end to pass.
            other => (other, None),
        }
    }

    /// The time left of a countdown (never above its total) or a Pomodoro phase (never above a
    /// day). None for the other timers.
    pub fn left(&self, now: u64) -> Option<u64> {
        let (run, cap) = match self {
            Self::Countdown { run, total } => (run, *total),
            Self::Pomodoro { run, .. } => (run, MAX_MS),
            _ => return None,
        };
        let left = match *run {
            Run::Running(end) => end.saturating_sub(now),
            Run::Paused(left) => left,
        };
        // The cap guards against a wall clock set backwards, which would make the time left grow.
        Some(left.min(cap))
    }

    /// The time a stopwatch has counted, at most a day. None for the other timers.
    pub fn elapsed(&self, now: u64) -> Option<u64> {
        match self {
            Self::Stopwatch { run: Run::Running(start) } => Some(now.saturating_sub(*start).min(MAX_MS)),
            Self::Stopwatch { run: Run::Paused(e) } => Some((*e).min(MAX_MS)),
            _ => None,
        }
    }

    pub fn running(&self) -> bool {
        matches!(
            self,
            Self::Countdown { run: Run::Running(_), .. }
                | Self::Pomodoro { run: Run::Running(_), .. }
                | Self::Stopwatch { run: Run::Running(_) }
        )
    }

    /// Whether the timer is paused. An idle timer is not paused: it is not started.
    pub fn paused_now(&self) -> bool {
        matches!(
            self,
            Self::Countdown { run: Run::Paused(_), .. }
                | Self::Pomodoro { run: Run::Paused(_), .. }
                | Self::Stopwatch { run: Run::Paused(_) }
        )
    }

    /// The kind of timer as a word, for log lines.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Idle { .. } => "idle",
            Self::Countdown { .. } => "countdown",
            Self::Pomodoro { .. } => "pomodoro",
            Self::Stopwatch { .. } => "stopwatch",
        }
    }

    /// The next instant the wiring must look at the timer: the end, the next change of the chip's
    /// text (the minute) or, with `each_second`, of the popup's seconds. `Some(now)` when the end
    /// has passed; None when nothing runs.
    ///
    /// This is what lets the timer sleep: the wiring arms one deadline at this instant and does
    /// nothing in between.
    pub fn next_change(&self, now: u64, each_second: bool) -> Option<u64> {
        let step = if each_second { SECOND_MS } else { MINUTE_MS };
        match self {
            Self::Countdown { run: Run::Running(end), .. } | Self::Pomodoro { run: Run::Running(end), .. } => {
                Some(if now >= *end { now } else { countdown_tick(*end, now, step) })
            }
            Self::Stopwatch { run: Run::Running(start) } => {
                let cap = start.saturating_add(MAX_MS);
                if now >= cap {
                    return Some(now);
                }
                // A stopwatch's figure is rounded down: it changes at each whole step after the
                // start. The cap is an event of its own, so the answer never goes past it.
                let elapsed = now.saturating_sub(*start);
                let next = start.saturating_add((elapsed / step).saturating_add(1).saturating_mul(step));
                Some(next.min(cap))
            }
            // Idle and paused timers change nothing by themselves: no deadline.
            _ => None,
        }
    }

    /// What the bar draws, at `now`; None when the chip is hidden.
    pub fn chip(&self, now: u64, idle: TimerIdle) -> Option<TimerChip> {
        // For a timer that runs or is paused: its own glyph, and its figure in minutes. A
        // countdown or a Pomodoro phase shows the minutes left rounded up (25:00 reads `25m` until
        // 24:00 is left), a stopwatch the minutes elapsed rounded down (`0m` for the first minute).
        let (mode_glyph, minutes) = match self {
            Self::Idle { done: true } => return Some(TimerChip { glyph: DONE, text: Some("done".into()), look: ChipLook::Done }),
            Self::Idle { done: false } => {
                return match idle {
                    TimerIdle::Glyph => Some(TimerChip { glyph: TIMER, text: None, look: ChipLook::Idle }),
                    TimerIdle::Hidden => None,
                };
            }
            Self::Countdown { .. } => (TIMER, self.left(now)?.div_ceil(MINUTE_MS)),
            Self::Pomodoro { phase, .. } => {
                let glyph = if *phase == Phase::Work { WORK } else { BREAK };
                (glyph, self.left(now)?.div_ceil(MINUTE_MS))
            }
            Self::Stopwatch { .. } => (STOPWATCH, self.elapsed(now)? / MINUTE_MS),
        };
        // A paused timer shows the pause glyph in place of its own.
        let paused = self.paused_now();
        let glyph = if paused { PAUSE } else { mode_glyph };
        let look = if paused { ChipLook::Paused } else { ChipLook::Running };
        Some(TimerChip { glyph, text: Some(minutes_text(minutes)), look })
    }

    /// The state file's line (see the timer spec, § 6); None while idle: nothing is kept, and
    /// `done` is not kept either.
    pub fn saved(&self) -> Option<String> {
        // Each arm writes the kind of timer, `running` or `paused`, then the numbers `parse_saved`
        // reads back in the same order. `{end}` puts the variable's value into the text.
        let line = match self {
            Self::Idle { .. } => return None,
            Self::Countdown { run: Run::Running(end), total } => format!("countdown running {end} {total}"),
            Self::Countdown { run: Run::Paused(left), total } => format!("countdown paused {left} {total}"),
            Self::Pomodoro { run: Run::Running(end), phase, round } => {
                format!("pomodoro running {end} {} {round}", phase.word())
            }
            Self::Pomodoro { run: Run::Paused(left), phase, round } => {
                format!("pomodoro paused {left} {} {round}", phase.word())
            }
            Self::Stopwatch { run: Run::Running(start) } => format!("stopwatch running {start}"),
            Self::Stopwatch { run: Run::Paused(elapsed) } => format!("stopwatch paused {elapsed}"),
        };
        Some(line)
    }

    /// A state file's text read back, treated as hostile (docs/decisions.md, "The state file is
    /// hostile input"): the file may be stale, edited by hand or not ours at all. Exact fields,
    /// digits only, and every bound of the spec: lengths from one second to a day, a running end
    /// at most 25 hours ahead, a stopwatch started ahead of now clamped to now, rounds 1 to 12.
    /// None for anything else.
    pub fn parse_saved(text: &str, now: u64) -> Option<Self> {
        // The text split on spaces. Each pattern below matches a list of exactly that many fields,
        // the first ones being fixed words; anything of another shape reaches the last arm, `_`.
        // `?` after a call that gives None ends the function with None: the text is refused.
        let fields: Vec<&str> = text.split_whitespace().collect();
        match fields.as_slice() {
            ["countdown", state, end_or_left, total] => {
                let (end_or_left, total) = (num(end_or_left)?, num(total)?);
                if !(SECOND_MS..=MAX_MS).contains(&total) {
                    return None;
                }
                let run = match *state {
                    "running" => Run::Running(believable_end(end_or_left, now)?),
                    "paused" if end_or_left <= total => Run::Paused(end_or_left),
                    _ => return None,
                };
                Some(Self::Countdown { run, total })
            }
            ["pomodoro", state, end_or_left, phase, round] => {
                let (end_or_left, phase, round) = (num(end_or_left)?, Phase::of_word(phase)?, num(round)?);
                let Ok(round) = u8::try_from(round) else { return None };
                if !(1..=ROUNDS_MAX).contains(&round) {
                    return None;
                }
                let run = match *state {
                    "running" => Run::Running(believable_end(end_or_left, now)?),
                    "paused" if end_or_left <= MAX_MS => Run::Paused(end_or_left),
                    _ => return None,
                };
                Some(Self::Pomodoro { run, phase, round })
            }
            ["stopwatch", "running", start] => {
                let start = num(start)?;
                // A start ahead of now (the clock was set back) is clamped to now, not refused.
                Some(Self::Stopwatch { run: Run::Running(start.min(now)) })
            }
            ["stopwatch", "paused", elapsed] => {
                let elapsed = num(elapsed)?;
                if elapsed > MAX_MS {
                    return None;
                }
                Some(Self::Stopwatch { run: Run::Paused(elapsed) })
            }
            _ => None,
        }
    }
}

/// A running end read from the state file, if it is believable: at most `SAVED_AHEAD_MS` after
/// `now`. An end in the past is fine (the end passed while the daemon was off, and is applied once
/// at start); one far in the future cannot be ours.
fn believable_end(end: u64, now: u64) -> Option<u64> {
    if end <= now.saturating_add(SAVED_AHEAD_MS) { Some(end) } else { None }
}

/// Plain decimal digits only (no sign, no space), as many as a u64 holds.
fn num(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A number of minutes as the chip writes it: `25m`, `1h05`.
pub fn minutes_text(minutes: u64) -> String {
    // `{:02}` pads to two digits, so five minutes past the hour reads `05`.
    if minutes < 60 { format!("{minutes}m") } else { format!("{}h{:02}", minutes / 60, minutes % 60) }
}

/// Seconds as a clock: `24:13`, `0:05`, `1:02:03`.
pub fn clock_text(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    // Under an hour the minutes are not padded: `0:05`, not `00:05`.
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{}:{s:02}", secs / 60) }
}

/// A length for a notification: `25 min`, `45 s`, `1 min 30 s`.
pub fn length_text(ms: u64) -> String {
    let secs = ms / SECOND_MS;
    // The pair is (whole minutes, seconds left over); each pattern says which parts to write.
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

/// The notification an end becomes (timer spec § 5, shaped like `battery_alert::incoming`).
///
/// Normal urgency, the notification server's own timeout, and one tag (`timer`) so that a
/// Pomodoro's next notice replaces the last while it is still on screen. None for the stopwatch's
/// cap, which says nothing.
pub fn incoming(end: End, l: &Lengths) -> Option<Incoming> {
    let (summary, body) = match end {
        End::Countdown { total } => ("Timer done".to_string(), length_text(total)),
        End::WorkDone { next: Phase::LongBreak } => ("Break time".into(), format!("Long break \u{b7} {}", length_text(l.long))),
        End::WorkDone { next } => ("Break time".into(), format!("Short break \u{b7} {}", length_text(l.phase(next)))),
        End::BreakDone { round } => ("Back to work".into(), format!("Round {round} of {} \u{b7} {}", l.rounds, length_text(l.work))),
        End::SetDone { rounds } => ("Pomodoro done".into(), format!("{rounds} {}", if rounds == 1 { "round" } else { "rounds" })),
        End::StopwatchCap => return None,
    };
    Some(Incoming {
        app_name: "alpheus".into(),
        replaces_id: 0,
        summary,
        body,
        actions: Vec::new(),
        urgency: Urgency::Normal,
        // -1 asks for the server's default timeout.
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: Some("timer".into()),
    })
}

// The timer's other two files are folders of their own, declared here: the wiring (`app.rs`) and
// the popup (`popup.rs`).
pub mod app;
pub mod popup;

#[cfg(test)]
mod tests;
