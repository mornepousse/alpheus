//! The captures on the daemon's state: the verbs, the flow's steps, the frame, the worker thread
//! that encodes and delivers it, and the notice.
//!
//! This is the wiring half (the pure half is `mod.rs`'s `Flow`). A verb reaches `capture_verb`,
//! which asks the plan what it wants: a refusal is the reply (exit 2), a capture is `Flow::begin`.
//! The steps the flow answers with are carried out here: open the selector (`select_app.rs`), wait
//! for the compositor to have removed it (a `wl_display.sync`, answered by `Settled`), ask one frame
//! of the focused output (`screencopy.rs`), hand the frame to the short-lived `alpheus-capture`
//! thread (`deliver.rs`), and show the notice of its ending. For the colour picker the frame opens
//! the picker instead (`pick_app.rs`), and its answer's value, in `[captures] pick_format`, is what
//! the worker copies. An annotation or a recognition starts its external tool once the worker has
//! written the image (`tools.rs`); a recording starts the recorder instead of asking a frame
//! (`record.rs`), lights the privacy screen glyph, and is stopped by `stop` or the daemon's shutdown.
//! The frame and the worker's result come back through the loop's mail (`Mail::Capture`), the end
//! of a tool or of the recorder through its pidfd on the loop (`capture_child_ready`), so the flow
//! only ever moves on the loop's thread.
//!
//! Never:
//! - ask for a frame in the turn the selector was destroyed: the compositor must first have dealt
//!   with the removal, or the dim is in the capture (docs/decisions.md, "A capture waits for the
//!   selector's removal");
//! - keep a frame, a thread or a file once the capture has ended, or start a second capture while
//!   one runs (`Flow`);
//! - write a path, a geometry, an output name or a file name into a log line (a guardrail,
//!   `the_capture_logs_fixed_words_only`);
//! - poll, or keep a timer armed when no frame is awaited: every step is an event of the compositor,
//!   the mail or the verb, and the one deadline (`GRAB_LIMIT`) runs only from the frame request to
//!   its answer (`the_frame_deadline_is_armed_only_while_a_frame_is_awaited`); a tool or the
//!   recorder is heard through its pidfd, readable only when it ends;
//! - signal anything but the recorder it started, through `Recording::stop` (a guardrail,
//!   `the_recording_signal_reaches_only_the_recorder_alpheus_started`).
//!
//! See: docs/features/captures.md, docs/decisions.md ("A capture never touches the clipboard unless
//! it succeeded", "A capture waits for the selector's removal", "The capture's PNG is encoded on a
//! short-lived thread", "An external capture tool runs as a watched child of the daemon", "Recording
//! is the external tool's, driven and stopped by alpheus").

use super::deliver::{Delivery, Work, run};
use super::paths::{file_name, geometry, save_dir, user_dir, video_name, videos_dir};
use super::pick::value_text;
use super::record::{LIVE, Recording, STOP_LIMIT};
use super::screencopy::{CaptureDone, CaptureError, Frame, PendingFrame, buffer_area};
use super::select_app::guarded;
use super::tools::{Tool, ToolFiles, judge, start as start_tool};
use super::{
    Finish, Flow, GRAB_LIMIT, Goal, Job, PickAnswer, Plan, Rect, Refusal, Scope, SelectorAnswer, Step, ToolEnd, Tools, notice, plan,
};
use crate::clock::LocalTime;
use crate::deadline::DeadlineTimer;
use crate::launcher::host::read_config;
use crate::log::write as log;
use crate::notify::{Incoming, Urgency};
use crate::session_bus::notifications::minute_now;
use crate::startup::Mail;
use crate::wayland::App;
use alpheus_common::exec::Watched;
use alpheus_common::launcher_ipc::{Reply, Request};
use alpheus_common::paths;
use alpheus_common::theme::Rgb;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// What reaches the loop from a capture: the compositor's frame, then the worker's ending.
pub enum CaptureMail {
    /// The frame of the capture numbered by the first field (`Flow::generation`).
    Frame(u64, Result<Frame, CaptureError>),
    /// The worker's ending of the capture numbered by the first field.
    Delivered(u64, Finish),
}

