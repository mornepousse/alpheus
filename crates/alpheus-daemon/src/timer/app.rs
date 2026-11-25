//! The focus timer's wiring: everything that ties the pure timer (`mod.rs`) to the running daemon.
//!
//! Every change to the timer, from a click, a key or an `alpheus-launch --timer` verb, goes
//! through one function, `App::timer_do`, which saves the state, writes the log line, updates the
//! chip and re-arms the deadline. The deadline (a `DeadlineTimer`, alpheus's one-shot timer on the
//! event loop) is armed only while a timer runs, so an idle or paused timer wakes nothing. A
//! suspended laptop's deadline can fire late, so the clock's minute tick also re-checks the timer.
//! This file also sends the end's notification and plays its sound, and reads and writes the state
//! file.
//!
//! The scheduled shutdown (`shutdown.rs`) lives beside the timer here: its own deadline, armed only
//! while a shutdown is scheduled, its warning notice (critical, with a Cancel action) and, at its
//! end, the `[quick] poweroff` command through the executor. It never touches the timer's model.
//!
//! Never:
//! - start a thread: everything runs on the event loop;
//! - wait for the end's sound: it is started through the daemon's executor (the object every
//!   program the daemon runs goes through, which tests replace with a fake) and never waited for;
//! - wait for the power off command either: it is a watched child, its end heard on its pidfd by the
//!   event loop, and a failure to start or an error exit is told to the user;
//! - log the sound's or the power-off command, or a path: logs carry fixed words and numbers only
//!   (a command may name the user);
//! - power off before the warning: a late end warns first (`shutdown_step`).
//!
//! See: docs/features/timer.md, and
//! docs/decisions.md ("One deadline, armed only while a timer runs", "A change applies a due end
//! first").

use crate::actions::Button;
use crate::config::ModuleId;
use crate::deadline::DeadlineTimer;
use crate::grid::Target;
use crate::keys::Key;
use crate::log::write as log;
use crate::notify::CloseReason;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer};
use crate::quick::app::verb_debounced;
use crate::session_bus::notifications::minute_now;
use crate::state::Msg;
use crate::state_file;
use crate::timer::lockscreen;
use crate::timer::popup::{TimerAction, TimerPopup};
use crate::timer::shutdown::{self, Shutdown, ShutdownChoice, ShutdownStep, warning_holds};
use crate::timer::{Change, End, Lengths, Phase, Timer as Model, chip_with_shutdown, incoming, length_text, saved_pair};
use crate::wayland::App;
use alpheus_common::exec::{Executor, Watched};
use alpheus_common::launcher_ipc::{Reply, Request, TIMER_MAX_SECS, TIMER_MIN_SECS, TimerVerb};
use alpheus_common::paths;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::{Duration, Instant};

/// The most a deadline looks ahead, in milliseconds: the longest timer is a day, and a running
/// end read back from the file is at most 25 hours ahead. A longer wait is cut to this.
const WAIT_MAX_MS: u64 = 25 * 3_600_000;
/// Two deadlines this close are the same one: `timer_sync` leaves an armed deadline alone rather
/// than re-arm it for a few milliseconds' difference, so the clock tick does not re-arm a timer
/// already set.
const SAME_DEADLINE: Duration = Duration::from_millis(5);

/// The timer's state on the app: the model, the `[timer]` lengths, its one deadline.
///
/// (`pub(crate)` means the item is visible inside the daemon, and nowhere outside it.)
pub(crate) struct TimerApp {
    /// `timer` is in `modules`: nothing else here runs without it.
    pub(crate) enabled: bool,
    pub(crate) model: Model,
    pub(crate) lengths: Lengths,
    /// Armed only while a timer runs.
    pub(crate) deadline: DeadlineTimer,
    /// The popup's countdown row, in minutes: kept for the session, never saved.
    pub(crate) minutes: u32,
    /// When a `--timer popup` verb opened the popup. A second one within 250 ms is ignored: some
    /// compositors fire a key bind twice.
    pub(crate) verb_opened: Option<Instant>,
    /// The scheduled shutdown, beside the timer (never replacing it, never replaced by it).
    pub(crate) shutdown: Option<Shutdown>,
    /// When the warning was given (Unix ms), for the shutdown scheduled. Never saved: a restart
    /// warns again. A warning the machine slept through, or that the clock went back before, no
    /// longer counts (`shutdown::warning_holds`).
    pub(crate) warned: Option<u64>,
    /// The monotonic moment of that warning: with the wall-clock one it tells a sleep (the wall
    /// clock runs through it, this one does not) from a warning really watched to its end.
    pub(crate) warned_mono: Option<Instant>,
    /// When a due warning first waited for the notification server to own its name. It waits at
    /// most `SERVER_GRACE`: a warning nobody can see must not start the minute.
    pub(crate) deferred_since: Option<Instant>,
    /// A step is held because the session is locked (`lockscreen`): says so in the log once, and
    /// again once it is let go.
    pub(crate) lock_held: bool,
    /// When the last look found the session locked, while a step is held: a look answered that
    /// recently is not repeated (the deadline and the clock's tick, a moment apart, share one).
    pub(crate) lock_seen: Option<Instant>,
    /// The warning notice's id in the notification server, while it is shown.
    pub(crate) warn_id: Option<u32>,
    /// Armed only while a shutdown is scheduled.
    pub(crate) shutdown_deadline: DeadlineTimer,
    /// The power off command, from its start until its end is heard.
    pub(crate) poweroff: Option<PowerOffRun>,
}

/// The `[quick] poweroff` command while it runs: the watched child, and its pidfd's place on the
/// event loop, which is quiet until the command ends. Dropped, it lets the command run on (never
/// kills it): a power off that ends the session before it returns must not be cut short.
pub(crate) struct PowerOffRun {
    child: Option<Box<dyn Watched>>,
    watch: Option<RegistrationToken>,
}

impl Drop for PowerOffRun {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            child.release();
        }
    }
}

