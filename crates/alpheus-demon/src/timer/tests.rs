//! Tests of the timer's model (`mod.rs`): every change, the ends, the chip, the next deadline, the
//! state file's line and the notification texts. No clock, no file, no daemon: each test passes
//! the time it wants.

use super::*;
use crate::config::TimerIdle;
use crate::notify::Urgency;

/// Lengths used by every test: 25 minutes of work, 5 of short break, 15 of long break, 4 rounds.
const L: Lengths = Lengths { work: 25 * MINUTE_MS, short: 5 * MINUTE_MS, long: 15 * MINUTE_MS, rounds: 4 };
/// The instant every test starts from (Unix milliseconds); any time works, none is special.
const T0: u64 = 1_000_000_000;

/// A countdown of `total` milliseconds that is running and ends at `end`.
fn running_countdown(end: u64, total: u64) -> Timer {
    Timer::Countdown { run: Run::Running(end), total }
}

/// A Pomodoro phase that is running and ends at `end`.
fn running_pomodoro(end: u64, phase: Phase, round: u8) -> Timer {
    Timer::Pomodoro { run: Run::Running(end), phase, round }
}

#[test]
fn a_countdown_starts_pauses_resumes_takes_a_minute_and_stops() {
    let t = Timer::Idle { done: false }.apply(Change::Countdown(1_500_000), &L, T0);
    assert_eq!(t, running_countdown(T0 + 1_500_000, 1_500_000));
    let p = t.apply(Change::Pause, &L, T0 + 60_000);
    assert_eq!(p, Timer::Countdown { run: Run::Paused(1_440_000), total: 1_500_000 });
    let r = p.apply(Change::Resume, &L, T0 + 100_000);
    assert_eq!(r, running_countdown(T0 + 1_540_000, 1_500_000));
    assert_eq!(r.apply(Change::Add(60_000), &L, T0 + 100_000), running_countdown(T0 + 1_600_000, 1_560_000));
    // Capped: the total never exceeds a day.
    let big = running_countdown(T0 + MAX_MS - 10_000, MAX_MS - 10_000);
    assert_eq!(big.apply(Change::Add(60_000), &L, T0), running_countdown(T0 + MAX_MS, MAX_MS));
    let full = running_countdown(T0 + MAX_MS, MAX_MS);
    assert_eq!(full.apply(Change::Add(60_000), &L, T0), full, "nothing to add");
    assert_eq!(t.apply(Change::Toggle, &L, T0 + 60_000), p);
    assert_eq!(p.apply(Change::Toggle, &L, T0 + 100_000), r);
    assert_eq!(t.apply(Change::Stop, &L, T0), Timer::Idle { done: false });
    let idle = Timer::Idle { done: false };
    assert_eq!(idle.apply(Change::Pause, &L, T0), idle, "nothing to pause");
    assert_eq!(idle.apply(Change::Resume, &L, T0), idle);
    assert_eq!(idle.apply(Change::Toggle, &L, T0), idle);
    assert_eq!(idle.apply(Change::Countdown(0), &L, T0), running_countdown(T0 + SECOND_MS, SECOND_MS));
    assert_eq!(idle.apply(Change::Countdown(MAX_MS + 1), &L, T0), running_countdown(T0 + MAX_MS, MAX_MS));
}

#[test]
fn a_new_start_replaces_whatever_runs_and_seen_clears_done() {
    let running = running_countdown(T0 + 5_000, 5_000);
    assert_eq!(running.apply(Change::Stopwatch, &L, T0), Timer::Stopwatch { run: Run::Running(T0) });
    assert_eq!(running.apply(Change::Pomodoro, &L, T0), running_pomodoro(T0 + L.work, Phase::Work, 1));
    let done = Timer::Idle { done: true };
    assert_eq!(done.apply(Change::Seen, &L, T0), Timer::Idle { done: false });
    assert_eq!(done.apply(Change::Stop, &L, T0), Timer::Idle { done: false });
    assert_eq!(running.apply(Change::Seen, &L, T0), running);
    assert_eq!(done.apply(Change::Countdown(60_000), &L, T0), running_countdown(T0 + 60_000, 60_000));
}

