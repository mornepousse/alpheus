//! One calloop timer at a deadline while something is pending, and none otherwise.
//!
//! A `DeadlineTimer` is how a feature of the daemon waits for a moment to come: the tray's 2 s call
//! timeouts, the tooltip's hover delay, the timer chip's next minute, a `failed` tag's four
//! seconds. `sync` arms it, moves it, or removes it when the deadline is None; a deadline it
//! already holds is left alone, so a repeated sync does not churn the loop. The callback marks it
//! fired before anything arms it again. A periodic tick would cost a wake-up a second for ever:
//! this costs nothing at rest.
//!
//! Never:
//! - stay armed with nothing pending: `nothing_resident_while_the_launcher_is_closed` holds every
//!   timer the daemon builds with the reason it does not run at rest, and each feature's
//!   `at_rest_no_wakeups_*` test measures that nothing wakes;
//! - fire twice for one deadline: the callback drops its own timer (`fired`).
//!
//! See: docs/ARCHITECTURE.md ("The core", "Threads"), docs/decisions.md ("One deadline, armed only
//! while a timer runs"), docs/measurements.md (the wake-up counts at rest).

use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::RegistrationToken;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use std::time::Instant;

/// One calloop Timer at `deadline` while there is one, none otherwise: nothing wakes the loop at rest.
#[derive(Debug, Default)]
pub struct DeadlineTimer {
    token: Option<RegistrationToken>,
    at: Option<Instant>,
    /// How many times `sync` actually registered a new timer: used by `armed_count` (a test-only
    /// getter below) to prove that `sync`'s first guard — a repeated, unchanged deadline — really
    /// skips re-arming, not just that firing still happens once regardless, which it would even
    /// without the guard. A plain field here, gated to tests nowhere: the attribute that would gate
    /// it is the very marker the guardrail tests look for to tell a file's production code from its
    /// test module, and inside the struct it would hide the rest of the file from them
    /// (`production_code`, tests/guardrails.rs).
    #[allow(dead_code)]
    armed: u32,
}

impl DeadlineTimer {
    pub fn new() -> DeadlineTimer {
        DeadlineTimer::default()
    }

    /// Arms (or moves) the timer to `deadline`; removes it when `deadline` is None. `fire` runs with
    /// the loop's state and the firing instant; the callback then drops its own timer (`fired`).
    pub fn sync<D: 'static>(&mut self, handle: &LoopHandle<'static, D>, deadline: Option<Instant>, fire: fn(&mut D, Instant)) {
        if deadline == self.at && self.token.is_some() == deadline.is_some() {
            return;
        }
        if let Some(token) = self.token.take() {
            handle.remove(token);
        }
        self.at = deadline;
        if let Some(at) = deadline {
            let timer = Timer::from_deadline(at);
            self.token = handle
                .insert_source(timer, move |now, _, state| {
                    fire(state, now);
                    TimeoutAction::Drop
                })
                .ok();
            #[cfg(test)]
            {
                self.armed += 1;
            }
        }
    }

    /// `sync` when the loop handle exists (before the loop is built there is nothing to arm).
    pub fn sync_opt<D: 'static>(&mut self, handle: Option<&LoopHandle<'static, D>>, deadline: Option<Instant>, fire: fn(&mut D, Instant)) {
        if let Some(handle) = handle {
            self.sync(handle, deadline, fire);
        }
    }

    #[cfg(test)]
    fn armed_count(&self) -> u32 {
        self.armed
    }

    /// Called from `fire`: calloop dropped the source. `fire` MUST call it
    /// before any `sync` (the tray's and the hover's callbacks both do): otherwise `sync` would remove a
    /// token calloop already dropped, or keep, for the same deadline, a timer
    /// that no longer exists.
    pub fn fired(&mut self) {
        self.token = None;
        self.at = None;
    }

    /// The deadline armed, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.at.filter(|_| self.token.is_some())
    }

    pub fn is_armed(&self) -> bool {
        self.token.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::EventLoop;
    use std::time::Duration;

    fn count(n: &mut u32, _: Instant) {
        *n += 1;
    }

    /// Runs the loop for `d`, dispatching whatever fires.
    fn run(l: &mut EventLoop<'static, u32>, n: &mut u32, d: Duration) {
        let end = Instant::now() + d;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            l.dispatch(Some(left), n).unwrap();
        }
    }

    #[test]
    fn the_deadline_timer_exists_only_while_calls_are_pending() {
        let mut l: EventLoop<'static, u32> = EventLoop::try_new().unwrap();
        let mut fired = 0;
        let mut t = DeadlineTimer::new();
        t.sync(&l.handle(), None, count);
        assert!(!t.is_armed(), "nothing pending: no timer at all");
        t.sync(&l.handle(), Some(Instant::now() + Duration::from_millis(50)), count);
        assert!(t.is_armed());
        t.sync(&l.handle(), None, count);
        assert!(!t.is_armed(), "the answer came: the timer is removed");
        run(&mut l, &mut fired, Duration::from_millis(400));
        assert_eq!(fired, 0, "a removed timer never wakes the loop");
        t.sync(&l.handle(), Some(Instant::now() + Duration::from_millis(50)), count);
        run(&mut l, &mut fired, Duration::from_millis(400));
        assert_eq!(fired, 1);
        t.fired();
        assert!(!t.is_armed());
        run(&mut l, &mut fired, Duration::from_millis(400));
        assert_eq!(fired, 1, "fired once, then gone");
    }

    #[test]
    fn a_moved_deadline_rearms_once() {
        let mut l: EventLoop<'static, u32> = EventLoop::try_new().unwrap();
        let mut fired = 0;
        let mut t = DeadlineTimer::new();
        let start = Instant::now();
        let late = start + Duration::from_millis(400);
        t.sync(&l.handle(), Some(start + Duration::from_millis(50)), count);
        t.sync(&l.handle(), Some(late), count);
        t.sync(&l.handle(), Some(late), count);
        assert_eq!(t.armed_count(), 2, "the repeated deadline must not re-arm a new timer");
        run(&mut l, &mut fired, Duration::from_millis(200));
        assert_eq!(fired, 0, "the first deadline moved away: it never fires");
        run(&mut l, &mut fired, Duration::from_millis(400));
        assert_eq!(fired, 1, "the same deadline twice is one timer, firing once");
    }
}
