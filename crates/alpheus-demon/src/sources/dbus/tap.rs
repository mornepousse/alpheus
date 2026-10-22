//! What the loop asks the D-Bus thread, for the quick panel's Wi-Fi and Bluetooth readings: the
//! scope the open panel shows, and a socket pair whose first end the thread polls beside the bus's
//! own socket; the loop writes one byte to the second when the scope changes, never otherwise (no
//! wake-up at rest). Without the pair (`default()`, the tests' tap) the scope is still kept, and
//! nothing wakes: a session reads it when it starts and on each `Signal::poke`. The tap also counts
//! the panel's opens (Closed to any other scope): a close and a reopen the thread reads as one poke
//! are still a new open, and a state read for an earlier open is told apart from the current one's.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, PoisonError};

/// A scope whose default is the closed panel.
pub trait Scope: Copy + PartialEq + Default {}

impl Scope for crate::system::WifiScope {}
impl Scope for crate::system::BtScope {}

#[derive(Debug, Default)]
pub struct Tap<S: Scope> {
    /// The scope asked (the default: closed), and the opens so far.
    scope: Mutex<(S, u64)>,
    wake: Option<(UnixStream, UnixStream)>,
}

impl<S: Scope> Tap<S> {
    /// A tap with its socket pair, both ends non-blocking and close-on-exec.
    pub fn new() -> io::Result<Tap<S>> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(Tap { scope: Mutex::new((S::default(), 0)), wake: Some((read, write)) })
    }

    pub fn scope(&self) -> S {
        self.snapshot().0
    }

    /// The scope asked and the opens so far, read together.
    pub fn snapshot(&self) -> (S, u64) {
        *self.scope.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The loop asks for `scope`: kept, and the thread woken when it changed (a byte already waiting
    /// is enough: a full socket is not an error); from closed to another scope, one more open. True
    /// when it changed.
    pub fn set(&self, scope: S) -> bool {
        let changed = {
            let mut s = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = s.0 != scope;
            if s.0 == S::default() && scope != S::default() {
                s.1 = s.1.wrapping_add(1);
            }
            s.0 = scope;
            changed
        };
        if changed && let Some((_, w)) = &self.wake {
            let _ = (&*w).write(&[1]);
        }
        changed
    }

    /// The end the D-Bus thread polls; None for the tests' tap.
    pub fn wake_fd(&self) -> Option<RawFd> {
        self.wake.as_ref().map(|(r, _)| r.as_raw_fd())
    }

    /// Reads every byte waiting, so the poll sleeps again.
    pub fn drain(&self) {
        if let Some((r, _)) = &self.wake {
            let mut buf = [0u8; 64];
            while matches!((&*r).read(&mut buf), Ok(n) if n > 0) {}
        }
    }
}

pub type WifiTap = Tap<crate::system::WifiScope>;
pub type BtTap = Tap<crate::system::BtScope>;
