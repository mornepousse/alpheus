//! Clean shutdown on signal, shared by the TUI and the daemon.
//! - TUI: a handler raises `STOP_REQUESTED`, the loop sees it on the next
//!   turn (`install_signals`).
//! - Daemon: the terminating signals are BLOCKED in every thread and
//!   awaited by the watchdog thread (`block_signals`, `wait_signal`): no
//!   periodic wake-up while the daemon runs.
//!
//! In both cases, `watch` forces the exit if the loop does not finish on
//! its own within the grace period.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// Raised by SIGTERM or SIGHUP (TUI): the loop exits normally.
pub static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Number of the signal received (exit code 128 + n if the exit is forced).
pub static SIGNAL_RECEIVED: AtomicI32 = AtomicI32::new(0);

/// The daemon's terminating signals: SIGTERM (pkill, end of session),
/// SIGHUP, SIGINT.
pub const STOP_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// Blocks the terminating signals in the calling thread and in every
/// thread spawned AFTERWARDS (they inherit the mask): call before any
/// thread exists.
pub fn block_signals() -> io::Result<libc::sigset_t> {
    // SAFETY: sigset_t is a plain C struct, initialised by sigemptyset
    // before use; pthread_sigmask keeps no pointer.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in STOP_SIGNALS {
            libc::sigaddset(&mut set, s);
        }
        let r = libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        if r != 0 {
            return Err(io::Error::from_raw_os_error(r));
        }
        Ok(set)
    }
}

/// Waits, without spinning, for one of the blocked signals; returns its number.
pub fn wait_signal(set: &libc::sigset_t) -> libc::c_int {
    let mut sig: libc::c_int = 0;
    loop {
        // SAFETY: `set` and `sig` live for the duration of the call.
        if unsafe { libc::sigwait(set, &mut sig) } == 0 {
            return sig;
        }
    }
}

/// Signal handler: two atomic stores, async-signal-safe.
extern "C" fn on_signal(sig: libc::c_int) {
    SIGNAL_RECEIVED.store(sig, Ordering::SeqCst);
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

/// Installs the handler for SIGTERM and SIGHUP. SA_RESTART: an interrupted
/// system call (reading the terminal, waiting on a tool) resumes instead of
/// failing; the loop sees the flag on the next turn (≤ 100 ms).
pub fn install_signals() -> io::Result<()> {
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: sigaction is a plain C struct, valid when zero-filled.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART;
        // SAFETY: `sa` is valid for the duration of the call; the handler
        // only does an atomic store (async-signal-safe).
        let r = unsafe {
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut())
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Watchdog thread: waits for the stop flag, gives the loop `grace` to exit
/// on its own (and raise `done`), otherwise calls `force`. Returns `true` if
/// the exit was forced. Depends on neither the terminal nor crossterm:
/// testable with local flags and short delays.
pub fn watch(stop: &AtomicBool, done: &AtomicBool, grace: Duration, step: Duration, force: impl FnOnce()) -> bool {
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(step);
    }
    let t = Instant::now();
    while t.elapsed() < grace {
        if done.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(step);
    }
    if done.load(Ordering::SeqCst) {
        return false;
    }
    force();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const GRACE: Duration = Duration::from_millis(150);
    const STEP: Duration = Duration::from_millis(5);

    #[test]
    fn watchdog_forces_exit_if_the_loop_is_stuck() {
        let (stop, done) = (AtomicBool::new(true), AtomicBool::new(false));
        let mut forced = false;
        let t = Instant::now();
        assert!(watch(&stop, &done, GRACE, STEP, || forced = true));
        assert!(forced, "stuck loop: exit forced");
        assert!(t.elapsed() >= GRACE, "only after the grace period ({:?})", t.elapsed());
    }

    #[test]
    fn watchdog_lets_the_loop_exit_in_time() {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (s2, d2) = (stop.clone(), done.clone());
        let h = std::thread::spawn(move || {
            let mut forced = false;
            let r = watch(&s2, &d2, GRACE, STEP, || forced = true);
            (r, forced)
        });
        // Longer than the grace period: without a signal, the watchdog counts nothing.
        std::thread::sleep(GRACE + Duration::from_millis(100));
        assert!(!h.is_finished(), "without a signal, the watchdog waits");
        stop.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        done.store(true, Ordering::SeqCst); // the loop exits within the delay
        assert_eq!(h.join().unwrap(), (false, false), "no forced exit");
    }

    /// Current disposition of signal `sig`.
    fn handler(sig: libc::c_int) -> libc::sighandler_t {
        // SAFETY: read-only lookup of the disposition (new action null).
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(sig, std::ptr::null(), &mut old), 0);
            old.sa_sigaction
        }
    }

    #[test]
    fn sigterm_and_sighup_raise_the_flag() {
        install_signals().unwrap();
        let expected = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for sig in [libc::SIGTERM, libc::SIGHUP] {
            // Checked BEFORE raise: without a handler, the signal would kill the tests.
            assert_eq!(handler(sig), expected, "signal {sig} without a handler");
            STOP_REQUESTED.store(false, Ordering::SeqCst);
            // SAFETY: raise(3) targets ourselves, handler installed above.
            assert_eq!(unsafe { libc::raise(sig) }, 0);
            assert!(STOP_REQUESTED.load(Ordering::SeqCst), "signal {sig}: flag not raised");
        }
        STOP_REQUESTED.store(false, Ordering::SeqCst);
        // SAFETY: restores the default disposition for the rest of the tests.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
        }
    }

    #[test]
    fn blocked_signal_awaited_by_the_watchdog_thread() {
        let h = std::thread::spawn(|| {
            let set = block_signals().unwrap();
            // SAFETY: raise(3) targets THIS thread; the signal is blocked
            // there, it stays pending until sigwait (never delivered elsewhere).
            assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
            wait_signal(&set)
        });
        assert_eq!(h.join().unwrap(), libc::SIGTERM);
    }
}
