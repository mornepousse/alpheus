//! Tests of the scheduled shutdown's model (`shutdown.rs`): the warning, the late end, the chip
//! text, the state line and the choices. No clock, no file, no daemon.

use super::*;
use crate::notify::Urgency;

/// The instant every test starts from (Unix milliseconds).
const T0: u64 = 1_000_000_000;

#[test]
fn the_warning_comes_sixty_seconds_before_the_end() {
    let s = Shutdown { end: T0 + 3_600_000, set: T0 };
    // Far from the end: nothing due, the next step is the warning.
    assert_eq!(next(&s, T0, false), (Some(T0 + 3_600_000 - WARN_MS), None));
    assert_eq!(next(&s, T0 + 3_600_000 - WARN_MS - 1, false), (Some(T0 + 3_600_000 - WARN_MS), None));
    // At T-60 s the warning is due, and it is given now.
    let at = T0 + 3_600_000 - WARN_MS;
    assert_eq!(next(&s, at, false).1, Some(ShutdownStep::Warn { at }));
    // Warned: the next step is the end, then the power off.
    assert_eq!(next(&s, at, true), (Some(s.end), None));
    assert_eq!(next(&s, s.end - 1, true), (Some(s.end), None));
    assert_eq!(next(&s, s.end, true), (None, Some(ShutdownStep::PowerOff)));
}

#[test]
fn a_past_shutdown_warns_first_then_powers_off() {
    // The laptop woke up an hour after the end: the warning first, never the power off.
    let s = Shutdown { end: T0, set: T0 };
    let now = T0 + 3_600_000;
    let (_, due) = next(&s, now, false);
    assert_eq!(due, Some(ShutdownStep::Warn { at: now }), "a warning, not a power off");
    // Once warned, the end is a minute from now.
    let warned = s.warned_at(now);
    assert_eq!(warned.end, now + WARN_MS);
    assert_eq!(next(&warned, now, true), (Some(now + WARN_MS), None), "nothing at once");
    assert_eq!(next(&warned, now + WARN_MS, true).1, Some(ShutdownStep::PowerOff));
    // An end that is still ahead is kept when the warning is late by less than its minute.
    let ahead = Shutdown { end: now + 30_000, set: T0 };
    assert_eq!(ahead.warned_at(now).end, now + WARN_MS, "never less than a minute after the warning");
    let far = Shutdown { end: now + 90_000, set: T0 };
    assert_eq!(far.warned_at(now).end, far.end, "an end past the minute is kept");
}

#[test]
fn cancel_clears_it() {
    assert_eq!(apply(Some(Shutdown { end: T0 + 1, set: T0 }), ShutdownChoice::Cancel, T0), None);
    assert_eq!(apply(None, ShutdownChoice::Cancel, T0), None);
    // A duration replaces the one pending.
    let s = apply(Some(Shutdown { end: T0 + 1, set: T0 }), ShutdownChoice::After(1_800_000), T0);
    assert_eq!(s, Some(Shutdown { end: T0 + 1_800_000, set: T0 }));
}

#[test]
fn the_chip_text_is_the_time_left_and_turns_warn_in_the_last_minute() {
    let s = Shutdown { end: T0 + 80 * MINUTE_MS, set: T0 };
    assert_eq!(s.chip_text(T0), "1h20");
    assert_eq!(s.chip_text(T0 + 1), "1h20", "rounded up");
    assert!(!s.in_warning(T0));
    assert!(!s.in_warning(s.end - WARN_MS - 1));
    assert!(s.in_warning(s.end - WARN_MS));
    assert_eq!(s.chip_text(s.end - 30_000), "1m");
    // The chip changes once a minute, and at the warning.
    assert_eq!(wake(&s, T0, false), Some(T0 + MINUTE_MS)); // 1h20 reads 1h19 from a minute on
    let quiet = Shutdown { end: T0 + 80 * MINUTE_MS - 10, set: T0 };
    assert_eq!(wake(&quiet, T0, false), Some(T0 + 80 * MINUTE_MS - 10 - 79 * MINUTE_MS));
}

#[test]
fn the_state_line_round_trips_and_refuses_the_rest() {
    let s = Shutdown { end: T0 + 5, set: T0 };
    assert_eq!(s.saved(), format!("shutdown {} {T0}", T0 + 5));
    assert_eq!(Shutdown::parse_saved(&s.saved(), T0), Some(s));
    // Past ends are kept: the warning handles them.
    assert_eq!(Shutdown::parse_saved("shutdown 5 7", T0), Some(Shutdown { end: 5, set: 7 }));
    // The old one-number line reads with a scheduled-at time of 0.
    assert_eq!(Shutdown::parse_saved("shutdown 5", T0), Some(Shutdown { end: 5, set: 0 }));
    for bad in [
        "shutdown",
        "shutdown -5",
        "shutdown 1 2 3",
        "shutdown 1 x",
        "shutdown 12x",
        "shutdown  ",
        "poweroff 5",
        "shutdown 99999999999999999999999",
    ] {
        assert_eq!(Shutdown::parse_saved(bad, T0), None, "{bad}");
    }
    assert_eq!(Shutdown::parse_saved(&format!("shutdown {}", T0 + 26 * 3_600_000), T0), None, "too far ahead to be ours");
}

