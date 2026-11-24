//! One daemon at a time: an exclusive `flock` held for as long as the daemon lives.
//!
//! The lock is `$XDG_RUNTIME_DIR/alpheus-demon.lock`, and the file holds the pid, so a second
//! daemon can name it in the line it logs before exiting. The kernel releases the lock when the
//! process ends, however it ends, so nothing stale is ever left to block the next start. A failed
//! `flock` reads as "already running" only when the error is `EWOULDBLOCK`: any other error shows
//! its own message, so a permission or file-system problem is not mistaken for a second daemon.
//!
//! Never:
//! - leave the lock behind: it belongs to the open file, and the kernel drops it;
//! - wait for it: `LOCK_NB` refuses instead, and the daemon exits 1.
//!
//! See: docs/cli.md ("One instance"), docs/ARCHITECTURE.md ("Entry points").

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Message for a failed `flock`: "already running" only if the error is
/// `EWOULDBLOCK` (the lock is held by another daemon, `LOCK_NB` refused to
/// wait); any other error (permissions, filesystem…) shows its real message
/// — otherwise it would be mistaken for a second daemon when the cause is
/// elsewhere.
fn flock_message(err: &io::Error, path: &Path, pid: &str) -> String {
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        format!("alpheus-demon is already running (pid {}); lock {}", pid.trim(), path.display())
    } else {
        format!("lock {}: {err}", path.display())
    }
}

pub fn acquire(path: &Path) -> Result<InstanceLock, String> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    // SAFETY: flock(2) on a valid descriptor that `f` owns.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        let mut pid = String::new();
        let _ = f.read_to_string(&mut pid);
        return Err(flock_message(&err, path, &pid));
    }
    f.set_len(0)
        .and_then(|_| f.write_all(format!("{}\n", std::process::id()).as_bytes()))
        .map_err(|e| format!("lock {}: {e}", path.display()))?;
    Ok(InstanceLock { _file: f })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_daemon_at_a_time() {
        let p = std::env::temp_dir().join(format!("alpheus-lock-{}", std::process::id()));
        let v = acquire(&p).unwrap();
        let e = acquire(&p).unwrap_err();
        assert!(e.contains("already") && e.contains(&std::process::id().to_string()), "{e}");
        drop(v);
        // Other tests of this binary fork children: between fork and exec a
        // child briefly shares the lock's file description (O_CLOEXEC only
        // closes it at exec), so the release can lag by a few milliseconds.
        let mut again = acquire(&p);
        for _ in 0..100 {
            if again.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            again = acquire(&p);
        }
        let _v2 = again.expect("lock released at the end of the daemon");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn missing_directory_clear_message() {
        assert!(acquire(Path::new("/nonexistent/alpheus-demon.lock")).unwrap_err().contains("/nonexistent"));
    }

    #[test]
    fn flock_failure_other_than_already_running_shows_the_real_error() {
        // EWOULDBLOCK (lock already held, LOCK_NB): "already running" message.
        let path = Path::new("/run/alpheus-demon.lock");
        let m = flock_message(&io::Error::from_raw_os_error(libc::EWOULDBLOCK), path, "42");
        assert!(m.contains("already") && m.contains("42"), "{m}");
        // Any other error (simulated here, without really calling flock):
        // the real message, not "already running", which would suggest a
        // second daemon when the cause is elsewhere (permissions, FS…).
        let m = flock_message(&io::Error::from_raw_os_error(libc::EACCES), path, "");
        assert!(!m.contains("already"), "{m}");
        assert!(m.contains(&path.display().to_string()), "{m}");
    }
}