#[test]
fn add_and_skip_in_the_wrong_mode_change_nothing() {
    let sw = Timer::Stopwatch { run: Run::Running(T0) };
    let pm = running_pomodoro(T0 + 1000, Phase::Work, 1);
    let cd = running_countdown(T0 + 5_000, 5_000);
    let idle = Timer::Idle { done: false };
    for t in [sw, pm, idle] {
        assert_eq!(t.apply(Change::Add(60_000), &L, T0), t, "Add on {}", t.word());
    }
    for t in [sw, cd, idle] {
        assert_eq!(t.apply(Change::Skip, &L, T0), t, "Skip on {}", t.word());
    }
}

#[test]
fn add_on_a_paused_countdown_adds_to_left_and_total() {
    let p = Timer::Countdown { run: Run::Paused(30_000), total: 60_000 };
    assert_eq!(p.apply(Change::Add(60_000), &L, T0), Timer::Countdown { run: Run::Paused(90_000), total: 120_000 });
    let near = Timer::Countdown { run: Run::Paused(MAX_MS - 5_000), total: MAX_MS - 1_000 };
    assert_eq!(
        near.apply(Change::Add(60_000), &L, T0),
        Timer::Countdown { run: Run::Paused(MAX_MS - 4_000), total: MAX_MS },
        "capped by the total's room"
    );
}

#[test]
fn skip_on_a_paused_pomodoro_starts_the_next_phase_running() {
    let p = Timer::Pomodoro { run: Run::Paused(10_000), phase: Phase::Work, round: 2 };
    assert_eq!(p.apply(Change::Skip, &L, T0), running_pomodoro(T0 + L.short, Phase::ShortBreak, 2));
    let b = Timer::Pomodoro { run: Run::Paused(10_000), phase: Phase::ShortBreak, round: 2 };
    assert_eq!(b.apply(Change::Skip, &L, T0), running_pomodoro(T0 + L.work, Phase::Work, 3));
}

#[test]
fn the_stopwatch_counts_and_caps() {
    let s = Timer::Idle { done: false }.apply(Change::Stopwatch, &L, T0);
    assert_eq!(s, Timer::Stopwatch { run: Run::Running(T0) });
    assert_eq!(s.elapsed(T0 + 61_000), Some(61_000));
    assert_eq!(s.left(T0 + 61_000), None);
    let p = s.apply(Change::Pause, &L, T0 + 61_000);
    assert_eq!(p, Timer::Stopwatch { run: Run::Paused(61_000) });
    assert_eq!(p.elapsed(T0 + 99_999_999), Some(61_000), "paused: frozen");
    assert_eq!(p.apply(Change::Resume, &L, T0 + 90_000), Timer::Stopwatch { run: Run::Running(T0 + 29_000) });
    assert_eq!(s.due(&L, T0 + MAX_MS - 1), (s, None));
    assert_eq!(s.due(&L, T0 + MAX_MS), (Timer::Stopwatch { run: Run::Paused(MAX_MS) }, Some(End::StopwatchCap)));
    assert_eq!(s.elapsed(T0 + MAX_MS + 5_000), Some(MAX_MS), "never above a day");
}

