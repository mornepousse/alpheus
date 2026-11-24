//! Tests of the timer's wiring (`app.rs`) that need no running daemon: its pure helpers (what an
//! end does, the log lines, the sound, the wait, the clock check, loading the state file). A fake
//! executor stands in for the real one, so no program is ever started.

use super::*;
use crate::timer::shutdown::WARN_MS;
use crate::timer::{MAX_MS, MINUTE_MS, Run};
use alpheus_common::exec::ExecError;
use alpheus_common::exec::fake::FakeExecutor;

/// The same lengths as the model's tests: 25 minutes of work, 5 and 15 of break, 4 rounds.
const L: Lengths = Lengths { work: 25 * MINUTE_MS, short: 5 * MINUTE_MS, long: 15 * MINUTE_MS, rounds: 4 };
/// The instant every test starts from (Unix milliseconds).
const T0: u64 = 1_000_000_000;
/// A timer with nothing running and nothing to dismiss.
const IDLE: Model = Model::Idle { done: false };

/// A Pomodoro phase that is running and ends at `end`.
fn running_pomodoro(end: u64, phase: Phase, round: u8) -> Model {
    Model::Pomodoro { run: Run::Running(end), phase, round }
}

/// A sound command; it is never run, the tests only check whether one is set.
fn sound() -> Vec<String> {
    vec!["x".to_string()]
}

#[test]
fn effects_follow_dnd_serving_and_the_sound() {
    assert_eq!(effects(true, false, &sound()), Effects { notify: true, sound: true });
    assert_eq!(effects(true, true, &sound()), Effects { notify: true, sound: false });
    assert_eq!(effects(false, false, &sound()), Effects { notify: false, sound: true });
    assert_eq!(effects(false, true, &sound()), Effects { notify: false, sound: false });
    assert_eq!(effects(true, false, &[]), Effects { notify: true, sound: false });
}

#[test]
fn change_lines_are_fixed_words() {
    let line = |before: Model, c: Change| change_line(&before, &before.apply(c, &L, T0), c, &L);
    assert_eq!(line(IDLE, Change::Countdown(1_500_000)).as_deref(), Some("timer: countdown started (25 min)"));
    assert_eq!(line(IDLE, Change::Countdown(90_000)).as_deref(), Some("timer: countdown started (1 min 30 s)"));
    assert_eq!(line(IDLE, Change::Pomodoro).as_deref(), Some("timer: pomodoro started"));
    assert_eq!(line(IDLE, Change::Stopwatch).as_deref(), Some("timer: stopwatch started"));
    let running = IDLE.apply(Change::Countdown(1_500_000), &L, T0);
    let paused = running.apply(Change::Pause, &L, T0);
    assert_eq!(line(running, Change::Pause).as_deref(), Some("timer: paused"));
    assert_eq!(line(running, Change::Toggle).as_deref(), Some("timer: paused"));
    assert_eq!(line(paused, Change::Resume).as_deref(), Some("timer: resumed"));
    assert_eq!(line(paused, Change::Toggle).as_deref(), Some("timer: resumed"));
    assert_eq!(line(running, Change::Add(60_000)).as_deref(), Some("timer: added 1 min"));
    assert_eq!(line(running, Change::Stop).as_deref(), Some("timer: stopped"));
    assert_eq!(line(running_pomodoro(T0 + 1, Phase::Work, 1), Change::Skip).as_deref(), Some("timer: skipped to short break"));
    assert_eq!(line(running_pomodoro(T0 + 1, Phase::Work, 4), Change::Skip).as_deref(), Some("timer: skipped to long break"));
    assert_eq!(line(running_pomodoro(T0 + 1, Phase::ShortBreak, 1), Change::Skip).as_deref(), Some("timer: skipped to work"));
    assert_eq!(line(running_pomodoro(T0 + 1, Phase::LongBreak, 4), Change::Skip).as_deref(), Some("timer: skipped to the end"));
    assert_eq!(line(IDLE, Change::Pause), None, "nothing to pause");
    assert_eq!(line(IDLE, Change::Skip), None, "nothing to skip");
    let done = Model::Idle { done: true };
    assert_eq!(line(done, Change::Seen), None, "Seen says nothing");
    assert_eq!(change_line(&running, &running, Change::Seen, &L), None);
}

#[test]
fn each_end_has_its_fixed_log_line() {
    assert_eq!(end_line(End::Countdown { total: 1 }), "timer: countdown ended");
    assert_eq!(end_line(End::WorkDone { next: Phase::ShortBreak }), "timer: pomodoro work ended");
    assert_eq!(end_line(End::BreakDone { round: 2 }), "timer: pomodoro break ended");
    assert_eq!(end_line(End::SetDone { rounds: 4 }), "timer: pomodoro set ended");
    assert_eq!(end_line(End::StopwatchCap), "timer: stopwatch stopped at 24 h");
}