#[test]
fn the_choices_are_the_five_durations_and_a_cancel_while_one_is_pending() {
    let idle = labels(None);
    assert_eq!(idle, ["30 min", "1 h", "1 h 30", "2 h", "3 h"]);
    let pending = labels(Some(80 * MINUTE_MS));
    assert_eq!(pending[0], "Cancel shutdown (in 1h20)");
    assert_eq!(&pending[1..], ["30 min", "1 h", "1 h 30", "2 h", "3 h"]);
    assert_eq!(choice_of("30 min"), Some(ShutdownChoice::After(30 * MINUTE_MS)));
    assert_eq!(choice_of("1 h"), Some(ShutdownChoice::After(60 * MINUTE_MS)));
    assert_eq!(choice_of("1 h 30"), Some(ShutdownChoice::After(90 * MINUTE_MS)));
    assert_eq!(choice_of("2 h"), Some(ShutdownChoice::After(120 * MINUTE_MS)));
    assert_eq!(choice_of("3 h"), Some(ShutdownChoice::After(180 * MINUTE_MS)));
    assert_eq!(choice_of(&pending[0]), Some(ShutdownChoice::Cancel));
    assert_eq!(choice_of("Cancel shutdown"), Some(ShutdownChoice::Cancel));
    assert_eq!(choice_of("4 h"), None);
}

#[test]
fn the_warning_is_a_sticky_critical_notice_with_a_cancel_action() {
    let n = warning();
    assert_eq!(n.urgency, Urgency::Critical);
    assert_eq!(n.expire_ms, 0, "never expires by itself");
    assert_eq!(n.tag.as_deref(), Some("shutdown"));
    assert_eq!(n.actions.len(), 1);
    assert_eq!((n.actions[0].key.as_str(), n.actions[0].label.as_str()), ("default", "Cancel"));
    assert!(!n.transient);
}

#[test]
fn a_warning_holds_only_if_the_wall_clock_did_not_outrun_the_monotonic_one() {
    let s = Shutdown { end: T0 + 600_000, set: T0 };
    let warned = Some(s.end - 60_000);
    let ms = Duration::from_millis;
    // No sleep: equal elapsed times.
    assert!(warning_holds(&s, warned, ms(60_000), s.end));
    // Not warned: nothing holds.
    assert!(!warning_holds(&s, None, ms(60_000), s.end));
    // 12 s of sleep: the wall clock ran 12 s more than the monotonic one.
    assert!(!warning_holds(&s, warned, ms(60_000), s.end + 12_000));
    assert!(!warning_holds(&s, warned, ms(48_000), s.end));
    // The slack: a second of difference is drift, not a sleep.
    assert!(warning_holds(&s, warned, ms(59_000), s.end));
    assert!(!warning_holds(&s, warned, ms(58_999), s.end));
    assert!(!warning_holds(&s, warned, ms(60_000), s.end + 3_000), "a 3 s sleep");
    // The clock went back before the warning.
    assert!(!warning_holds(&s, warned, ms(10), s.end - 61_000));
    // The end passed by more than the slack, however the clocks agree.
    assert!(!warning_holds(&s, warned, ms(66_000), s.end + 6_000));
}

/// While the warning's minute runs the lock is looked at every few seconds; at rest, and before the
/// warning, the wake is what it was.
#[test]
fn the_lock_is_sampled_only_while_the_warning_minute_runs() {
    let s = Shutdown { end: T0 + 80 * MINUTE_MS, set: T0 };
    assert_eq!(wake(&s, T0, false), Some(T0 + MINUTE_MS), "far from the end: only the chip's minute");
    let warned_at = s.end - 60_000;
    let w = Shutdown { end: warned_at + WARN_MS, set: T0 };
    assert_eq!(wake(&w, warned_at, true), Some(warned_at + LOCK_SAMPLE_MS));
    assert_eq!(wake(&w, warned_at + 57_000, true), Some(w.end), "the end comes before the next look");
    assert_eq!(wake(&w, w.end, true), None, "nothing left once the power off is due");
    assert_eq!(LOCK_SAMPLE_MS * 12, WARN_MS, "twelve looks a minute at most");
}
