//! What the loop asks the D-Bus thread, for the quick panel's Wi-Fi and Bluetooth readings: the
//! scope the open panel shows, and a socket pair whose first end the thread polls beside the bus's
//! own socket; the loop writes one byte to the second when the scope changes, never otherwise (no
//! wake-up at rest). Without the pair (`default()`, the tests' tap) the scope is still kept, and
//! nothing wakes: a session reads it when it starts and on each `Signal::poke`. The tap also counts
//! the panel's opens (Closed to any other scope): a close and a reopen the thread reads as one poke
//! are still a new open, and a state read for an earlier open is told apart from the current one's.

use crate::system::PanelScope;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// What the D-Bus thread polls beside the bus's socket: the end it waits on, and the word to
/// drain once it is read.
pub trait Wake: Send + Sync {
    fn wake_fd(&self) -> Option<RawFd>;
    fn drain(&self);
}

#[derive(Debug, Default)]
pub struct Tap {
    /// The scope asked (the default: closed), and the opens so far.
    scope: Mutex<(PanelScope, u64)>,
    wake: Option<(UnixStream, UnixStream)>,
}

impl Tap {
    /// A tap with its socket pair, both ends non-blocking and close-on-exec.
    pub fn new() -> io::Result<Tap> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(Tap { scope: Mutex::new((PanelScope::Closed, 0)), wake: Some((read, write)) })
    }

    pub fn scope(&self) -> PanelScope {
        self.snapshot().0
    }

    /// The scope asked and the opens so far, read together.
    pub fn snapshot(&self) -> (PanelScope, u64) {
        *self.scope.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The loop asks for `scope`: kept, and the thread woken when it changed (a byte already waiting
    /// is enough: a full socket is not an error); from closed to another scope, one more open. True
    /// when it changed.
    pub fn set(&self, scope: PanelScope) -> bool {
        let changed = {
            let mut s = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = s.0 != scope;
            if s.0 == PanelScope::Closed && scope != PanelScope::Closed {
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
        drain(&self.wake);
    }
}

fn drain(wake: &Option<(UnixStream, UnixStream)>) {
    if let Some((r, _)) = wake {
        let mut buf = [0u8; 64];
        while matches!((&*r).read(&mut buf), Ok(n) if n > 0) {}
    }
}

impl Wake for Tap {
    fn wake_fd(&self) -> Option<RawFd> {
        Tap::wake_fd(self)
    }

    fn drain(&self) {
        Tap::drain(self);
    }
}

/// What the loop asks the D-Bus thread about the failed-units count (diagnostics spec § 6): one
/// word, "read it again now" (`failed-refresh`). Unlike a panel tap it has no scope: each poke is
/// counted, so one that arrives while the thread is busy is still read once, and a count read
/// twice for one poke never happens. The same socket pair, polled beside the bus: no thread, no
/// timer, nothing at rest.
#[derive(Debug, Default)]
pub struct UnitsTap {
    pokes: AtomicU64,
    wake: Option<(UnixStream, UnixStream)>,
}

impl UnitsTap {
    /// A tap with its socket pair, both ends non-blocking and close-on-exec.
    pub fn new() -> io::Result<UnitsTap> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        write.set_nonblocking(true)?;
        Ok(UnitsTap { pokes: AtomicU64::new(0), wake: Some((read, write)) })
    }

    /// Asks for a read now: counted, and the thread woken (a byte already waiting is enough).
    pub fn poke(&self) {
        self.pokes.fetch_add(1, Ordering::SeqCst);
        if let Some((_, w)) = &self.wake {
            let _ = (&*w).write(&[1]);
        }
    }

    /// The pokes so far.
    pub fn pokes(&self) -> u64 {
        self.pokes.load(Ordering::SeqCst)
    }

    /// Whether a poke came since `seen`, which then catches up with it.
    pub fn poked_since(&self, seen: &mut u64) -> bool {
        let now = self.pokes();
        std::mem::replace(seen, now) != now
    }
}

impl Wake for UnitsTap {
    fn wake_fd(&self) -> Option<RawFd> {
        self.wake.as_ref().map(|(r, _)| r.as_raw_fd())
    }

    fn drain(&self) {
        drain(&self.wake);
    }
}

/// Names only: `WifiTap`, `BtTap` and `VpnTap` are one type, the compiler does not tell them apart.
pub type WifiTap = Tap;
pub type BtTap = Tap;
pub type VpnTap = Tap;