#[test]
fn the_sound_spawns_its_argv_and_logs_no_command() {
    let fake = FakeExecutor::new();
    fake.ok("touch /fictional/rang", "");
    let argv = vec!["touch".to_string(), "/fictional/rang".to_string()];
    assert_eq!(spawn_sound(&fake, &argv), None);
    assert_eq!(fake.calls(), ["touch /fictional/rang"]);
    let fake = FakeExecutor::new();
    let mine = vec!["/home/someone/bin/ring".to_string(), "--user".into(), "someone".into()];
    let line = spawn_sound(&fake, &mine).unwrap_or_default();
    assert_eq!(line, "timer: sound could not start (not found)");
    assert!(!line.contains("someone") && !line.contains("ring"), "never the command: {line}");
    for (e, word) in [(ExecError::Io("/home/someone/x: busy".into()), "io"), (ExecError::Timeout, "timeout")] {
        let fake = FakeExecutor::new();
        fake.respond("ring", Err(e));
        let line = spawn_sound(&fake, &["ring".to_string()]).unwrap_or_default();
        assert_eq!(line, format!("timer: sound could not start ({word})"), "a kind word, never the error's text");
    }
    let fake = FakeExecutor::new();
    assert_eq!(spawn_sound(&fake, &[]), None);
    assert!(fake.calls().is_empty(), "no sound set: nothing spawned");
}

#[test]
fn the_wait_for_a_target_is_zero_when_past_and_never_overflows() {
    assert_eq!(wait_for(10, 0), Duration::from_millis(10));
    assert_eq!(wait_for(0, 10), Duration::ZERO);
    assert_eq!(wait_for(u64::MAX, 0), Duration::from_millis(25 * 3_600_000));
}

#[test]
fn the_clock_check_applies_one_end_and_says_to_save() {
    let (m, end, save) = step(running_pomodoro(T0, Phase::Work, 1), &L, T0 + 5_000);
    assert_eq!(end, Some(End::WorkDone { next: Phase::ShortBreak }));
    assert_eq!(m, running_pomodoro(T0 + 5_000 + L.short, Phase::ShortBreak, 1), "one phase only, started now");
    assert!(save);
    let before = running_pomodoro(T0 + 1_000, Phase::Work, 1);
    assert_eq!(step(before, &L, T0), (before, None, false));
    assert_eq!(step(IDLE, &L, T0), (IDLE, None, false));
    let (m, end, save) = step(Model::Countdown { run: Run::Running(T0), total: MINUTE_MS }, &L, T0);
    assert_eq!((m, end.is_some(), save), (Model::Idle { done: true }, true, true));
}

#[test]
fn loading_reads_a_good_file_drops_a_bad_one_and_ends_silently() {
    let l = load(None, &L, T0);
    assert_eq!(l, Loaded { model: IDLE, shutdown: None, unreadable: false, ended: false });
    let running = running_pomodoro(T0 + 60_000, Phase::Work, 2);
    let l = load(running.saved().as_deref(), &L, T0);
    assert_eq!(l, Loaded { model: running, shutdown: None, unreadable: false, ended: false });
    for bad in ["", "garbage", "countdown running 1 2 3", "countdown paused 999 999"] {
        let l = load(Some(bad), &L, T0);
        assert_eq!(l, Loaded { model: IDLE, shutdown: None, unreadable: true, ended: false }, "{bad:?}");
    }
    let late = Model::Countdown { run: Run::Running(T0 - 1), total: MAX_MS };
    let l = load(late.saved().as_deref(), &L, T0);
    assert_eq!(l, Loaded { model: Model::Idle { done: true }, shutdown: None, unreadable: false, ended: true });
}

#[test]
fn a_deadline_already_armed_for_that_instant_stays() {
    let t = Instant::now();
    assert!(same_deadline(t, t + Duration::from_millis(4)));
    assert!(same_deadline(t + Duration::from_millis(4), t));
    assert!(!same_deadline(t, t + Duration::from_millis(6)));
    assert!(!same_deadline(t + Duration::from_millis(6), t));
}

