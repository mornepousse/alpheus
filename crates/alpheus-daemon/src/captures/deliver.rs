//! What the capture's worker thread does with a frame: cut it to its area, encode it, then copy it
//! to the clipboard or save it, and say how it ended.
//!
//! `run` is the whole job and runs on the short-lived `alpheus-capture` thread (`app.rs` starts
//! it); `render` and `deliver` are its two halves, tested apart. The copy goes through the
//! executor like every other tool: the PNG is written to a private file in `$XDG_RUNTIME_DIR/alpheus`
//! (owner only, atomically) and handed to the copy command as its standard input, since a PNG is
//! far past the page a pipe write may carry (`run_with_stdin_file`), whose exit code is what "copied" means; the file is removed as soon
//! as the command is done. A save writes a new file in the pictures folder, never over one.
//!
//! Never:
//! - copy a capture that failed, or save it: the clipboard is only reached with PNG bytes in hand
//!   (docs/decisions.md, "A capture never touches the clipboard unless it succeeded");
//! - write over a file of the user: a taken name gets a number;
//! - keep a frame, a buffer or a file once it has ended;
//! - panic or log: the glue logs one fixed word per ending.
//!
//! See: docs/features/captures.md, docs/decisions.md ("A custom copy command must accept PNG on
//! its standard input").

use super::encode::{EncodeError, encode_png};
use super::paths::numbered;
use super::screencopy::{Frame, cut_size, to_rgba};
use super::{Finish, Goal, Job, Rect};
use crate::state_file::{dir_is_ours, remove_private, write_private};
use alpheus_common::exec::Executor;
use std::fs::OpenOptions;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::time::Duration;

/// Where a capture goes, and under what name.
pub struct Delivery<'a> {
    pub job: Job,
    /// `launcher.toml`'s `copy`: the program and its arguments; empty: no copy command.
    pub copy: &'a [String],
    /// Where `Goal::Save` writes, made when missing; None: no folder could be chosen (no home to
    /// anchor the default), and a save fails rather than land in the working directory.
    pub save_dir: Option<&'a Path>,
    /// The dated name a save takes (`paths::file_name`).
    pub file_name: String,
    /// The private folder of the copy's file (`$XDG_RUNTIME_DIR/alpheus`).
    pub temp_dir: &'a Path,
}

/// The name of the private file the copy command reads: one capture at a time, so one name.
const COPY_FILE: &str = "capture.png";

/// The longest the copy command's foreground may take to read the image and exit.
pub const COPY_LIMIT: Duration = Duration::from_secs(5);

/// The most numbered tries for a save name taken already: past it the save fails.
const MAX_TRIES: u32 = 99;

/// Clears what a daemon killed mid-copy left in `dir` (the private folder): `capture.png` and the
/// writer's `.capture.png.<pid>`, best effort, never anything else, never a folder that is not ours.
pub fn remove_stale(dir: &Path) {
    if dir_is_ours(dir).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == COPY_FILE || name.starts_with(&format!(".{COPY_FILE}.")) {
            let _ = remove_private(dir, &name);
        }
    }
}

/// The frame cut to `area` (buffer pixels; None: all of it) and encoded as a PNG. An empty cut, an
/// area wholly outside the frame, is an error.
pub fn render(frame: &Frame, area: Option<Rect>) -> Result<Vec<u8>, EncodeError> {
    let area = area.unwrap_or(Rect { x: 0, y: 0, width: frame.width, height: frame.height });
    let (width, height) = cut_size(frame, area);
    encode_png(&to_rgba(frame, area), width, height)
}

/// The whole job: render, then deliver. A frame that renders to nothing is `CaptureFailed`, and
/// then nothing is copied or saved.
///
/// A panic anywhere in it (the executor included) is caught: the capture ends as `CaptureFailed`
/// and the private file is removed, so the flow is never left waiting for a worker that is gone.
pub fn run(exec: &dyn Executor, frame: Frame, area: Option<Rect>, d: &Delivery) -> Finish {
    let work = catch_unwind(AssertUnwindSafe(|| {
        let png = render(&frame, area);
        // The frame's pixels are not needed past here: dropped before the copy waits on anything.
        drop(frame);
        match png {
            Ok(png) => deliver(exec, &png, d),
            Err(_) => Finish::CaptureFailed,
        }
    }));
    work.unwrap_or_else(|_| {
        let _ = remove_private(d.temp_dir, COPY_FILE);
        Finish::CaptureFailed
    })
}

/// The copy command for a PNG: the user's, as written, unless it is the bare default `wl-copy`,
/// which is told the type (it would guess from the bytes, but only with the type does every
/// paste target see an image). A custom command must accept PNG on its standard input.
pub fn copy_argv(copy: &[String]) -> Vec<String> {
    let mut argv = copy.to_vec();
    if copy == ["wl-copy"] {
        argv.extend(["--type".to_string(), "image/png".to_string()]);
    }
    argv
}

/// Copies or saves the PNG as `d.job` says, and reports how it ended.
pub fn deliver(exec: &dyn Executor, png: &[u8], d: &Delivery) -> Finish {
    match d.job.goal {
        Goal::Copy => copy(exec, png, d),
        Goal::Save => save(png, d),
    }
}

fn copy(exec: &dyn Executor, png: &[u8], d: &Delivery) -> Finish {
    let argv = copy_argv(d.copy);
    let Some((prog, args)) = argv.split_first() else { return Finish::CopyFailed };
    if write_private(d.temp_dir, COPY_FILE, png).is_err() {
        return Finish::CopyFailed;
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ended = exec.run_with_stdin_file(prog, &args, &d.temp_dir.join(COPY_FILE), COPY_LIMIT);
    // The command has read it, or never started, or was killed: the file goes either way.
    let _ = remove_private(d.temp_dir, COPY_FILE);
    // Copied means the command took the image: its foreground exit is 0 (`wl-copy` forks to hold the
    // clipboard and its foreground ends once it has read the file), as `screenshot.sh` checks it.
    match ended {
        Ok(Some(0)) => Finish::Copied(d.job.scope),
        _ => Finish::CopyFailed,
    }
}

fn save(png: &[u8], d: &Delivery) -> Finish {
    let Some(save_dir) = d.save_dir else { return Finish::SaveFailed };
    if std::fs::create_dir_all(save_dir).is_err() {
        return Finish::SaveFailed;
    }
    // A taken name gets a number: never over a file the user has.
    for n in std::iter::once(0).chain(2..=MAX_TRIES) {
        let name = numbered(&d.file_name, n);
        match OpenOptions::new().write(true).create_new(true).open(save_dir.join(&name)) {
            Ok(mut file) => {
                return match file.write_all(png) {
                    Ok(()) => Finish::Saved(name),
                    Err(_) => {
                        let _ = std::fs::remove_file(save_dir.join(&name));
                        Finish::SaveFailed
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Finish::SaveFailed,
        }
    }
    Finish::SaveFailed
}

#[cfg(test)]
mod tests;
