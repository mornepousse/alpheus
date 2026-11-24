//! What the capture's worker thread does with a frame: cut it to its area, encode it, then copy it
//! to the clipboard or save it, and say how it ended; or, for the colour picker, copy its value.
//!
//! `run` is the whole job and runs on the short-lived `alpheus-capture` thread (`app.rs` starts
//! it); `render` and `deliver` are its two halves, tested apart. The copy goes through the
//! executor like every other tool: the PNG is written to a private file in `$XDG_RUNTIME_DIR/alpheus`
//! (owner only, atomically) and handed to the copy command as its standard input, since a PNG is
//! far past the page a pipe write may carry (`run_with_stdin_file`). The command's exit code is what
//! "copied" means, and the file is removed as soon as the command is done. The colour picker's
//! value goes the same way, as text (`pick.txt`), to the copy command as written, so that its exit
//! says whether it was copied. A save writes a new file in the pictures folder, never over one.
//!
//! For an annotation or a text recognition the worker runs twice. First it writes the PNG where the
//! external tool will open it (`capture-<n>.png`, owner only: `Prepared`); once the tool has ended,
//! it copies what the tool gave: the annotated image straight from the tool's file, as a PNG, or the
//! text through `ocr.txt`, as written; then the file goes.
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
use super::tools::is_tool_file;
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
    /// For an annotation or a recognition: the name, in `temp_dir`, of the image the tool opens
    /// (`tools::ToolFiles::input`).
    pub tool_input: Option<&'a str>,
}

/// The name of the private file the copy command reads: one capture at a time, so one name.
const COPY_FILE: &str = "capture.png";
/// The colour picker's: its value's text.
const PICK_FILE: &str = "pick.txt";
/// A text recognition's: the text to copy.
const OCR_FILE: &str = "ocr.txt";

/// The longest the copy command's foreground may take to read the image and exit.
pub const COPY_LIMIT: Duration = Duration::from_secs(5);

/// The most numbered tries for a save name taken already: past it the save fails.
const MAX_TRIES: u32 = 99;

/// Clears what a daemon killed mid-copy or mid-tool left in `dir` (the private folder):
/// `capture.png`, `pick.txt`, `ocr.txt`, a tool run's `capture-<n>.png`, `capture-<n>-out.png` and
/// `capture-<n>.txt`, and the writer's `.<name>.<pid>` of each, best effort, never anything else,
/// never a folder that is not ours.
pub fn remove_stale(dir: &Path) {
    if dir_is_ours(dir).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ours = |file: &str| name == file || name.starts_with(&format!(".{file}."));
        if ours(COPY_FILE) || ours(PICK_FILE) || ours(OCR_FILE) || is_tool_file(&name) {
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

/// What the worker is handed.
pub enum Work {
    /// A frame, and its area in buffer pixels (None: all of it), to encode and deliver.
    Image { frame: Frame, area: Option<Rect> },
    /// The colour picker's value, to copy as text.
    Text(String),
    /// The annotation tool's image, the file of this name in `temp_dir`: copied as a PNG, then
    /// removed.
    Annotated(String),
    /// The text a recognition printed, to copy as text.
    Recognised(String),
}

/// The whole job: render, then deliver; or copy the picked value. A frame that renders to nothing
/// is `CaptureFailed`, and then nothing is copied or saved.
///
/// A panic anywhere in it (the executor included) is caught: the capture ends as `CaptureFailed`
/// and the private file is removed, so the flow is never left waiting for a worker that is gone.
pub fn run(exec: &dyn Executor, work: Work, d: &Delivery) -> Finish {
    let done = catch_unwind(AssertUnwindSafe(|| match work {
        Work::Image { frame, area } => {
            let png = render(&frame, area);
            // The frame's pixels are not needed past here: dropped before the copy waits on anything.
            drop(frame);
            match png {
                Ok(png) => deliver(exec, &png, d),
                Err(_) => Finish::CaptureFailed,
            }
        }
        // The text as written, by the copy command as written: no image type for it.
        Work::Text(value) => {
            if copy_through(exec, d.copy, value.as_bytes(), d.temp_dir, PICK_FILE) {
                Finish::Picked(value)
            } else {
                Finish::CopyFailed
            }
        }
        Work::Annotated(name) => copy_file(exec, d, &name),
        Work::Recognised(text) => {
            if copy_through(exec, d.copy, text.as_bytes(), d.temp_dir, OCR_FILE) {
                Finish::TextCopied
            } else {
                Finish::CopyFailed
            }
        }
    }));
    done.unwrap_or_else(|_| {
        let _ = remove_private(d.temp_dir, COPY_FILE);
        let _ = remove_private(d.temp_dir, PICK_FILE);
        let _ = remove_private(d.temp_dir, OCR_FILE);
        if let Some(input) = d.tool_input {
            let _ = remove_private(d.temp_dir, input);
        }
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
        Goal::Annotate | Goal::Ocr => prepare(png, d),
        // A recording never reaches the worker: nothing to deliver.
        Goal::Record => Finish::CaptureFailed,
    }
}

/// The PNG written, owner only, where the external tool will open it: `Prepared`, the tool runs next.
fn prepare(png: &[u8], d: &Delivery) -> Finish {
    let Some(input) = d.tool_input else { return Finish::CaptureFailed };
    match write_private(d.temp_dir, input, png) {
        Ok(()) => Finish::Prepared,
        Err(_) => {
            let _ = remove_private(d.temp_dir, input);
            Finish::CaptureFailed
        }
    }
}

/// The annotated image `name` (in `d.temp_dir`, written by the tool) copied as a PNG, through the
/// file itself on the copy command's standard input, then removed whatever came of it.
fn copy_file(exec: &dyn Executor, d: &Delivery, name: &str) -> Finish {
    let argv = copy_argv(d.copy);
    let copied = match argv.split_first() {
        Some((prog, args)) => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            matches!(exec.run_with_stdin_file(prog, &args, &d.temp_dir.join(name), COPY_LIMIT), Ok(Some(0)))
        }
        None => false,
    };
    let _ = remove_private(d.temp_dir, name);
    if copied { Finish::Annotated } else { Finish::CopyFailed }
}

fn copy(exec: &dyn Executor, png: &[u8], d: &Delivery) -> Finish {
    if copy_through(exec, &copy_argv(d.copy), png, d.temp_dir, COPY_FILE) { Finish::Copied(d.job.scope) } else { Finish::CopyFailed }
}

/// Runs `argv` with `bytes` on its standard input, through the private file `name` of `temp_dir`,
/// and says whether it took them: copied means its foreground exit is 0 (`wl-copy` forks to hold the
/// clipboard and its foreground ends once it has read the file), as `screenshot.sh` checks it.
fn copy_through(exec: &dyn Executor, argv: &[String], bytes: &[u8], temp_dir: &Path, name: &str) -> bool {
    let Some((prog, args)) = argv.split_first() else { return false };
    if write_private(temp_dir, name, bytes).is_err() {
        return false;
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ended = exec.run_with_stdin_file(prog, &args, &temp_dir.join(name), COPY_LIMIT);
    // The command has read it, or never started, or was killed: the file goes either way.
    let _ = remove_private(temp_dir, name);
    matches!(ended, Ok(Some(0)))
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
