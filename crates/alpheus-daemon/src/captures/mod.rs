//! Captures: the daemon takes screenshots natively.
//!
//! This folder holds the capture pipeline. `screencopy.rs` is the only file that speaks the
//! screencopy protocol and turns one output into a `Frame`; `select.rs` and `select_app.rs` are the
//! area selector (a pure model and its layer surface); `pick.rs` and `pick_app.rs` are the colour
//! picker (its lens and value, and its layer surface over the frozen frame); `encode.rs` writes a
//! PNG, `deliver.rs` copies it or saves it, `paths.rs` says where; `tools.rs` runs the external
//! annotation and text recognition tools and `record.rs` the recorder; `app.rs` wires all of it to
//! the daemon.
//!
//! This file is the pure flow: which verb asks for what (`plan`), where a capture stands
//! (`Flow`, one at a time, an external tool's run included), how it ended (`Finish`) and the notice
//! that follows (`notice`). A recording leaves the flow once it starts: it runs beside the captures.
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
pub mod pick;
pub mod pick_app;
pub mod record;
pub mod screencopy;
pub mod select;
pub mod select_app;
pub mod tools;

use alpheus_common::theme::Rgb;

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

/// What the area selector answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorAnswer {
    /// An area, in the output's logical pixels.
    Area(Rect),
    /// The user ended it (Esc, a click without a drag, focus taken, another surface opened): silent.
    Cancelled,
    /// It could not open, or the compositor took it away (its layer closed, its output gone).
    Failed,
}

/// What the colour picker answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickAnswer {
    /// The colour of the pixel under the pointer, in the frozen frame.
    Colour(Rgb),
    /// The user ended it (Esc, a right click, focus taken, another surface opened): silent.
    Cancelled,
    /// It could not open, or the compositor took it away (its layer closed, its output gone).
    Failed,
}

/// What is captured.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    #[default]
    /// The whole focused output.
    Screen,
    /// An area picked with the selector.
    Area,
    /// One pixel's colour, picked on the frozen output.
    Pick,
}

/// What becomes of the capture.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    /// On the clipboard, through `launcher.toml`'s `copy`: the image, or the colour picker's value.
    #[default]
    Copy,
    /// In the pictures folder.
    Save,
    /// Handed to `[captures] annotate`; the image it writes back is copied.
    Annotate,
    /// Handed to `[captures] ocr`; the text it prints is copied.
    Ocr,
    /// Recorded by `[captures] record`, which alpheus starts and stops.
    Record,
}

/// One capture: a scope and a goal.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Job {
    pub scope: Scope,
    pub goal: Goal,
}

/// Why a verb does nothing; the text is what `alpheus-launch` prints (exit 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The compositor has no screencopy protocol: the captures are off.
    NoScreencopy,
    /// The verb's external tool is `[]` in `[captures]`.
    NoTool,
    /// `stop` while nothing records.
    NotRecording,
}

impl Refusal {
    /// A fixed sentence: never a name, a path or a geometry.
    pub fn text(self) -> &'static str {
        match self {
            Refusal::NoScreencopy => "captures are off: the compositor offers no screencopy",
            Refusal::NoTool => "this capture has no tool configured in [captures]",
            Refusal::NotRecording => "no recording is running",
        }
    }
}

/// What a verb asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Capture(Job),
    /// The debug verb: the selector alone, nothing captured.
    Select,
    /// End the recording.
    Stop,
    Refuse(Refusal),
}

/// Which external tools `[captures]` names (a key set to `[]` names none): a verb whose tool is
/// missing is refused.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tools {
    pub annotate: bool,
    pub ocr: bool,
    pub record: bool,
}

/// The plan of `verb`. Without screencopy every capture is refused; only the debug selector, which
/// captures nothing, and `stop`, which ends what runs, still go on. A verb whose tool `tools` does
/// not name is refused.
pub fn plan(verb: alpheus_common::launcher_ipc::CaptureVerb, has_screencopy: bool, tools: Tools) -> Plan {
    use alpheus_common::launcher_ipc::CaptureVerb as V;
    match verb {
        V::Select => return Plan::Select,
        V::Stop => return Plan::Stop,
        _ if !has_screencopy => return Plan::Refuse(Refusal::NoScreencopy),
        _ => {}
    }
    let (scope, goal, named) = match verb {
        V::Full => (Scope::Screen, Goal::Copy, true),
        V::Area => (Scope::Area, Goal::Copy, true),
        V::AreaSave => (Scope::Area, Goal::Save, true),
        V::Pick => (Scope::Pick, Goal::Copy, true),
        V::Annotate => (Scope::Area, Goal::Annotate, tools.annotate),
        V::Ocr => (Scope::Area, Goal::Ocr, tools.ocr),
        V::Record => (Scope::Screen, Goal::Record, tools.record),
        V::RecordArea => (Scope::Area, Goal::Record, tools.record),
        // Answered above.
        V::Select => return Plan::Select,
        V::Stop => return Plan::Stop,
    };
    if !named {
        return Plan::Refuse(Refusal::NoTool);
    }
    Plan::Capture(Job { scope, goal })
}