/// Everything the captures keep on the `App`: the flow, and while one runs, its output and area.
pub(crate) struct CaptureApp {
    flow: Flow,
    /// The output being captured: the focused one at the verb (the selector's own for an area).
    output: Option<WlOutput>,
    /// The area, in the output's logical pixels; none for the whole screen.
    area: Option<Rect>,
    /// The frame asked and not yet answered, so a capture that gives up can destroy it.
    pending: Option<PendingFrame>,
    /// When the frame is due, while one is awaited; the timer is armed for it and nothing else.
    due: Option<Instant>,
    timer: DeadlineTimer,
    /// The external tool of the capture in progress, while it runs.
    tool: Option<ToolRun>,
    /// The recording, while it runs: beside the captures, never in their slot.
    recording: Option<RecordRun>,
}

/// An external tool running for capture `generation`: the child, its files, and the loop's watch on
/// its pidfd.
struct ToolRun {
    child: Box<dyn Watched>,
    files: ToolFiles,
    tool: Tool,
    generation: u64,
    watch: Option<RegistrationToken>,
}

/// The recorder running, and the loop's watch on its pidfd.
struct RecordRun {
    recording: Recording,
    watch: Option<RegistrationToken>,
}

/// Which watched child a pidfd's readiness is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Child {
    /// The external tool of capture number `.0`.
    Tool(u64),
    Recorder,
}

/// The external tool a goal hands its image to, if any.
fn tool_of(goal: Goal) -> Option<Tool> {
    match goal {
        Goal::Annotate => Some(Tool::Annotate),
        Goal::Ocr => Some(Tool::Ocr),
        Goal::Copy | Goal::Save | Goal::Record => None,
    }
}

/// What a recording's notice is about, whatever capture ran since it started.
const RECORD_JOB: Job = Job { scope: Scope::Screen, goal: Goal::Record };

impl CaptureApp {
    pub(crate) fn new() -> CaptureApp {
        CaptureApp {
            flow: Flow::default(),
            output: None,
            area: None,
            pending: None,
            due: None,
            timer: DeadlineTimer::new(),
            tool: None,
            recording: None,
        }
    }

    /// Whether alpheus records the screen: the Tools menu then shows "Stop recording".
    pub(crate) fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// The number of the capture in progress (or the last one), for an answer of the picker.
    pub(super) fn generation(&self) -> u64 {
        self.flow.generation()
    }

    /// The output being captured, for the picker that opens over it.
    pub(super) fn output(&self) -> Option<WlOutput> {
        self.output.clone()
    }
}

/// The user data of the round trip that waits for what just closed, with the capture it is for.
pub struct Settled(pub u64);

impl Dispatch2<WlCallback, App> for Settled {
    fn event(&self, app: &mut App, _: &WlCallback, event: wl_callback::Event, _: &Connection, _: &QueueHandle<App>) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            let generation = self.0;
            app.capture_guarded("settled", |app| {
                let step = app.capture.flow.settled(generation);
                app.capture_step(step);
            });
        }
    }
}

impl App {
    /// `alpheus-launch --capture <word>` (`launcher/socket_glue.rs`). A capture that cannot run says
    /// why (the reply is an error, exit 2); one asked while another runs is ignored, with a log word.
    pub(crate) fn capture_verb(&mut self, request: &Request) -> Reply {
        let Request::Capture(verb) = request else { return Reply::Done };
        match plan(*verb, self.screencopy.is_some(), self.capture_tools()) {
            Plan::Refuse(why) => {
                log("capture: refused");
                Reply::Error(why.text().into())
            }
            Plan::Stop => {
                if self.capture_stop() {
                    Reply::Done
                } else {
                    log("capture: not recording");
                    Reply::Error(Refusal::NotRecording.text().into())
                }
            }
            // One recording at a time: a second one is ignored, as a second capture is.
            Plan::Capture(job) if job.goal == Goal::Record && self.capture.recording.is_some() => {
                log("capture: already recording");
                Reply::Done
            }
            Plan::Select => {
                // The debug selector is not a capture's: while one runs, its selector is not ours to toggle.
                if self.capture.flow.is_busy() {
                    log("capture: busy");
                } else {
                    self.select_toggle();
                }
                Reply::Done
            }
            Plan::Capture(job) => {
                self.capture_guarded("verb", |app| app.capture_begin(job));
                Reply::Done
            }
        }
    }

