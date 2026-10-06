//! One calloop `Timer` at a deadline while there is one, none otherwise:
//! the tray's 2 s call timeouts (and, later, the tooltip's hover delay) cost
//! nothing at rest — no source, no wake-up — instead of a periodic tick.

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
    /// getter below) to prove the no-op guard above — a repeated, unchanged deadline — really skips
    /// re-arming, not just that firing still happens once regardless, which it would even without
    /// the guard. A plain field here, not test-gated: gating a struct field that way reads oddly to
    /// the simple brace-counting some text-based tests use on whole items.
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

    #[cfg(test)]
    fn armed_count(&self) -> u32 {
        self.armed
    }

    /// Called from `fire`: calloop dropped the source. `fire` MUST call it
    /// before any `sync` (tasks 11 and 16): otherwise `sync` would remove a
    /// token calloop already dropped, or keep, for the same deadline, a timer
    /// that no longer exists.
    pub fn fired(&mut self) {
        self.token = None;
        self.at = None;
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
