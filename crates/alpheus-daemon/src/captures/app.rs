//! The captures on the daemon's state: the verbs, the flow's steps, the frame, the worker thread
//! that encodes and delivers it, and the notice.
//!
//! This is the wiring half (the pure half is `mod.rs`'s `Flow`). A verb reaches `capture_verb`,
//! which asks the plan what it wants: a refusal is the reply (exit 2), a capture is `Flow::begin`.
//! The steps the flow answers with are carried out here: open the selector (`select_app.rs`), wait
//! for the compositor to have removed it (a `wl_display.sync`, answered by `Settled`), ask one frame
//! of the focused output (`screencopy.rs`), hand the frame to the short-lived `alpheus-capture`
//! thread (`deliver.rs`), and show the notice of its ending. The frame and the worker's result come
//! back through the loop's mail (`Mail::Capture`), so the flow only ever moves on the loop's thread.
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
//!   its answer (`the_frame_deadline_is_armed_only_while_a_frame_is_awaited`).
//!
//! See: docs/features/captures.md, docs/decisions.md ("A capture never touches the clipboard unless
//! it succeeded", "A capture waits for the selector's removal", "The capture's PNG is encoded on a
//! short-lived thread").

use super::deliver::{Delivery, run};
use super::paths::{file_name, save_dir, user_dir};
use super::screencopy::{CaptureDone, CaptureError, Frame, PendingFrame, buffer_area};
use super::select_app::guarded;
use super::{Finish, Flow, GRAB_LIMIT, Job, Plan, Rect, SelectorAnswer, Step, notice, plan};
use crate::clock::LocalTime;
use crate::deadline::DeadlineTimer;
use crate::launcher::host::read_config;
use crate::log::write as log;
use crate::notify::{Incoming, Urgency};
use crate::session_bus::notifications::minute_now;
use crate::startup::Mail;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{Reply, Request};
use alpheus_common::paths;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use std::io::Read;
use std::path::PathBuf;
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
}

impl CaptureApp {
    pub(crate) fn new() -> CaptureApp {
        CaptureApp { flow: Flow::default(), output: None, area: None, pending: None, due: None, timer: DeadlineTimer::new() }
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
        match plan(*verb, self.screencopy.is_some()) {
            Plan::Refuse(why) => {
                log("capture: refused");
                Reply::Error(why.text().into())
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

    /// Back to rest: nothing of the capture is kept, the frame asked is given up, the timer disarmed.
    fn capture_reset(&mut self) {
        self.capture.flow.finished();
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
                self.select_open(&qh, true);
            }
            Step::Settle => self.capture_settle(),
            Step::Grab(area) => self.capture_grab(area),
            Step::Encode | Step::Nothing => {}
            Step::Finish(finish) => self.capture_finish(finish),
        }
        self.capture_timer_sync();
    }

    /// The selector answered (`select_app.rs`, once its surface is destroyed): the flow decides what
    /// that means, and a selector nobody here asked for (the debug verb) means nothing to it.
    pub(crate) fn capture_selected(&mut self, output: Option<WlOutput>, answer: SelectorAnswer) {
        if self.capture.flow.is_busy() {
            self.capture.output = output;
            self.capture.area = match answer {
                SelectorAnswer::Area(area) => Some(area),
                SelectorAnswer::Cancelled | SelectorAnswer::Failed => None,
            };
        }
        self.capture_guarded("selected", |app| {
            let step = app.capture.flow.selected(answer);
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

    /// Hands the frame to the short-lived `alpheus-capture` thread, which cuts, encodes, copies or
    /// saves it and mails its ending back. A thread that cannot start ends the capture as failed.
    fn capture_encode(&mut self, frame: Frame) {
        let (Some(job), Some(tx)) = (self.capture.flow.job(), self.mail.clone()) else {
            return self.capture_finish(Finish::CaptureFailed);
        };
        let generation = self.capture.flow.generation();
        let info = self.capture.output.as_ref().and_then(|o| self.outputs.info(o));
        let area = self.capture.area.take().map(|logical| {
            let size = info.as_ref().and_then(|i| i.logical_size).map(|(w, h)| (w.max(0) as u32, h.max(0) as u32));
            let scale = info.as_ref().map_or(1, |i| i.scale_factor);
            let transform = info.as_ref().map_or(Transform::Normal, |i| i.transform);
            buffer_area((frame.width, frame.height), logical, size, scale, transform)
        });
        // The two small files (launcher.toml, user-dirs.dirs) are read here, on the loop, as the
        // launcher reads launcher.toml at each open: read in the worker, a fault in them would come
        // before the job whose panic is caught, and the ending might never be mailed.
        let launcher = read_config(&paths::config_dir(), &mut |_| {});
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        let dir = save_dir(&self.bar.config.captures.dir, pictures_dir().as_deref(), home.as_deref());
        let name = file_name(&LocalTime::now().0);
        let temp_dir = paths::runtime_dir().join("alpheus");
        let exec = self.exec.clone();
        let started = std::thread::Builder::new().name("alpheus-capture".into()).spawn(move || {
            let delivery = Delivery { job, copy: &launcher.copy, save_dir: dir.as_deref(), file_name: name, temp_dir: &temp_dir };
            let finish = run(exec.as_ref(), frame, area, &delivery);
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
            Finish::Saved(_) => log("capture: saved"),
            Finish::CaptureFailed => log("capture: failed"),
            Finish::CopyFailed => log("capture: copy failed"),
            Finish::SaveFailed => log("capture: save failed"),
        }
        if let Some(n) = notice(&finish) {
            self.capture_notify(n);
        }
        crate::memory::release_memory();
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

/// The user's pictures folder: `$XDG_PICTURES_DIR` when absolute, else the entry of
/// `user-dirs.dirs` (read, at most 64 KiB, at each save: nothing is kept).
fn pictures_dir() -> Option<PathBuf> {
    let absolute = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(p) = absolute("XDG_PICTURES_DIR") {
        return Some(p);
    }
    let home = absolute("HOME")?;
    let config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    let mut text = String::new();
    std::fs::File::open(config.join("user-dirs.dirs")).ok()?.take(64 * 1024).read_to_string(&mut text).ok()?;
    user_dir(&text, "XDG_PICTURES_DIR", &home)
}
