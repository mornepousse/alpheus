//! Shield mode's model: the state read from the flag file, its edges, and what the bar and the quick
//! panel read of it.
//!
//! Shield mode is a security mode for public networks whose root work is done by commands the user
//! configures (`[shield]` in `bar.toml`); alpheus shows the state, switches it, lists the blocked USB
//! devices and allows one. This file is the pure half of that: the state (`Shield`) with its edges
//! and the attempt of a switch waiting for its flag, the jobs and what becomes of them (`Job`,
//! `RunOutcome`, `ShieldMail`, `Finish`, `Reason`), the notices, the text `--shield status` prints,
//! and what each verb of `alpheus-launch --shield` asks for (`plan_verb`). The other half does the
//! talking to the outside: `app.rs` is the wiring on the daemon's state (the flag's re-reads, the
//! verbs, the timer, the notices), `run.rs` runs one command on a short-lived thread, `usb.rs`
//! reads USBGuard's text, and the flag's watch is in `watch.rs`.
//!
//! Never:
//! - read a clock: the caller always passes `now`;
//! - start a thread or run a command, or write a log line;
//! - panic: a guardrail test (`the_shield_files_have_no_panicking_path`) reads this file's code.
//!
//! The one thing read here is the flag itself, by `read_flag`, which tests whether the file exists.
//!
//! See: docs/specs/2026-11-21-alpheus-shield-design.md (§§ 3, 7, 8, 9), docs/features/shield.md, and
//! docs/decisions.md ("An unknown Shield state is never read as off", "A Shield switch succeeds
//! when the flag reaches its target", "A running Shield command is never killed at its deadline",
//! "A Shield verb for the state already held runs nothing").

use crate::config::ShieldConfig;
use crate::quick::FAILED_SHOWN;
use crate::shield::usb::{Parsed, UsbDevice};
use alpheus_common::launcher_ipc::{MAX_SHIELD_BODY, Reply, ShieldVerb};
use std::path::Path;
use std::time::{Duration, Instant};

/// A switch may wait this long for its flag, counted from the command's start: long enough to type
/// a password. It judges the switch only; the command itself is never killed at this deadline (see
/// docs/decisions.md, "A running Shield command is never killed at its deadline").
pub const ACTION_DEADLINE: Duration = Duration::from_secs(60);
/// After an exit 0, the flag has this long to reach its target. The 60 s bound never cuts it short
/// (a command that ended at 58 s still gets all of it).
pub const FLAG_GRACE: Duration = Duration::from_secs(5);

/// What a worker thread runs: one command or one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// `on` (true) or `off` (false).
    Switch(bool),
    /// `allow` for this device id.
    Allow(u32),
    /// `blocked`.
    List,
}

/// How a job ended, as the worker saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The command's exit code; None when a signal ended it.
    Exited(Option<i32>),
    TimedOut,
    /// The output passed its cap (the command was killed).
    Capped,
    NotFound,
    CouldNotStart,
    /// The worker panicked.
    WorkerFailed,
}

/// What a worker mails back to the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShieldMail {
    Done {
        job: Job,
        /// The attempt the job belongs to: a report for one that already ended only frees its slot.
        generation: u64,
        outcome: RunOutcome,
        /// A listing, parsed on the worker (bounded): the loop never handles raw output.
        listed: Option<Parsed>,
    },
}

impl ShieldMail {
    pub fn worker_failed(job: Job, generation: u64) -> ShieldMail {
        ShieldMail::Done { job, generation, outcome: RunOutcome::WorkerFailed, listed: None }
    }
}

/// Why a command or a read failed: the closed list of the log's `<reason>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Exit(i32),
    Killed,
    OutputTooLong,
    NoChangeAfter,
    NoChangeIn60,
    TimedOut,
    NotFound,
    CouldNotStart,
}

impl Reason {
    pub fn text(&self) -> String {
        match self {
            Reason::Exit(n) => format!("exit {n}"),
            Reason::Killed => "killed".into(),
            Reason::OutputTooLong => "output too long".into(),
            Reason::NoChangeAfter => "no change after the command".into(),
            Reason::NoChangeIn60 => "no change in 60 s".into(),
            Reason::TimedOut => "timed out".into(),
            Reason::NotFound => "program not found".into(),
            Reason::CouldNotStart => "could not start".into(),
        }
    }
}

/// How an attempt or a read ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    Switched(bool),
    SwitchFailed(bool, Reason),
    Allowed(u32, String),
    AllowFailed(u32, Reason),
    Listed(Parsed),
    ListFailed(Reason),
}

