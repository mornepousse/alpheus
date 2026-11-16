//! The focus timer's model (timer spec): a countdown, a Pomodoro or a stopwatch, every change
//! made to it, the end it comes to, the chip it draws, when the chip next changes, the state
//! file's line and the notification an end becomes. Pure: times are wall-clock Unix milliseconds
//! handed in by the wiring (`timer_app.rs`), nothing here reads a clock, a file or starts anything.

use crate::config::{TimerConfig, TimerIdle};
use crate::notify::{Incoming, Urgency};
use crate::segments::PAUSE;

pub const SECOND_MS: u64 = 1_000;
pub const MINUTE_MS: u64 = 60_000;
/// The longest countdown and the stopwatch's cap: a day.
pub const MAX_MS: u64 = 86_400_000;
/// A running end further than this from now is not believed when read back from the file.
const SAVED_AHEAD_MS: u64 = 25 * 3_600_000;
/// The most Pomodoro rounds (`[timer] rounds`).
const ROUNDS_MAX: u8 = 12;

// Material Design glyphs of the Nerd Font.
pub const TIMER: char = '\u{f051b}'; // md-timer_outline
pub const WORK: char = '\u{f051f}'; // md-timer_sand
pub const BREAK: char = '\u{f0176}'; // md-coffee
pub const STOPWATCH: char = '\u{f13ab}'; // md-timer
pub const DONE: char = '\u{f009e}'; // md-bell_ring

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Work,
    ShortBreak,
    LongBreak,
}

impl Phase {
    /// The word the state file keeps.
    pub fn word(self) -> &'static str {
        match self {
            Phase::Work => "work",
            Phase::ShortBreak => "short",
            Phase::LongBreak => "long",
        }
    }

    fn of_word(w: &str) -> Option<Phase> {
        [Phase::Work, Phase::ShortBreak, Phase::LongBreak].into_iter().find(|p| p.word() == w)
    }
}

/// `Running(at)`: the end (countdown, Pomodoro) or the start (stopwatch). `Paused(ms)`: the time
/// left, or the time elapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    Running(u64),
    Paused(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Nothing runs; `done` while the end of a countdown or a set has not been seen.
    Idle {
        done: bool,
    },
    Countdown {
        run: Run,
        total: u64,
    },
    Pomodoro {
        run: Run,
        phase: Phase,
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
    pub rounds: u8,
}

impl Lengths {
    pub fn of(c: &TimerConfig) -> Lengths {
        let ms = |minutes: u32| u64::from(minutes).saturating_mul(MINUTE_MS);
        Lengths { work: ms(c.work), short: ms(c.short_break), long: ms(c.long_break), rounds: c.rounds }
    }

    pub fn phase(&self, p: Phase) -> u64 {
        match p {
            Phase::Work => self.work,
            Phase::ShortBreak => self.short,
            Phase::LongBreak => self.long,
        }
    }
}

/// What the user, a verb or the popup asks of the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Start a countdown of this many milliseconds (clamped to one second to a day).
    Countdown(u64),
    Pomodoro,
    Stopwatch,
    Pause,
    Resume,
    Toggle,
    /// Lengthen a countdown by this many milliseconds, up to a day in all.
    Add(u64),
    /// A Pomodoro's next phase now.
    Skip,
    Stop,
    /// The end was seen: `done` clears.
    Seen,
}

/// What came to its end, for the notification and the sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Countdown {
        total: u64,
    },
    WorkDone {
        next: Phase,
    },
    /// A break ended; `round` is the work round that starts.
    BreakDone {
        round: u8,
    },
    SetDone {
        rounds: u8,
    },
    StopwatchCap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipLook {
    Idle,
    Running,
    Paused,
    Done,
}

/// What the bar draws: a glyph and, but at rest, a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerChip {
    pub glyph: char,
    pub text: Option<String>,
    pub look: ChipLook,
}

/// The phase after `phase` of `round` (a work phase on the last round goes to the long break);
/// None after the long break: the set is over.
fn next_phase(phase: Phase, round: u8, rounds: u8) -> Option<(Phase, u8)> {
    match phase {
        Phase::Work if round >= rounds => Some((Phase::LongBreak, round)),
        Phase::Work => Some((Phase::ShortBreak, round)),
        Phase::ShortBreak => Some((Phase::Work, round.saturating_add(1))),
        Phase::LongBreak => None,
    }
}

