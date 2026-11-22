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
//! Never:
//! - start a thread: everything runs on the event loop;
//! - wait for the end's sound: it is started through the daemon's executor (the object every
//!   program the daemon runs goes through, which tests replace with a fake) and never waited for;
//! - log the sound's command or a path: logs carry fixed words and numbers only (the command may
//!   name the user).
//!
//! See: docs/specs/2026-11-15-alpheus-timer-design.md (§ 4, § 5, § 6), docs/features/timer.md, and
//! docs/decisions.md ("One deadline, armed only while a timer runs", "A change applies a due end
//! first").

use crate::actions::Button;
use crate::config::ModuleId;
use crate::deadline::DeadlineTimer;
use crate::grid::Target;
use crate::keys::Key;
use crate::log::write as log;
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, PopupBody, PopupClose, PopupOps, PopupPointer};
use crate::quick_app::verb_debounced;
use crate::session_bus::notifications::minute_now;
use crate::state::Msg;
use crate::state_file;
use crate::timer::popup::{TimerAction, TimerPopup};
use crate::timer::{Change, End, Lengths, Phase, Timer as Model, incoming, length_text};
use crate::wayland::App;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::launcher_ipc::{Reply, Request, TIMER_MAX_SECS, TIMER_MIN_SECS, TimerVerb};
use alpheus_commun::paths;
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
        Err(e) => Some(format!("timer: sound could not start ({})", spawn_error_word(&e))),
    }
}

/// A failed start as a fixed word: never the error's text, which may carry a path.
fn spawn_error_word(e: &ExecError) -> &'static str {
    match e {
        ExecError::NotFound => "not found",
        ExecError::Timeout => "timeout",
        _ => "io",
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
    /// The file was there and unreadable (to be dropped).
    pub unreadable: bool,
    /// An end passed while the daemon was not running (applied once, silently).
    pub ended: bool,
}

/// Reads the state file's text (None when there is no file) into the timer to start with.
pub(crate) fn load(text: Option<&str>, l: &Lengths, now: u64) -> Loaded {
    // No file, or a file we cannot read: start idle.
    let idle = Model::Idle { done: false };
    let Some(text) = text else { return Loaded { model: idle, unreadable: false, ended: false } };
    let Some(model) = Model::parse_saved(text, now) else { return Loaded { model: idle, unreadable: true, ended: false } };
    // An end that passed while the daemon was not running is applied now, once.
    let (model, end, _) = step(model, l, now);
    Loaded { model, unreadable: false, ended: end.is_some() }
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
            TimerVerb::Popup => return Reply::Done,
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
        }
    }

    /// Draws the open popup again (`popup_paint` sends nothing to the screen when its rows did not
    /// change). Its actions first follow the timer, whoever changed it (a verb, the deadline, the
    /// clock): when the list of actions changes, the selection goes back to the first one.
    fn timer_refresh(&mut self) {
        let App { popup, timer, .. } = self;
        let rows = match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Timer(p)) => {
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
        let popup = TimerPopup::new(self.timer.minutes, &self.timer.model);
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
        if !self.timer.enabled || !self.timer.model.running() {
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
                })
            });
        } else if !serving && self.bar.config.modules.contains(&ModuleId::Notifications) {
            log("timer: no notification (not serving)");
        }
        if fx.sound {
            if let Some(line) = spawn_sound(self.exec.as_ref(), &self.bar.config.timer.sound) {
                log(&line);
            }
        }
    }

    /// Sends the bar the chip as it reads now; the bar repaints only when the chip changed.
    fn timer_chip(&mut self) {
        let chip = self.timer.model.chip(unix_ms(), self.bar.config.timer.idle);
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
        if let (Some(armed), Some(at)) = (self.timer.deadline.deadline(), at) {
            if same_deadline(armed, at) {
                return;
            }
        }
        self.timer.deadline.sync_opt(self.loop_handle.as_ref(), at, App::timer_tick);
    }

    /// Writes the state file, or removes it when idle. A failure is logged by its kind only.
    fn timer_save(&self) {
        let text = self.timer.model.saved();
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
        let loaded = load(text.as_deref(), &self.timer.lengths, now_ms);
        self.timer.model = loaded.model;
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

#[cfg(test)]
mod tests;
