//! The clipboard history on `App` (clipboard spec): the control, the history, the read deadline
//! and the episode flags. Logs fixed words only: never a text, a length, a type or a count.

use crate::clipboard::control::{self, Control, Offer, Reading, is_current, read_once};
use crate::clipboard::history::History;
use crate::clipboard::offer::{READ_DEADLINE, Step, Verdict, in_quiet_window, verdict};
use crate::deadline::DeadlineTimer;
use crate::log::write as log;
use crate::wayland::App;
use smithay_client_toolkit::reexports::calloop::PostAction;
use smithay_client_toolkit::reexports::client::QueueHandle;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use std::io::{ErrorKind, Read};
use std::time::Instant;

pub(crate) struct ClipboardApp {
    pub(crate) control: Control,
    pub(crate) history: History,
    /// The read deadline, armed only while a read is pending.
    timer: DeadlineTimer,
    /// Which protocol was bound: the start line's.
    protocol: &'static str,
    timed_out_logged: bool,
    failed_logged: bool,
    /// An alpheus `--password` prompt is open, or closed at `password_closed_at` (wired later).
    password_open: bool,
    password_closed_at: Option<Instant>,
}

/// The read's deadline, if a read started at `started`.
pub(crate) fn read_deadline(started: Option<Instant>) -> Option<Instant> {
    started.map(|t| t + READ_DEADLINE)
}

impl ClipboardApp {
    /// The manager bound, or None (no data-control protocol).
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<ClipboardApp> {
        let (control, protocol) = control::bind(globals, qh)?;
        Some(ClipboardApp {
            control,
            history: History::default(),
            timer: DeadlineTimer::new(),
            protocol,
            timed_out_logged: false,
            failed_logged: false,
            password_open: false,
            password_closed_at: None,
        })
    }
}

impl App {
    /// The start line: the protocol bound, or why there is no history.
    pub(crate) fn clipboard_start_line(&self) {
        match self.clipboard.as_ref() {
            Some(c) if c.protocol == control::EXT_NAME => log("clipboard: ext-data-control-v1"),
            Some(_) => log("clipboard: wlr-data-control-unstable-v1"),
            None if self.bar.config.clipboard.enabled => log("clipboard: the compositor offers no data-control protocol; no history"),
            None => {}
        }
    }