#[test]
fn pomodoro_runs_its_rounds_then_ends() {
    // Chains `due` at each end; returns the Ends seen and the last state.
    fn run(l: &Lengths) -> (Vec<End>, Timer) {
        let mut t = Timer::Idle { done: false }.apply(Change::Pomodoro, l, T0);
        let mut now = T0;
        let mut ends = Vec::new();
        for _ in 0..100 {
            let end = match t {
                Timer::Pomodoro { run: Run::Running(e), .. } => e,
                _ => break,
            };
            now = end;
            let (n, e) = t.due(l, now);
            ends.extend(e);
            t = n;
        }
        let _ = now;
        (ends, t)
    }
    let (ends, last) = run(&L);
    use Phase::*;
    assert_eq!(
        ends,
        vec![
            End::WorkDone { next: ShortBreak },
            End::BreakDone { round: 2 },
            End::WorkDone { next: ShortBreak },
            End::BreakDone { round: 3 },
            End::WorkDone { next: ShortBreak },
            End::BreakDone { round: 4 },
            End::WorkDone { next: LongBreak },
            End::SetDone { rounds: 4 },
        ]
    );
    assert_eq!(last, Timer::Idle { done: true });
    let one = Lengths { rounds: 1, ..L };
    let (ends, last) = run(&one);
    assert_eq!(ends, vec![End::WorkDone { next: LongBreak }, End::SetDone { rounds: 1 }]);
    assert_eq!(last, Timer::Idle { done: true });
    let twelve = Lengths { rounds: 12, ..L };
    let (ends, _) = run(&twelve);
    assert_eq!(ends.iter().filter(|e| matches!(e, End::WorkDone { .. })).count(), 12);
    assert_eq!(ends.last(), Some(&End::SetDone { rounds: 12 }));
    // Skip: no End, and the long break's skip ends the set without `done`.
    let w1 = running_pomodoro(T0 + L.work, Phase::Work, 1);
    assert_eq!(w1.apply(Change::Skip, &L, T0 + 5), running_pomodoro(T0 + 5 + L.short, Phase::ShortBreak, 1));
    let last_work = running_pomodoro(T0 + L.work, Phase::Work, 4);
    assert_eq!(last_work.apply(Change::Skip, &L, T0), running_pomodoro(T0 + L.long, Phase::LongBreak, 4));
    let long = running_pomodoro(T0 + L.long, Phase::LongBreak, 4);
    assert_eq!(long.apply(Change::Skip, &L, T0), Timer::Idle { done: false });
}

#[test]
fn a_late_end_is_applied_once_and_the_next_phase_starts_now() {
    let w1 = running_pomodoro(T0 + L.work, Phase::Work, 1);
    let late = T0 + L.work + 3 * 3_600_000;
    let (t, e) = w1.due(&L, late);
    assert_eq!(t, running_pomodoro(late + L.short, Phase::ShortBreak, 1));
    assert_eq!(e, Some(End::WorkDone { next: Phase::ShortBreak }));
    let c = running_countdown(T0 + 60_000, 60_000);
    assert_eq!(c.due(&L, T0 + 60_000 + 9_000_000), (Timer::Idle { done: true }, Some(End::Countdown { total: 60_000 })));
    assert_eq!(c.due(&L, T0 + 59_999), (c, None), "before the end");
    let paused = c.apply(Change::Pause, &L, T0 + 10);
    assert_eq!(paused.due(&L, T0 + 99_999_999), (paused, None), "paused never ends");
    let idle = Timer::Idle { done: false };
    assert_eq!(idle.due(&L, T0), (idle, None));
}

#[test]
fn the_time_left_is_clamped_and_each_kind_has_its_word() {
    let c = running_countdown(T0 + 5_000_000, 1_000_000);
    assert_eq!(c.left(T0), Some(1_000_000), "a clock set back never shows more than the total");
    assert_eq!(c.left(T0 + 4_999_000), Some(1_000));
    assert_eq!(c.left(T0 + 9_000_000), Some(0));
    assert_eq!(running_pomodoro(T0 + 3 * MAX_MS, Phase::Work, 1).left(T0), Some(MAX_MS));
    assert_eq!(Timer::Countdown { run: Run::Paused(7), total: 9 }.left(T0), Some(7));
    assert_eq!(Timer::Idle { done: false }.left(T0), None);
    assert!(c.running());
    assert!(!c.apply(Change::Pause, &L, T0).running());
    assert!(!Timer::Idle { done: true }.running());
    let words = [
        (Timer::Idle { done: false }, "idle"),
        (c, "countdown"),
        (running_pomodoro(T0, Phase::Work, 1), "pomodoro"),
        (Timer::Stopwatch { run: Run::Paused(1) }, "stopwatch"),
    ];
    for (t, w) in words {
        assert_eq!(t.word(), w);
    }
    assert_eq!([Phase::Work.word(), Phase::ShortBreak.word(), Phase::LongBreak.word()], ["work", "short", "long"]);
    assert_eq!(L.phase(Phase::ShortBreak), 5 * MINUTE_MS);
}