/// What the external tool of an annotation or a text recognition came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEnd {
    /// The annotation tool exited 0 and wrote an image: it is copied.
    Annotated,
    /// The recognition tool exited 0 and printed this text (trimmed, not empty): it is copied.
    Text(String),
    /// Exited 0 with no output: the user closed it, silent.
    Cancelled,
    /// It could not start, exited with another code, was killed, or printed too much.
    Failed,
}

/// How a capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The selector was cancelled: silent.
    Cancelled,
    Copied(Scope),
    /// The colour picker's value copied; its text (the notice gives it, the log never does).
    Picked(String),
    /// Saved; the file's name, never its path.
    Saved(String),
    /// The compositor gave no frame, or the area held no pixel.
    CaptureFailed,
    /// The frame was fine but the copy command did not take it.
    CopyFailed,
    SaveFailed,
    /// Not an ending: the image is written for the external tool, which runs next.
    Prepared,
    /// The annotation tool's image copied.
    Annotated,
    /// The recognised text copied (the notice never gives it, the log neither).
    TextCopied,
    /// The external tool could not start, failed, was killed or printed too much.
    ToolFailed,
    /// A recording ended and its file is there; the file's name, never its path.
    Recorded(String),
    /// The recorder could not start, failed, was killed, or left no file.
    RecordFailed,
}

impl Finish {
    /// Whether the clipboard now holds this capture: only a copy that succeeded.
    pub fn touches_the_clipboard(&self) -> bool {
        matches!(self, Finish::Copied(_) | Finish::Picked(_) | Finish::Annotated | Finish::TextCopied)
    }
}

/// A notice to show: app `alpheus`, the summary `Screenshot`, `Colour picker`, `Text from area` or
/// `Screen recording`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub summary: &'static str,
    pub body: String,
    pub critical: bool,
}