    /// Runs a capture entry point; a panic ends the capture, the bar goes on.
    fn capture_guarded<R>(&mut self, what: &str, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = guarded(what, || f(self));
        if r.is_none() {
            self.capture_reset();
        }
        r
    }

    /// Which external tools `[captures]` names (a key set to `[]` names none).
    fn capture_tools(&self) -> Tools {
        let c = &self.bar.config.captures;
        Tools { annotate: !c.annotate.is_empty(), ocr: !c.ocr.is_empty(), record: !c.record.is_empty() }
    }

    /// Back to rest: nothing of the capture is kept, the frame asked is given up, the timer disarmed,
    /// an open picker goes with its frame (without an answer: the flow is the one resetting), and a
    /// tool still running is stopped with its files. A recording is not the capture's: it goes on.
    fn capture_reset(&mut self) {
        self.capture.flow.finished();
        if let Some(run) = self.capture.tool.take() {
            self.capture_unwatch(run.watch);
            // Dropping the child kills it with its group, then the files go.
            drop(run.child);
            run.files.remove();
        }
        self.pick_discard();
        self.capture.output = None;
        self.capture.area = None;
        if let Some(pending) = self.capture.pending.take() {
            pending.cancel();
        }
        self.capture.due = None;
        self.capture_timer_sync();
    }

    /// The timer follows the awaited frame's due time: armed only while one is awaited.
    fn capture_timer_sync(&mut self) {
        let at = self.capture.due;
        self.capture.timer.sync_opt(self.loop_handle.as_ref(), at, App::capture_tick);
    }

    /// The frame did not come in time (the timer's callback: its source is dropped, `fired` says so):
    /// the capture fails, the frame object is destroyed, and the slot is free.
    pub(crate) fn capture_tick(app: &mut App, _now: Instant) {
        app.capture.timer.fired();
        app.capture.due = None;
        app.capture_guarded("timeout", |app| {
            let generation = app.capture.flow.generation();
            let step = app.capture.flow.grab_expired(generation);
            if matches!(step, Step::Finish(_)) {
                log("capture: frame timed out");
            }
            if let Some(pending) = app.capture.pending.take() {
                pending.cancel();
            }
            app.capture_step(step);
        });
        app.capture_timer_sync();
    }

    fn capture_begin(&mut self, job: Job) {
        let Some(step) = self.capture.flow.begin(job) else {
            log("capture: busy");
            return;
        };
        // A capture of the whole screen has no selector, but a menu may be closing: it waits too.
        self.capture.output = self.focused_output();
        self.capture_step(step);
    }

    /// Carries out what the flow says next.
    fn capture_step(&mut self, step: Step) {
        match step {
            Step::Select => {
                let qh = self.qh.clone();
                self.select_open(&qh, Some(self.capture.flow.generation()));
            }
            Step::Settle => self.capture_settle(),
            Step::Grab(area) => self.capture_grab(area),
            Step::Copy(colour) => self.capture_copy_value(colour),
            Step::RunTool => self.capture_run_tool(),
            Step::CopyText(text) => self.capture_deliver(Work::Recognised(text)),
            Step::Record(area) => self.capture_record(area),
            // The frame is in hand where the first two come (`capture_frame`), the tool's file where
            // the third does (`capture_tool_ended`): handled there.
            Step::Encode | Step::Pick | Step::CopyAnnotated | Step::Nothing => {}
            Step::Finish(finish) => self.capture_finish(finish),
        }
        self.capture_timer_sync();
    }

