//! Captures: the daemon takes screenshots natively.
//!
//! This folder holds the capture pipeline. `screencopy.rs` is the only file that speaks the
//! screencopy protocol and turns one output into a `Frame`; `select.rs` and `select_app.rs` are the
//! area selector (a pure model and its layer surface); `encode.rs` writes a PNG, `deliver.rs` copies
//! it or saves it, `paths.rs` says where; `app.rs` wires all of it to the daemon.
//!
//! This file is the pure flow: which verb asks for what (`plan`), where a capture stands
//! (`Flow`, one at a time), how it ended (`Finish`) and the notice that follows (`notice`).
//!
//! Never:
//! - hold anything at rest: a frame, a buffer or the selector's surface lives only while its
//!   capture is open;
//! - read a clock, a file or the environment, start anything or log: the glue does;
//! - notify a cancel, or a success that did not happen: the clipboard and the notice follow the
//!   capture's own result (docs/decisions.md, "A capture never touches the clipboard unless it
//!   succeeded");
//! - log a path, a geometry or an output name.
//!
//! See: docs/features/captures.md, docs/decisions.md.

pub mod app;
pub mod deliver;
pub mod encode;
pub mod paths;
pub mod screencopy;
pub mod select;
pub mod select_app;

/// A rectangle of whole pixels, `x` and `y` of its top-left corner. A logical area (origin at the
/// output's top left, never negative, never empty) when it comes from the selector, a buffer area
/// after `screencopy::map_area`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// What is captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The whole focused output.
    Screen,
    /// An area picked with the selector.
    Area,
}

/// What becomes of the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    /// On the clipboard, through `launcher.toml`'s `copy`.
    Copy,
    /// In the pictures folder.
    Save,
}

/// One capture: a scope and a goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Job {
    pub scope: Scope,
    pub goal: Goal,
}

/// Why a verb does nothing; the text is what `alpheus-launch` prints (exit 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The compositor has no screencopy protocol: the captures are off.
    NoScreencopy,
    /// The verb is not built yet.
    NotYet,
}

impl Refusal {
    /// A fixed sentence: never a name, a path or a geometry.
    pub fn text(self) -> &'static str {
        match self {
            Refusal::NoScreencopy => "captures are off: the compositor offers no screencopy",
            Refusal::NotYet => "this capture is not available yet",
        }
    }
}

/// What a verb asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Capture(Job),
    /// The debug verb: the selector alone, nothing captured.
    Select,
    Refuse(Refusal),
}

/// The plan of `verb`. Without screencopy every capture is refused, and only the debug selector,
/// which captures nothing, still runs.
pub fn plan(verb: alpheus_common::launcher_ipc::CaptureVerb, has_screencopy: bool) -> Plan {
    use alpheus_common::launcher_ipc::CaptureVerb as V;
    if verb == V::Select {
        return Plan::Select;
    }
    if !has_screencopy {
        return Plan::Refuse(Refusal::NoScreencopy);
    }
    match verb {
        V::Full => Plan::Capture(Job { scope: Scope::Screen, goal: Goal::Copy }),
        V::Area => Plan::Capture(Job { scope: Scope::Area, goal: Goal::Copy }),
        V::AreaSave => Plan::Capture(Job { scope: Scope::Area, goal: Goal::Save }),
        V::Pick | V::Annotate | V::Ocr | V::Record | V::RecordArea | V::Stop | V::Select => Plan::Refuse(Refusal::NotYet),
    }
}

/// How a capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The selector was cancelled: silent.
    Cancelled,
    Copied(Scope),
    /// Saved; the file's name, never its path.
    Saved(String),
    /// The compositor gave no frame, or the area held no pixel.
    CaptureFailed,
    /// The frame was fine but the copy command did not take it.
    CopyFailed,
    SaveFailed,
}

impl Finish {
    /// Whether the clipboard now holds this capture: only a copy that succeeded.
    pub fn touches_the_clipboard(&self) -> bool {
        matches!(self, Finish::Copied(_))
    }
}

/// A notice to show: app `alpheus`, the summary always `Screenshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub summary: &'static str,
    pub body: String,
    pub critical: bool,
}

/// The notice an ending gives: none for a cancel, a calm one for a success, a critical one for
/// each failure (docs/decisions.md, "A capture never touches the clipboard unless it succeeded").
pub fn notice(finish: &Finish) -> Option<Notice> {
    let (body, critical) = match finish {
        Finish::Cancelled => return None,
        Finish::Copied(Scope::Screen) => ("Screen copied".to_string(), false),
        Finish::Copied(Scope::Area) => ("Area copied".to_string(), false),
        Finish::Saved(name) => (format!("Saved: {name}"), false),
        Finish::CaptureFailed => ("Capture failed".to_string(), true),
        Finish::CopyFailed => ("Not copied: the copy command failed".to_string(), true),
        Finish::SaveFailed => ("Not saved: the file could not be written".to_string(), true),
    };
    Some(Notice { summary: "Screenshot", body, critical })
}