/// How long a look that found the session locked answers the next ones: the two checks of a minute
/// (the deadline and the clock's tick) land within a moment of each other, and read `/proc` once.
const HELD_MEMO: Duration = Duration::from_secs(30);

/// The most a held step waits for its next real look, counted from the last one: the once-a-minute
/// recheck. Every check that finds the step held (the deadline, the clock's minute tick, answered
/// from the memo) moves the deadline a minute on, so without this cap a check that lands inside
/// `HELD_MEMO` pushes the next real look to its own time plus a minute: up to 90 s after the last
/// look, and an unlock goes unseen for that long.
const HELD_RECHECK: Duration = Duration::from_millis(shutdown::WARN_MS);

/// When the shutdown's deadline next fires while a step may be held: `wake`, but never later than
/// `HELD_RECHECK` after the last look that found the session locked (`lock_seen`, set while held).
fn held_wake(wake: Instant, lock_held: bool, lock_seen: Option<Instant>) -> Instant {
    match lock_seen {
        Some(seen) if lock_held => wake.min(seen + HELD_RECHECK),
        _ => wake,
    }
}

impl TimerApp {
    pub(crate) fn new(config: &crate::config::BarConfig) -> TimerApp {
        TimerApp {
            enabled: config.modules.contains(&ModuleId::Timer),
            model: Model::Idle { done: false },
            lengths: Lengths::of(&config.timer),
            deadline: DeadlineTimer::new(),
            minutes: config.timer.countdown,
            verb_opened: None,
            shutdown: None,
            warned: None,
            warned_mono: None,
            deferred_since: None,
            lock_held: false,
            lock_seen: None,
            warn_id: None,
            shutdown_deadline: DeadlineTimer::new(),
            poweroff: None,
        }
    }
}

/// The timer popup's row in the table of popups (`popup::POPUPS`): what the shared popup code
/// calls to open it, to give it a key or the pointer, and when it closes.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Timer,
    // A closure that captures nothing, written in place of a plain function.
    toggle: |app, qh, anchor, _| app.timer_toggle(qh, Some(anchor)),
    key: App::timer_key,
    pointer: App::timer_pointer,
    // The countdown row's length outlives the popup.
    flush: App::timer_flush,
    // The one-second tick ends with the popup, whatever closed it, and an end the popup showed
    // counts as seen.
    closed: App::timer_closed,
};

/// What an end does besides writing the log line: whether to notify and whether to play the sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Effects {
    pub notify: bool,
    pub sound: bool,
}

/// What an end does, given whether the notification server is `serving` (it owns its D-Bus name),
/// whether do-not-disturb is on, and the `sound` command from `[timer]`.
///
/// The notice goes to the server whenever it is serving, even under do-not-disturb: the server
/// applies do-not-disturb itself, with no bubble but the history and the bell still keep the
/// notice. The sound plays only if a command is set and do-not-disturb is off. See
/// docs/decisions.md, "Under do-not-disturb the end goes to the history only".
pub(crate) fn effects(serving: bool, dnd: bool, sound: &[String]) -> Effects {
    Effects { notify: serving, sound: !dnd && !sound.is_empty() }
}

/// The change to make when the popup closes: `Seen` if the timer reads `done`, else none.
///
/// An end the popup showed is seen by the time it closes: the chip must not say `done` about
/// something the popup already said. See docs/decisions.md, "`done` is cleared by looking, and is
/// never saved".
pub(crate) fn seen_at_close(m: &Model) -> Option<Change> {
    matches!(m, Model::Idle { done: true }).then_some(Change::Seen)
}

/// The total length of a countdown, for the `added` line; None for any other timer.
fn total_of(t: &Model) -> Option<u64> {
    match t {
        Model::Countdown { total, .. } => Some(*total),
        _ => None,
    }
}

/// The log line a change makes (the words are fixed: see docs/decisions.md, "Logs carry fixed
/// words and numbers only"). None when the change was not worth a line: `Seen`, or a change with
/// nothing to act on.
// a Toggle is logged as the pause or resume it stands for.
#[allow(clippy::match_same_arms)]
pub(crate) fn change_line(before: &Model, after: &Model, c: Change, _l: &Lengths) -> Option<String> {
    if before == after {
        return None;
    }
    // `?` on the `total_of` calls gives None (no line) if a countdown's length is missing.
    Some(match c {
        Change::Seen => return None,
        Change::Countdown(_) => format!("timer: countdown started ({})", length_text(total_of(after)?)),
        Change::Pomodoro => "timer: pomodoro started".into(),
        Change::Stopwatch => "timer: stopwatch started".into(),
        Change::Pause => "timer: paused".into(),
        Change::Resume => "timer: resumed".into(),
        Change::Toggle if before.running() => "timer: paused".into(),
        Change::Toggle => "timer: resumed".into(),
        Change::Add(_) => {
            let added = total_of(after)?.saturating_sub(total_of(before)?);
            format!("timer: added {}", length_text(added))
        }
        Change::Skip => match after {
            Model::Pomodoro { phase: Phase::ShortBreak, .. } => "timer: skipped to short break".into(),
            Model::Pomodoro { phase: Phase::LongBreak, .. } => "timer: skipped to long break".into(),
            Model::Pomodoro { phase: Phase::Work, .. } => "timer: skipped to work".into(),
            _ => "timer: skipped to the end".into(),
        },
        Change::Stop => "timer: stopped".into(),
    })
}

/// The log line of an end, in fixed words. The stopwatch's stop at 24 hours only logs: see
/// docs/decisions.md, "The 24-hour stopwatch stop is silent".
pub(crate) fn end_line(end: End) -> &'static str {
    match end {
        End::Countdown { .. } => "timer: countdown ended",
        End::WorkDone { .. } => "timer: pomodoro work ended",
        End::BreakDone { .. } => "timer: pomodoro break ended",
        End::SetDone { .. } => "timer: pomodoro set ended",
        End::StopwatchCap => "timer: stopwatch stopped at 24 h",
    }
}

