//! The screen recording: the recorder alpheus starts as its own child, stops with SIGINT, and judges
//! by the file it leaves.
//!
//! `Recording::start` picks a free name in the videos folder (`paths::videos_dir`, never over a file
//! that is there), fills `[captures] record`'s `{geometry}` and `{out}` (whole arguments) and starts
//! the recorder as a watched child (`Executor::spawn_watched`): the loop hears its end on its pidfd,
//! nothing waits for it. `stop` sends SIGINT to the recorder's own process group and nothing else;
//! the recorder finishes its file and exits, and `finish` turns that exit into the notice's ending:
//! saved when it exited 0 and the file holds something, failed otherwise. At the daemon's shutdown,
//! `end_at_shutdown` does the same and waits at most `STOP_LIMIT` for it; a recorder still running
//! after that is let go, never killed (a kill would leave its file unplayable), and its ending is
//! not known. One recording at a time, and nothing survives a restart: the daemon keeps
//! no state of it.
//!
//! Never:
//! - signal anything but the recorder it started (its own process group, by the pid it is still the
//!   parent of: `Watched::interrupt`), nor a process found by name;
//! - write over a file of the user: a name that is taken gets a number;
//! - log or panic: the glue logs one fixed word per ending, never the file's name or its path.
//!
//! See: docs/features/captures.md, docs/decisions.md ("Recording is the external tool's, driven and
//! stopped by alpheus").

use super::Finish;
use super::paths::numbered;
use super::tools::{GEOMETRY, OUT, fill};
use alpheus_common::exec::{Executor, Watched};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// How long the daemon's shutdown waits for the recorder to finish its file after SIGINT. See
/// docs/decisions.md, "Recording is the external tool's, driven and stopped by alpheus".
pub const STOP_LIMIT: Duration = Duration::from_secs(2);

/// Whether a recording runs. The signal guard reads it (`startup.rs`): a daemon asked to stop while
/// it records is given `STOP_LIMIT` more before its exit is forced, so the recorder can finish.
pub static LIVE: AtomicBool = AtomicBool::new(false);

/// The most numbered tries for a taken name: past it the recording does not start.
const MAX_TRIES: u32 = 99;

/// A name in `dir` (made when missing) that no file holds: `name`, else `name` numbered. None when
/// the folder cannot be made or every try is taken. The recorder creates the file; a file another
/// program makes between this look and its start is the recorder's to refuse.
pub fn free_path(dir: &Path, name: &str) -> Option<(PathBuf, String)> {
    std::fs::create_dir_all(dir).ok()?;
    for n in std::iter::once(0).chain(2..=MAX_TRIES) {
        let candidate = numbered(name, n);
        let path = dir.join(&candidate);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some((path, candidate)),
            // Taken, or unreadable: never this one.
            _ => continue,
        }
    }
    None
}

/// The recorder running, and the file it writes.
pub struct Recording {
    child: Box<dyn Watched>,
    path: PathBuf,
    name: String,
}

impl Recording {
    /// Starts `template` (`[captures] record`) recording `geometry` (`x,y wxh`) into a free name
    /// like `name` in `dir`. None when no name is free or the recorder cannot start.
    pub fn start(exec: &dyn Executor, template: &[String], geometry: &str, dir: &Path, name: &str) -> Option<Recording> {
        let (path, name) = free_path(dir, name)?;
        let argv = fill(template, &[(GEOMETRY, geometry), (OUT, &path.to_string_lossy())]);
        let (program, args) = argv.split_first()?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let child = exec.spawn_watched(program, &args, None).ok()?;
        Some(Recording { child, path, name })
    }

    /// The descriptor that turns readable when the recorder ends, for the loop to watch.
    pub fn exit_fd(&self) -> Option<std::os::fd::OwnedFd> {
        self.child.exit_fd()
    }

    /// SIGINT to the recorder (its own process group, nothing else): it finishes its file and exits.
    /// False when it has already ended.
    pub fn stop(&mut self) -> bool {
        self.child.interrupt()
    }

    /// The recorder's end, once it has ended (None while it records).
    pub fn try_exit(&mut self) -> Option<Option<i32>> {
        self.child.try_exit()
    }

    /// The ending of a recorder that exited with `exit` (None: ended by a signal): saved when it
    /// exited 0 and its file holds something; otherwise failed, and an empty file it left is removed
    /// (a file with something in it stays: it is the user's recording, however it ended).
    pub fn finish(self, exit: Option<i32>) -> Finish {
        let size = match std::fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.is_file() => Some(meta.len()),
            _ => None,
        };
        if size == Some(0) {
            let _ = std::fs::remove_file(&self.path);
        }
        match (exit, size) {
            (Some(0), Some(len)) if len > 0 => Finish::Recorded(self.name),
            _ => Finish::RecordFailed,
        }
    }

    /// The daemon stops: SIGINT, then at most `limit` for the recorder to finish its file, then the
    /// ending. None when the recorder still runs past the limit: it was interrupted and may still be
    /// writing the end of its file, so it is let go (`Watched::release`), never killed, which would
    /// leave the file unplayable; the file is left as it is.
    pub fn end_at_shutdown(mut self, limit: Duration) -> Option<Finish> {
        self.child.interrupt();
        match self.child.wait_exit(limit) {
            Some(exit) => Some(self.finish(exit)),
            None => {
                self.child.release();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests;