/// The notices' kinds (shield spec § 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeKind {
    On,
    Off,
    SwitchFailed,
    Allowed(String),
    AllowFailed,
    NothingBlocked,
}

/// A notice's summary and body.
pub fn notice(kind: &NoticeKind, config: &ShieldConfig) -> (String, String) {
    const FAILED: &str = "The command failed or was cancelled.";
    let (summary, body) = match kind {
        NoticeKind::On => ("Shield on", config.on_body()),
        NoticeKind::Off => ("Shield off", config.off_body()),
        NoticeKind::SwitchFailed => ("Shield unchanged", FAILED),
        // The one notice whose body is not a fixed text: the device's cleaned name.
        NoticeKind::Allowed(name) => return ("USB device allowed".into(), name.clone()),
        NoticeKind::AllowFailed => ("USB device not allowed", FAILED),
        NoticeKind::NothingBlocked => ("No blocked USB device", ""),
    };
    (summary.into(), body.into())
}

/// What `alpheus-launch --shield status` prints (shield spec § 9).
///
/// The devices are listed only while the mode is on (shield spec § 9: `status` reads the list only
/// while on, and when `blocked` is set).
pub fn status_text(state: Option<bool>, listing: Option<Result<&Parsed, Reason>>) -> String {
    let head = match state {
        Some(true) => "Shield: on",
        Some(false) => "Shield: off",
        None => return "Shield: unknown".into(),
    };
    let mut out = head.to_string();
    if state != Some(true) {
        return out;
    }
    match listing {
        None => {}
        Some(Err(why)) => out.push_str(&format!("\nBlocked USB devices: cannot list ({})", why.text())),
        Some(Ok(parsed)) if parsed.devices.is_empty() => out.push_str("\nBlocked USB devices: none"),
        Some(Ok(parsed)) => {
            out.push_str("\nBlocked USB devices:");
            // The reply has a cap (`MAX_SHIELD_BODY`, the client refuses more): lines that would pass
            // it, keeping room for the cut's own line, are left out and counted.
            const MORE_ROOM: usize = 32;
            for (i, device) in parsed.devices.iter().enumerate() {
                let line = format!("\n  {}", crate::shield::usb::picker_line(device));
                if out.len() + line.len() + MORE_ROOM > MAX_SHIELD_BODY {
                    out.push_str(&format!("\n  \u{2026} {} more", parsed.devices.len() - i));
                    break;
                }
                out.push_str(&line);
            }
        }
    }
    out
}

/// What the model knows when a verb arrives (`plan_verb`).
#[derive(Debug, Clone, Copy)]
pub struct VerbCtx {
    pub state: Option<bool>,
    /// An action runs (the slot is held).
    pub action_busy: bool,
    /// `blocked` is set.
    pub can_list: bool,
    /// `allow` is set.
    pub can_allow: bool,
}

/// What the daemon does for a verb (shield spec § 9), decided without side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerbPlan {
    /// Answer at once.
    Answer(Reply),
    /// Start the switch to this state, then answer Done.
    Switch(bool),
    /// Read the blocked devices, then print the status.
    StatusRead,
    /// Read the blocked devices, then open the picker.
    AllowRead,
}

/// The picker's prompt (shield spec § 9).
pub const PICKER_PROMPT: &str = "Allow a blocked USB device";
/// The words of a refusal (`shield: not started (<why>)` in the log, the client's error line): one
/// wording per condition, whoever asked (a verb, the panel, the picker).
pub const ERR_BUSY: &str = "a shield command is already running";
pub const ERR_UNKNOWN: &str = "the shield state is unknown";
pub const ERR_SAME: &str = "already in that state";
pub const ERR_NO_ALLOW: &str = "allow is not configured";
pub const ERR_OFF: &str = "the shield is off";
pub const ERR_NOT_CONFIGURED: &str = "shield is not configured in bar.toml";
/// Every word a refusal may carry (the log's closed list).
pub const ERR_ALL: [&str; 6] = [ERR_BUSY, ERR_UNKNOWN, ERR_SAME, ERR_NO_ALLOW, ERR_OFF, ERR_NOT_CONFIGURED];

/// A refusal: the client hears `why` as an error.
fn refusal(why: &str) -> VerbPlan {
    VerbPlan::Answer(Reply::Error(why.into()))
}