/// The notice an ending of capture `job` gives: none for a cancel, a calm one for a success, a
/// critical one for each failure (docs/decisions.md, "A capture never touches the clipboard unless
/// it succeeded"). Its summary is `Colour picker` for a pick, `Text from area` for a text
/// recognition and `Screen recording` for a recording, failures included, else `Screenshot`. The
/// recognised text is never in it.
pub fn notice(finish: &Finish, job: Job) -> Option<Notice> {
    let (body, critical) = match finish {
        // `Prepared` is a step, not an ending: it never reaches the user.
        Finish::Cancelled | Finish::Prepared => return None,
        Finish::Copied(Scope::Area) => ("Area copied".to_string(), false),
        // A pick ends as `Picked`, with its value: a copied image is the screen's or an area's.
        Finish::Copied(_) => ("Screen copied".to_string(), false),
        Finish::Picked(value) => (format!("Copied: {value}"), false),
        Finish::Saved(name) => (format!("Saved: {name}"), false),
        Finish::Annotated => ("Annotated image copied".to_string(), false),
        Finish::TextCopied => ("Text copied".to_string(), false),
        Finish::Recorded(name) => (format!("Recording saved: {name}"), false),
        Finish::CaptureFailed => ("Capture failed".to_string(), true),
        Finish::CopyFailed => ("Not copied: the copy command failed".to_string(), true),
        Finish::SaveFailed => ("Not saved: the file could not be written".to_string(), true),
        Finish::ToolFailed if job.goal == Goal::Ocr => ("Text recognition failed".to_string(), true),
        Finish::ToolFailed => ("Annotation failed".to_string(), true),
        Finish::RecordFailed => ("Recording failed".to_string(), true),
    };
    let summary = match (job.scope, job.goal) {
        (Scope::Pick, _) => "Colour picker",
        (_, Goal::Ocr) => "Text from area",
        (_, Goal::Record) => "Screen recording",
        _ => "Screenshot",
    };
    Some(Notice { summary, body, critical })
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
    /// The colour picker is open over the frozen frame.
    Picking,
    /// The worker encodes and delivers (or copies the picked value, or the tool's result).
    Encoding,
    /// The external tool (annotation, text recognition) runs: as long as the user keeps it open.
    Tool,
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
    /// Open the colour picker over the frame.
    Pick,
    /// Hand the picked colour to the worker, which copies its value.
    Copy(Rgb),
    /// Start the external tool on the image the worker wrote.
    RunTool,
    /// Hand the annotation tool's image to the worker, which copies it.
    CopyAnnotated,
    /// Hand the recognised text to the worker, which copies it.
    CopyText(String),
    /// Start the recorder on the output, or on this area of it (logical pixels); the flow is at rest
    /// again (a recording runs beside the captures, never in their slot).
    Record(Option<Rect>),
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
    /// The capture in progress, or the last one once it ended: what its notice says.
    last: Job,
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

    /// Whether the external tool runs and its end is awaited.
    pub fn waits_for_tool(&self) -> bool {
        self.stage == Stage::Tool
    }

    /// The scope of the capture in progress, or of the last one, kept once it ended.
    pub fn scope(&self) -> Scope {
        self.last.scope
    }

    /// The capture in progress, or the last one, kept once it ended: what its notice says.
    pub fn last_job(&self) -> Job {
        self.last
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
        self.last = job;
        self.generation += 1;
        Some(match job.scope {
            Scope::Area => {
                self.stage = Stage::Selecting;
                Step::Select
            }
            // The picker shows the whole output, frozen: its frame is the whole screen's.
            Scope::Screen | Scope::Pick => {
                self.stage = Stage::Settling(None);
                Step::Settle
            }
        })
    }

    /// The selector answered. A cancel is silent; a selector that failed ends the capture as a
    /// failure. `generation` is the capture that opened the selector: the answer of an earlier
    /// capture's selector (one left open when the glue reset its capture, then replaced by the
    /// next capture's) is none of this one's business.
    pub fn selected(&mut self, generation: u64, answer: SelectorAnswer) -> Step {
        if self.stage != Stage::Selecting || generation != self.generation {
            return Step::Nothing;
        }
        match answer {
            SelectorAnswer::Area(area) => {
                self.stage = Stage::Settling(Some(area));
                Step::Settle
            }
            SelectorAnswer::Cancelled => self.end(Finish::Cancelled),
            SelectorAnswer::Failed => self.end(Finish::CaptureFailed),
        }
    }

    /// The compositor confirmed it has dealt with the removal: the frame may be asked, so the dim
    /// (or a closing menu) is never in it. A recording starts instead, and leaves the slot free: it
    /// runs beside the captures. `generation` is the capture the round trip was for.
    pub fn settled(&mut self, generation: u64) -> Step {
        let Stage::Settling(area) = self.stage else { return Step::Nothing };
        if generation != self.generation {
            return Step::Nothing;
        }
        if self.job.is_some_and(|j| j.goal == Goal::Record) {
            self.finished();
            return Step::Record(area);
        }
        self.stage = Stage::Grabbing;
        Step::Grab(area)
    }

    /// The frame of capture `generation` came (`true`) or did not. An answer of another capture is
    /// none of this one's.
    pub fn grabbed(&mut self, generation: u64, ok: bool) -> Step {
        if self.stage != Stage::Grabbing || generation != self.generation {
            return Step::Nothing;
        }
        if !ok {
            return self.end(Finish::CaptureFailed);
        }
        if self.job.is_some_and(|j| j.scope == Scope::Pick) {
            self.stage = Stage::Picking;
            Step::Pick
        } else {
            self.stage = Stage::Encoding;
            Step::Encode
        }
    }

    /// The colour picker answered for capture `generation`: a colour goes to the worker to be copied,
    /// a cancel is silent, a picker that failed ends the capture as a failure. An answer of another
    /// capture's picker is none of this one's.
    pub fn picked(&mut self, generation: u64, answer: PickAnswer) -> Step {
        if self.stage != Stage::Picking || generation != self.generation {
            return Step::Nothing;
        }
        match answer {
            PickAnswer::Colour(rgb) => {
                self.stage = Stage::Encoding;
                Step::Copy(rgb)
            }
            PickAnswer::Cancelled => self.end(Finish::Cancelled),
            PickAnswer::Failed => self.end(Finish::CaptureFailed),
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

    /// The worker's result for capture `generation`: the capture is over, unless the worker only
    /// wrote the image an external tool now opens (`Prepared`). An ending of another capture (a
    /// worker left running when the glue reset its capture) is none of this one's.
    pub fn delivered(&mut self, generation: u64, finish: Finish) -> Step {
        if self.stage != Stage::Encoding || generation != self.generation {
            return Step::Nothing;
        }
        if finish != Finish::Prepared {
            return self.end(finish);
        }
        match self.job.map(|j| j.goal) {
            Some(Goal::Annotate | Goal::Ocr) => {
                self.stage = Stage::Tool;
                Step::RunTool
            }
            // A worker that prepared an image no tool asked for: nothing can follow it.
            _ => self.end(Finish::CaptureFailed),
        }
    }

    /// The external tool of capture `generation` ended: its image or its text goes to the worker to
    /// be copied, an exit with no output is a silent cancel, a failure is critical. An end of
    /// another capture's tool is none of this one's.
    pub fn tool_ended(&mut self, generation: u64, end: ToolEnd) -> Step {
        if self.stage != Stage::Tool || generation != self.generation {
            return Step::Nothing;
        }
        match end {
            ToolEnd::Annotated => {
                self.stage = Stage::Encoding;
                Step::CopyAnnotated
            }
            ToolEnd::Text(text) => {
                self.stage = Stage::Encoding;
                Step::CopyText(text)
            }
            ToolEnd::Cancelled => self.end(Finish::Cancelled),
            ToolEnd::Failed => self.end(Finish::ToolFailed),
        }
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
