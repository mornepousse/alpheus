//! Shield mode's model (shield spec §§ 3 and 7): the state read from the flag file, its edges, and
//! what the bar and the quick panel read of it. Pure apart from `read_flag`: the commands, the
//! watch and the notices live in `shield_app.rs`, `shield_run.rs` and `watch.rs`.

use crate::config::ShieldConfig;
use crate::quick::FAILED_SHOWN;
use crate::shield_usb::{Parsed, UsbDevice};
use alpheus_commun::launcher_ipc::{MAX_SHIELD_BODY, Reply, ShieldVerb};
use std::path::Path;
use std::time::{Duration, Instant};

/// A command (and its attempt) may take this long: long enough to type a password.
pub const ACTION_DEADLINE: Duration = Duration::from_secs(60);
/// After an exit 0, the flag has this long to reach its target.
pub const FLAG_GRACE: Duration = Duration::from_secs(5);

/// What a worker thread runs.
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
    let (s, b) = match kind {
        NoticeKind::On => ("Shield on", config.on_body()),
        NoticeKind::Off => ("Shield off", config.off_body()),
        NoticeKind::SwitchFailed => ("Shield unchanged", FAILED),
        NoticeKind::Allowed(name) => return ("USB device allowed".into(), name.clone()),
        NoticeKind::AllowFailed => ("USB device not allowed", FAILED),
        NoticeKind::NothingBlocked => ("No blocked USB device", ""),
    };
    (s.into(), b.into())
}