#[test]
fn lengths_come_from_the_config() {
    let c = crate::config::TimerConfig { work: 50, short_break: 10, long_break: 30, rounds: 3, ..Default::default() };
    assert_eq!(Lengths::of(&c), Lengths { work: 50 * MINUTE_MS, short: 10 * MINUTE_MS, long: 30 * MINUTE_MS, rounds: 3 });
}

#[test]
fn next_change_follows_the_chip_or_the_seconds() {
    let e = T0 + 1_500_000;
    let c = running_countdown(e, 1_500_000);
    assert_eq!(c.next_change(T0, false), Some(e - 24 * MINUTE_MS));
    assert_eq!(c.next_change(T0, true), Some(T0 + 1_000));
    assert_eq!(c.next_change(e - 1, false), Some(e));
    assert_eq!(c.next_change(e - 1, true), Some(e));
    assert_eq!(c.next_change(e, false), Some(e));
    assert_eq!(c.next_change(e + 5, true), Some(e + 5));
    // On a boundary the next one is a whole step later.
    assert_eq!(c.next_change(e - 24 * MINUTE_MS, false), Some(e - 23 * MINUTE_MS));
    assert_eq!(c.apply(Change::Pause, &L, T0).next_change(T0, true), None);
    assert_eq!(Timer::Idle { done: true }.next_change(T0, true), None);
    let s = Timer::Stopwatch { run: Run::Running(T0) };
    assert_eq!(s.next_change(T0 + 500, false), Some(T0 + 60_000));
    assert_eq!(s.next_change(T0 + 500, true), Some(T0 + 1_000));
    assert_eq!(s.next_change(T0 + MAX_MS - 10, false), Some(T0 + MAX_MS));
    assert_eq!(s.next_change(T0 + MAX_MS - 10, true), Some(T0 + MAX_MS));
    assert_eq!(s.next_change(T0 + MAX_MS + 1, true), Some(T0 + MAX_MS + 1));
    assert_eq!(running_pomodoro(e, Phase::Work, 1).next_change(T0, false), Some(e - 24 * MINUTE_MS));
}

/// What the chip shows at `now` (idle chip as a glyph): its glyph, text and look.
fn chip_parts(t: Timer, now: u64) -> Option<(char, Option<&'static str>, ChipLook)> {
    // The text is leaked into a static to keep the table below short.
    t.chip(now, TimerIdle::Glyph).map(|c| (c.glyph, c.text.map(|s| &*Box::leak(s.into_boxed_str())), c.look))
}

