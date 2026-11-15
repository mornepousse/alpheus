//! The focus timer's wiring (timer spec § 4, § 5, § 6): every change goes through `App::timer_do`
//! (save, log, chip, deadline), one `DeadlineTimer` armed only while a timer runs (and re-checked
//! by the clock's minute tick, a suspended laptop's timer being late), the end's notification and
//! sound, the state file. No thread; the sound goes through the daemon's executor, never waited
//! for. Logs carry fixed words and numbers only: never the sound's command, never a path.

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
use crate::timer::{Change, End, Lengths, Phase, Timer as Model, incoming, length_text};
use crate::timer_popup::{TimerAction, TimerPopup};
use crate::wayland::App;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::launcher_ipc::{Reply, Request, TIMER_MAX_SECS, TIMER_MIN_SECS, TimerVerb};
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::client::QueueHandle;
use std::time::{Duration, Instant};

/// The most a deadline looks ahead: the longest timer is a day, a running end read back from the
/// file at most 25 hours.
const WAIT_MAX_MS: u64 = 25 * 3_600_000;
/// Two deadlines this close are the same one: the clock tick does not re-arm a timer already set.
const SAME_DEADLINE: Duration = Duration::from_millis(5);

/// The timer's state on the app: the model, the `[timer]` lengths, its one deadline.
pub(crate) struct TimerApp {
    /// `timer` is in `modules`: nothing else here runs without it.
    pub(crate) enabled: bool,
    pub(crate) model: Model,
    pub(crate) lengths: Lengths,
    /// Armed only while a timer runs.
    pub(crate) deadline: DeadlineTimer,
    /// The popup's countdown row, in minutes: kept for the session, never saved.
    pub(crate) minutes: u32,
    /// When a `--timer popup` verb opened the popup (its re-fired press is ignored).
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

/// The timer popup's row of `popup::POPUPS`.
pub(crate) const OPS: PopupOps = PopupOps {
    kind: ModulePopup::Timer,
    toggle: |app, qh, anchor, _| app.timer_toggle(qh, Some(anchor)),
    key: App::timer_key,
    pointer: App::timer_pointer,
    // The countdown row's length outlives the popup.
    flush: App::timer_flush,
    // The one-second cadence ends with the popup, whatever closed it.
    closed: App::timer_sync,
};

/// What an end does besides the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Effects {
    pub notify: bool,
    pub sound: bool,
}

/// The notice goes to the server whenever it holds its name (it applies do-not-disturb itself: no
/// bubble, the history and the bell keep it); the sound needs a command and no do-not-disturb.
pub(crate) fn effects(serving: bool, dnd: bool, sound: &[String]) -> Effects {
    Effects { notify: serving, sound: !dnd && !sound.is_empty() }
}

/// The time a timer has in all, for the `added` line.
fn total_of(t: &Model) -> Option<u64> {
    match t {
        Model::Countdown { total, .. } => Some(*total),
        _ => None,
    }
}

/// The log line a change makes (timer spec decision 18), None when it changed nothing worth a line
/// (`Seen`, or a change with nothing to act on).
pub(crate) fn change_line(before: &Model, after: &Model, c: Change, _l: &Lengths) -> Option<String> {
    if before == after {
        return None;
    }
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

/// The log line of an end (timer spec decision 18).
pub(crate) fn end_line(end: End) -> &'static str {
    match end {
        End::Countdown { .. } => "timer: countdown ended",
        End::WorkDone { .. } => "timer: pomodoro work ended",
        End::BreakDone { .. } => "timer: pomodoro break ended",
        End::SetDone { .. } => "timer: pomodoro set ended",
        End::StopwatchCap => "timer: stopwatch stopped at 24 h",
    }
}

/// Spawns the end's sound as given, never through a shell, never waited for. The failure's log
/// line, if any; never the command itself (it may name the user).
pub(crate) fn spawn_sound(exec: &dyn Executor, argv: &[String]) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.spawn(program, &args).err().map(|e| format!("timer: sound could not start ({})", spawn_error_word(&e)))
}

/// A failed spawn as a fixed word: never the error's text, which may carry a path.
fn spawn_error_word(e: &ExecError) -> &'static str {
    match e {
        ExecError::NotFound => "not found",
        ExecError::Timeout => "timeout",
        _ => "io",
    }
}