    /// The selector answered (`select_app.rs`, once its surface is destroyed): the flow decides what
    /// that means for capture `generation`, the one that opened the selector: an earlier capture's
    /// selector (left open by a reset) sets nothing here, and the flow ignores it.
    pub(crate) fn capture_selected(&mut self, output: Option<WlOutput>, generation: u64, answer: SelectorAnswer) {
        if self.capture.flow.is_busy() && generation == self.capture.flow.generation() {
            self.capture.output = output;
            self.capture.area = match answer {
                SelectorAnswer::Area(area) => Some(area),
                SelectorAnswer::Cancelled | SelectorAnswer::Failed => None,
            };
        }
        self.capture_guarded("selected", |app| {
            let step = app.capture.flow.selected(generation, answer);
            app.capture_step(step);
        });
    }

    /// Waits for the compositor: a `wl_display.sync` sent after the selector's destruction comes back
    /// once the compositor has handled it, so a frame asked then is drawn without the dim. With no
    /// way to ask (no output, no connection) the frame is asked at once.
    fn capture_settle(&mut self) {
        let connection = self.capture.output.as_ref().and_then(|o| o.backend().upgrade()).map(Connection::from_backend);
        match connection {
            Some(connection) => {
                connection.display().sync(&self.qh, Settled(self.capture.flow.generation()));
                log("capture: waiting for the compositor");
            }
            None => {
                let step = self.capture.flow.settled(self.capture.flow.generation());
                self.capture_step(step);
            }
        }
    }

    /// Asks for the frame of the output; its answer comes back as `Mail::Capture`, numbered by the
    /// capture it is for, and a deadline (`GRAB_LIMIT`) fails the capture if it never does.
    fn capture_grab(&mut self, area: Option<Rect>) {
        self.capture.area = area;
        let generation = self.capture.flow.generation();
        let (Some(output), Some(tx)) = (self.capture.output.clone(), self.mail.clone()) else {
            let step = self.capture.flow.grabbed(generation, false);
            return self.capture_step(step);
        };
        let done: CaptureDone = Box::new(move |result| {
            // A closed channel means the loop has gone: nobody is left to hear the frame.
            let _ = tx.send(Mail::Capture(CaptureMail::Frame(generation, result)));
        });
        let qh = self.qh.clone();
        match self.capture_output(&output, &qh, done) {
            Some(pending) => {
                self.capture.pending = Some(pending);
                self.capture.due = Some(Instant::now() + GRAB_LIMIT);
                log("capture: frame asked");
            }
            None => {
                let step = self.capture.flow.grabbed(generation, false);
                self.capture_step(step);
            }
        }
    }

    /// A capture mail, on the loop's thread.
    pub(crate) fn capture_mail(&mut self, mail: CaptureMail) {
        self.capture_guarded("mail", |app| match mail {
            CaptureMail::Frame(generation, result) => app.capture_frame(generation, result),
            CaptureMail::Delivered(generation, finish) => {
                let step = app.capture.flow.delivered(generation, finish);
                app.capture_step(step);
            }
        });
    }

    /// The compositor's answer for capture `generation`: a frame goes to the worker, an error ends
    /// the capture. An answer of another capture (one that timed out) is dropped unread.
    fn capture_frame(&mut self, generation: u64, result: Result<Frame, CaptureError>) {
        if generation != self.capture.flow.generation() || !self.capture.flow.waits_for_frame() {
            log("capture: late frame ignored");
            drop(result);
            crate::memory::release_memory();
            return;
        }
        // It answered: nothing is pending, nothing is due.
        self.capture.pending = None;
        self.capture.due = None;
        match result {
            Ok(frame) => match self.capture.flow.grabbed(generation, true) {
                Step::Encode => self.capture_encode(frame),
                Step::Pick => self.pick_open(frame, generation),
                step => {
                    drop(frame);
                    crate::memory::release_memory();
                    self.capture_step(step);
                }
            },
            Err(error) => {
                match error {
                    CaptureError::Failed => log("capture: frame failed"),
                    CaptureError::Unsupported => log("capture: frame unsupported"),
                    CaptureError::Buffer => log("capture: frame buffer failed"),
                }
                let step = self.capture.flow.grabbed(generation, false);
                self.capture_step(step);
            }
        }
        self.capture_timer_sync();
    }

