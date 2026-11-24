//! The camera devices' wiring on the bar: the watch on the event loop, the debounce and the scan.
//!
//! This is the glue half of the camera watch; the file-system half is `camera.rs`. `start` puts
//! one inotify fd on the event loop, level-triggered and drained to the end (EAGAIN), so it never
//! wakes the loop at rest. An event is only a trigger: it moves the 300 ms debounce (`camera.timer`)
//! back, at most 1 s after the first trigger of the burst, and when that timer fires one scan of
//! `/proc` decides what is shown. The scan at start runs once the watches are set, so a camera held
//! across a restart of the daemon lights with no event.
//!
//! Never:
//! - name a process or a pid in a log line;
//! - be started without `privacy` in `modules` (`startup.rs` calls `start` only with it): with no
//!   watch there is no fd, no timer and no scan;
//! - start a thread, a process or a command: the scan runs on the event loop.
//!
//! See: docs/features/privacy.md and
//! docs/decisions.md ("The camera is read from `/proc` after a trigger, never counted", "A burst of
//! camera triggers is scanned at most one second after its first", "The camera scan runs on the
//! event loop", "The camera watch is a second inotify instance").

use crate::deadline::DeadlineTimer;
use crate::log::write as log;
use crate::privacy::camera::{CameraWatch, scan, scan_deadline};
use crate::wayland::App;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode as Trigger, PostAction};
use std::path::Path;
use std::time::Instant;

/// The camera feature's state, one field of `App`.
pub(crate) struct Camera {
    /// The camera devices' inotify watch. None without `privacy`, or when `dev_dir` cannot be watched.
    pub(crate) watch: Option<CameraWatch>,
    /// The (`dev_dir`, `proc_dir`) the camera scan reads; set with the watch.
    pub(crate) roots: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// The camera scan's debounce: armed only after a device event, for `SCAN_DEBOUNCE` (or less,
    /// when the burst's cap comes first).
    pub(crate) timer: DeadlineTimer,
    /// The first trigger of the burst waiting for its scan, from which the debounce's cap counts;
    /// cleared by the scan.
    pub(crate) burst: Option<Instant>,
    /// Whether the line saying a scan bound was reached has been logged (it is logged once per
    /// daemon).
    pub(crate) bound_logged: bool,
}

impl Camera {
    /// Nothing watched, nothing pending. It is written out rather than derived so that the guardrail
    /// that lists every timer the daemon builds still counts this one.
    pub(crate) fn new() -> Camera {
        Camera { watch: None, roots: None, timer: DeadlineTimer::new(), burst: None, bound_logged: false }
    }
}

/// What follows a scan that was given to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterScan {
    /// What is shown changed: the bar, the popup and the off-delay's timer follow.
    Changed,
    /// Nothing shown changed, but a pending off may have appeared, and that arms the off-delay's
    /// timer (the indicators' own, `privacy_timer`): so only that timer is synced.
    SyncOffDelay,
}

pub(crate) fn after_scan(changed: bool) -> AfterScan {
    if changed { AfterScan::Changed } else { AfterScan::SyncOffDelay }
}

fn euid() -> u32 {
    // SAFETY: geteuid(2) cannot fail.
    unsafe { libc::geteuid() }
}

/// Watches the video devices and scans once. Without a watch (inotify unavailable, or `dev_dir`
/// cannot be watched) one line is logged and the PipeWire indicators work alone.
pub(crate) fn start(state: &mut App, handle: &LoopHandle<'static, App>, dev_dir: &Path, proc_dir: &Path) {
    let watch = match CameraWatch::start(dev_dir) {
        Ok(w) => w,
        Err(why) => {
            log(&format!("privacy: camera devices not watched ({why})"));
            return;
        }
    };
    log(&format!("privacy: watching {} video device(s)", watch.watched()));
    let fd = match watch.watch_fd() {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("privacy: camera devices not watched (inotify fd: {word})", word = crate::log::error_word(&e)));
            return;
        }
    };
    // Level-triggered: `drain` reads everything queued, so the fd is never left readable at rest.
    let inserted = handle.insert_source(Generic::new(fd, Interest::READ, Trigger::Level), |_, _, app: &mut App| {
        app.camera_readable();
        Ok(PostAction::Continue)
    });
    if inserted.is_err() {
        log("privacy: camera devices not watched (event loop refused the source)");
        return;
    }
    state.camera.watch = Some(watch);
    state.camera.roots = Some((dev_dir.to_path_buf(), proc_dir.to_path_buf()));
    state.camera_scan(Instant::now());
}

impl App {
    /// The inotify fd is readable: it is drained, and if a record asks for a scan the debounce moves
    /// back (never past the burst's cap).
    pub(crate) fn camera_readable(&mut self) {
        let Some(watch) = self.camera.watch.as_mut() else { return };
        if !watch.drain() {
            return;
        }
        if let Some(handle) = self.loop_handle.clone() {
            let now = Instant::now();
            // `get_or_insert` keeps the burst's first trigger if there is one, and records `now` if
            // this is the first.
            let first = *self.camera.burst.get_or_insert(now);
            self.camera.timer.sync(&handle, Some(scan_deadline(first, now)), App::camera_tick);
        }
    }

    /// The debounce fired (the loop dropped its source): one scan.
    pub(crate) fn camera_tick(app: &mut App, now: Instant) {
        app.camera.timer.fired();
        app.camera.burst = None;
        app.camera_scan(now);
    }

    /// One scan of the processes, given to the model: a change redraws, and a scan that changed
    /// nothing shown still syncs the off-delay's timer, since a pending off arms it. A scan that
    /// found nobody holding a camera, just after an open whose holder has exited, lights nothing.
    fn camera_scan(&mut self, now: Instant) {
        let Some((dev_dir, proc_dir)) = self.camera.roots.clone() else { return };
        let found = scan(&proc_dir, &dev_dir, euid(), std::process::id());
        if found.bounded && !self.camera.bound_logged {
            self.camera.bound_logged = true;
            log("privacy: process scan bound reached; some processes skipped");
        }
        let changed = self.bar.privacy.set_devices(found.camera, now);
        match after_scan(changed) {
            AfterScan::Changed => self.privacy_changed(),
            AfterScan::SyncOffDelay => self.privacy_timer_sync(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scan that changed nothing shown must still sync the off-delay (a pending off arms the indicators'
    /// off-delay timer); one that changed something redraws.
    #[test]
    fn an_unchanged_scan_still_syncs_the_off_delay() {
        assert_eq!(after_scan(true), AfterScan::Changed);
        assert_eq!(after_scan(false), AfterScan::SyncOffDelay);
    }
}
