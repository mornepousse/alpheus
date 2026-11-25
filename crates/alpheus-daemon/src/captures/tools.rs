//! The external tools of a capture: an annotation editor and a text recognizer, started on a private
//! copy of the area and judged by what they leave.
//!
//! The worker (`deliver.rs`) writes the area's PNG to `capture-<n>.png` in the private folder
//! (`$XDG_RUNTIME_DIR/alpheus`, owner only). `start` makes the tool's output file there, empty and
//! owner-only (`capture-<n>-out.png` for an annotation, which the tool writes into; `capture-<n>.txt`
//! for a recognition, which is the tool's standard output), fills `{file}` and `{out}` with the two
//! paths, each a whole argument, and starts the tool as a watched child of the daemon
//! (`Executor::spawn_watched`): the loop hears its end on its pidfd, however long the user keeps it
//! open. `judge` reads what it left once it ended: exit 0 with an image or a text is a result, exit
//! 0 with nothing is a cancel, anything else a failure. Every file goes on every path: `judge`
//! removes them all but an annotated image, which the worker copies and then removes, and `remove`
//! is for a run given up.
//!
//! Never:
//! - put a path inside a longer argument, or run a shell: `{file}` and `{out}` are whole arguments,
//!   as `[captures]` is checked to hold them (`config.rs`);
//! - read more than `MAX_TEXT` of a recognition, or follow a link: the private reads refuse both;
//! - log or panic: the glue logs one fixed word per ending, and the text is never one.
//!
//! See: docs/features/captures.md, docs/decisions.md ("A capture never touches the clipboard unless
//! it succeeded", "An external capture tool runs as a watched child of the daemon").

use super::ToolEnd;
use crate::state_file::{PrivateRead, read_private, remove_private, write_private};
use alpheus_common::exec::{Executor, Watched};
use std::path::{Path, PathBuf};

/// Where the image handed to a tool goes in its argv.
pub const FILE: &str = "{file}";
/// Where a tool's output goes: the annotated image, or the recording.
pub const OUT: &str = "{out}";
/// The recorder's area, `x,y wxh` in the compositor's logical pixels.
pub const GEOMETRY: &str = "{geometry}";

/// The most text a recognition may print: past it the run is a failure, never a cut text copied.
pub const MAX_TEXT: u64 = 256 * 1024;

/// Which tool runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `[captures] annotate`: writes the edited image to `{out}`.
    Annotate,
    /// `[captures] ocr`: prints the text on its standard output.
    Ocr,
}

/// `template` with each element that is exactly one of `values`' placeholders replaced by its value,
/// whole: a placeholder inside a longer element stays as written (`[captures]` refuses one anyway).
pub fn fill(template: &[String], values: &[(&str, &str)]) -> Vec<String> {
    let mut argv = Vec::with_capacity(template.len());
    for element in template {
        let mut filled = element.clone();
        for (placeholder, value) in values {
            if element == placeholder {
                filled = (*value).to_string();
            }
        }
        argv.push(filled);
    }
    argv
}

/// The private files of one tool run, by name in `dir`: the image handed to the tool, and what it
/// gives back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFiles {
    pub dir: PathBuf,
    pub input: String,
    pub output: String,
}

impl ToolFiles {
    /// The files of capture `n`'s run of `tool`, in the private folder `dir`.
    pub fn new(dir: &Path, n: u64, tool: Tool) -> ToolFiles {
        let output = match tool {
            Tool::Annotate => format!("capture-{n}-out.png"),
            Tool::Ocr => format!("capture-{n}.txt"),
        };
        ToolFiles { dir: dir.to_path_buf(), input: format!("capture-{n}.png"), output }
    }

    pub fn input_path(&self) -> PathBuf {
        self.dir.join(&self.input)
    }

    pub fn output_path(&self) -> PathBuf {
        self.dir.join(&self.output)
    }

    /// Both files removed, best effort (a run given up, or a daemon stopping).
    pub fn remove(&self) {
        let _ = remove_private(&self.dir, &self.input);
        let _ = remove_private(&self.dir, &self.output);
    }
}

/// Whether `name` is one of a tool run's private files, or the writer's temporary file of one
/// (`.capture-<n>.png.<pid>`): what a daemon killed during a run may have left.
pub fn is_tool_file(name: &str) -> bool {
    let name = match name.strip_prefix('.') {
        // `.capture-3.png.1234`: the file's name, then the writer's pid.
        Some(hidden) => match hidden.rsplit_once('.') {
            Some((file, pid)) if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) => file,
            _ => return false,
        },
        None => name,
    };
    let Some(rest) = name.strip_prefix("capture-") else { return false };
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(n) = rest.strip_suffix("-out.png") {
        return number(n);
    }
    match rest.strip_suffix(".png").or_else(|| rest.strip_suffix(".txt")) {
        Some(n) => number(n),
        None => false,
    }
}

/// Starts `tool` (`template`, `[captures] annotate` or `ocr`) on the image already written to
/// `files.input_path()`. Its output file is made first, empty and owner-only, so whatever the tool's
/// umask the result stays private, and an editor closed without saving leaves it empty. None when
/// it cannot start (no program, no output file): the files are removed and the run is a failure.
pub fn start(exec: &dyn Executor, tool: Tool, template: &[String], files: &ToolFiles) -> Option<Box<dyn Watched>> {
    if write_private(&files.dir, &files.output, b"").is_err() {
        files.remove();
        return None;
    }
    let (input, output) = (files.input_path(), files.output_path());
    let argv = fill(template, &[(FILE, &input.to_string_lossy()), (OUT, &output.to_string_lossy())]);
    let Some((program, args)) = argv.split_first() else {
        files.remove();
        return None;
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = match tool {
        Tool::Annotate => None,
        Tool::Ocr => Some(output.as_path()),
    };
    match exec.spawn_watched(program, &args, stdout) {
        Ok(child) => Some(child),
        Err(_) => {
            files.remove();
            None
        }
    }
}

/// What `tool` came to, from its `exit` (None: ended by a signal) and what it left in `files`. The
/// image handed to it goes at once; so does the output, except an annotated image, which the worker
/// copies and then removes.
pub fn judge(tool: Tool, exit: Option<i32>, files: &ToolFiles) -> ToolEnd {
    let _ = remove_private(&files.dir, &files.input);
    if exit != Some(0) {
        let _ = remove_private(&files.dir, &files.output);
        return ToolEnd::Failed;
    }
    match tool {
        Tool::Annotate => match std::fs::symlink_metadata(files.output_path()) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => ToolEnd::Annotated,
            _ => {
                let _ = remove_private(&files.dir, &files.output);
                ToolEnd::Cancelled
            }
        },
        Tool::Ocr => {
            let read = read_private(&files.dir, &files.output, MAX_TEXT);
            let _ = remove_private(&files.dir, &files.output);
            match read {
                PrivateRead::Bytes(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let text = text.trim();
                    if text.is_empty() { ToolEnd::Cancelled } else { ToolEnd::Text(text.to_string()) }
                }
                PrivateRead::Absent => ToolEnd::Cancelled,
                PrivateRead::TooLarge(_) | PrivateRead::NotOurs | PrivateRead::Failed => ToolEnd::Failed,
            }
        }
    }
}

#[cfg(test)]
mod tests;
