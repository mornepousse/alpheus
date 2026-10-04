//! Only one daemon at a time: an exclusive `flock` on
//! `$XDG_RUNTIME_DIR/alpheus-demon.lock`, held as long as the daemon lives
//! and released by the kernel when it ends, even abruptly (no orphan file
//! left behind to block the next one).

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
        let _v2 = acquire(&p).expect("lock released at the end of the daemon");
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