#[test]
fn the_chip_reads_minutes() {
    let left = |ms: u64| running_countdown(T0 + ms, 4_000_000);
    assert_eq!(chip_parts(left(1_500_000), T0), Some((TIMER, Some("25m"), ChipLook::Running)));
    assert_eq!(chip_parts(left(1_440_001), T0), Some((TIMER, Some("25m"), ChipLook::Running)));
    assert_eq!(chip_parts(left(1_440_000), T0), Some((TIMER, Some("24m"), ChipLook::Running)));
    assert_eq!(chip_parts(left(1), T0), Some((TIMER, Some("1m"), ChipLook::Running)));
    assert_eq!(chip_parts(left(3_900_000), T0), Some((TIMER, Some("1h05"), ChipLook::Running)));
    assert_eq!(chip_parts(running_pomodoro(T0 + 900_000, Phase::ShortBreak, 1), T0), Some((BREAK, Some("15m"), ChipLook::Running)));
    assert_eq!(chip_parts(running_pomodoro(T0 + 900_000, Phase::LongBreak, 4), T0), Some((BREAK, Some("15m"), ChipLook::Running)));
    assert_eq!(chip_parts(running_pomodoro(T0 + 900_000, Phase::Work, 1), T0), Some((WORK, Some("15m"), ChipLook::Running)));
    let sw = Timer::Stopwatch { run: Run::Running(T0) };
    assert_eq!(chip_parts(sw, T0 + 59_999), Some((STOPWATCH, Some("0m"), ChipLook::Running)));
    assert_eq!(chip_parts(sw, T0 + 720_000), Some((STOPWATCH, Some("12m"), ChipLook::Running)));
    let p = Timer::Countdown { run: Run::Paused(1_440_000), total: 1_500_000 };
    assert_eq!(chip_parts(p, T0), Some((crate::segments::PAUSE, Some("24m"), ChipLook::Paused)));
    let ps = Timer::Stopwatch { run: Run::Paused(720_000) };
    assert_eq!(chip_parts(ps, T0), Some((crate::segments::PAUSE, Some("12m"), ChipLook::Paused)));
    let idle = Timer::Idle { done: false };
    assert_eq!(idle.chip(T0, TimerIdle::Glyph), Some(TimerChip { glyph: TIMER, text: None, look: ChipLook::Idle }));
    assert_eq!(idle.chip(T0, TimerIdle::Hidden), None);
    let done = TimerChip { glyph: DONE, text: Some("done".into()), look: ChipLook::Done };
    for i in [TimerIdle::Glyph, TimerIdle::Hidden] {
        assert_eq!(Timer::Idle { done: true }.chip(T0, i), Some(done.clone()));
    }
    // Hidden only hides the idle chip.
    assert!(left(60_000).chip(T0, TimerIdle::Hidden).is_some());
}

#[test]
fn minutes_clock_and_length_texts_read_as_written() {
    for (m, s) in [(0, "0m"), (59, "59m"), (60, "1h00"), (65, "1h05"), (1440, "24h00")] {
        assert_eq!(minutes_text(m), s);
    }
    for (c, s) in [(0, "0:00"), (5, "0:05"), (1453, "24:13"), (3723, "1:02:03")] {
        assert_eq!(clock_text(c), s);
    }
    for (ms, s) in [(1_500_000, "25 min"), (45_000, "45 s"), (90_000, "1 min 30 s")] {
        assert_eq!(length_text(ms), s);
    }
}

/// One timer of every kind, running and paused, for the state file's round trip.
fn a_timer_in_every_state() -> Vec<Timer> {
    use Phase::*;
    let mut v = vec![
        running_countdown(T0 + 1_500_000, 1_500_000),
        Timer::Countdown { run: Run::Paused(1_440_000), total: 1_500_000 },
        Timer::Stopwatch { run: Run::Running(T0 - 5_000) },
        Timer::Stopwatch { run: Run::Paused(61_000) },
    ];
    for (phase, round) in [(Work, 1), (ShortBreak, 12), (LongBreak, 4)] {
        v.push(running_pomodoro(T0 + 60_000, phase, round));
        v.push(Timer::Pomodoro { run: Run::Paused(60_000), phase, round });
    }
    v
}

#[test]
fn the_saved_line_round_trips() {
    for t in a_timer_in_every_state() {
        let line = t.saved().expect("saved");
        assert!(!line.contains('\n'), "one line: {line}");
        assert_eq!(Timer::parse_saved(&line, T0), Some(t), "{line}");
        assert_eq!(Timer::parse_saved(&format!("{line}\n"), T0), Some(t), "with its newline");
    }
    assert_eq!(Timer::Idle { done: false }.saved(), None);
    assert_eq!(Timer::Idle { done: true }.saved(), None);
    assert_eq!(running_countdown(T0 + 5, 7).saved().as_deref(), Some(&*format!("countdown running {} 7", T0 + 5)));
    assert_eq!(running_pomodoro(9, Phase::ShortBreak, 2).saved().as_deref(), Some("pomodoro running 9 short 2"));
    assert_eq!(Timer::Stopwatch { run: Run::Paused(3) }.saved().as_deref(), Some("stopwatch paused 3"));
}