/// Starts the end's sound as given: the program and its arguments, never through a shell, and
/// never waited for. Returns the log line of a failure, or None when it started (or no command is
/// set). The line never holds the command itself, which may name the user.
pub(crate) fn spawn_sound(exec: &dyn Executor, argv: &[String]) -> Option<String> {
    // The first word is the program, the rest its arguments; an empty list means no sound is set.
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match exec.spawn(program, &args) {
        Ok(()) => None,
        Err(e) => Some(format!("timer: sound could not start ({})", crate::log::exec_word(&e))),
    }
}

/// How long to wait until `target_ms`, from `now_ms` (both Unix milliseconds): zero once the
/// target is past, and never more than 25 hours.
pub(crate) fn wait_for(target_ms: u64, now_ms: u64) -> Duration {
    Duration::from_millis(target_ms.saturating_sub(now_ms).min(WAIT_MAX_MS))
}

/// The wall clock now, in Unix milliseconds. This is the one place the timer reads Unix time.
///
/// A clock set before 1970 reads as 0, and a time too large for the number reads as the largest
/// number: neither can happen in practice, and neither panics.
pub(crate) fn unix_ms() -> u64 {
    let Ok(since_epoch) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else { return 0 };
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// The pure step the clock check takes: the timer once its end has passed (one end, never two),
/// what ended, and whether the state file must be written (it must, whenever something ended).
pub(crate) fn step(model: Model, l: &Lengths, now: u64) -> (Model, Option<End>, bool) {
    let (next, end) = model.due(l, now);
    (next, end, end.is_some())
}

/// A change at `now`, with an end that is already due applied first. Returns the timer after that
/// end, what ended, and the timer after the change.
///
/// This order matters: after a laptop resumes, a click on Pause or an Add must not pause or
/// extend a timer that has already ended. The end comes first, so the change finds nothing to act
/// on.
pub(crate) fn change_step(model: Model, c: Change, l: &Lengths, now: u64) -> (Model, Option<End>, Model) {
    let (due, end, _) = step(model, l, now);
    (due, end, due.apply(c, l, now))
}

/// What a state file read at start makes of the timer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Loaded {
    pub model: Model,
    /// The shutdown on file, even one whose end has passed: the wiring warns first.
    pub shutdown: Option<Shutdown>,
    /// Why a shutdown on file was dropped, if it was.
    pub dropped: Option<Dropped>,
    /// The file was there and unreadable (to be dropped).
    pub unreadable: bool,
    /// An end passed while the daemon was not running (applied once, silently).
    pub ended: bool,
}

/// Why a shutdown on file is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dropped {
    /// Saved by an earlier boot (the state directory outlives a reboot), or of an unknown age.
    BeforeBoot,
    /// `[quick] poweroff` is empty: there is nothing to run at the end.
    NoPoweroff,
}

impl Dropped {
    /// The log line, a fixed word.
    pub(crate) fn line(self) -> &'static str {
        match self {
            Dropped::BeforeBoot => "timer: shutdown from before this boot, dropped",
            Dropped::NoPoweroff => "timer: shutdown dropped (no poweroff command)",
        }
    }
}

/// What `load` needs to know about the machine, read by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoadCtx {
    /// This boot's start, in Unix seconds (`/proc/stat`'s `btime`); None when unreadable, which
    /// drops any shutdown on file.
    pub boot_secs: Option<u64>,
    /// `[quick] poweroff` is set.
    pub has_poweroff: bool,
}

/// `/proc/stat`'s `btime` line: the boot time in Unix seconds.
pub(crate) fn parse_btime(text: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse().ok())
}

/// This boot's start in Unix seconds, read once at load; None when `/proc/stat` cannot be read.
pub(crate) fn boot_time_secs() -> Option<u64> {
    std::fs::read_to_string("/proc/stat").ok().as_deref().and_then(parse_btime)
}

/// What a `--timer shutdown` verb is refused for, if it is: no command to run at the end.
pub(crate) fn shutdown_refusal(poweroff: &[String]) -> Option<&'static str> {
    poweroff.is_empty().then_some("no [quick] poweroff command is set")
}

/// How long a due warning waits for the notification server to own its name, at most.
pub(crate) const SERVER_GRACE: Duration = Duration::from_secs(10);

/// Whether a due warning must wait: the notifications module is on, its server does not hold the
/// name yet, and the grace is not spent. Past it the warning goes out anyway (the chip and the log
/// still say it), so a missing server never stops the power off from being announced.
pub(crate) fn warning_waits(module_on: bool, serving: bool, waited: Duration) -> bool {
    module_on && !serving && waited < SERVER_GRACE
}

/// The wait of a due warning for the server, after a check: kept while a warning is due, gone
/// otherwise. The grace deadline is armed from it, so one left behind would re-arm a past instant
/// forever.
pub(crate) fn deferral_after(since: Option<Instant>, warn_due: bool) -> Option<Instant> {
    since.filter(|_| warn_due)
}

/// Reads the state file's text (None when there is no file) into the timer to start with.
pub(crate) fn load(text: Option<&str>, l: &Lengths, now: u64, ctx: &LoadCtx) -> Loaded {
    // No file, or a file we cannot read: start idle.
    let idle = Model::Idle { done: false };
    let Some(text) = text else { return Loaded { model: idle, shutdown: None, dropped: None, unreadable: false, ended: false } };
    let Some((model, shutdown)) = Model::parse_pair(text, now) else {
        return Loaded { model: idle, shutdown: None, dropped: None, unreadable: true, ended: false };
    };
    // A shutdown is kept only if it can run and was scheduled during this boot: a file left by an
    // earlier session must never power the machine off after the next login.
    let (shutdown, dropped) = match shutdown {
        Some(_) if !ctx.has_poweroff => (None, Some(Dropped::NoPoweroff)),
        Some(s) if ctx.boot_secs.is_none_or(|b| s.set < b.saturating_mul(1000)) => (None, Some(Dropped::BeforeBoot)),
        other => (other, None),
    };
    // An end that passed while the daemon was not running is applied now, once.
    let (model, end, _) = step(model, l, now);
    Loaded { model, shutdown, dropped, unreadable: false, ended: end.is_some() }
}