/// Where a capture stands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Stage {
    #[default]
    Idle,
    /// The selector is open.
    Selecting,
    /// The selector (or nothing, for a whole screen) is gone; the compositor has not yet confirmed
    /// it. The area, none for the whole screen.
    Settling(Option<Rect>),
    /// A frame was asked.
    Grabbing,
    /// The worker encodes and delivers.
    Encoding,
}

/// How long a frame may take: past it the capture fails and the slot is free again.
pub const GRAB_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// What the glue does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Open the selector.
    Select,
    /// Wait for the compositor to have dealt with what just closed (the selector's surface, a menu
    /// the verb came from: a round trip), then `Flow::settled`.
    Settle,
    /// Ask for a frame of the output; the area (logical pixels) is None for the whole screen.
    Grab(Option<Rect>),
    /// Hand the frame to the worker.
    Encode,
    /// It is over: show `notice(finish)`.
    Finish(Finish),
    /// Not for this flow (out of turn, or no capture asked): do nothing.
    Nothing,
}

/// One capture at a time, from its verb to its end.
#[derive(Debug, Default)]
pub struct Flow {
    stage: Stage,
    job: Option<Job>,
    /// Counts the captures begun: an answer carries the one it was asked for, so a late answer of a
    /// capture that has ended is never taken for the next one's.
    generation: u64,
}

impl Flow {
    pub fn is_busy(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// Whether a frame has been asked and is awaited.
    pub fn waits_for_frame(&self) -> bool {
        self.stage == Stage::Grabbing
    }

    /// The capture in progress.
    pub fn job(&self) -> Option<Job> {
        self.job
    }

    /// The number of the capture in progress (or the last one): what each answer must carry.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A capture is asked for: None while another runs (the second is ignored). Every scope waits
    /// for the compositor before its frame (`Step::Settle`): a whole screen may be asked right as a
    /// menu closes.
    pub fn begin(&mut self, job: Job) -> Option<Step> {
        if self.is_busy() {
            return None;
        }
        self.job = Some(job);
        self.generation += 1;
        Some(match job.scope {
            Scope::Area => {
                self.stage = Stage::Selecting;
                Step::Select
            }
            Scope::Screen => {
                self.stage = Stage::Settling(None);
                Step::Settle
            }
        })
    }

    /// The selector answered, with an area or none. A selector nobody here asked for (the debug
    /// verb) is none of this flow's business.
    pub fn selected(&mut self, area: Option<Rect>) -> Step {
        if self.stage != Stage::Selecting {
            return Step::Nothing;
        }
        match area {
            Some(area) => {
                self.stage = Stage::Settling(Some(area));
                Step::Settle
            }
            None => self.end(Finish::Cancelled),
        }
    }

    /// The compositor confirmed it has dealt with the removal: the frame may be asked, so the dim
    /// (or a closing menu) is never in it. `generation` is the capture the round trip was for.
    pub fn settled(&mut self, generation: u64) -> Step {
        match self.stage {
            Stage::Settling(area) if generation == self.generation => {
                self.stage = Stage::Grabbing;
                Step::Grab(area)
            }
            _ => Step::Nothing,
        }
    }

    /// The frame of capture `generation` came (`true`) or did not. An answer of another capture is
    /// none of this one's.
    pub fn grabbed(&mut self, generation: u64, ok: bool) -> Step {
        if self.stage != Stage::Grabbing || generation != self.generation {
            return Step::Nothing;
        }
        if ok {
            self.stage = Stage::Encoding;
            Step::Encode
        } else {
            self.end(Finish::CaptureFailed)
        }
    }

    /// The frame of capture `generation` did not come within `GRAB_LIMIT`: it fails, if it still
    /// waits for it.
    pub fn grab_expired(&mut self, generation: u64) -> Step {
        if self.stage != Stage::Grabbing || generation != self.generation {
            return Step::Nothing;
        }
        self.end(Finish::CaptureFailed)
    }

    /// The worker's result: the capture is over.
    pub fn delivered(&mut self, finish: Finish) -> Step {
        if self.stage != Stage::Encoding {
            return Step::Nothing;
        }
        self.end(finish)
    }

    /// Back to rest, whatever the stage.
    pub fn finished(&mut self) {
        self.stage = Stage::Idle;
        self.job = None;
    }

    fn end(&mut self, finish: Finish) -> Step {
        self.finished();
        Step::Finish(finish)
    }
}

#[cfg(test)]
mod tests;
