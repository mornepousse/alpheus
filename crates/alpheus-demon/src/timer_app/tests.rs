use super::*;
use crate::timer::{MAX_MS, MINUTE_MS, Run};
use alpheus_commun::exec::fake::FakeExecutor;

const L: Lengths = Lengths { work: 25 * MINUTE_MS, short: 5 * MINUTE_MS, long: 15 * MINUTE_MS, rounds: 4 };
const T0: u64 = 1_000_000_000;
const IDLE: Model = Model::Idle { done: false };

fn pomo(end: u64, phase: Phase, round: u8) -> Model {
    Model::Pomodoro { run: Run::Running(end), phase, round }
}

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
    assert_eq!(line(pomo(T0 + 1, Phase::Work, 1), Change::Skip).as_deref(), Some("timer: skipped to short break"));
    assert_eq!(line(pomo(T0 + 1, Phase::Work, 4), Change::Skip).as_deref(), Some("timer: skipped to long break"));
    assert_eq!(line(pomo(T0 + 1, Phase::ShortBreak, 1), Change::Skip).as_deref(), Some("timer: skipped to work"));
    assert_eq!(line(pomo(T0 + 1, Phase::LongBreak, 4), Change::Skip).as_deref(), Some("timer: skipped to the end"));
    assert_eq!(line(IDLE, Change::Pause), None, "nothing to pause");
    assert_eq!(line(IDLE, Change::Skip), None, "nothing to skip");
    let done = Model::Idle { done: true };
    assert_eq!(line(done, Change::Seen), None, "Seen says nothing");
    assert_eq!(change_line(&running, &running, Change::Seen, &L), None);
}

#[test]
fn end_lines() {
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
fn wait_for_never_overflows() {
    assert_eq!(wait_for(10, 0), Duration::from_millis(10));
    assert_eq!(wait_for(0, 10), Duration::ZERO);
    assert_eq!(wait_for(u64::MAX, 0), Duration::from_millis(25 * 3_600_000));
}

#[test]
fn check_clock_applies_one_end_and_saves() {
    let (m, end, save) = step(pomo(T0, Phase::Work, 1), &L, T0 + 5_000);
    assert_eq!(end, Some(End::WorkDone { next: Phase::ShortBreak }));
    assert_eq!(m, pomo(T0 + 5_000 + L.short, Phase::ShortBreak, 1), "one phase only, started now");
    assert!(save);
    let before = pomo(T0 + 1_000, Phase::Work, 1);
    assert_eq!(step(before, &L, T0), (before, None, false));
    assert_eq!(step(IDLE, &L, T0), (IDLE, None, false));
    let (m, end, save) = step(Model::Countdown { run: Run::Running(T0), total: MINUTE_MS }, &L, T0);
    assert_eq!((m, end.is_some(), save), (Model::Idle { done: true }, true, true));
}

#[test]
fn loading_reads_drops_or_ends_silently() {
    let l = load(None, &L, T0);
    assert_eq!(l, Loaded { model: IDLE, unreadable: false, ended: false });
    let running = pomo(T0 + 60_000, Phase::Work, 2);
    let l = load(running.saved().as_deref(), &L, T0);
    assert_eq!(l, Loaded { model: running, unreadable: false, ended: false });
    for bad in ["", "garbage", "countdown running 1 2 3", "countdown paused 999 999"] {
        let l = load(Some(bad), &L, T0);
        assert_eq!(l, Loaded { model: IDLE, unreadable: true, ended: false }, "{bad:?}");
    }
    let late = Model::Countdown { run: Run::Running(T0 - 1), total: MAX_MS };
    let l = load(late.saved().as_deref(), &L, T0);
    assert_eq!(l, Loaded { model: Model::Idle { done: true }, unreadable: false, ended: true });
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