/// What a clock check does to the shutdown `s` (`warned` or not) at `now`: the shutdown after it
/// (None once it powers off), whether it is warned, and the step due. A warning moves the end to a
/// minute from now when it is nearer, so the power off is never sooner than a minute after the
/// warning, and never before it.
///
/// `locked` says whether the session is locked. It is asked when a step is due, and while a warning
/// is in force (its minute runs: the wiring looks every `LOCK_SAMPLE_MS`), never at rest. A step due
/// under the lock is held: the notice would sit under the lock screen, unseen, and its minute would
/// run out behind it. A lock seen during the minute does the same: the warning stops counting, so
/// that the unlock, however soon, finds the end near and gives the warning again, with its whole
/// minute.
pub(crate) fn shutdown_step(
    s: Shutdown,
    warned: Option<u64>,
    mono_elapsed: Duration,
    now: u64,
    locked: impl FnOnce() -> bool,
) -> (Option<Shutdown>, Option<u64>, Option<ShutdownStep>) {
    // A warning the machine slept through, even briefly, or that the clock went back before is as
    // good as none: it is given again, and its minute starts over.
    let holds = warning_holds(&s, warned, mono_elapsed, now);
    let step = shutdown::next(&s, now, holds).1;
    if (step.is_some() || holds) && locked() {
        return (Some(s), None, None);
    }
    match step {
        Some(step @ ShutdownStep::Warn { .. }) => (Some(s.warned_at(now)), Some(now), Some(step)),
        Some(ShutdownStep::PowerOff) => (None, None, Some(ShutdownStep::PowerOff)),
        None => (Some(s), warned.filter(|_| holds), None),
    }
}