    /// The picker answered (`pick_app.rs`, once its surface and its frame are gone): the flow decides
    /// what that means for capture `generation`, the one that opened it.
    pub(crate) fn capture_picked(&mut self, generation: u64, answer: PickAnswer) {
        self.capture_guarded("picked", |app| {
            let step = app.capture.flow.picked(generation, answer);
            app.capture_step(step);
        });
    }

    /// The picked colour's value (`[captures] pick_format`) goes to the worker, which copies it.
    fn capture_copy_value(&mut self, colour: Rgb) {
        let value = value_text(colour, &self.bar.config.captures.pick_format);
        self.capture_deliver(Work::Text(value));
    }

    /// The frame, cut to the capture's area mapped to its pixels, goes to the worker.
    fn capture_encode(&mut self, frame: Frame) {
        let info = self.capture.output.as_ref().and_then(|o| self.outputs.info(o));
        let area = self.capture.area.take().map(|logical| {
            let size = info.as_ref().and_then(|i| i.logical_size).map(|(w, h)| (w.max(0) as u32, h.max(0) as u32));
            let scale = info.as_ref().map_or(1, |i| i.scale_factor);
            let transform = info.as_ref().map_or(Transform::Normal, |i| i.transform);
            buffer_area((frame.width, frame.height), logical, size, scale, transform)
        });
        self.capture_deliver(Work::Image { frame, area });
    }

    /// Hands the work to the short-lived `alpheus-capture` thread, which cuts, encodes, copies or
    /// saves a frame, or copies a value, and mails its ending back. A thread that cannot start ends
    /// the capture as failed.
    fn capture_deliver(&mut self, work: Work) {
        let (Some(job), Some(tx)) = (self.capture.flow.job(), self.mail.clone()) else {
            return self.capture_finish(Finish::CaptureFailed);
        };
        let generation = self.capture.flow.generation();
        // The two small files (launcher.toml, user-dirs.dirs) are read here, on the loop, as the
        // launcher reads launcher.toml at each open: read in the worker, a fault in them would come
        // before the job whose panic is caught, and the ending might never be mailed.
        let launcher = read_config(&paths::config_dir(), &mut |_| {});
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        let dir = save_dir(&self.bar.config.captures.dir, user_folder("XDG_PICTURES_DIR").as_deref(), home.as_deref());
        let name = file_name(&LocalTime::now().0);
        let temp_dir = paths::runtime_dir().join("alpheus");
        // An annotation or a recognition: the name of the image its tool will open.
        let tool_input = tool_of(job.goal).map(|tool| ToolFiles::new(&temp_dir, generation, tool).input);
        let exec = self.exec.clone();
        let started = std::thread::Builder::new().name("alpheus-capture".into()).spawn(move || {
            let delivery = Delivery {
                job,
                copy: &launcher.copy,
                save_dir: dir.as_deref(),
                file_name: name,
                temp_dir: &temp_dir,
                tool_input: tool_input.as_deref(),
            };
            let finish = run(exec.as_ref(), work, &delivery);
            // The frame and the image went with `run`: the loop frees what the thread's arena kept.
            let _ = tx.send(Mail::Capture(CaptureMail::Delivered(generation, finish)));
        });
        if started.is_err() {
            self.capture_finish(Finish::CaptureFailed);
        }
    }

