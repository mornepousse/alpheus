//! Wayland objects whose last reference must not be dropped inside calloop's dispatch.
//!
//! SCTK's keyboard bound with repeat on the loop keeps its repeat timer's calloop token in the
//! `wl_keyboard`'s user data, and that data's Drop removes the token from the loop. The token
//! stays there after the timer ran out (SCTK only replaces it at the next repeating key). A
//! released `wl_keyboard` lives on in wayland-backend's object map until the compositor's
//! `delete_id`, which the Wayland source reads in calloop's `before_handle_events`, while calloop
//! borrows its sources: if the map held the last reference then, the Drop re-borrows them and
//! panics ("RefCell already borrowed"), which then poisons wayland-backend and aborts the bar
//! (Mae's crash, 2026-10-06 and 2026-10-09, after a launcher or popup close).
//!
//! So a released keyboard is never just dropped: `Retired` holds a clone, and once no one else
//! does (the map let go, the repeat timer's closure is gone), drops it after `dispatch` returns.
//!
//! One case SCTK 0.20 gives no handle on: a keyboard lost (seat capability or seat removed) while
//! a key repeats on a live surface, with no `leave` first. Its repeat stops only at that key's
//! release, a `leave`, or the surface dying, and a released `wl_keyboard` gets no more events: the
//! timer then repeats (`App::key_event`, sent nowhere once nothing is open) until the focused
//! surface goes, and its closure keeps this keyboard retired until then. SCTK exposes neither the
//! timer's token nor a way to clear the repeat.

use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard;
use std::sync::Arc;

/// Whether someone besides this handle still holds the object.
pub trait Shared {
    fn shared(&self) -> bool;
}

/// Its user data's other holders: wayland-backend's object map (until `delete_id`), the repeat
/// timer's closure, a queued event.
impl Shared for wl_keyboard::WlKeyboard {
    fn shared(&self) -> bool {
        self.object_data().is_some_and(|d| Arc::strong_count(d) > 1)
    }
}

/// The released objects still held elsewhere. Empty at rest: a `delete_id` follows every release
/// promptly, and a repeat timer's closure goes at its next firing once its key is up or its surface
/// dead, each waking the loop by itself (nothing is armed here). The exception: a keyboard lost
/// with a key repeating (module doc).
#[derive(Debug)]
pub struct Retired<T: Shared> {
    held: Vec<T>,
}

impl<T: Shared> Default for Retired<T> {
    fn default() -> Self {
        Retired { held: Vec::new() }
    }
}

impl<T: Shared> Retired<T> {
    /// Keeps `t` until `drain` finds it held by no one else.
    pub fn hold(&mut self, t: T) {
        self.held.push(t);
    }

    /// Called once after `dispatch` returns (never inside it): drops what no one else holds any
    /// more, which makes this drop the last one, outside calloop's borrow.
    pub fn drain(&mut self) {
        self.held.retain(Shared::shared);
    }

    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::calloop::ping::{PingSource, make_ping};
    use smithay_client_toolkit::reexports::calloop::{
        EventLoop, EventSource, LoopHandle, Poll, PostAction, Readiness, RegistrationToken, Token, TokenFactory,
    };
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::rc::Rc;
    use std::time::Duration;

    /// SCTK's `RepeatData`: its Drop removes its timer's token from the loop.
    struct Repeat {
        handle: LoopHandle<'static, ()>,
        token: Option<RegistrationToken>,
        dropped: Rc<Cell<bool>>,
    }

    impl Drop for Repeat {
        fn drop(&mut self) {
            if let Some(token) = self.token.take() {
                self.handle.remove(token);
            }
            self.dropped.set(true);
        }
    }

    impl Shared for Rc<Repeat> {
        fn shared(&self) -> bool {
            Rc::strong_count(self) > 1
        }
    }