/// Runs the `[quick] poweroff` command `argv`, never through a shell, as a watched child that does not die with
/// the daemon: its end is heard on its pidfd, never waited for. Err holds the log line of a command that did not start:
/// a fixed word, with the kind of the failure, never the command.
pub(crate) fn spawn_poweroff(exec: &dyn Executor, argv: &[String]) -> Result<Box<dyn Watched>, String> {
    let Some((program, args)) = argv.split_first() else { return Err("timer: shutdown has no poweroff command".into()) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.spawn_watched_free(program, &args)
        .map_err(|e| format!("timer: shutdown power off could not start ({})", crate::log::exec_word(&e)))
}

/// How a power off command that ended with `exit` (`Watched::try_exit`: None when a signal ended
/// it) is told: its log line, and whether the user is told too. Only a non-zero exit is a failure
/// (a refusal by polkit or an inhibitor, a program that is not there). An end by a signal is not:
/// the session's teardown signals the command of a power off that is really happening. The line
/// holds a number or a word, never the command.
pub(crate) fn poweroff_end(exit: Option<i32>) -> (String, bool) {
    match exit {
        Some(0) => ("timer: shutdown command ended".into(), false),
        Some(code) => (format!("timer: shutdown power off failed (exit {code})"), true),
        None => ("timer: shutdown command ended by a signal".into(), false),
    }
}

/// Whether two instants are within `SAME_DEADLINE` of each other, in either order.
fn same_deadline(a: Instant, b: Instant) -> bool {
    a.saturating_duration_since(b) < SAME_DEADLINE && b.saturating_duration_since(a) < SAME_DEADLINE
}

impl App {
    /// Applies one change. This is the one path for every click, key and verb. Returns false when
    /// the change did nothing (or the module is off); the verbs then log "nothing to <word>".
    pub(crate) fn timer_do(&mut self, c: Change) -> bool {
        if !self.timer.enabled {
            return false;
        }
        // An end that is already due comes first (see `change_step`). `before` is the timer after
        // that end, and `after` the timer after the change itself.
        let (before, end, after) = change_step(self.timer.model, c, &self.timer.lengths, unix_ms());
        if let Some(end) = end {
            self.timer.model = before;
            self.timer_save();
            log(end_line(end));
            self.timer_end(end);
        }
        if after == before {
            // The change did nothing, but an end happened: the chip and deadline still need
            // updating.
            if end.is_some() {
                self.timer_chip();
                self.timer_sync();
            }
            return false;
        }
        self.timer.model = after;
        // `Seen` only clears `done`, which is never saved, so there is nothing to write.
        if c != Change::Seen {
            self.timer_save();
        }
        if let Some(line) = change_line(&before, &after, c, &self.timer.lengths) {
            log(&line);
        }
        self.timer_chip();
        self.timer_sync();
        true
    }

    /// Handles an `alpheus-launch --timer …` request; it is answered before the launcher's own
    /// guard (see `launcher/socket_glue.rs`). The answer is an error when the module is not in
    /// `modules`, and Done otherwise. Every verb first applies `Seen`, so a `done` chip is
    /// dismissed; "nothing to <word>" goes to the log only when neither that nor the verb changed
    /// anything.
    pub(crate) fn timer_verb(&mut self, request: &Request) -> Reply {
        let Request::Timer(verb) = request else { return Reply::Done };
        if !self.timer.enabled {
            return Reply::Error("the timer module is not in modules".into());
        }
        // The popup verb toggles the popup. It does not go through `timer_do`; opening applies
        // `Seen`, which clears `done`.
        if *verb == TimerVerb::Popup {
            // A second verb right after the first opened the popup is the compositor firing the
            // key bind twice: ignore it.
            if self.timer_shown() && verb_debounced(self.timer.verb_opened, Instant::now()) {
                log("timer: popup debounced");
                return Reply::Done;
            }
            let qh = self.qh.clone();
            self.timer_toggle(&qh, None);
            return Reply::Done;
        }
        // Nothing would run at the end: a shutdown is refused up front, never scheduled.
        if matches!(verb, TimerVerb::Shutdown(_))
            && let Some(refusal) = shutdown_refusal(&self.bar.config.quick.poweroff)
        {
            return Reply::Error(refusal.into());
        }
        // The shutdown is a slot of its own: its verbs never touch the timer, and dismiss no `done`.
        match *verb {
            TimerVerb::Shutdown(secs) if (TIMER_MIN_SECS..=TIMER_MAX_SECS).contains(&secs) => {
                self.shutdown_do(ShutdownChoice::After(u64::from(secs) * 1000));
                return Reply::Done;
            }
            TimerVerb::Shutdown(_) => return Reply::Error("timer seconds out of range".into()),
            TimerVerb::ShutdownCancel => {
                if !self.shutdown_do(ShutdownChoice::Cancel) {
                    log("timer: nothing to shutdown-cancel");
                }
                return Reply::Done;
            }
            _ => {}
        }
        let change = match *verb {
            TimerVerb::Start(secs) if (TIMER_MIN_SECS..=TIMER_MAX_SECS).contains(&secs) => Change::Countdown(u64::from(secs) * 1000),
            // The request comes from a socket, so it is not trusted: the decoder checks the range
            // and so do we.
            TimerVerb::Start(_) => return Reply::Error("timer seconds out of range".into()),
            TimerVerb::Pomodoro => Change::Pomodoro,
            TimerVerb::Stopwatch => Change::Stopwatch,
            TimerVerb::Pause => Change::Pause,
            TimerVerb::Resume => Change::Resume,
            TimerVerb::Toggle => Change::Toggle,
            TimerVerb::Stop => Change::Stop,
            TimerVerb::Skip => Change::Skip,
            // Handled above; kept so that adding a verb forces a decision here.
            TimerVerb::Popup | TimerVerb::Shutdown(_) | TimerVerb::ShutdownCancel => return Reply::Done,
        };
        let seen = self.timer_do(Change::Seen);
        if !self.timer_do(change) && !seen {
            log(&format!("timer: nothing to {}", verb.word()));
        }
        Reply::Done
    }

    /// Whether the timer popup is the popup currently open.
    pub(crate) fn timer_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Timer(_)))
    }

    /// A key for the open popup: ↑ ↓ Tab Shift+Tab, ← →, Enter, Space, Escape. `repeat` tells a
    /// held key from a fresh press.
    fn timer_key(&mut self, k: Key) {
        let repeat = self.keyboard.repeat;
        let action = self.timer_with(|p, t| p.key(t, k, repeat));
        self.timer_action(action);
    }

    /// Hovering selects, a left press acts, the wheel over the Timer row changes its length.
    fn timer_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        let action = match (kind, cell.map(|(_, row)| row)) {
            (PopupPointer::Motion, Some(r)) => self.timer_with(|p, t| p.hover(t, r)),
            (PopupPointer::Press(Button::Left), Some(r)) => self.timer_with(|p, t| p.press(t, r)),
            (PopupPointer::Scroll(steps), Some(r)) => self.timer_with(|p, t| p.scroll(t, r, steps)),
            _ => TimerAction::Nothing,
        };
        self.timer_action(action);
    }

    /// Runs `f` on the open popup and the timer, and returns its answer; Nothing when the timer
    /// popup is not open. (`f` is any function or closure taking those two and used exactly once.)
    fn timer_with(&mut self, f: impl FnOnce(&mut TimerPopup, &Model) -> TimerAction) -> TimerAction {
        // Taking the two fields out of `self` at once lets the borrow checker see that the popup
        // and the timer are different things, so one can change while the other is read.
        let App { popup, timer, .. } = self;
        match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Timer(p)) => f(p, &timer.model),
            _ => TimerAction::Nothing,
        }
    }

    /// Carries out what the popup asked for.
    fn timer_action(&mut self, action: TimerAction) {
        match action {
            TimerAction::Nothing => {}
            TimerAction::Redraw => self.timer_refresh(),
            TimerAction::Close => self.popup_close(PopupClose::Escape),
            TimerAction::Do(c) => {
                self.timer_do(c);
                self.timer_refresh();
            }
            TimerAction::CancelShutdown => {
                self.shutdown_do(ShutdownChoice::Cancel);
                self.timer_refresh();
            }
        }
    }

    /// Draws the open popup again (`popup_paint` sends nothing to the screen when its rows did not
    /// change). Its actions first follow the timer, whoever changed it (a verb, the deadline, the
    /// clock): when the list of actions changes, the selection goes back to the first one.
    fn timer_refresh(&mut self) {
        let App { popup, timer, .. } = self;
        let rows = match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Timer(p)) => {
                p.set_shutdown(timer.shutdown.is_some());
                p.follow(&timer.model);
                p.render(&timer.model, &timer.lengths, unix_ms())
            }
            _ => return,
        };
        self.popup_paint(rows);
    }

    /// Called just before the popup's content is dropped: keeps the Timer row's length for the
    /// next time it opens (for this session only, it is never saved).
    fn timer_flush(&mut self) {
        if let Some(PopupBody::Timer(p)) = self.popup_body() {
            self.timer.minutes = p.minutes;
        }
    }

    /// Opens the popup, or closes it if it is open. Called for a press on the chip, with its place
    /// (`anchor`), or for the verb, with None. A click grabs the keyboard; the verb takes the
    /// keyboard without a grab. The place is read before `Seen` is applied, because `Seen` can
    /// change the chip's width (`done` becomes the idle glyph).
    pub(crate) fn timer_toggle(&mut self, qh: &QueueHandle<App>, anchor: Option<PopupAnchor>) {
        if self.timer_shown() {
            self.popup_close(if anchor.is_some() { PopupClose::OutsideClick } else { PopupClose::Verb });
            return;
        }
        if !self.timer.enabled {
            return;
        }
        let (anchor, input) = match anchor {
            Some(a) => (a, Input::Grab),
            None => match self.timer_anchor() {
                Some(a) => (a, Input::Keys),
                None => {
                    log("timer: no bar to open under");
                    return;
                }
            },
        };
        // `Seen` is applied once the popup is open, and the rows and the chip are made again after
        // it. The place was read before, as said above.
        let mut popup = TimerPopup::new(self.timer.minutes, &self.timer.model);
        popup.set_shutdown(self.timer.shutdown.is_some());
        popup.follow(&self.timer.model);
        let rows = popup.render(&self.timer.model, &self.timer.lengths, unix_ms());
        let (columns, height) = rows.cells();
        if self.popup_open(qh, anchor, PopupBody::Timer(popup), columns, height, input) {
            self.timer.verb_opened = (input == Input::Keys).then(Instant::now);
            self.popup_paint(rows);
            log(&format!("timer shown ({})", self.timer.model.word()));
            self.timer_do(Change::Seen);
            self.timer_refresh();
            self.timer_sync();
        }
    }

    /// Where the verb opens the popup: under the chip on the focused output's bar, else (with
    /// `idle = "hidden"` there is no chip) under the bar's right end, as the quick panel does.
    /// See docs/decisions.md, "The verb opens the popup under the chip".
    fn timer_anchor(&self) -> Option<PopupAnchor> {
        let s = self.verb_bar()?;
        // `s.last` is the last grid drawn for that bar; its zones are the clickable areas, and the
        // chip's is the one aimed at the timer module.
        let zone = match &s.last {
            Some(grid) => grid.zones().iter().find(|z| z.target == Target::Module(ModuleId::Timer)),
            None => None,
        };
        match zone {
            Some(z) => Some(PopupAnchor { output: s.name.clone(), start: z.start, end: z.end }),
            None => self.quick_anchor(),
        }
    }

    /// The deadline fired. The event loop (calloop) has already dropped it, so `fired` is called
    /// first to say so; then the wall clock decides what happened. The two parameters are what
    /// the deadline's callback must take: the app, and the instant it fired.
    fn timer_tick(app: &mut App, _: Instant) {
        app.timer.deadline.fired();
        app.timer_check_clock();
    }

    /// Applies the end that passed, if one did, and keeps the chip and the deadline right. Called
    /// by the deadline and by the clock's minute tick (a suspended laptop's deadline is late). A
    /// deadline that fires early finds no end and only arms the next one. See docs/decisions.md,
    /// "The clock's minute tick re-checks the timer".
    pub(crate) fn timer_check_clock(&mut self) {
        if !self.timer.enabled {
            return;
        }
        // The shutdown has its own end, whatever the timer does (or does not run).
        self.shutdown_check_clock();
        if !self.timer.model.running() {
            return;
        }
        let (next, end, save) = step(self.timer.model, &self.timer.lengths, unix_ms());
        self.timer.model = next;
        if save {
            self.timer_save();
        }
        if let Some(end) = end {
            log(end_line(end));
            self.timer_end(end);
        }
        self.timer_chip();
        self.timer_sync();
    }

    /// Announces an end: the notice goes through alpheus's own notification server (only while
    /// that server owns its name, and never queued for later: a late notice is noise), and the
    /// sound plays if `effects` allows it.
    fn timer_end(&mut self, end: End) {
        let Some(n) = incoming(end, &self.timer.lengths) else { return };
        // `serving`: the notification server exists and owns its D-Bus name.
        let serving = self.notifications().is_some_and(|s| s.held());
        let fx = effects(serving, self.dnd, &self.bar.config.timer.sound);
        if fx.notify {
            // The nested closures hand the server a small job to run: it sends the notice, then
            // returns the centre's effects (`take_effects`: a notification closed, an action
            // invoked), which `act` turns into D-Bus signals.
            self.notify_act(move |s| {
                s.act(|c| {
                    c.notify(n, Instant::now(), minute_now());
                    c.take_effects()
                });
            });
        } else if !serving && self.bar.config.modules.contains(&ModuleId::Notifications) {
            log("timer: no notification (not serving)");
        }
        if fx.sound
            && let Some(line) = spawn_sound(self.exec.as_ref(), &self.bar.config.timer.sound)
        {
            log(&line);
        }
    }

    /// Sends the bar the chip as it reads now; the bar repaints only when the chip changed.
    fn timer_chip(&mut self) {
        let now = unix_ms();
        let chip = chip_with_shutdown(self.timer.model.chip(now, self.bar.config.timer.idle), self.timer.shutdown.as_ref(), now);
        if self.bar.apply(Msg::Timer(chip)) {
            self.dirty = true;
        }
        self.timer_refresh();
    }

    /// The popup's `closed` hook, run whatever closed the popup: `Seen` for an end it showed, then
    /// the deadline again, now without the popup's one-second tick. See docs/decisions.md, "The
    /// popup's one-second tick exists only while it is open".
    fn timer_closed(&mut self) {
        if let Some(c) = seen_at_close(&self.timer.model) {
            self.timer_do(c);
        }
        self.timer_sync();
    }

    /// Sets the one deadline: at the next change of the chip (or the end) while a timer runs, and
    /// none otherwise. A deadline already armed for that instant stays as it is.
    fn timer_sync(&mut self) {
        let now = unix_ms();
        // The popup's one-second tick comes with the popup and goes with it (see `timer_closed`).
        // The next change is a Unix time; it becomes an `Instant` (the event loop's own clock) by
        // adding the wait to the current instant. `checked_add` gives None instead of overflowing.
        let at = match self.timer.model.next_change(now, self.timer_shown()) {
            Some(t) => Instant::now().checked_add(wait_for(t, now)),
            None => None,
        };
        // Already armed for that instant: leave it, so the loop is not touched for nothing.
        if let (Some(armed), Some(at)) = (self.timer.deadline.deadline(), at)
            && same_deadline(armed, at)
        {
            return;
        }
        self.timer.deadline.sync_opt(self.loop_handle.as_ref(), at, App::timer_tick);
    }

    /// Writes the state file, or removes it when idle. A failure is logged by its kind only.
    fn timer_save(&self) {
        let text = saved_pair(&self.timer.model, self.timer.shutdown.as_ref());
        if let Err(e) = state_file::save_timer(&paths::state_dir(), text.as_deref()) {
            log(&format!("timer: state could not be saved ({:?})", e.kind()));
        }
    }

    /// Restores the state as it was left. Called at startup, once the event loop exists. A bad
    /// file is dropped; an end that passed while the daemon was down is applied once and silently
    /// (no notification, no sound); the chip is sent, the idle glyph too, and the deadline armed.
    pub(crate) fn timer_load(&mut self, now_ms: u64) {
        if !self.timer.enabled {
            return;
        }
        let text = state_file::read_timer(&paths::state_dir());
        let ctx = LoadCtx { boot_secs: boot_time_secs(), has_poweroff: shutdown_refusal(&self.bar.config.quick.poweroff).is_none() };
        let loaded = load(text.as_deref(), &self.timer.lengths, now_ms, &ctx);
        self.timer.model = loaded.model;
        self.timer.shutdown = loaded.shutdown;
        self.timer.warned = None;
        if let Some(why) = loaded.dropped {
            self.timer_save();
            log(why.line());
        }
        if loaded.unreadable {
            log("timer: saved state unreadable, dropped");
            self.timer_save();
        } else if loaded.ended {
            self.timer_save();
            log("timer: ended while not running");
        }
        if !matches!(loaded.model, Model::Idle { .. }) {
            log(&format!("timer: restored {}", loaded.model.word()));
        }
        if loaded.shutdown.is_some() {
            log("timer: shutdown restored");
        }
        // A shutdown whose end passed while the daemon was off warns first, now.
        self.shutdown_check_clock();
        self.timer_chip();
        self.timer_sync();
    }

    /// A middle click on the chip: pauses or resumes a timer, dismisses `done`, and does nothing
    /// when idle. (A left click opens the popup; right click and wheel do nothing.)
    pub(crate) fn timer_middle_click(&mut self) {
        let c = match self.timer.model {
            Model::Idle { done: true } => Change::Seen,
            Model::Idle { done: false } => return,
            _ => Change::Toggle,
        };
        self.timer_do(c);
    }
}