/// The plan for `on` or `off`: nothing runs when the state asked for is already held (see
/// docs/decisions.md, "A Shield verb for the state already held runs nothing").
fn plan_switch(target: bool, context: &VerbCtx) -> VerbPlan {
    match context.state {
        None => refusal(ERR_UNKNOWN),
        Some(on) if on == target => VerbPlan::Answer(Reply::Done),
        Some(_) if context.action_busy => refusal(ERR_BUSY),
        Some(_) => VerbPlan::Switch(target),
    }
}

/// What the daemon does for `verb`, from what the model knows (`context`).
pub fn plan_verb(verb: ShieldVerb, context: &VerbCtx) -> VerbPlan {
    match verb {
        ShieldVerb::On => plan_switch(true, context),
        ShieldVerb::Off => plan_switch(false, context),
        ShieldVerb::Toggle => match context.state {
            None => refusal(ERR_UNKNOWN),
            Some(_) if context.action_busy => refusal(ERR_BUSY),
            Some(on) => VerbPlan::Switch(!on),
        },
        // The devices are read only while the mode is on and `blocked` is set; otherwise the state
        // alone is printed.
        ShieldVerb::Status => match (context.state, context.can_list) {
            (Some(true), true) => VerbPlan::StatusRead,
            (state, _) => VerbPlan::Answer(Reply::Shield(status_text(state, None))),
        },
        ShieldVerb::Allow => match context.state {
            None => refusal(ERR_UNKNOWN),
            Some(false) => refusal(ERR_OFF),
            Some(true) if !(context.can_allow && context.can_list) => refusal(ERR_NO_ALLOW),
            Some(true) => VerbPlan::AllowRead,
        },
    }
}

/// The picker's lines and, in the same order, the device ids they stand for (the ids never reach
/// the box: a chosen position maps back to one).
pub fn picker(devices: &[UsbDevice]) -> (Vec<String>, Vec<(u32, String)>) {
    (devices.iter().map(crate::shield::usb::picker_line).collect(), devices.iter().map(|d| (d.id, d.name.clone())).collect())
}

/// The name the notice gives a device the panel allows (the listing it was picked from); `?` when
/// that listing no longer has it.
pub fn device_name(listing: Option<&Result<Parsed, Reason>>, id: u32) -> String {
    let Some(Ok(parsed)) = listing else { return "?".to_string() };
    for device in &parsed.devices {
        if device.id == id {
            return device.name.clone();
        }
    }
    "?".to_string()
}

/// The id and name a picker's answer stands for: a chosen position only. Typed text (`Index(None)`), a
/// line's text, Escape or an out-of-range position run nothing.
pub fn picked(ids: &[(u32, String)], reply: Reply) -> Option<(u32, String)> {
    match reply {
        Reply::Index(Some(i)) => ids.get(i).cloned(),
        _ => None,
    }
}

/// A switch or an allow in progress. A switch ends on the flag (or its deadline), an allow on its
/// command's exit.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    kind: AttemptKind,
    /// Its number, given by `take_generation`; the worker's report carries it back.
    generation: u64,
    started: Instant,
    /// The command exited 0: the flag has until then.
    grace_end: Option<Instant>,
    /// The output passed its cap: the command's outcome is unknown, the flag decides.
    capped: bool,
}

/// What an attempt is for: switching to a state, or allowing a device by id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptKind {
    Switch(bool),
    Allow(u32),
}

