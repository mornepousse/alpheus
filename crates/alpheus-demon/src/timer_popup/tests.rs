use super::*;
use crate::timer::{BREAK, Run, Timer};

const L: Lengths = Lengths { work: 25 * MINUTE_MS, short: 5 * MINUTE_MS, long: 15 * MINUTE_MS, rounds: 4 };
const T0: u64 = 1_000_000_000;

fn idle() -> Timer {
    Timer::Idle { done: false }
}

fn countdown(end: u64, total: u64) -> Timer {
    Timer::Countdown { run: Run::Running(end), total }
}

fn pomo(phase: Phase, round: u8) -> Timer {
    Timer::Pomodoro { run: Run::Running(T0 + 60_000), phase, round }
}

fn stopwatch() -> Timer {
    Timer::Stopwatch { run: Run::Running(T0) }
}

fn paused() -> Timer {
    Timer::Countdown { run: Run::Paused(60_000), total: 120_000 }
}

fn texts(b: &Boxed) -> Vec<String> {
    b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
}

fn rows(p: &TimerPopup, t: &Timer) -> Boxed {
    p.render(t, &L, T0)
}

#[test]
fn the_items_follow_the_state() {
    use Item::*;
    assert_eq!(items(&idle()), [Countdown, Pomodoro, Stopwatch]);
    assert_eq!(items(&Timer::Idle { done: true }), [Countdown, Pomodoro, Stopwatch]);
    assert_eq!(items(&countdown(T0 + 1000, 1000)), [Pause, Add, Stop]);
    assert_eq!(items(&paused()), [Resume, Add, Stop]);
    assert_eq!(items(&pomo(Phase::Work, 1)), [Pause, Skip, Stop]);
    assert_eq!(items(&stopwatch()), [Pause, Stop]);
}

#[test]
fn step_minutes_steps_one_then_five() {
    for (from, delta, to) in
        [(1, -1, 1), (9, 1, 10), (10, 1, 15), (10, -1, 9), (15, -1, 10), (12, -1, 10), (12, 1, 15), (180, 1, 180), (9, 3, 20)]
    {
        assert_eq!(step_minutes(from, delta), to, "{from} {delta:+}");
    }
}

#[test]
fn the_rows_read_the_state() {
    let p = TimerPopup::new(10, &idle());
    let r = texts(&rows(&p, &idle()));
    assert_eq!(r[0], format!(" {TIMER} Ready"));
    assert!(r[2].starts_with(&format!(" {TIMER} Timer")) && r[2].ends_with("\u{2039} 10 min \u{203a}"), "{:?}", r[2]);
    assert!(r[3].ends_with("25 \u{b7} 5 min"), "{:?}", r[3]);
    assert_eq!(r[4], format!(" {STOPWATCH} Stopwatch"));
    let done = Timer::Idle { done: true };
    let b = rows(&TimerPopup::new(10, &done), &done);
    assert_eq!(texts(&b)[0], format!(" {DONE} Done"));
    assert_eq!(&b.rows[0].styles()[1..7], "w.wwww", "in warn (the gap is plain)");
    let c = countdown(T0 + 24 * 60_000 + 13_000, 25 * 60_000);
    let r = texts(&rows(&TimerPopup::new(10, &c), &c));
    assert!(r[0].starts_with(&format!(" {TIMER} 24:13")) && r[0].ends_with("of 25:00"), "{:?}", r[0]);
    assert!(r[2].starts_with(&format!(" {PAUSE} Pause")));
    for (t, end) in
        [(pomo(Phase::Work, 2), "work 2/4"), (pomo(Phase::ShortBreak, 2), "break 2/4"), (pomo(Phase::LongBreak, 4), "long break")]
    {
        let r = texts(&rows(&TimerPopup::new(10, &t), &t));
        assert!(r[0].ends_with(end), "{:?}", r[0]);
    }
    assert!(
        texts(&rows(&TimerPopup::new(10, &pomo(Phase::ShortBreak, 1)), &pomo(Phase::ShortBreak, 1)))[0].starts_with(&format!(" {BREAK}"))
    );
    let p = paused();
    let b = rows(&TimerPopup::new(10, &p), &p);
    assert!(texts(&b)[0].starts_with(&format!(" {PAUSE}")));
    assert_eq!(&b.rows[0].styles()[1..7], "m.mmmm", "muted (the gap is plain)");
    assert!(texts(&b)[2].starts_with(&format!(" {PLAY} Resume")));
    let s = Timer::Stopwatch { run: Run::Running(T0 - 65_000) };
    assert!(texts(&rows(&TimerPopup::new(10, &s), &s))[0].starts_with(&format!(" {STOPWATCH} 1:05")));
    let mut p = TimerPopup::new(10, &idle());
    let b = rows(&p, &idle());
    assert_eq!(b.rows[2].styles(), "S".repeat(usize::from(WIDTH)), "the selection band across the row");
    assert_eq!(b.cells(), (WIDTH, 5));
    assert_eq!(b.separators, [1]);
    p.selected = 1;
    assert_eq!(rows(&p, &idle()).rows[3].styles(), "S".repeat(usize::from(WIDTH)));
}