impl App {
    /// Schedules or cancels the shutdown. A duration replaces the one pending (and its warning
    /// starts over); a cancel with none pending changes nothing and returns false. The timer is
    /// never touched.
    pub(crate) fn shutdown_do(&mut self, choice: ShutdownChoice) -> bool {
        if !self.timer.enabled {
            return false;
        }
        let now = unix_ms();
        let after = shutdown::apply(self.timer.shutdown, choice, now);
        if after == self.timer.shutdown {
            return false;
        }
        self.timer.shutdown = after;
        self.timer.warned = None;
        self.timer.deferred_since = None;
        self.timer.lock_held = false;
        self.shutdown_notice_close();
        self.timer_save();
        match choice {
            ShutdownChoice::Cancel => log("timer: shutdown cancelled"),
            ShutdownChoice::After(ms) => log(&format!("timer: shutdown scheduled ({})", length_text(ms))),
        }
        self.timer_chip();
        self.shutdown_sync();
        true
    }

    /// The deadline fired: `fired` first (the loop has dropped it), then the clock decides.
    fn shutdown_tick(app: &mut App, _: Instant) {
        app.timer.shutdown_deadline.fired();
        app.shutdown_check_clock();
    }

    /// The monotonic time since the warning was given; zero when none was (it then holds nothing).
    fn warned_elapsed(&self) -> Duration {
        self.timer.warned_mono.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Takes the one step due, if any: the warning, or the power off. Called by the deadline, the
    /// clock's minute tick (a suspended laptop's deadline is late) and at load. The warning always
    /// comes first and a minute before the power off, even when the end is long past.
    pub(crate) fn shutdown_check_clock(&mut self) {
        let Some(s) = self.timer.shutdown else { return };
        // Nothing to run at the end: the shutdown goes before any warning.
        if shutdown_refusal(&self.bar.config.quick.poweroff).is_some() {
            self.timer.shutdown = None;
            self.timer.warned = None;
            self.timer.deferred_since = None;
            self.timer.lock_held = false;
            self.shutdown_notice_close();
            self.timer_save();
            log(Dropped::NoPoweroff.line());
            self.timer_chip();
            self.shutdown_sync();
            return;
        }
        let memo = self.timer.lock_held && self.timer.lock_seen.is_some_and(|t| t.elapsed() < HELD_MEMO);
        let root = &self.bar.config.timer.proc_dir;
        let mut looked = None;
        let (after, warned, due) = shutdown_step(s, self.timer.warned, self.warned_elapsed(), unix_ms(), || {
            let locked = memo || lockscreen::session_locked(root);
            looked = Some(locked);
            locked
        });
        // A step held by the lock says so once; the unlock that lets it go, once.
        match looked {
            Some(true) => {
                if !self.timer.lock_held {
                    self.timer.lock_held = true;
                    log("timer: shutdown waits for the unlock");
                }
                if !memo {
                    self.timer.lock_seen = Some(Instant::now());
                }
            }
            Some(false) if self.timer.lock_held => {
                self.timer.lock_held = false;
                self.timer.lock_seen = None;
                log("timer: shutdown goes on after the unlock");
            }
            _ => {}
        }
        let warn_due = matches!(due, Some(ShutdownStep::Warn { .. }));
        // Only a warning that is due waits for the server: anything else leaves no wait behind (a
        // clock that stepped back must not keep a past grace deadline armed).
        self.timer.deferred_since = deferral_after(self.timer.deferred_since, warn_due);
        if warn_due {
            // The notice must be seen before its minute starts: while the server has not yet taken
            // its name (the check runs at load, before it can), the warning waits for it, at most
            // `SERVER_GRACE`. The server's arrival re-runs this check.
            let module_on = self.bar.config.modules.contains(&ModuleId::Notifications);
            let serving = self.notifications().is_some_and(|n| n.held());
            let waited = self.timer.deferred_since.map_or(Duration::ZERO, |t| t.elapsed());
            if warning_waits(module_on, serving, waited) {
                self.timer.deferred_since.get_or_insert_with(Instant::now);
                self.timer_chip();
                self.shutdown_sync();
                return;
            }
        }
        self.timer.shutdown = after;
        self.timer.warned = warned;
        match due {
            Some(ShutdownStep::Warn { .. }) => {
                self.timer.deferred_since = None;
                self.timer.warned_mono = Some(Instant::now());
                self.timer_save();
                log("timer: shutdown warning");
                // A warning given again replaces the old notice.
                self.shutdown_notice_close();
                self.shutdown_warn();
            }
            Some(ShutdownStep::PowerOff) => {
                self.timer_save();
                self.shutdown_notice_close();
                match spawn_poweroff(self.exec.as_ref(), &self.bar.config.quick.poweroff) {
                    Ok(child) => {
                        log("timer: shutdown power off");
                        self.shutdown_watch(child);
                    }
                    // The shutdown is gone and nothing powers off: the user is told.
                    Err(line) => {
                        log(&line);
                        self.shutdown_failed();
                    }
                }
            }
            None => {}
        }
        self.timer_chip();
        self.shutdown_sync();
    }

    /// Sends the warning through alpheus's own notification server, and keeps its id to close it or
    /// to know its click. With no server serving the chip still shows the last minute, and one log
    /// line says the warning was not shown.
    fn shutdown_warn(&mut self) {
        let serving = self.notifications().is_some_and(|s| s.held());
        if !serving {
            log("timer: shutdown warning not shown (not serving)");
            return;
        }
        let mut id = 0;
        self.notify_act(|s| {
            s.act(|c| {
                id = c.notify_exempt(shutdown::warning(), Instant::now(), minute_now());
                c.take_effects()
            });
        });
        self.timer.warn_id = (id != 0).then_some(id);
    }

    /// Puts the power off command's pidfd on the event loop: readable once it has ended, quiet until
    /// then, so nothing wakes while it runs. A command nobody could hear is let go and logged.
    fn shutdown_watch(&mut self, child: Box<dyn Watched>) {
        // A command still running from an earlier power off goes first, its watch with it.
        self.shutdown_unwatch();
        let watch = match (self.loop_handle.clone(), child.exit_fd()) {
            (Some(handle), Some(fd)) => handle
                .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, app: &mut App| Ok(app.shutdown_command_ready()))
                .ok(),
            _ => None,
        };
        if watch.is_none() {
            log("timer: shutdown power off not watched");
            child.release();
            return;
        }
        self.timer.poweroff = Some(PowerOffRun { child: Some(child), watch });
    }

