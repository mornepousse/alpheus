//! Whether the session is locked: a screen locker of the user's own is running.
//!
//! No locker tells a bar when it ends. The logind session has a `LockedHint` property, but hyprlock
//! and hypridle 0.1.7 (the owner's setup; their binaries were read, no other locker was) never
//! mention it, and the session's `Lock` signal comes without an `Unlock` when hyprlock is left by
//! its password (it does not call `loginctl unlock-session`). A state kept from those would stay
//! "locked" for ever. A locker is a process for as long as it holds the lock, so this looks for one
//! by name, and only when the shutdown has a step to take or its warning's minute runs: nothing
//! watches, nothing wakes at rest.
//!
//! Never:
//! - run at rest: the caller asks when a step is due, never on a timer of its own;
//! - log, or keep what it reads: a process's name is the user's business;
//! - take unknown for locked: a `/proc` that cannot be read, or a locker this list does not know,
//!   reads as not locked, which is the behaviour the shutdown had before it looked at all.
//!
//! See: docs/features/timer.md, and
//! docs/decisions.md ("A scheduled shutdown always warns a minute before, even when its time has
//! passed").

use crate::launcher::processes::owner;
use std::fs;
use std::path::Path;

/// The screen lockers known by name: `comm`, the first 15 characters of the program's file name.
/// A name followed by a dash (`swaylock-effects`, a Nix wrapper's `.hyprlock-wrapped`) is the same
/// program.
const LOCKERS: [&str; 6] = ["hyprlock", "swaylock", "gtklock", "waylock", "i3lock", "xsecurelock"];

/// The most process folders one look reads: far above any real machine, so a hostile `/proc` is
/// read to a bound.
const SCAN_MAX: usize = 100_000;

/// Whether `comm` (a process's name as `/proc/<pid>/comm` gives it) is a known screen locker.
pub(crate) fn is_locker(comm: &str) -> bool {
    let name = comm.trim().trim_start_matches('.');
    LOCKERS.iter().any(|l| name.strip_prefix(l).is_some_and(|rest| rest.is_empty() || rest.starts_with('-')))
}

/// Whether the session is locked now: a locker of the user's own runs under `root` (`[timer]
/// proc_dir`, `/proc` unless a test points it elsewhere).
pub(crate) fn session_locked(root: &Path) -> bool {
    // SAFETY: getuid(2) takes nothing and cannot fail.
    let uid = unsafe { libc::getuid() };
    locker_running(root, uid)
}

/// Whether a process of user `uid` under `root` (`/proc`) is a screen locker. False when `root`
/// cannot be read.
pub(crate) fn locker_running(root: &Path, uid: u32) -> bool {
    let Ok(entries) = fs::read_dir(root) else { return false };
    entries.flatten().take(SCAN_MAX).any(|entry| {
        // Only the numeric folders are processes (`/proc/sys`, `/proc/net`... are not).
        if entry.file_name().to_str().is_none_or(|n| n.parse::<u32>().is_err()) {
            return false;
        }
        // The name first: it is one small read, and almost nothing matches it. The owner is read
        // only for a match.
        let dir = entry.path();
        let named = fs::read_to_string(dir.join("comm")).is_ok_and(|comm| is_locker(&comm));
        // A zombie has let go of the lock: its parent has not read its end yet.
        named && fs::read_to_string(dir.join("status")).is_ok_and(|s| owner(&s) == Some(uid) && !zombie(&s))
    })
}

/// Whether a `status` file says the process is a zombie (a `State:` line starting with `Z`).
fn zombie(status: &str) -> bool {
    status.lines().find_map(|l| l.strip_prefix("State:")).is_some_and(|v| v.trim_start().starts_with('Z'))
}

#[cfg(test)]
mod tests;