#[test]
fn the_keys_move_adjust_and_act() {
    let t = idle();
    let mut p = TimerPopup::new(10, &t);
    assert_eq!(p.selected, 0);
    for k in [Key::Down, Key::Tab, Key::Down] {
        p.key(&t, k, false);
    }
    assert_eq!(p.selected, 2, "clamped");
    assert_eq!(p.key(&t, Key::Down, false), TimerAction::Nothing);
    p.key(&t, Key::Up, false);
    p.key(&t, Key::BackTab, false);
    assert_eq!(p.selected, 0);
    assert_eq!(p.key(&t, Key::Right, false), TimerAction::Redraw);
    assert_eq!(p.minutes, 15);
    p.key(&t, Key::Left, false);
    assert_eq!(p.key(&t, Key::Left, false), TimerAction::Redraw);
    assert_eq!(p.minutes, 9);
    assert_eq!(p.key(&t, Key::Enter, false), TimerAction::Do(Change::Countdown(9 * 60_000)));
    p.key(&t, Key::Down, false);
    assert_eq!(p.key(&t, Key::Left, false), TimerAction::Nothing, "another row");
    assert_eq!(p.key(&t, Key::Char(' '), false), TimerAction::Do(Change::Pomodoro));
    assert_eq!(p.key(&t, Key::Escape, false), TimerAction::Close);
    assert_eq!(p.key(&t, Key::Enter, true), TimerAction::Nothing, "a held Enter acts once");
    assert_eq!(p.key(&t, Key::Down, true), TimerAction::Redraw, "a held arrow moves");
    let r = pomo(Phase::Work, 1);
    let mut p = TimerPopup::new(10, &countdown(T0 + 1000, 1000));
    let t = countdown(T0 + 1000, 1000);
    assert_eq!(p.key(&t, Key::Enter, false), TimerAction::Do(Change::Pause));
    p.key(&t, Key::Down, false);
    assert_eq!(p.key(&t, Key::Enter, false), TimerAction::Do(Change::Add(60_000)));
    p.key(&t, Key::Down, false);
    assert_eq!(p.key(&t, Key::Enter, false), TimerAction::Do(Change::Stop));
    let mut p = TimerPopup::new(10, &r);
    p.key(&r, Key::Down, false);
    assert_eq!(p.key(&r, Key::Enter, false), TimerAction::Do(Change::Skip));
}

#[test]
fn follow_resets_the_selection_when_the_items_change() {
    let mut p = TimerPopup::new(10, &idle());
    p.key(&idle(), Key::Down, false);
    p.key(&idle(), Key::Down, false);
    assert_eq!(p.selected, 2);
    assert!(p.follow(&stopwatch()));
    assert_eq!(p.selected, 0);
    assert_eq!(items(&stopwatch())[p.selected], Item::Pause);
    assert!(!p.follow(&stopwatch()), "the same items again");
}

#[test]
fn the_pointer_selects_presses_and_scrolls() {
    let t = idle();
    let mut p = TimerPopup::new(10, &t);
    assert_eq!(p.hover(&t, FIRST_ITEM_ROW + 1), TimerAction::Redraw);
    assert_eq!(p.selected, 1);
    assert_eq!(p.hover(&t, 0), TimerAction::Nothing, "the status row");
    assert_eq!(p.hover(&t, FIRST_ITEM_ROW + 3), TimerAction::Nothing, "below the last");
    assert_eq!(p.press(&t, FIRST_ITEM_ROW), TimerAction::Do(Change::Countdown(10 * 60_000)));
    assert_eq!(p.selected, 0);
    assert_eq!(p.press(&t, 1), TimerAction::Nothing);
    assert_eq!(p.scroll(&t, FIRST_ITEM_ROW, -1), TimerAction::Redraw);
    assert_eq!(p.minutes, 15);
    assert_eq!(p.scroll(&t, FIRST_ITEM_ROW, 1), TimerAction::Redraw);
    assert_eq!(p.minutes, 10);
    assert_eq!(p.scroll(&t, FIRST_ITEM_ROW + 1, -1), TimerAction::Nothing);
}