    /// Takes the power off command, running or not, off the loop; it runs on (`PowerOffRun`'s drop).
    fn shutdown_unwatch(&mut self) {
        let Some(mut run) = self.timer.poweroff.take() else { return };
        if let (Some(handle), Some(token)) = (self.loop_handle.as_ref(), run.watch.take()) {
            handle.remove(token);
        }
    }

    /// The command's pidfd turned readable: it has ended. Its end is read (which reaps it) and the
    /// watch goes with this answer. A failing exit is told to the user.
    fn shutdown_command_ready(&mut self) -> PostAction {
        let Some(run) = self.timer.poweroff.as_mut() else { return PostAction::Remove };
        let Some(exit) = run.child.as_mut().and_then(|c| c.try_exit()) else { return PostAction::Continue };
        // The loop removes this watch on our answer: it must not be removed again.
        self.timer.poweroff = None;
        let (line, tell) = poweroff_end(exit);
        log(&line);
        if tell {
            self.shutdown_failed();
        }
        PostAction::Remove
    }

    /// Tells the user the power off did not happen: a critical notice, set apart from the user's
    /// rules like the warning (a drop or a mute must never hide it). With no server serving, one log
    /// line says it was not shown.
    fn shutdown_failed(&mut self) {
        if !self.notifications().is_some_and(|s| s.held()) {
            log("timer: shutdown failure not shown (not serving)");
            return;
        }
        self.notify_act(|s| {
            s.act(|c| {
                c.notify_exempt(shutdown::failure(), Instant::now(), minute_now());
                c.take_effects()
            });
        });
    }

