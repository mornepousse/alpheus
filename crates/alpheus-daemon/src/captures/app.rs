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
//! - poll or keep a timer: every step is an event of the compositor, the mail or the verb.
//!
//! See: docs/features/captures.md, docs/decisions.md ("A capture never touches the clipboard unless
//! it succeeded", "A capture waits for the selector's removal", "The capture's PNG is encoded on a
//! short-lived thread").

use super::deliver::{Delivery, run};
use super::paths::{file_name, save_dir, user_dir};
use super::screencopy::{CaptureDone, CaptureError, Frame, buffer_area};
use super::select_app::guarded;
use super::{Finish, Flow, Job, Plan, Rect, Step, notice, plan};
use crate::clock::LocalTime;
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
    Frame(Result<Frame, CaptureError>),
    Delivered(Finish),
}

/// Everything the captures keep on the `App`: the flow, and while one runs, its output and area.
#[derive(Default)]
pub(crate) struct CaptureApp {
    flow: Flow,
    /// The output being captured: the focused one at the verb (the selector's own for an area).
    output: Option<WlOutput>,
    /// The area, in the output's logical pixels; none for the whole screen.
    area: Option<Rect>,
}

/// The user data of the round trip that waits for the selector's removal.
pub struct Settled;

impl Dispatch2<WlCallback, App> for Settled {
    fn event(&self, app: &mut App, _: &WlCallback, event: wl_callback::Event, _: &Connection, _: &QueueHandle<App>) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            app.capture_guarded("settled", |app| {
                let step = app.capture.flow.settled();
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
                self.select_toggle();
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
            self.capture.flow.finished();
            self.capture.output = None;
            self.capture.area = None;
        }
        r
    }

    fn capture_begin(&mut self, job: Job) {
        let Some(step) = self.capture.flow.begin(job) else {
            log("capture: busy");
            return;
        };
        self.capture.output = self.focused_output();
        self.capture_step(step);
    }

    /// Carries out what the flow says next.
    fn capture_step(&mut self, step: Step) {
        match step {
            Step::Select => {
                let qh = self.qh.clone();
                self.select_open(&qh);
            }
            Step::Settle => self.capture_settle(),
            Step::Grab(area) => self.capture_grab(area),
            Step::Encode | Step::Nothing => {}
            Step::Finish(finish) => self.capture_finish(finish),
        }
    }

    /// The selector answered (`select_app.rs`, once its surface is destroyed): the flow decides what
    /// that means, and a selector nobody here asked for (the debug verb) means nothing to it.
    pub(crate) fn capture_selected(&mut self, output: Option<WlOutput>, area: Option<Rect>) {
        if self.capture.flow.is_busy() {
            self.capture.output = output;
            self.capture.area = area;
        }
        self.capture_guarded("selected", |app| {
            let step = app.capture.flow.selected(area);
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
                connection.display().sync(&self.qh, Settled);
                log("capture: waiting for the compositor");
            }
            None => {
                let step = self.capture.flow.settled();
                self.capture_step(step);
            }
        }
    }

    /// Asks for the frame of the output; its answer comes back as `Mail::Capture`.
    fn capture_grab(&mut self, area: Option<Rect>) {
        self.capture.area = area;
        let (Some(output), Some(tx)) = (self.capture.output.clone(), self.mail.clone()) else {
            let step = self.capture.flow.grabbed(false);
            return self.capture_step(step);
        };
        let done: CaptureDone = Box::new(move |result| {
            // A closed channel means the loop has gone: nobody is left to hear the frame.
            let _ = tx.send(Mail::Capture(CaptureMail::Frame(result)));
        });
        let qh = self.qh.clone();
        if self.capture_output(&output, &qh, done) {
            log("capture: frame asked");
        } else {
            let step = self.capture.flow.grabbed(false);
            self.capture_step(step);
        }
    }

    /// A capture mail, on the loop's thread.
    pub(crate) fn capture_mail(&mut self, mail: CaptureMail) {
        self.capture_guarded("mail", |app| match mail {
            CaptureMail::Frame(result) => app.capture_frame(result),
            CaptureMail::Delivered(finish) => {
                let step = app.capture.flow.delivered(finish);
                app.capture_step(step);
            }
        });
    }

    /// The compositor's answer: a frame goes to the worker, an error ends the capture.
    fn capture_frame(&mut self, result: Result<Frame, CaptureError>) {
        match result {
            Ok(frame) => match self.capture.flow.grabbed(true) {
                Step::Encode => self.capture_encode(frame),
                step => {
                    // Out of turn: nobody asked for this frame.
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
                let step = self.capture.flow.grabbed(false);
                self.capture_step(step);
            }
        }
    }

    /// Hands the frame to the short-lived `alpheus-capture` thread, which cuts, encodes, copies or
    /// saves it and mails its ending back. A thread that cannot start ends the capture as failed.
    fn capture_encode(&mut self, frame: Frame) {
        let (Some(job), Some(tx)) = (self.capture.flow.job(), self.mail.clone()) else {
            return self.capture_finish(Finish::CaptureFailed);
        };
        let info = self.capture.output.as_ref().and_then(|o| self.outputs.info(o));
        let area = self.capture.area.take().map(|logical| {
            let size = info.as_ref().and_then(|i| i.logical_size).map_or((0, 0), |(w, h)| (w.max(0) as u32, h.max(0) as u32));
            let transform = info.as_ref().map_or(Transform::Normal, |i| i.transform);
            buffer_area((frame.width, frame.height), logical, size, transform)
        });
        let launcher = read_config(&paths::config_dir(), &mut |_| {});
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        let dir = save_dir(&self.bar.config.captures.dir, pictures_dir().as_deref(), home.as_deref());
        let name = file_name(&LocalTime::now().0);
        let temp_dir = paths::runtime_dir().join("alpheus");
        let exec = self.exec.clone();
        let started = std::thread::Builder::new().name("alpheus-capture".into()).spawn(move || {
            let delivery = Delivery { job, copy: &launcher.copy, save_dir: &dir, file_name: name, temp_dir: &temp_dir };
            let finish = run(exec.as_ref(), frame, area, &delivery);
            // The frame and the image went with `run`: the loop frees what the thread's arena kept.
            let _ = tx.send(Mail::Capture(CaptureMail::Delivered(finish)));
        });
        if started.is_err() {
            self.capture_finish(Finish::CaptureFailed);
        }
    }

    /// The capture is over: one log word, the notice, and the pages its frame held given back.
    fn capture_finish(&mut self, finish: Finish) {
        self.capture.flow.finished();
        self.capture.output = None;
        self.capture.area = None;
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