/// The flag's existence: `Some(true)` while it exists, `Some(false)` when it does not, None when it
/// cannot be told (any error but "not found": never "off" on a guess).
pub fn read_flag(flag: &Path) -> Option<bool> {
    // `metadata` follows a link: a regular file (or a link to one) is on; nothing there, or a dangling
    // link, is off; a directory or anything else there, or an error, is unknown (never "on" by a guess).
    match std::fs::metadata(flag) {
        Ok(metadata) => {
            if metadata.is_file() {
                Some(true)
            } else {
                None
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// A change between the two known states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    On,
    Off,
}

/// What the bar and the panel read: a `Copy` view kept up to date by `ShieldApp`. It is `Copy`
/// because the quick panel's `Live`, which derives `Copy`, holds it (shield plan 1, "Decisions
/// written down", item 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShieldView {
    pub state: Option<bool>,
    /// A switch waits for its flag.
    pub switching: bool,
    /// The row reads `failed` until then.
    pub failed_until: Option<Instant>,
    /// `blocked` is set: the detail exists.
    pub can_list: bool,
    /// `allow` is set: a device can be allowed.
    pub can_allow: bool,
}

/// The Shield state and its bookkeeping: what the flag last said, the one action slot and the one
/// read slot, and the switch or allow waiting for its end.
#[derive(Debug, Default)]
pub struct Shield {
    state: Option<bool>,
    /// The last state that was not unknown: an edge is a change from this one.
    last_known: Option<bool>,
    /// The action slot: held from a command's start until its worker's `Done`, whatever its attempt's state.
    action_held: bool,
    /// The read slot: held from a listing's start until its worker's `Done`.
    read_held: bool,
    /// The last number given to an attempt (see `Attempt::generation`).
    next_generation: u64,
    attempt: Option<Attempt>,
    /// The name of the device the live allow targets, taken when it began (the listing may be gone by its end).
    allow_name: Option<String>,
    failed_until: Option<Instant>,
    /// The unknown spell already logged.
    unknown_noted: bool,
}

impl Shield {
    pub fn state(&self) -> Option<bool> {
        self.state
    }

    /// A reading of the flag. The first known one seeds (`last_known` empty: no edge); afterwards a known state
    /// turning into the other known state is an edge; unknown in between changes nothing (`on,
    /// unknown, on` is no change, `on, unknown, off` is one).
    pub fn observe(&mut self, now: Option<bool>) -> Option<Edge> {
        self.state = now;
        let now = now?;
        self.unknown_noted = false;
        let edge = match self.last_known {
            Some(before) if before != now => {
                if now {
                    Some(Edge::On)
                } else {
                    Some(Edge::Off)
                }
            }
            _ => None,
        };
        self.last_known = Some(now);
        edge
    }

    /// True once per unknown spell, for the log line.
    pub fn first_unknown(&mut self) -> bool {
        if self.state.is_some() || self.unknown_noted {
            return false;
        }
        self.unknown_noted = true;
        true
    }

    /// A reading of the flag with its consequences: the edge, and the attempt it ends (a switch
    /// succeeds when the flag reaches its target, before or after the command's exit). Any edge
    /// clears a shown `failed`: a late change is announced as any change. `_now` is unused (reason
    /// not recorded).
    pub fn flag(&mut self, state: Option<bool>, _now: Instant) -> (Option<Edge>, Option<Finish>) {
        let edge = self.observe(state);
        if edge.is_some() {
            self.failed_until = None;
        }
        let finish = match (self.attempt, state) {
            (Some(Attempt { kind: AttemptKind::Switch(target), .. }), Some(now_on)) if now_on == target => {
                self.attempt = None;
                Some(Finish::Switched(target))
            }
            _ => None,
        };
        (edge, finish)
    }

    /// Starts a switch toward the other state; None when the state is unknown or a command holds the slot.
    pub fn begin_switch(&mut self, now: Instant) -> Option<(bool, u64)> {
        // `?` ends the function with None when the state is unknown.
        let on = self.state?;
        let target = !on;
        if self.action_held {
            return None;
        }
        let generation = self.take_generation();
        self.action_held = true;
        self.failed_until = None;
        self.attempt = Some(Attempt { kind: AttemptKind::Switch(target), generation, started: now, grace_end: None, capped: false });
        Some((target, generation))
    }

    /// Starts an allow of device `id`, named `name` in the notice: None when a command holds the slot.
    pub fn begin_allow(&mut self, id: u32, name: String, now: Instant) -> Option<u64> {
        if self.action_held {
            return None;
        }
        let generation = self.take_generation();
        self.action_held = true;
        self.allow_name = Some(name);
        self.attempt = Some(Attempt { kind: AttemptKind::Allow(id), generation, started: now, grace_end: None, capped: false });
        Some(generation)
    }

    /// False: a read already runs (the asker joins it).
    pub fn begin_list(&mut self) -> bool {
        if self.read_held {
            return false;
        }
        self.read_held = true;
        true
    }

    /// The next attempt's number: a report carries it back, so one for an attempt that already ended
    /// is told from the live one.
    fn take_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    /// A command is running (or a setuid helper the deadline cannot kill still is).
    pub fn action_busy(&self) -> bool {
        self.action_held
    }

    /// A switch waits for its flag.
    pub fn switching(&self) -> bool {
        matches!(self.attempt, Some(Attempt { kind: AttemptKind::Switch(_), .. }))
    }

    /// A worker reported back: its slot is free; the result when it is the live attempt's (a report
    /// for an attempt that already ended only frees the slot).
    pub fn done(&mut self, mail: ShieldMail, now: Instant) -> Option<Finish> {
        let ShieldMail::Done { job, generation, outcome, listed } = mail;
        if job == Job::List {
            self.read_held = false;
            return Some(match (outcome, listed) {
                (RunOutcome::Exited(Some(0)), Some(p)) => Finish::Listed(p),
                (outcome, _) => Finish::ListFailed(reason_of(outcome, false)),
            });
        }
        self.action_held = false;
        // A report for an attempt that is no longer the live one only freed the slot, above.
        let attempt = self.attempt?;
        if attempt.generation != generation {
            return None;
        }
        match attempt.kind {
            AttemptKind::Allow(id) => {
                self.attempt = None;
                let name = self.allow_name.take().unwrap_or_else(|| "?".into());
                Some(match outcome {
                    RunOutcome::Exited(Some(0)) => Finish::Allowed(id, name),
                    other => Finish::AllowFailed(id, reason_of(other, false)),
                })
            }
            AttemptKind::Switch(target) => match outcome {
                RunOutcome::Exited(Some(0)) => {
                    self.attempt = Some(Attempt { grace_end: now.checked_add(FLAG_GRACE), ..attempt });
                    None
                }
                RunOutcome::Capped => {
                    // The command ended, outcome unknown: the flag decides until the bound.
                    self.attempt = Some(Attempt { capped: true, ..attempt });
                    None
                }
                other => Some(self.fail_switch(target, reason_of(other, true), now)),
            },
        }
    }

    fn fail_switch(&mut self, target: bool, reason: Reason, now: Instant) -> Finish {
        self.attempt = None;
        self.failed_until = now.checked_add(FAILED_SHOWN);
        Finish::SwitchFailed(target, reason)
    }

    /// When a waiting switch must be judged: the grace's end once the command exited 0 (the bound
    /// does not cut it short: a command that ended at 58 s still gets its 5 s), else the 60 s bound.
    pub fn deadline(&self) -> Option<Instant> {
        let attempt = self.attempt?;
        if !matches!(attempt.kind, AttemptKind::Switch(_)) {
            return None;
        }
        match attempt.grace_end {
            Some(grace_end) => Some(grace_end),
            None => attempt.started.checked_add(ACTION_DEADLINE),
        }
    }

    /// The deadline passed: the switch fails (the slot stays held until its worker reports).
    pub fn expired(&mut self, now: Instant) -> Option<Finish> {
        let attempt = self.attempt?;
        let AttemptKind::Switch(target) = attempt.kind else { return None };
        let deadline = self.deadline()?;
        if now < deadline {
            return None;
        }
        let reason = if attempt.grace_end.is_some() {
            Reason::NoChangeAfter
        } else if attempt.capped {
            Reason::OutputTooLong
        } else {
            Reason::NoChangeIn60
        };
        Some(self.fail_switch(target, reason, now))
    }

    /// Clears a `failed` whose 4 s ended; true when it did.
    pub fn due(&mut self, now: Instant) -> bool {
        if let Some(until) = self.failed_until {
            if now >= until {
                self.failed_until = None;
                return true;
            }
        }
        false
    }

    /// What the bar and the panel read at `now`; a `failed` whose time has passed is not shown.
    pub fn view(&self, config: &ShieldConfig, now: Instant) -> ShieldView {
        let failed_until = match self.failed_until {
            Some(until) if until > now => Some(until),
            _ => None,
        };
        ShieldView {
            state: self.state,
            switching: self.switching(),
            failed_until,
            can_list: !config.blocked_argv().is_empty(),
            can_allow: !config.allow.is_empty(),
        }
    }
}

/// A job's end as the reason it failed; `switch`: a cap is not a failure of its own (the flag decides).
fn reason_of(outcome: RunOutcome, switch: bool) -> Reason {
    match outcome {
        RunOutcome::Exited(Some(n)) => Reason::Exit(n),
        RunOutcome::Exited(None) => Reason::Killed,
        RunOutcome::TimedOut if switch => Reason::NoChangeIn60,
        RunOutcome::TimedOut => Reason::TimedOut,
        RunOutcome::Capped => Reason::OutputTooLong,
        RunOutcome::NotFound => Reason::NotFound,
        RunOutcome::CouldNotStart | RunOutcome::WorkerFailed => Reason::CouldNotStart,
    }
}

pub mod app;
pub mod run;
pub mod usb;

#[cfg(test)]
mod tests;