/// How long until `target_ms`, from `now_ms` (Unix milliseconds): zero once past, at most 25 hours.
pub(crate) fn wait_for(target_ms: u64, now_ms: u64) -> Duration {
    Duration::from_millis(target_ms.saturating_sub(now_ms).min(WAIT_MAX_MS))
}

/// Unix milliseconds now.
pub(crate) fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The pure step the clock check takes: the timer once its end passed (one end, never two), what
/// ended, and whether the state file must be written.
pub(crate) fn step(model: Model, l: &Lengths, now: u64) -> (Model, Option<End>, bool) {
    let (next, end) = model.due(l, now);
    (next, end, end.is_some())
}

/// A change at `now` with a due end applied first: the timer after that end (and what ended), then
/// after the change. A click or an Add after a resume cannot pause or extend a timer that already
/// ended.
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

pub(crate) fn load(text: Option<&str>, l: &Lengths, now: u64) -> Loaded {
    let idle = Model::Idle { done: false };
    let Some(text) = text else { return Loaded { model: idle, unreadable: false, ended: false } };
    let Some(model) = Model::parse_saved(text, now) else { return Loaded { model: idle, unreadable: true, ended: false } };
    let (model, end, _) = step(model, l, now);
    Loaded { model, unreadable: false, ended: end.is_some() }
}

/// Two instants within `SAME_DEADLINE` of each other.
fn same_deadline(a: Instant, b: Instant) -> bool {
    a.saturating_duration_since(b) < SAME_DEADLINE && b.saturating_duration_since(a) < SAME_DEADLINE
}

