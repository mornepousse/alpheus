//! The clipboard history on `App` (clipboard spec): the control, the history, the read deadline
//! and the episode flags. Logs fixed words only: never a text, a length, a type or a count.

use crate::clipboard::control::{self, Control, Offer, Reading, is_current, read_once};
use crate::clipboard::history::History;
use crate::clipboard::offer::{PasswordQuiet, READ_DEADLINE, Step, Verdict, verdict};
use crate::deadline::DeadlineTimer;
use crate::launcher::model::ClipEdit;
use crate::log::write as log;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{Reply, Request};
use smithay_client_toolkit::reexports::calloop::PostAction;
use smithay_client_toolkit::reexports::client::QueueHandle;
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use std::io::{ErrorKind, Read};
use std::os::fd::OwnedFd;
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
    /// The device was re-made once after a `finished` with no seat event since (see `refinish`).
    refinished: bool,
    /// Nothing is stored while an alpheus `--password` prompt is open, or for 5 s after it.
    quiet: PasswordQuiet,
}

/// A `finished` device is made again once, then only after a seat event: a compositor that
/// finishes each new device cannot loop us. True when this `finished` may re-attach.
pub(crate) fn refinish(refinished: &mut bool) -> bool {
    !std::mem::replace(refinished, true)
}

/// A read stored a text: the error episodes end. A refused read (oversize, blank, not UTF-8)
/// proves nothing about the pipe, so it leaves them standing.
pub(crate) fn episodes_end(stored: bool, timed_out_logged: &mut bool, failed_logged: &mut bool) {
    if stored {
        *timed_out_logged = false;
        *failed_logged = false;
    }
}

/// The read's deadline, if a read started at `started`.
pub(crate) fn read_deadline(started: Option<Instant>) -> Option<Instant> {
    started.map(|t| t + READ_DEADLINE)
}

/// Why a verb cannot be served: the history is off (`[clipboard] enabled`), or on without a
/// data-control protocol. Nothing else tells the two apart: no extra field.
pub(crate) fn availability(enabled: bool, bound: bool) -> Result<(), &'static str> {
    match (enabled, bound) {
        (false, _) => Err("the clipboard history is off ([clipboard] enabled in bar.toml)"),
        (true, false) => Err("the compositor offers no clipboard protocol (data-control)"),
        (true, true) => Ok(()),
    }
}

/// The launcher's edit applied to the real history: ids only. Whether the history was cleared.
fn apply_edit(history: &mut History, edit: &ClipEdit) -> bool {
    match edit {
        ClipEdit::Pin(id) => {
            // The model refused an eleventh pin itself; a row gone meanwhile changes nothing.
            let _ = history.toggle_pin(*id);
            false
        }
        ClipEdit::Delete(id) => {
            history.delete(*id);
            false
        }
        ClipEdit::Clear => {
            history.clear();
            true
        }
    }
}

pub(crate) const NOT_AVAILABLE: &str = "the clipboard is not available";

/// Enter on entry `id`: `own` makes its text the selection; only a copy that succeeded moves the
/// entry to the front (a failed one must not reorder the history).
fn copy_entry(history: &mut History, id: u64, own: impl FnOnce(&str) -> bool) -> Result<(), String> {
    let Some(text) = history.text_of(id) else { return Err("the entry is gone".into()) };
    if !own(text) {
        return Err(NOT_AVAILABLE.into());
    }
    history.touch(id);
    Ok(())
}

/// The answer to a verb, and whether the history was cleared (the caller logs it, one fixed line).
/// `history` is None when nothing is bound.
fn verb_reply(enabled: bool, history: Option<&mut History>, request: &Request) -> (Reply, bool) {
    if let Err(why) = availability(enabled, history.is_some()) {
        return (Reply::Error(why.into()), false);
    }
    match (request, history) {
        (Request::ClipboardClear, Some(history)) => {
            history.clear();
            (Reply::Done, true)
        }
        _ => (Reply::Error("not a clipboard request".into()), false),
    }
}

/// Destroys what it holds when dropped, unless it was handed on: a panic between the moment an
/// offer is taken and the moment it is read or skipped still destroys the offer.
pub(crate) trait Destroy {
    fn destroy(&self);
}

