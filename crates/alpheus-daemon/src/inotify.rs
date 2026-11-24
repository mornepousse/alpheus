//! The one inotify wrapper: a watch instance, its descriptor, and the records read from it.
//!
//! Two features watch files, and each owns one instance of this: the theme's watch (`watch.rs`,
//! which carries the Shield's flag too) and the camera's (`privacy/camera.rs`). The descriptor is
//! non-blocking and close-on-exec, it sits on the event loop, and `drain` reads it until the queue
//! is empty, so the loop is not woken again for records already read. A truncated tail is dropped
//! rather than read past.
//!
//! Never:
//! - write to the file system: this only watches (`the_theme_and_camera_watches_only_read` checks
//!   this file, `watch.rs` and `privacy/camera.rs`, and that no other file of the daemon names
//!   inotify);
//! - panic on a malformed record: every field is read through a checked slice.
//!
//! See: docs/features/themes.md (the hot reload), docs/features/privacy.md (the camera watch),
//! docs/decisions.md ("The camera watch is a second inotify instance", "The Shield flag is watched
//! in the theme watch's inotify instance").

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// `struct inotify_event`'s fixed part: wd, mask, cookie, len, 4 bytes each.
pub(crate) const HEADER: usize = 16;

/// One inotify record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Event<'a> {
    pub wd: i32,
    pub mask: u32,
    /// The entry's name without its NUL padding; empty for an event on the watched directory itself.
    pub name: &'a [u8],
}

/// The records of a read(2) buffer, in order; a truncated tail is dropped (never a panic).
pub(crate) struct Events<'a>(pub(crate) &'a [u8]);

impl<'a> Iterator for Events<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        let buf = self.0;
        let word = |i: usize| buf.get(i..i + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_ne_bytes);
        let (wd, mask, len) = (word(0)? as i32, word(4)?, word(12)? as usize);
        let raw = buf.get(HEADER..HEADER + len)?;
        self.0 = buf.get(HEADER + len..).unwrap_or_default();
        let name = raw.split(|b| *b == 0).next().unwrap_or_default();
        Some(Event { wd, mask, name })
    }
}

/// An inotify instance.
pub(crate) struct Inotify {
    fd: OwnedFd,
}

impl Inotify {
    /// A new instance. Err: one line for the log.
    pub(crate) fn new() -> Result<Inotify, String> {
        // SAFETY: inotify_init1 only takes flags; it returns a new descriptor or -1.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(format!("inotify unavailable ({})", io::Error::last_os_error()));
        }
        // SAFETY: `raw` was just returned by inotify_init1 and belongs to nothing else.
        Ok(Inotify { fd: unsafe { OwnedFd::from_raw_fd(raw) } })
    }

    /// A duplicate of the inotify fd for the event loop (the same instance: reading either drains both).
    pub(crate) fn watch_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// Watches `path` with `mask`; the watch descriptor.
    pub(crate) fn add(&self, path: &Path, mask: u32) -> io::Result<i32> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: a NUL-terminated path that lives through the call, and this instance's own fd.
        let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), c.as_ptr(), mask) };
        if wd < 0 { Err(io::Error::last_os_error()) } else { Ok(wd) }
    }

    /// Drops this instance's watch `wd`; EINVAL (the kernel already dropped it, IN_IGNORED) is harmless.
    pub(crate) fn rm(&self, wd: i32) {
        // SAFETY: removes a watch of this instance by number; nothing is read or written through it.
        unsafe { libc::inotify_rm_watch(self.fd.as_raw_fd(), wd) };
    }

    /// Reads every queued chunk of records (until EAGAIN) and hands each to `f` (see `Events`).
    pub(crate) fn drain(&self, mut f: impl FnMut(&[u8])) {
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: read(2) writes at most `buf.len()` bytes into a buffer this function owns.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            // -1: EAGAIN (drained) or an error; 0 never comes from inotify. Either way, done.
            let Ok(n) = usize::try_from(n) else { break };
            if n == 0 {
                break;
            }
            f(buf.get(..n).unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_common::temp_path::TempPath;

    fn names(i: &Inotify) -> Vec<(i32, u32, Vec<u8>)> {
        let mut seen = Vec::new();
        i.drain(|chunk| seen.extend(Events(chunk).map(|e| (e.wd, e.mask, e.name.to_vec()))));
        seen
    }

    #[test]
    fn a_watch_reports_a_creation_and_drain_stops_when_the_queue_is_empty() {
        let dir = TempPath::dir("inotify-wrapper");
        let i = Inotify::new().unwrap();
        let wd = i.add(dir.as_ref(), libc::IN_CREATE).unwrap();
        assert!(names(&i).is_empty(), "nothing yet: drain returns, it does not block");
        std::fs::write(dir.as_ref().join("a"), b"").unwrap();
        assert_eq!(names(&i), vec![(wd, libc::IN_CREATE, b"a".to_vec())]);
        assert!(names(&i).is_empty(), "read once, gone");
    }

    #[test]
    fn a_removed_watch_reports_nothing_more_and_a_missing_path_is_an_error() {
        let dir = TempPath::dir("inotify-wrapper-rm");
        let i = Inotify::new().unwrap();
        let wd = i.add(dir.as_ref(), libc::IN_CREATE).unwrap();
        i.rm(wd);
        names(&i);
        std::fs::write(dir.as_ref().join("b"), b"").unwrap();
        assert!(names(&i).is_empty(), "the watch is gone");
        let missing = i.add(&dir.as_ref().join("nowhere"), libc::IN_CREATE).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    }
}