impl App {
    /// Applies one change, the one path for every click, key and verb. False when it changed
    /// nothing (or the module is off): the verbs answer "nothing to <word>" then.
    pub(crate) fn timer_do(&mut self, c: Change) -> bool {
        if !self.timer.enabled {
            return false;
        }
        // A due end first (`change_step`): a click or an Add after a resume must not pause or
        // extend a timer that has already ended.
        let (before, end, after) = change_step(self.timer.model, c, &self.timer.lengths, unix_ms());
        if let Some(end) = end {
            self.timer.model = before;
            self.timer_save();
            log(end_line(end));
            self.timer_end(end);
        }
        if after == before {
            if end.is_some() {
                self.timer_chip();
                self.timer_sync();
            }
            return false;
        }
        self.timer.model = after;
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

    /// `alpheus-launch --timer …`, answered before the launcher's guard (`launcher/socket_glue.rs`).
    /// An error when the module is not in `modules`; otherwise Done. Every verb first applies
    /// `Seen` (a `done` chip is dismissed), and answers "nothing to <word>" in the log only when
    /// neither that nor the verb changed anything.
    pub(crate) fn timer_verb(&mut self, request: &Request) -> Reply {
        let Request::Timer(verb) = request else { return Reply::Done };
        if !self.timer.enabled {
            return Reply::Error("the timer module is not in modules".into());
        }
        if *verb == TimerVerb::Popup {
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
            // The wire is not trusted: the decoder bounds it, and so does this.
            TimerVerb::Start(_) => return Reply::Error("timer seconds out of range".into()),
            TimerVerb::Pomodoro => Change::Pomodoro,
            TimerVerb::Stopwatch => Change::Stopwatch,
            TimerVerb::Pause => Change::Pause,
            TimerVerb::Resume => Change::Resume,
            TimerVerb::Toggle => Change::Toggle,
            TimerVerb::Stop => Change::Stop,
            TimerVerb::Skip => Change::Skip,
            TimerVerb::Popup => return Reply::Done,
        };
        let seen = self.timer_do(Change::Seen);
        if !self.timer_do(change) && !seen {
            log(&format!("timer: nothing to {}", verb.word()));
        }
        Reply::Done
    }

    pub(crate) fn timer_shown(&self) -> bool {
        matches!(self.popup_body(), Some(PopupBody::Timer(_)))
    }

    /// ↑ ↓ Tab Shift+Tab, ← →, Enter, Space, Escape; the repeat tells a held key.
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

    /// `f` on the open popup and the model; Nothing when it is not open.
    fn timer_with(&mut self, f: impl FnOnce(&mut TimerPopup, &Model) -> TimerAction) -> TimerAction {
        let App { popup, timer, .. } = self;
        match popup.as_mut().map(|p| &mut p.body) {
            Some(PopupBody::Timer(p)) => f(p, &timer.model),
            _ => TimerAction::Nothing,
        }
    }

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

    /// The open popup repainted (`popup_draw` commits nothing when its rows did not change). Its
    /// actions follow the model first, whoever changed it (a verb, the deadline, the clock): the
    /// selection goes back to the first when they change.
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

    /// Before the popup's body goes: its countdown length is kept.
    fn timer_flush(&mut self) {
        if let Some(PopupBody::Timer(p)) = self.popup_body() {
            self.timer.minutes = p.minutes;
        }
    }

    /// A press on the chip, or the verb (`anchor` None): toggles the popup under it. A click grabs
    /// the keyboard, the verb takes it without a grab. The anchor is read before `Seen`, which can
    /// change the chip's width (`done` to the idle glyph).
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
        // `Seen` comes once the popup is open (the anchor was read before it: `done` and the idle
        // glyph differ in width), and the rows and the chip are made again after it.
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

    /// The verb's anchor (plan decision 17): the chip's zone on the last grid of the focused bar,
    /// else (`idle = "hidden"`: no chip) the bar's right end, as the quick panel's.
    fn timer_anchor(&self) -> Option<PopupAnchor> {
        let s = self.verb_bar()?;
        let zone = s.last.as_ref().and_then(|g| g.zones().iter().find(|z| z.target == Target::Module(ModuleId::Timer)));
        match zone {
            Some(z) => Some(PopupAnchor { output: s.name.clone(), start: z.start, end: z.end }),
            None => self.quick_anchor(),
        }
    }

    /// The deadline fired: calloop dropped it (`fired` first), then the wall clock decides.
    fn timer_tick(app: &mut App, _: Instant) {
        app.timer.deadline.fired();
        app.timer_check_clock();
    }

    /// Applies the end that passed, if one did, and keeps the chip and the deadline right. Called
    /// by the deadline and by the clock's tick (a suspended laptop's deadline is late); an early
    /// fire only re-arms.
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

    /// The end's notice (through the server, only while it holds the name) and sound.
    fn timer_end(&mut self, end: End) {
        let Some(n) = incoming(end, &self.timer.lengths) else { return };
        let serving = self.notifications().is_some_and(|s| s.held());
        let fx = effects(serving, self.dnd, &self.bar.config.timer.sound);
        if fx.notify {
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

    /// The chip as the bar draws it now; a repaint only when it changed.
    fn timer_chip(&mut self) {
        let chip = self.timer.model.chip(unix_ms(), self.bar.config.timer.idle);
        if self.bar.apply(Msg::Timer(chip)) {
            self.dirty = true;
        }
        self.timer_refresh();
    }

    /// The deadline: the next change of the chip (or the end) while a timer runs, none otherwise.
    /// A deadline already armed for that instant stays.
    fn timer_sync(&mut self) {
        let now = unix_ms();
        // The popup's seconds come with the popup, and go with it (`closed`).
        let at = self.timer.model.next_change(now, self.timer_shown()).and_then(|t| Instant::now().checked_add(wait_for(t, now)));
        if let (Some(armed), Some(at)) = (self.timer.deadline.deadline(), at) {
            if same_deadline(armed, at) {
                return;
            }
        }
        self.timer.deadline.sync_opt(self.loop_handle.as_ref(), at, App::timer_tick);
    }

    fn timer_save(&self) {
        let text = self.timer.model.saved();
        if let Err(e) = state_file::save_timer(&paths::state_dir(), text.as_deref()) {
            log(&format!("timer: state could not be saved ({:?})", e.kind()));
        }
    }

    /// The state as it was left (startup, with the loop handle made): a bad file is dropped, an
    /// end that passed while the daemon was down is applied once and silently, the chip is sent
    /// (the idle glyph too) and the deadline armed.
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

    /// A middle click on the chip (timer spec decision 16): pauses or resumes a timer, dismisses
    /// `done`, nothing when idle.
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
