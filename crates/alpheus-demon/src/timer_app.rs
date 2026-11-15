//! The focus timer's wiring (timer spec § 4, § 5, § 6): every change goes through `App::timer_do`
//! (save, log, chip, deadline), one `DeadlineTimer` armed only while a timer runs (and re-checked
//! by the clock's minute tick, a suspended laptop's timer being late), the end's notification and
//! sound, the state file. No thread; the sound goes through the daemon's executor, never waited
//! for. Logs carry fixed words and numbers only: never the sound's command, never a path.

use crate::config::ModuleId;
use crate::deadline::DeadlineTimer;
use crate::log::write as log;
use crate::session_bus::notifications::minute_now;
use crate::state::Msg;
use crate::state_file;
use crate::timer::{Change, End, Lengths, Phase, Timer as Model, incoming, length_text};
use crate::wayland::App;
use alpheus_commun::exec::Executor;
use alpheus_commun::paths;
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
    /// The popup's countdown row, in minutes (read by the popup, which comes after this wiring).
    #[allow(dead_code)]
    pub(crate) minutes: u32,
    /// When a `--timer popup` verb opened the popup (its re-fired press is ignored); read by the
    /// verb, which comes after this wiring.
    #[allow(dead_code)]
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
    exec.spawn(program, &args).err().map(|e| format!("timer: sound could not start ({e:?})"))
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
        let before = self.timer.model;
        let after = before.apply(c, &self.timer.lengths, unix_ms());
        if after == before {
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
    }

    /// The deadline: the next change of the chip (or the end) while a timer runs, none otherwise.
    /// A deadline already armed for that instant stays.
    fn timer_sync(&mut self) {
        let now = unix_ms();
        // The popup's seconds come with the popup: for now the chip's minute.
        let at = self.timer.model.next_change(now, false).and_then(|t| Instant::now().checked_add(wait_for(t, now)));
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