    /// The capture is over: one log word, the notice, and the pages its frame held given back.
    fn capture_finish(&mut self, finish: Finish) {
        self.capture_reset();
        match &finish {
            Finish::Cancelled => log("capture: cancelled"),
            Finish::Copied(_) => log("capture: copied"),
            Finish::Picked(_) => log("capture: colour copied"),
            Finish::Saved(_) => log("capture: saved"),
            Finish::CaptureFailed => log("capture: failed"),
            Finish::CopyFailed => log("capture: copy failed"),
            Finish::SaveFailed => log("capture: save failed"),
            Finish::Annotated => log("capture: annotated image copied"),
            Finish::TextCopied => log("capture: text copied"),
            Finish::ToolFailed => log("capture: tool failed"),
            // A recording's ending goes through `capture_recording_end`, a preparation through the
            // flow: neither reaches here, each still gets its word.
            Finish::Prepared => log("capture: prepared"),
            Finish::Recorded(_) => log("capture: recording saved"),
            Finish::RecordFailed => log("capture: recording failed"),
        }
        if let Some(n) = notice(&finish, self.capture.flow.last_job()) {
            self.capture_notify(n);
        }
        crate::memory::release_memory();
    }

    /// Starts the external tool of the capture in progress on the image the worker wrote, and
    /// watches its pidfd: its end comes back through `capture_child_ready`, however long the user
    /// keeps it open, with no thread and no timer. A tool that cannot start, or cannot be watched,
    /// fails the capture.
    fn capture_run_tool(&mut self) {
        let generation = self.capture.flow.generation();
        let Some(tool) = self.capture.flow.job().and_then(|job| tool_of(job.goal)) else {
            let step = self.capture.flow.tool_ended(generation, ToolEnd::Failed);
            return self.capture_step(step);
        };
        let files = ToolFiles::new(&paths::runtime_dir().join("alpheus"), generation, tool);
        let template = match tool {
            Tool::Annotate => self.bar.config.captures.annotate.clone(),
            Tool::Ocr => self.bar.config.captures.ocr.clone(),
        };
        let Some(child) = start_tool(self.exec.as_ref(), tool, &template, &files) else {
            log("capture: tool could not start");
            let step = self.capture.flow.tool_ended(generation, ToolEnd::Failed);
            return self.capture_step(step);
        };
        let Some(watch) = self.capture_watch(child.exit_fd(), Child::Tool(generation)) else {
            log("capture: tool could not be watched");
            drop(child);
            files.remove();
            let step = self.capture.flow.tool_ended(generation, ToolEnd::Failed);
            return self.capture_step(step);
        };
        self.capture.tool = Some(ToolRun { child, files, tool, generation, watch: Some(watch) });
        log("capture: tool started");
    }

    /// Puts a watched child's pidfd on the event loop: readable once the child has ended, and quiet
    /// until then (it never wakes the loop while the tool or the recorder runs). None without a loop
    /// or a descriptor.
    fn capture_watch(&mut self, fd: Option<OwnedFd>, child: Child) -> Option<RegistrationToken> {
        let (Some(handle), Some(fd)) = (self.loop_handle.clone(), fd) else { return None };
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        handle.insert_source(source, move |_, _, app: &mut App| Ok(app.capture_child_ready(child))).ok()
    }

    /// Takes a child's watch off the loop (a run given up from outside its own callback).
    fn capture_unwatch(&mut self, watch: Option<RegistrationToken>) {
        if let (Some(handle), Some(token)) = (self.loop_handle.as_ref(), watch) {
            handle.remove(token);
        }
    }