    /// The Wayland source: in `before_handle_events` (where it reads `delete_id`), it drops its
    /// reference, as wayland-backend's object map does.
    struct Reader {
        ping: PingSource,
        map: Option<Rc<Repeat>>,
    }

    impl EventSource for Reader {
        type Event = ();
        type Metadata = ();
        type Ret = ();
        type Error = std::io::Error;
        const NEEDS_EXTRA_LIFECYCLE_EVENTS: bool = true;

        fn process_events<F>(&mut self, readiness: Readiness, token: Token, _: F) -> Result<PostAction, Self::Error>
        where
            F: FnMut((), &mut ()),
        {
            self.ping.process_events(readiness, token, |_, _| {}).map_err(std::io::Error::other)
        }

        fn register(&mut self, poll: &mut Poll, f: &mut TokenFactory) -> smithay_client_toolkit::reexports::calloop::Result<()> {
            self.ping.register(poll, f)
        }

        fn reregister(&mut self, poll: &mut Poll, f: &mut TokenFactory) -> smithay_client_toolkit::reexports::calloop::Result<()> {
            self.ping.reregister(poll, f)
        }

        fn unregister(&mut self, poll: &mut Poll) -> smithay_client_toolkit::reexports::calloop::Result<()> {
            self.ping.unregister(poll)
        }

        fn before_handle_events(&mut self, _: smithay_client_toolkit::reexports::calloop::EventIterator<'_>) {
            self.map = None;
        }
    }

    /// A loop with a Reader holding `repeat`'s other reference, and that repeat, whose token names a
    /// timer source that already ran out (as SCTK's does once a key's repeat delay has passed).
    fn setup() -> (EventLoop<'static, ()>, Rc<Repeat>, Rc<Cell<bool>>) {
        let event_loop: EventLoop<'static, ()> = EventLoop::try_new().unwrap();
        let handle = event_loop.handle();
        let (_ping, ping_source) = make_ping().unwrap();
        let token = handle.insert_source(make_ping().unwrap().1, |_, _, _| {}).unwrap();
        handle.remove(token);
        let dropped = Rc::new(Cell::new(false));
        let repeat = Rc::new(Repeat { handle: handle.clone(), token: Some(token), dropped: dropped.clone() });
        handle.insert_source(Reader { ping: ping_source, map: Some(repeat.clone()) }, |_, _, _| {}).unwrap();
        (event_loop, repeat, dropped)
    }

    /// The hazard itself, as calloop 0.14 has it: the last reference dropped inside
    /// `before_handle_events` makes the Drop re-borrow the loop's sources and panic.
    #[test]
    fn a_last_reference_dropped_inside_before_handle_events_panics() {
        let (mut event_loop, repeat, dropped) = setup();
        drop(repeat); // the map's is now the last one
        let r = catch_unwind(AssertUnwindSafe(|| event_loop.dispatch(Some(Duration::ZERO), &mut ())));
        assert!(r.is_err(), "calloop no longer panics there: this module may no longer be needed");
        assert!(!dropped.get(), "the Drop panicked before its end");
    }

    /// The fix: retired, the object outlives the map's reference, then goes after the dispatch.
    #[test]
    fn a_retired_object_goes_after_the_dispatch_once_no_one_else_holds_it() {
        let (mut event_loop, repeat, dropped) = setup();
        let mut retired = Retired::default();
        retired.hold(repeat);
        retired.drain();
        assert_eq!(retired.len(), 1, "still in the map: kept");
        assert!(!dropped.get());
        event_loop.dispatch(Some(Duration::ZERO), &mut ()).unwrap();
        assert!(!dropped.get(), "the map's reference went inside the dispatch, not the last one");
        retired.drain();
        assert!(retired.is_empty(), "no one else holds it: dropped");
        assert!(dropped.get(), "the last reference went after the dispatch");
        // And the loop still runs.
        event_loop.dispatch(Some(Duration::ZERO), &mut ()).unwrap();
    }
}