/// What `alpheus-launch --shield status` prints (shield spec § 9).
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
        Some(Err(r)) => out.push_str(&format!("\nBlocked USB devices: cannot list ({})", r.text())),
        Some(Ok(p)) if p.devices.is_empty() => out.push_str("\nBlocked USB devices: none"),
        Some(Ok(p)) => {
            out.push_str("\nBlocked USB devices:");
            // The reply has a cap (`MAX_SHIELD_BODY`, the client refuses more): lines that would pass
            // it, keeping room for the cut's own line, are left out and counted.
            const MORE_ROOM: usize = 32;
            for (i, d) in p.devices.iter().enumerate() {
                let line = format!("\n  {}", crate::shield_usb::picker_line(d));
                if out.len() + line.len() + MORE_ROOM > MAX_SHIELD_BODY {
                    out.push_str(&format!("\n  \u{2026} {} more", p.devices.len() - i));
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

/// What the daemon does for a verb (spec § 9), decided without side effects.
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

/// The picker's prompt (spec § 9).
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

pub fn plan_verb(verb: ShieldVerb, c: &VerbCtx) -> VerbPlan {
    let error = |t: &str| VerbPlan::Answer(Reply::Error(t.into()));
    let switch = |target: bool| match c.state {
        None => error(ERR_UNKNOWN),
        // The asked state already holds: nothing runs (spec decision 9).
        Some(on) if on == target => VerbPlan::Answer(Reply::Done),
        Some(_) if c.action_busy => error(ERR_BUSY),
        Some(_) => VerbPlan::Switch(target),
    };
    match verb {
        ShieldVerb::On => switch(true),
        ShieldVerb::Off => switch(false),
        ShieldVerb::Toggle => match c.state {
            None => error(ERR_UNKNOWN),
            Some(_) if c.action_busy => error(ERR_BUSY),
            Some(on) => VerbPlan::Switch(!on),
        },
        ShieldVerb::Status => match (c.state, c.can_list) {
            (Some(true), true) => VerbPlan::StatusRead,
            (state, _) => VerbPlan::Answer(Reply::Shield(status_text(state, None))),
        },
        ShieldVerb::Allow => match c.state {
            None => error(ERR_UNKNOWN),
            Some(false) => error(ERR_OFF),
            Some(true) if !(c.can_allow && c.can_list) => error(ERR_NO_ALLOW),
            Some(true) => VerbPlan::AllowRead,
        },
    }
}

/// The picker's lines and, in the same order, the device ids they stand for (the ids never reach
/// the box: a chosen position maps back to one).
pub fn picker(devices: &[UsbDevice]) -> (Vec<String>, Vec<(u32, String)>) {
    (devices.iter().map(crate::shield_usb::picker_line).collect(), devices.iter().map(|d| (d.id, d.name.clone())).collect())
}

/// The name the notice gives a device the panel allows (the listing it was picked from); `?` when
/// that listing no longer has it.
pub fn device_name(listing: Option<&Result<Parsed, Reason>>, id: u32) -> String {
    let found = listing.and_then(|l| l.as_ref().ok()).and_then(|p| p.devices.iter().find(|d| d.id == id));
    found.map_or_else(|| "?".to_string(), |d| d.name.clone())
}

/// The id and name a picker's answer stands for: a chosen position only. Typed text (`Index(None)`), a
/// line's text, Escape or an out-of-range position run nothing.
pub fn picked(ids: &[(u32, String)], reply: Reply) -> Option<(u32, String)> {
    match reply {
        Reply::Index(Some(i)) => ids.get(i).cloned(),
        _ => None,
    }
}

/// A switch waiting for its flag.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    kind: AttemptKind,
    generation: u64,
    started: Instant,
    /// The command exited 0: the flag has until then.
    grace_end: Option<Instant>,
    /// The output passed its cap: the command's outcome is unknown, the flag decides.
    capped: bool,
}

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
        Ok(m) => m.is_file().then_some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// A change between the two known states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    On,
    Off,
}

/// What the bar and the panel read: a `Copy` view kept up to date by `ShieldApp`.
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

#[derive(Debug, Default)]
pub struct Shield {
    state: Option<bool>,
    last_known: Option<bool>,
    /// The action slot: held from a command's start until its worker's `Done`, whatever its attempt's state.
    action_held: bool,
    read_held: bool,
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
            Some(before) if before != now => Some(if now { Edge::On } else { Edge::Off }),
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
    /// clears a shown `failed`: a late change is announced as any change.
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
        let target = !self.state?;
        if self.action_held {
            return None;
        }
        let generation = self.take_generation();
        self.action_held = true;
        self.failed_until = None;
        self.attempt = Some(Attempt { kind: AttemptKind::Switch(target), generation, started: now, grace_end: None, capped: false });
        Some((target, generation))
    }

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
        !std::mem::replace(&mut self.read_held, true)
    }

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
        let attempt = self.attempt.filter(|a| a.generation == generation)?;
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
        let a = self.attempt.filter(|a| matches!(a.kind, AttemptKind::Switch(_)))?;
        match a.grace_end {
            Some(g) => Some(g),
            None => a.started.checked_add(ACTION_DEADLINE),
        }
    }

    /// The deadline passed: the switch fails (the slot stays held until its worker reports).
    pub fn expired(&mut self, now: Instant) -> Option<Finish> {
        let a = self.attempt?;
        let AttemptKind::Switch(target) = a.kind else { return None };
        if now < self.deadline()? {
            return None;
        }
        let reason = if a.grace_end.is_some() {
            Reason::NoChangeAfter
        } else if a.capped {
            Reason::OutputTooLong
        } else {
            Reason::NoChangeIn60
        };
        Some(self.fail_switch(target, reason, now))
    }

    /// Clears a `failed` whose 4 s ended; true when it did.
    pub fn due(&mut self, now: Instant) -> bool {
        if self.failed_until.is_some_and(|t| now >= t) {
            self.failed_until = None;
            return true;
        }
        false
    }

    pub fn view(&self, config: &ShieldConfig, now: Instant) -> ShieldView {
        ShieldView {
            state: self.state,
            switching: self.switching(),
            failed_until: self.failed_until.filter(|t| *t > now),
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

#[cfg(test)]
mod tests;