    /// A watched pidfd turned readable: the child it is for has ended (its end is read, which reaps
    /// it), and its watch goes with this answer. A pidfd of a child no longer kept is dropped too.
    fn capture_child_ready(&mut self, child: Child) -> PostAction {
        let ended = self.capture_guarded("child", |app| match child {
            Child::Tool(generation) => {
                let Some(run) = app.capture.tool.as_mut().filter(|run| run.generation == generation) else { return true };
                let Some(exit) = run.child.try_exit() else { return false };
                // The loop removes this watch on our answer: it must not be removed again.
                run.watch = None;
                app.capture_tool_ended(exit);
                true
            }
            Child::Recorder => {
                let Some(run) = app.capture.recording.as_mut() else { return true };
                let Some(exit) = run.recording.try_exit() else { return false };
                run.watch = None;
                app.capture_recording_end(exit);
                true
            }
        });
        // A panic was caught and the capture reset: the watch goes, whatever it was for.
        if ended.unwrap_or(true) { PostAction::Remove } else { PostAction::Continue }
    }

    /// The external tool ended with `exit`: what it left decides (`tools::judge`), and the flow says
    /// what follows; the annotated image goes to the worker, which copies it and removes the file.
    fn capture_tool_ended(&mut self, exit: Option<i32>) {
        let Some(run) = self.capture.tool.take() else { return };
        let end = judge(run.tool, exit, &run.files);
        match &end {
            ToolEnd::Annotated => log("capture: tool wrote an image"),
            ToolEnd::Text(_) => log("capture: tool printed text"),
            ToolEnd::Cancelled => log("capture: tool closed with nothing"),
            ToolEnd::Failed => log("capture: tool failed"),
        }
        match self.capture.flow.tool_ended(run.generation, end) {
            Step::CopyAnnotated => self.capture_deliver(Work::Annotated(run.files.output.clone())),
            step => {
                // Whatever is left of the run (an image nobody will copy) goes.
                run.files.remove();
                self.capture_step(step);
            }
        }
    }

    /// Starts the recorder on the capture's output: the area (logical pixels), or the whole output,
    /// as `x,y wxh` in the compositor's layout, into a free dated name of the videos folder. The
    /// recorder is a watched child: `stop` interrupts it, its pidfd says when it ended. While it
    /// runs the privacy screen glyph is lit and the shutdown waits for it.
    fn capture_record(&mut self, area: Option<Rect>) {
        let output = self.capture.output.take();
        self.capture.area = None;
        let info = output.as_ref().and_then(|o| self.outputs.info(o));
        let origin = info.as_ref().and_then(|i| i.logical_position);
        let size = info.as_ref().and_then(|i| i.logical_size);
        let (Some(origin), Some((width, height))) = (origin, size) else {
            log("capture: recording could not start (no output)");
            return self.capture_recording_notify(Finish::RecordFailed);
        };
        let whole = Rect { x: 0, y: 0, width: width.max(0) as u32, height: height.max(0) as u32 };
        let geometry = geometry(origin, area.unwrap_or(whole));
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        let Some(dir) = videos_dir(user_folder("XDG_VIDEOS_DIR").as_deref(), home.as_deref()) else {
            log("capture: recording could not start (no folder)");
            return self.capture_recording_notify(Finish::RecordFailed);
        };
        let name = video_name(&LocalTime::now().0);
        let template = self.bar.config.captures.record.clone();
        let Some(recording) = Recording::start(self.exec.as_ref(), &template, &geometry, &dir, &name) else {
            log("capture: recording could not start");
            return self.capture_recording_notify(Finish::RecordFailed);
        };
        let Some(watch) = self.capture_watch(recording.exit_fd(), Child::Recorder) else {
            log("capture: recording could not be watched");
            // Dropped, the recorder is killed: what it wrote so far is judged as any ending.
            let finish = recording.finish(None);
            return self.capture_recording_notify(finish);
        };
        self.capture.recording = Some(RecordRun { recording, watch: Some(watch) });
        LIVE.store(true, Ordering::SeqCst);
        self.capture_privacy(true);
        log("capture: recording started");
    }

    /// `--capture stop` or the Stop row: SIGINT to the recorder alone; its end comes back through its
    /// pidfd. False when nothing records.
    fn capture_stop(&mut self) -> bool {
        let Some(run) = self.capture.recording.as_mut() else { return false };
        run.recording.stop();
        log("capture: stopping the recording");
        true
    }