    /// Runs a clipboard entry point; a panic is logged as a fixed word (never its message, which
    /// can embed a copy), any pending read dropped, the bar goes on.
    pub(crate) fn clipboard_guarded<R>(&mut self, what: &str, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self))) {
            Ok(r) => Some(r),
            Err(_) => {
                log(&format!("clipboard error in {what}"));
                self.clipboard_stop_read();
                self.clipboard_timer_sync();
                None
            }
        }
    }

    /// A new selection (or none): a newer one drops the previous read, then its offer is read or destroyed.
    pub(crate) fn clipboard_selection(&mut self, offer: Option<Offer>) {
        let mut offer = offer;
        self.clipboard_guarded("selection", |app| app.clipboard_selection_inner(&mut offer));
        // A panic left the offer unconsumed: it is destroyed all the same.
        if let Some(offer) = offer {
            offer.destroy();
        }
    }

    /// Takes the offer out of `slot` once it is consumed (read, or destroyed).
    fn clipboard_selection_inner(&mut self, slot: &mut Option<Offer>) {
        // No selection only clears the current one: a read in flight is kept (an app that copies
        // then quits sends it right after), and the deadline bounds it. Only a newer one drops it.
        let Some(offer) = slot.take() else { return };
        self.clipboard_stop_read();
        let Some(c) = self.clipboard.as_ref() else {
            return offer.destroy();
        };
        let quiet = in_quiet_window(c.password_open, c.password_closed_at, Instant::now());
        let types = offer.mime_types();
        match verdict(types.iter().map(String::as_str), quiet) {
            Verdict::Read(mime) => control::start_read(self, offer, mime),
            Verdict::Skip(_) => offer.destroy(),
        }
        self.clipboard_timer_sync();
    }

    /// One wake-up of the pipe: more data, or the end of the read.
    pub(crate) fn clipboard_read_wake(&mut self, generation: u64, file: &mut impl Read) -> PostAction {
        let current = self.clipboard.as_ref().and_then(|c| c.control.reading.get()).map(|r| r.generation);
        // A source that is no longer the pending read never touches its bytes.
        let Some(reading) = self.clipboard.as_mut().and_then(|c| c.control.reading.get_mut()).filter(|_| is_current(current, generation))
        else {
            return PostAction::Remove;
        };
        match read_once(file, &mut reading.buffer) {
            Step::Continue => PostAction::Continue,
            Step::Stop => {
                self.clipboard_finish_read(Ok(()));
                PostAction::Remove
            }
            Step::Failed(kind) => {
                self.clipboard_finish_read(Err(kind));
                PostAction::Remove
            }
        }
    }

    /// The read ended by itself (the loop removes its source on return).
    fn clipboard_finish_read(&mut self, outcome: Result<(), ErrorKind>) {
        let Some(reading) = self.clipboard.as_mut().and_then(|c| c.control.reading.take()) else { return };
        let Reading { buffer, .. } = reading;
        let text = match outcome {
            Ok(()) => buffer.finish(),
            Err(_) => None,
        };
        self.clipboard_read_done(outcome.map(|()| text));
    }

    /// A read's result: the text goes into the history; an error is logged once per episode.
    pub(crate) fn clipboard_read_done(&mut self, outcome: Result<Option<String>, ErrorKind>) {
        if let Some(c) = self.clipboard.as_mut() {
            match outcome {
                Ok(text) => {
                    c.timed_out_logged = false;
                    c.failed_logged = false;
                    if let Some(text) = text {
                        c.history.push(text);
                    }
                }
                Err(kind) => {
                    if !c.failed_logged {
                        c.failed_logged = true;
                        log(&format!("clipboard: a read failed ({kind:?})"));
                    }
                }
            }
        }
        self.clipboard_timer_sync();
    }

    /// Drops the pending read: its source leaves the loop and its bytes are zeroed.
    pub(crate) fn clipboard_stop_read(&mut self) {
        let Some(reading) = self.clipboard.as_mut().and_then(|c| c.control.reading.take()) else { return };
        if let Some(handle) = self.loop_handle.as_ref() {
            handle.remove(reading.token);
        }
    }

    /// The deadline exists only while a read is pending.
    pub(crate) fn clipboard_timer_sync(&mut self) {
        let Some(c) = self.clipboard.as_mut() else { return };
        let deadline = read_deadline(c.control.reading.get().map(|r| r.started));
        c.timer.sync_opt(self.loop_handle.as_ref(), deadline, App::clipboard_deadline);
    }

    fn clipboard_deadline(app: &mut App, _now: Instant) {
        if let Some(c) = app.clipboard.as_mut() {
            c.timer.fired();
        }
        app.clipboard_guarded("deadline", |app| {
            app.clipboard_stop_read();
            if let Some(c) = app.clipboard.as_mut() {
                if !c.timed_out_logged {
                    c.timed_out_logged = true;
                    log("clipboard: a read timed out");
                }
            }
            app.clipboard_timer_sync();
        });
    }

    /// The tracked seat appeared, or went: the device follows it.
    pub(crate) fn clipboard_seat(&mut self, qh: &QueueHandle<App>) {
        let seat = self.seat.clone();
        let Some(c) = self.clipboard.as_mut() else { return };
        match seat {
            Some(seat) => c.control.attach(&seat, qh),
            None => {
                c.control.detach();
                self.clipboard_stop_read();
                self.clipboard_timer_sync();
            }
        }
    }

    /// The compositor finished the device: one line, a later seat event makes a new one.
    pub(crate) fn clipboard_device_finished(&mut self) {
        self.clipboard_stop_read();
        if let Some(c) = self.clipboard.as_mut() {
            c.control.detach();
            log("clipboard: the data device finished");
        }
        self.clipboard_timer_sync();
    }

    /// The daemon stops: the read goes and the history is wiped before the app is dropped.
    pub(crate) fn clipboard_shutdown(&mut self) {
        self.clipboard_stop_read();
        self.clipboard_timer_sync();
        if let Some(c) = self.clipboard.as_mut() {
            // Everything, pins included.
            c.history = History::default();
            c.control.detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::offer::ReadBuffer;
    use smithay_client_toolkit::reexports::calloop::generic::Generic;
    use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, Mode};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    struct ReadTestState {
        buffer: ReadBuffer,
        step: Option<Step>,
        wakes: u32,
    }

    #[test]
    fn the_deadline_exists_only_while_reading() {
        let t = Instant::now();
        assert_eq!(read_deadline(None), None);
        assert_eq!(read_deadline(Some(t)), Some(t + READ_DEADLINE));
    }

    #[test]
    fn a_read_never_blocks_and_stops_at_the_cap() {
        let (reader, mut writer) = UnixStream::pair().expect("pair");
        reader.set_nonblocking(true).expect("nonblocking");
        let mut event_loop: EventLoop<ReadTestState> = EventLoop::try_new().expect("loop");
        let source = Generic::new(reader, Interest::READ, Mode::Level);
        event_loop
            .handle()
            .insert_source(source, |_, reader, state: &mut ReadTestState| {
                let mut stream: &UnixStream = reader.as_ref();
                state.wakes += 1;
                let step = read_once(&mut stream, &mut state.buffer);
                let stop = step != Step::Continue;
                state.step = Some(step);
                Ok(if stop { PostAction::Remove } else { PostAction::Continue })
            })
            .expect("insert");
        let mut state = ReadTestState { buffer: ReadBuffer::default(), step: None, wakes: 0 };

        // Nothing written yet: a dispatch returns at once instead of blocking.
        event_loop.dispatch(Duration::from_millis(0), &mut state).expect("loop turn");
        assert_eq!(state.wakes, 0);

        writer.write_all(&[b'a'; 2500]).expect("first half");
        event_loop.dispatch(Duration::from_millis(100), &mut state).expect("loop turn");
        assert_eq!(state.step, Some(Step::Continue));
        writer.write_all(&[b'a'; 2500]).expect("second half");
        for _ in 0..4 {
            event_loop.dispatch(Duration::from_millis(100), &mut state).expect("loop turn");
            if state.step == Some(Step::Stop) {
                break;
            }
        }
        assert_eq!(state.step, Some(Step::Stop));
        assert!(state.buffer.over());
        assert_eq!(std::mem::take(&mut state.buffer).finish(), None, "nothing stored");
    }

    #[test]
    fn a_short_text_then_the_writer_closing_is_stored() {
        let (reader, mut writer) = UnixStream::pair().expect("pair");
        let mut buffer = ReadBuffer::default();
        writer.write_all(b"alpha").expect("write");
        let mut stream: &UnixStream = &reader;
        assert_eq!(read_once(&mut stream, &mut buffer), Step::Continue);
        drop(writer);
        assert_eq!(read_once(&mut stream, &mut buffer), Step::Stop);
        assert_eq!(buffer.finish().as_deref(), Some("alpha"));
    }
}