impl Destroy for Offer {
    fn destroy(&self) {
        Offer::destroy(self);
    }
}

pub(crate) struct DestroyOnDrop<T: Destroy>(Option<T>);

impl<T: Destroy> DestroyOnDrop<T> {
    pub(crate) fn new(value: T) -> Self {
        DestroyOnDrop(Some(value))
    }

    /// Hands the value on: it is no longer destroyed here.
    pub(crate) fn consume(&mut self) -> Option<T> {
        self.0.take()
    }
}

impl<T: Destroy> Drop for DestroyOnDrop<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            value.destroy();
        }
    }
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
            refinished: false,
            quiet: PasswordQuiet::default(),
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
        // From here a panic still destroys the offer (the guard), unless it was handed on.
        let mut guard = DestroyOnDrop::new(offer);
        self.clipboard_stop_read();
        let Some(c) = self.clipboard.as_ref() else { return };
        let quiet = c.quiet.active(Instant::now());
        let types = guard.0.as_ref().map(Offer::mime_types).unwrap_or_default();
        if let Verdict::Read(mime) = verdict(types.iter().map(String::as_str), quiet) {
            if let Some(offer) = guard.consume() {
                control::start_read(self, offer, mime);
            }
        }
        // A skipped offer is destroyed as the guard goes.
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
                    episodes_end(text.is_some(), &mut c.timed_out_logged, &mut c.failed_logged);
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

    /// Enter on a history entry: it goes to the front and becomes the clipboard again, from the
    /// daemon's own source (its marker keeps the copy from being read back).
    pub(crate) fn clipboard_copy(&mut self, qh: &QueueHandle<App>, id: u64) -> Result<(), String> {
        let Some(c) = self.clipboard.as_mut() else { return Err(NOT_AVAILABLE.into()) };
        copy_entry(&mut c.history, id, |text| c.control.own_selection(text, qh))
    }

    /// The launcher's pin, delete or clear, applied to the history (ids only, nothing logged but a
    /// fixed word for a clear).
    pub(crate) fn clipboard_edit(&mut self, edit: &ClipEdit) {
        let Some(c) = self.clipboard.as_mut() else { return };
        if apply_edit(&mut c.history, edit) {
            log("clipboard: history cleared (row)");
            // The cleared entries' pages go back to the system (glibc keeps freed ones otherwise).
            crate::memory::release_memory();
        }
    }

    /// An alpheus `--password` prompt opened: nothing is stored until 5 s after it closes.
    pub(crate) fn clipboard_password_opened(&mut self) {
        if let Some(c) = self.clipboard.as_mut() {
            c.quiet.opened();
        }
    }

    pub(crate) fn clipboard_password_closed(&mut self) {
        if let Some(c) = self.clipboard.as_mut() {
            c.quiet.closed(Instant::now());
        }
    }

    /// After a panic the model is never read again: close the quiet state if any prompt was open.
    pub(crate) fn clipboard_password_closed_if_open(&mut self) {
        if let Some(c) = self.clipboard.as_mut() {
            c.quiet.closed_if_open(Instant::now());
        }
    }

    /// The history's rows for a launcher open: a snapshot (previews only).
    pub(crate) fn clipboard_rows(&self) -> Vec<crate::clipboard::history::ClipRow> {
        self.clipboard.as_ref().map(|c| c.history.rows()).unwrap_or_default()
    }

    /// Why the verbs and the launcher's `"` rows cannot be served (the history off, or no protocol).
    pub(crate) fn clipboard_availability(&self) -> Result<(), &'static str> {
        availability(self.bar.config.clipboard.enabled, self.clipboard.is_some())
    }

    /// `alpheus-launch --clipboard-clear`, answered before the launcher's guard: off or unbound is
    /// an error, else the unpinned entries go.
    pub(crate) fn clipboard_verb(&mut self, request: &Request) -> Reply {
        let enabled = self.bar.config.clipboard.enabled;
        let (reply, cleared) = verb_reply(enabled, self.clipboard.as_mut().map(|c| &mut c.history), request);
        if cleared {
            log("clipboard: history cleared (verb)");
            crate::memory::release_memory();
        }
        reply
    }

    /// A paste of the own source: its text goes out on `fd` (closed on return, or at once when the
    /// source is not the current own one).
    pub(crate) fn clipboard_source_send(&mut self, id: &ObjectId, mime: &str, fd: OwnedFd) {
        if let Some(own) = self.clipboard.as_ref().and_then(|c| c.control.own.as_ref()).filter(|o| o.is(id)) {
            own.send(mime, fd);
        }
    }

    /// The compositor replaced our selection: the own source is destroyed and its text wiped.
    pub(crate) fn clipboard_source_cancelled(&mut self, id: &ObjectId) {
        if let Some(c) = self.clipboard.as_mut() {
            if c.control.own.as_ref().is_some_and(|o| o.is(id)) {
                c.control.own = None;
            }
        }
    }

    /// The tracked seat appeared, or went: the device follows it.
    pub(crate) fn clipboard_seat(&mut self, qh: &QueueHandle<App>) {
        let seat = self.seat.clone();
        let Some(c) = self.clipboard.as_mut() else { return };
        c.refinished = false;
        match seat {
            Some(seat) => c.control.attach(&seat, qh),
            None => {
                c.control.detach();
                self.clipboard_stop_read();
                self.clipboard_timer_sync();
            }
        }
    }

    /// At start: a seat that exists already gets the device even if it has no capability yet (a
    /// compositor with no input device announces none), as data-control needs no focus nor
    /// capability. `new_seat` and `new_capability` follow it later as before.
    pub(crate) fn clipboard_attach_at_start(&mut self) {
        self.clipboard_attach_any(None);
    }

    /// Attaches to the tracked seat, else to any seat but `gone`, recording it as `new_seat`
    /// does (`remove_seat` compares against `self.seat`). Nothing happens with the history off.
    fn clipboard_attach_any(&mut self, gone: Option<&WlSeat>) {
        if self.clipboard.is_none() {
            return;
        }
        let Some(seat) = self.seat.clone().or_else(|| self.seats.seats().find(|s| Some(s) != gone)) else { return };
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
        }
        let qh = self.qh.clone();
        let Some(c) = self.clipboard.as_mut() else { return };
        c.control.attach(&seat, &qh);
    }

    /// The tracked seat `gone` was removed: the device follows another seat if one is left.
    pub(crate) fn clipboard_seat_removed(&mut self, gone: &WlSeat) {
        self.clipboard_attach_any(Some(gone));
    }

    /// The compositor finished the device: one line, then one new device (not a loop if it keeps
    /// finishing: the next one waits for a seat event).
    pub(crate) fn clipboard_device_finished(&mut self) {
        self.clipboard_stop_read();
        let mut again = false;
        if let Some(c) = self.clipboard.as_mut() {
            c.control.detach();
            log("clipboard: the data device finished");
            again = refinish(&mut c.refinished);
        }
        if again {
            self.clipboard_attach_any(None);
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
    fn a_finished_device_is_remade_once_until_a_seat_event() {
        let mut refinished = false;
        assert!(refinish(&mut refinished), "the first finish re-attaches");
        assert!(!refinish(&mut refinished), "a second one with no seat event in between does not");
        refinished = false; // what `clipboard_seat` does
        assert!(refinish(&mut refinished));
    }

    #[test]
    fn a_refused_read_does_not_end_the_error_episodes() {
        let (mut t, mut f) = (true, true);
        episodes_end(false, &mut t, &mut f);
        assert!(t && f, "a blank or oversize text proves nothing");
        episodes_end(true, &mut t, &mut f);
        assert!(!t && !f);
    }

    #[test]
    fn own_offer_is_never_read() {
        use crate::clipboard::offer::{Skip, own_offer_types};
        let types: Vec<&str> = own_offer_types().collect();
        assert_eq!(verdict(types.iter().copied(), false), Verdict::Skip(Skip::Own));
        assert_eq!(types.last().copied(), Some(crate::clipboard::offer::MARKER), "the marker is offered last");
        assert!(types.contains(&"text/plain;charset=utf-8"), "the text types are offered");
    }

    #[test]
    fn clipboard_edit_applies_pin_delete_clear() {
        use crate::clipboard::history::Pushed;
        let mut h = History::default();
        let id = |p: Pushed| match p {
            Pushed::Added(id) => id,
            other => panic!("{other:?}"),
        };
        let a = id(h.push("one".into()));
        let b = id(h.push("two".into()));
        let c = id(h.push("three".into()));
        assert!(!apply_edit(&mut h, &ClipEdit::Pin(a)));
        assert!(h.rows().iter().find(|r| r.id == a).unwrap().pinned, "pinned");
        assert!(!apply_edit(&mut h, &ClipEdit::Delete(b)));
        assert_eq!(h.text_of(b), None, "deleted");
        assert!(apply_edit(&mut h, &ClipEdit::Clear), "a clear says so");
        assert_eq!(h.text_of(c), None, "the unpinned one went");
        assert_eq!(h.text_of(a), Some("one"), "the pin stays");
        assert!(!apply_edit(&mut h, &ClipEdit::Delete(999)), "a row already gone changes nothing");
        assert!(!apply_edit(&mut h, &ClipEdit::Pin(999)));
        assert!(!apply_edit(&mut h, &ClipEdit::Pin(a)));
        assert!(!h.rows()[0].pinned, "the second Pin unpins");
    }

    #[test]
    fn a_failed_copy_does_not_reorder_the_history() {
        use crate::clipboard::history::Pushed;
        let mut h = History::default();
        let Pushed::Added(old) = h.push("old".into()) else { panic!() };
        h.push("new".into());
        let order = |h: &History| h.rows().into_iter().map(|r| r.id).collect::<Vec<_>>();
        let before = order(&h);
        assert_eq!(copy_entry(&mut h, old, |_| false), Err(NOT_AVAILABLE.to_string()));
        assert_eq!(order(&h), before, "a failed copy leaves the order alone");
        let mut seen = String::new();
        assert_eq!(
            copy_entry(&mut h, old, |t| {
                seen = t.into();
                true
            }),
            Ok(())
        );
        assert_eq!(seen, "old");
        assert_eq!(order(&h)[0], old, "a successful one moves it to the front");
        assert_eq!(copy_entry(&mut h, 999, |_| panic!("no text, no copy")), Err("the entry is gone".to_string()));
    }

    #[test]
    fn verbs_answer_by_availability() {
        let clear = Request::ClipboardClear;
        let off = "the clipboard history is off ([clipboard] enabled in bar.toml)";
        assert_eq!(verb_reply(false, None, &clear), (Reply::Error(off.into()), false));
        let mut history = History::default();
        assert_eq!(verb_reply(false, Some(&mut history), &clear).0, Reply::Error(off.into()), "off wins");
        let no_protocol = "the compositor offers no clipboard protocol (data-control)";
        assert_eq!(verb_reply(true, None, &clear), (Reply::Error(no_protocol.into()), false));

        history.push("one".into());
        history.push("two".into());
        let id = match history.push("kept".into()) {
            crate::clipboard::history::Pushed::Added(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(history.toggle_pin(id), Ok(true));
        assert_eq!(verb_reply(true, Some(&mut history), &clear), (Reply::Done, true));
        assert_eq!(history.len(), 1, "the pinned entry stays");
        assert_eq!(history.text_of(id), Some("kept"));
    }

    struct Counting(std::rc::Rc<std::cell::Cell<u32>>);

    impl Destroy for Counting {
        fn destroy(&self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn a_guarded_offer_is_destroyed_unless_consumed() {
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        drop(DestroyOnDrop::new(Counting(count.clone())));
        assert_eq!(count.get(), 1, "dropped: destroyed");
        let mut guard = DestroyOnDrop::new(Counting(count.clone()));
        let handed = guard.consume();
        drop(guard);
        assert_eq!(count.get(), 1, "consumed: left to its new owner");
        drop(handed);
    }

    #[test]
    fn a_panic_destroys_the_guarded_offer() {
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let guard = DestroyOnDrop::new(Counting(count.clone()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = guard;
            std::panic::resume_unwind(Box::new("boom"));
        }));
        assert!(result.is_err());
        assert_eq!(count.get(), 1, "unwinding drops the guard");
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