    /// The recorder ended with `exit`: the recording is judged by its file, the privacy glyph goes
    /// off, the notice says how it ended.
    fn capture_recording_end(&mut self, exit: Option<i32>) {
        let Some(run) = self.capture.recording.take() else { return };
        let finish = run.recording.finish(exit);
        self.capture_recording_notify(finish);
    }

    /// The recording is over (or never started): not live any more, one log word, the notice.
    fn capture_recording_notify(&mut self, finish: Finish) {
        LIVE.store(false, Ordering::SeqCst);
        self.capture_privacy(false);
        match &finish {
            Finish::Recorded(_) => log("capture: recording saved"),
            _ => log("capture: recording failed"),
        }
        if let Some(n) = notice(&finish, RECORD_JOB) {
            self.capture_notify(n);
        }
    }

    /// The privacy indicator's screen glyph follows alpheus's own recording. Its end changes nothing
    /// shown at once (the glyph stays for the off-delay) but sets a pending off: the timer follows it
    /// all the same, or the glyph would stay lit until the next message.
    fn capture_privacy(&mut self, recording: bool) {
        if self.bar.privacy.set_recording(recording, Instant::now()) {
            self.privacy_changed();
        } else {
            self.privacy_timer_sync();
        }
    }

    /// The daemon stops (`startup.rs`, before the executor's commands are killed): a tool still open
    /// is closed with its files; a recording gets SIGINT and at most `STOP_LIMIT` to finish its
    /// file, then its notice (docs/decisions.md, "Recording is the external tool's, driven and
    /// stopped by alpheus").
    pub(crate) fn capture_shutdown(&mut self) {
        self.capture_guarded("shutdown", App::capture_end_all);
    }

    /// `capture_shutdown`'s work, guarded there: a panic in it must not turn a stop into a crash.
    fn capture_end_all(&mut self) {
        if let Some(run) = self.capture.tool.take() {
            self.capture_unwatch(run.watch);
            drop(run.child);
            run.files.remove();
            log("capture: tool closed (shutdown)");
        }
        if let Some(run) = self.capture.recording.take() {
            self.capture_unwatch(run.watch);
            let finish = run.recording.end_at_shutdown(STOP_LIMIT);
            self.capture_recording_notify(finish);
        }
    }

    /// The notice, through alpheus's own server only while it serves: app `alpheus`, critical for a
    /// failure. Under do-not-disturb it goes to the history like every notice.
    fn capture_notify(&mut self, n: super::Notice) {
        if !self.notifications().is_some_and(|s| s.held()) {
            log("capture: no notification (not serving)");
            return;
        }
        let incoming = Incoming {
            app_name: "alpheus".into(),
            replaces_id: 0,
            summary: n.summary.into(),
            body: n.body,
            actions: Vec::new(),
            urgency: if n.critical { Urgency::Critical } else { Urgency::Normal },
            expire_ms: -1,
            desktop_entry: None,
            category: None,
            transient: false,
            resident: false,
            tag: None,
        };
        self.notify_act(move |s| {
            s.act(|c| {
                c.notify(incoming, Instant::now(), minute_now());
                c.take_effects()
            });
        });
    }
}

/// One of the user's folders, `key` (`XDG_PICTURES_DIR`, `XDG_VIDEOS_DIR`): the variable when
/// absolute, else its entry of `user-dirs.dirs` (read, at most 64 KiB, at each use: nothing is kept).
fn user_folder(key: &str) -> Option<PathBuf> {
    let absolute = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(p) = absolute(key) {
        return Some(p);
    }
    let home = absolute("HOME")?;
    let config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    let mut text = String::new();
    std::fs::File::open(config.join("user-dirs.dirs")).ok()?.take(64 * 1024).read_to_string(&mut text).ok()?;
    user_dir(&text, key, &home)
}