/// The next time a value counted down by `step` from `left` reads differently: the instant its
/// ceiling in steps drops by one.
fn countdown_tick(end: u64, now: u64, step: u64) -> u64 {
    let left = end.saturating_sub(now);
    end.saturating_sub((left.div_ceil(step).saturating_sub(1)).saturating_mul(step))
}

impl Timer {
    /// The timer after `c`, at `now`. A change that has nothing to act on (a pause when idle, a
    /// skip outside a Pomodoro, an add outside a countdown) returns the timer unchanged.
    pub fn apply(self, c: Change, l: &Lengths, now: u64) -> Self {
        match c {
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
                    let add = ms.min(MAX_MS.saturating_sub(total));
                    let run = match run {
                        Run::Running(end) => Run::Running(end.saturating_add(add)),
                        Run::Paused(left) => Run::Paused(left.saturating_add(add)),
                    };
                    Self::Countdown { run, total: total.saturating_add(add) }
                }
                other => other,
            },
            Change::Skip => match self {
                Self::Pomodoro { phase, round, .. } => match next_phase(phase, round, l.rounds) {
                    Some((phase, round)) => Self::Pomodoro { run: Run::Running(now.saturating_add(l.phase(phase))), phase, round },
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

    fn paused(self, now: u64) -> Self {
        match self {
            Self::Countdown { run: Run::Running(end), total } => {
                Self::Countdown { run: Run::Paused(end.saturating_sub(now).min(total)), total }
            }
            Self::Pomodoro { run: Run::Running(end), phase, round } => {
                Self::Pomodoro { run: Run::Paused(end.saturating_sub(now).min(MAX_MS)), phase, round }
            }
            Self::Stopwatch { run: Run::Running(start) } => Self::Stopwatch { run: Run::Paused(now.saturating_sub(start).min(MAX_MS)) },
            other => other,
        }
    }

    fn resumed(self, now: u64) -> Self {
        match self {
            Self::Countdown { run: Run::Paused(left), total } => Self::Countdown { run: Run::Running(now.saturating_add(left)), total },
            Self::Pomodoro { run: Run::Paused(left), phase, round } => {
                Self::Pomodoro { run: Run::Running(now.saturating_add(left)), phase, round }
            }
            Self::Stopwatch { run: Run::Paused(elapsed) } => Self::Stopwatch { run: Run::Running(now.saturating_sub(elapsed)) },
            other => other,
        }
    }

    /// The timer once its end has passed at `now`, and what ended; unchanged when it has not.
    /// A late end is applied once and the next Pomodoro phase starts at `now`, not at the old end.
    pub fn due(self, l: &Lengths, now: u64) -> (Self, Option<End>) {
        match self {
            Self::Countdown { run: Run::Running(end), total } if now >= end => (Self::Idle { done: true }, Some(End::Countdown { total })),
            Self::Pomodoro { run: Run::Running(end), phase, round } if now >= end => match next_phase(phase, round, l.rounds) {
                Some((next, round)) => {
                    let t = Self::Pomodoro { run: Run::Running(now.saturating_add(l.phase(next))), phase: next, round };
                    let end = if phase == Phase::Work { End::WorkDone { next } } else { End::BreakDone { round } };
                    (t, Some(end))
                }
                None => (Self::Idle { done: true }, Some(End::SetDone { rounds: round })),
            },
            Self::Stopwatch { run: Run::Running(start) } if now >= start.saturating_add(MAX_MS) => {
                (Self::Stopwatch { run: Run::Paused(MAX_MS) }, Some(End::StopwatchCap))
            }
            other => (other, None),
        }
    }

    /// The time left of a countdown (never above its total) or a Pomodoro phase (never above a day).
    pub fn left(&self, now: u64) -> Option<u64> {
        let (run, cap) = match self {
            Self::Countdown { run, total } => (run, *total),
            Self::Pomodoro { run, .. } => (run, MAX_MS),
            _ => return None,
        };
        Some(
            match *run {
                Run::Running(end) => end.saturating_sub(now),
                Run::Paused(left) => left,
            }
            .min(cap),
        )
    }

    /// The time a stopwatch has counted, at most a day.
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
                let elapsed = now.saturating_sub(*start);
                let next = start.saturating_add((elapsed / step).saturating_add(1).saturating_mul(step));
                Some(next.min(cap))
            }
            _ => None,
        }
    }

    /// What the bar draws, at `now`; None when the chip is hidden.
    pub fn chip(&self, now: u64, idle: TimerIdle) -> Option<TimerChip> {
        let chip = |glyph, minutes, look| TimerChip { glyph, text: Some(minutes_text(minutes)), look };
        let look = |paused: bool| if paused { ChipLook::Paused } else { ChipLook::Running };
        let paused = matches!(
            self,
            Self::Countdown { run: Run::Paused(_), .. }
                | Self::Pomodoro { run: Run::Paused(_), .. }
                | Self::Stopwatch { run: Run::Paused(_) }
        );
        let glyph = |g| if paused { PAUSE } else { g };
        match self {
            Self::Idle { done: true } => Some(TimerChip { glyph: DONE, text: Some("done".into()), look: ChipLook::Done }),
            Self::Idle { done: false } => match idle {
                TimerIdle::Glyph => Some(TimerChip { glyph: TIMER, text: None, look: ChipLook::Idle }),
                TimerIdle::Hidden => None,
            },
            Self::Countdown { .. } => Some(chip(glyph(TIMER), self.left(now)?.div_ceil(MINUTE_MS), look(paused))),
            Self::Pomodoro { phase, .. } => {
                let g = if *phase == Phase::Work { WORK } else { BREAK };
                Some(chip(glyph(g), self.left(now)?.div_ceil(MINUTE_MS), look(paused)))
            }
            Self::Stopwatch { .. } => Some(chip(glyph(STOPWATCH), self.elapsed(now)? / MINUTE_MS, look(paused))),
        }
    }

    /// The state file's line (timer spec § 6); None while idle (`done` is not kept).
    pub fn saved(&self) -> Option<String> {
        Some(match self {
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
        })
    }

    /// A state file's text read back, treated as hostile: exact fields, digits only, every bound
    /// of the spec (lengths one second to a day, a running end at most 25 hours ahead, a stopwatch
    /// started ahead of now is clamped to now, rounds 1 to 12). None for anything else.
    pub fn parse_saved(text: &str, now: u64) -> Option<Self> {
        let f: Vec<&str> = text.split_whitespace().collect();
        let ahead = |end: u64| (end <= now.saturating_add(SAVED_AHEAD_MS)).then_some(end);
        match f.as_slice() {
            ["countdown", state, a, b] => {
                let (a, total) = (num(a)?, num(b)?);
                if !(SECOND_MS..=MAX_MS).contains(&total) {
                    return None;
                }
                let run = match *state {
                    "running" => Run::Running(ahead(a)?),
                    "paused" if a <= total => Run::Paused(a),
                    _ => return None,
                };
                Some(Self::Countdown { run, total })
            }
            ["pomodoro", state, a, phase, round] => {
                let (a, phase, round) = (num(a)?, Phase::of_word(phase)?, num(round)?);
                let round = u8::try_from(round).ok().filter(|r| (1..=ROUNDS_MAX).contains(r))?;
                let run = match *state {
                    "running" => Run::Running(ahead(a)?),
                    "paused" if a <= MAX_MS => Run::Paused(a),
                    _ => return None,
                };
                Some(Self::Pomodoro { run, phase, round })
            }
            ["stopwatch", "running", start] => {
                let start = num(start)?;
                Some(Self::Stopwatch { run: Run::Running(start.min(now)) })
            }
            ["stopwatch", "paused", elapsed] => {
                let elapsed = num(elapsed)?;
                (elapsed <= MAX_MS).then_some(Self::Stopwatch { run: Run::Paused(elapsed) })
            }
            _ => None,
        }
    }
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
    if minutes < 60 { format!("{minutes}m") } else { format!("{}h{:02}", minutes / 60, minutes % 60) }
}

/// Seconds as a clock: `24:13`, `0:05`, `1:02:03`.
pub fn clock_text(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{}:{s:02}", secs / 60) }
}

/// A length for a notification: `25 min`, `45 s`, `1 min 30 s`.
pub fn length_text(ms: u64) -> String {
    let secs = ms / SECOND_MS;
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

/// The notification an end becomes (timer spec § 5, shaped like `battery_alert::incoming`): normal
/// urgency, the server's timeout, one tag so a Pomodoro's next notice replaces the last. None for
/// the stopwatch's cap, which says nothing.
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
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: Some("timer".into()),
    })
}

#[cfg(test)]
mod tests;