#[test]
fn parse_saved_refuses_what_is_out_of_bounds() {
    // 25 hours: the furthest a running end may be from now and still be believed.
    let h25 = 25 * 3_600_000;
    let bad = [
        format!("countdown running {} 1000", T0 + h25 + 1),
        "countdown paused 2000 1000".to_string(),
        "countdown paused 999 999".to_string(),
        format!("countdown running {} {}", T0 + 5, MAX_MS + 1),
        "countdown paused 1000 1000 extra".to_string(),
        "countdown paused 1000".to_string(),
        "countdown sleeping 1000 1000".to_string(),
        "pomodoro paused 1000 nap 1".to_string(),
        format!("pomodoro running {} work 13", T0 + 1000),
        format!("pomodoro running {} work 0", T0 + 1000),
        format!("pomodoro running {} work", T0 + 1000),
        format!("pomodoro running {} work 1", T0 + h25 + 1),
        format!("pomodoro paused {} work 1", MAX_MS + 1),
        format!("stopwatch paused {}", MAX_MS + 1),
        "stopwatch running 5 extra".to_string(),
        "".to_string(),
        "done".to_string(),
        "countdown running x 1000".to_string(),
        "countdown running +5 1000".to_string(),
        "countdown running -5 1000".to_string(),
        "countdown running 99999999999999999999999 1000".to_string(),
        "idle".to_string(),
    ];
    for b in bad {
        assert_eq!(Timer::parse_saved(&b, T0), None, "{b}");
    }
    // A stopwatch started "in the future" (the clock stepped back) is clamped to now, not dropped.
    for ahead in [T0 + 1, T0 + 3_600_000] {
        assert_eq!(
            Timer::parse_saved(&format!("stopwatch running {ahead}"), T0),
            Some(Timer::Stopwatch { run: Run::Running(T0) }),
            "{ahead}"
        );
    }
    // The bounds themselves are accepted.
    let good = [
        format!("countdown running {} {}", T0 + h25, MAX_MS),
        format!("countdown running {} 1000", T0 - 5_000),
        format!("stopwatch running {T0}"),
        format!("stopwatch paused {MAX_MS}"),
        "countdown paused 1000 1000".to_string(),
    ];
    for g in good {
        assert!(Timer::parse_saved(&g, T0).is_some(), "{g}");
    }
}

#[test]
fn the_notifications_say_what_ended() {
    let n = |e| incoming(e, &L);
    let same = |i: &crate::notify::Incoming| {
        assert_eq!((i.urgency, i.tag.as_deref(), i.app_name.as_str()), (Urgency::Normal, Some("timer"), "alpheus"));
        assert_eq!((i.expire_ms, i.replaces_id, i.category.as_deref()), (-1, 0, None));
        assert!(i.actions.is_empty() && !i.transient && !i.resident);
    };
    let c = n(End::Countdown { total: 1_500_000 }).unwrap();
    assert_eq!((c.summary.as_str(), c.body.as_str()), ("Timer done", "25 min"));
    same(&c);
    let w = n(End::WorkDone { next: Phase::ShortBreak }).unwrap();
    assert_eq!((w.summary.as_str(), w.body.as_str()), ("Break time", "Short break \u{b7} 5 min"));
    same(&w);
    let w = n(End::WorkDone { next: Phase::LongBreak }).unwrap();
    assert_eq!(w.body, "Long break \u{b7} 15 min");
    let b = n(End::BreakDone { round: 2 }).unwrap();
    assert_eq!((b.summary.as_str(), b.body.as_str()), ("Back to work", "Round 2 of 4 \u{b7} 25 min"));
    same(&b);
    let s = n(End::SetDone { rounds: 4 }).unwrap();
    assert_eq!((s.summary.as_str(), s.body.as_str()), ("Pomodoro done", "4 rounds"));
    same(&s);
    assert_eq!(n(End::SetDone { rounds: 1 }).unwrap().body, "1 round");
    assert_eq!(n(End::StopwatchCap), None);
}