/// A change after a resume applies the end that passed first: Pause on a countdown whose end is
/// behind us is "ended" (done), never "paused"; a change before the end is just the change.
#[test]
fn a_due_end_is_applied_before_the_change() {
    let ended = Model::Countdown { run: Run::Running(T0 - 1), total: 60_000 };
    let (due, end, after) = change_step(ended, Change::Pause, &L, T0);
    assert_eq!(end, Some(End::Countdown { total: 60_000 }));
    assert_eq!((due, after), (Model::Idle { done: true }, Model::Idle { done: true }), "nothing left to pause");
    let (_, end, after) = change_step(ended, Change::Add(60_000), &L, T0);
    assert_eq!((end, after), (Some(End::Countdown { total: 60_000 }), Model::Idle { done: true }), "nor to extend");
    let running = Model::Countdown { run: Run::Running(T0 + 30_000), total: 60_000 };
    let (due, end, after) = change_step(running, Change::Pause, &L, T0);
    assert_eq!((due, end), (running, None));
    assert_eq!(after, Model::Countdown { run: Run::Paused(30_000), total: 60_000 });
    let (due, end, after) = change_step(IDLE, Change::Seen, &L, T0);
    assert_eq!((due, end, after), (IDLE, None, IDLE));
}

/// An end seen in the open popup is seen: closing it dismisses `done`, and only `done`.
#[test]
fn closing_the_popup_dismisses_a_done_chip_only() {
    assert_eq!(seen_at_close(&Model::Idle { done: true }), Some(Change::Seen));
    assert_eq!(seen_at_close(&IDLE), None);
    assert_eq!(seen_at_close(&running_pomodoro(T0 + 1000, Phase::Work, 1)), None);
    assert_eq!(seen_at_close(&Model::Stopwatch { run: Run::Paused(5) }), None);
}

/// What a shutdown at `end` does at `now`: after one clock check (warned or not before).
fn check(end: u64, warned: bool, now: u64) -> (Option<Shutdown>, bool, Option<ShutdownStep>) {
    shutdown_step(Shutdown { end }, warned, now)
}

#[test]
fn the_shutdown_check_warns_then_powers_off_and_never_at_once() {
    // Not yet: nothing happens, the shutdown stays.
    assert_eq!(check(T0 + 10 * MINUTE_MS, false, T0), (Some(Shutdown { end: T0 + 10 * MINUTE_MS }), false, None));
    // The warning: given once, and the end is kept.
    let end = T0 + 10 * MINUTE_MS;
    let (s, warned, due) = check(end, false, end - 30_000);
    assert_eq!(due, Some(ShutdownStep::Warn { at: end - 30_000 }));
    assert!(warned);
    assert_eq!(s, Some(Shutdown { end: end + 30_000 }), "a warning always has its minute");
    // A laptop that wakes long after the end: the warning, not the power off.
    let (s, warned, due) = check(T0, false, T0 + 3_600_000);
    assert_eq!((warned, due), (true, Some(ShutdownStep::Warn { at: T0 + 3_600_000 })));
    assert_eq!(s, Some(Shutdown { end: T0 + 3_600_000 + WARN_MS }));
    // Warned and the end reached: it goes, and nothing is left.
    assert_eq!(check(T0, true, T0), (None, false, Some(ShutdownStep::PowerOff)));
    // Warned, the end ahead: waits.
    assert_eq!(check(T0 + 5, true, T0), (Some(Shutdown { end: T0 + 5 }), true, None));
}

#[test]
fn the_power_off_runs_the_configured_command_and_logs_no_command() {
    let fake = FakeExecutor::new();
    fake.ok("systemctl poweroff", "");
    let argv = vec!["systemctl".to_string(), "poweroff".to_string()];
    assert_eq!(spawn_poweroff(&fake, &argv), "timer: shutdown power off");
    assert_eq!(fake.calls(), ["systemctl poweroff"]);
    let mine = vec!["/home/someone/bin/off".to_string()];
    let fail = FakeExecutor::new();
    let line = spawn_poweroff(&fail, &mine);
    assert_eq!(line, "timer: shutdown power off could not start (not found)");
    assert!(!line.contains("someone"), "{line}");
    let none = FakeExecutor::new();
    assert_eq!(spawn_poweroff(&none, &[]), "timer: shutdown has no poweroff command");
    assert!(none.calls().is_empty(), "nothing set: nothing spawned");
}

#[test]
fn loading_keeps_a_shutdown_beside_the_timer_and_never_drops_a_late_one() {
    let running = running_pomodoro(T0 + 60_000, Phase::Work, 2);
    let s = Shutdown { end: T0 + 80 * MINUTE_MS };
    let text = saved_pair(&running, Some(&s)).unwrap_or_default();
    assert_eq!(load(Some(&text), &L, T0), Loaded { model: running, shutdown: Some(s), unreadable: false, ended: false });
    // A shutdown whose end is behind us is kept: the wiring warns first.
    let past = Shutdown { end: T0 - 1 };
    let l = load(Some(&past.saved()), &L, T0);
    assert_eq!(l, Loaded { model: IDLE, shutdown: Some(past), unreadable: false, ended: false });
    // A bad line drops both.
    let l = load(Some("shutdown nonsense"), &L, T0);
    assert_eq!(l, Loaded { model: IDLE, shutdown: None, unreadable: true, ended: false });
}