    /// Closes the warning notice when it is shown: the shutdown was cancelled or went.
    fn shutdown_notice_close(&mut self) {
        let Some(id) = self.timer.warn_id.take() else { return };
        let now = Instant::now();
        self.notify_act(|s| s.act(|c| c.close(id, CloseReason::Dismissed, now)));
    }

    /// A click on notification `id`: the warning's Cancel. Anything else is not ours.
    pub(crate) fn shutdown_notice_clicked(&mut self, id: u32) {
        if self.timer.warn_id != Some(id) {
            return;
        }
        // The click already closed the notice: only the shutdown is left to cancel.
        self.timer.warn_id = None;
        self.shutdown_do(ShutdownChoice::Cancel);
    }

    /// Sets the shutdown's one deadline: the next step or chip minute while a shutdown is scheduled,
    /// none otherwise.
    fn shutdown_sync(&mut self) {
        let now = unix_ms();
        let at = match self.timer.shutdown {
            Some(s) => {
                let holds = warning_holds(&s, self.timer.warned, self.warned_elapsed(), now);
                shutdown::wake(&s, now, holds).and_then(|t| Instant::now().checked_add(wait_for(t, now)))
            }
            None => None,
        };
        // A held step is looked at again within a minute of the last real look (`HELD_RECHECK`).
        let at = at.map(|a| held_wake(a, self.timer.lock_held, self.timer.lock_seen));
        // A warning waiting for the server wakes once more when the grace is spent.
        let grace = self.timer.deferred_since.map(|t| t + SERVER_GRACE).filter(|_| self.timer.shutdown.is_some());
        let at = match (at, grace) {
            (Some(a), Some(g)) => Some(a.min(g)),
            (a, g) => a.or(g),
        };
        if let (Some(armed), Some(at)) = (self.timer.shutdown_deadline.deadline(), at)
            && same_deadline(armed, at)
        {
            return;
        }
        self.timer.shutdown_deadline.sync_opt(self.loop_handle.as_ref(), at, App::shutdown_tick);
    }
}

#[cfg(test)]
mod tests;
