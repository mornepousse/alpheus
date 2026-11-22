//! The only file that names a data-control protocol (clipboard spec): ext-data-control-v1 first,
//! wlr-data-control-unstable-v1 (version 1) as the fallback, both through one macro. The manager,
//! the device, the offers and the one pending read on the loop. Never logs, and nothing here shows
//! an offer's types or text.

use crate::clipboard::offer::{MARKER, ReadBuffer, SENSITIVE, Step, TEXT_TYPES, own_offer_types, step};
use crate::sys::set_nonblocking;
use crate::wayland::App;
use alpheus_common::launcher_ipc::wipe;
use smithay_client_toolkit::data_device_manager::ReadPipe;
use smithay_client_toolkit::reexports::calloop::{PostAction, RegistrationToken};
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use smithay_client_toolkit::reexports::protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::{self, ExtDataControlManagerV1},
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use smithay_client_toolkit::reexports::protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::{self, ZwlrDataControlManagerV1},
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Instant;

/// An offer announcing more types than this has the rest ignored.
const MAX_TYPES: usize = 64;

/// The types an offer announced so far, kept on its proxy as they arrive. No `Debug`: a type list
/// is never printed.
#[derive(Default)]
pub(crate) struct OfferData(Mutex<Announced>);

/// The kept types (at most `MAX_TYPES`) and, apart from that cap, the first password hint and
/// whether the own marker came: a hint announced late is never lost to the cap.
#[derive(Default)]
struct Announced {
    types: Vec<String>,
    hint: Option<&'static str>,
    own: bool,
}

impl OfferData {
    fn push(&self, mime: String) {
        let mut a = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if a.hint.is_none() {
            a.hint = SENSITIVE.into_iter().find(|s| *s == mime);
        }
        a.own |= mime == MARKER;
        if a.types.len() < MAX_TYPES {
            a.types.push(mime);
        }
    }

    /// The kept types, plus the hint and the marker when the cap dropped them.
    fn types(&self) -> Vec<String> {
        let a = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut types = a.types.clone();
        if let Some(h) = a.hint.filter(|h| !types.iter().any(|t| t == h)) {
            types.push(h.to_string());
        }
        if a.own && !types.iter().any(|t| t == MARKER) {
            types.push(MARKER.to_string());
        }
        types
    }
}

/// One slot holding at most one value: a new one hands the old back, so it can be torn down and
/// two are never pending.
pub(crate) struct Slot<T>(Option<T>);

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot(None)
    }
}

impl<T> Slot<T> {
    pub(crate) fn replace(&mut self, new: T) -> Option<T> {
        self.0.replace(new)
    }

    pub(crate) fn take(&mut self) -> Option<T> {
        self.0.take()
    }

    pub(crate) fn get(&self) -> Option<&T> {
        self.0.as_ref()
    }

    pub(crate) fn get_mut(&mut self) -> Option<&mut T> {
        self.0.as_mut()
    }
}

/// The offer of the current selection, whichever protocol brought it.
pub(crate) enum Offer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl Offer {
    /// The types announced so far (a copy: the list stays on the proxy).
    pub(crate) fn mime_types(&self) -> Vec<String> {
        let data = match self {
            Offer::Ext(o) => o.data::<OfferData>(),
            Offer::Wlr(o) => o.data::<OfferData>(),
        };
        data.map(OfferData::types).unwrap_or_default()
    }

    fn receive(&self, mime: &str, fd: BorrowedFd<'_>) {
        match self {
            Offer::Ext(o) => o.receive(mime.to_string(), fd),
            Offer::Wlr(o) => o.receive(mime.to_string(), fd),
        }
    }

    pub(crate) fn destroy(&self) {
        match self {
            Offer::Ext(o) => o.destroy(),
            Offer::Wlr(o) => o.destroy(),
        }
    }
}

enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

/// Whether an event's proxy is the live device. wayland-client still delivers the queued events
/// of a proxy destroyed afterwards (a seat removed, a device finished), so a handler acts only
/// on the current device's.
pub(crate) fn same_device<T: PartialEq>(current: Option<T>, event: &T) -> bool {
    current.is_some_and(|c| c == *event)
}

impl Device {
    fn id(&self) -> ObjectId {
        match self {
            Device::Ext(d) => d.id(),
            Device::Wlr(d) => d.id(),
        }
    }

    fn destroy(&self) {
        match self {
            Device::Ext(d) => d.destroy(),
            Device::Wlr(d) => d.destroy(),
        }
    }
}

/// The daemon's own source, whichever protocol made it.
enum SourceProxy {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl SourceProxy {
    fn id(&self) -> ObjectId {
        match self {
            SourceProxy::Ext(s) => s.id(),
            SourceProxy::Wlr(s) => s.id(),
        }
    }

    fn offer(&self, mime: &str) {
        match self {
            SourceProxy::Ext(s) => s.offer(mime.to_string()),
            SourceProxy::Wlr(s) => s.offer(mime.to_string()),
        }
    }

    fn destroy(&self) {
        match self {
            SourceProxy::Ext(s) => s.destroy(),
            SourceProxy::Wlr(s) => s.destroy(),
        }
    }
}

/// A history entry put back on the clipboard: the source and the text it serves. Dropping it
/// destroys the source and wipes the text. No `Debug`: the text is never printed.
pub(crate) struct OwnSource {
    source: SourceProxy,
    text: String,
}

impl OwnSource {
    pub(crate) fn is(&self, id: &ObjectId) -> bool {
        self.source.id() == *id
    }

    /// A paste asked for `mime`: the text goes out on `fd`, which is closed on return.
    pub(crate) fn send(&self, mime: &str, fd: OwnedFd) {
        if set_nonblocking(fd.as_fd()).is_err() {
            return;
        }
        let mut file = std::fs::File::from(fd);
        serve(mime, &self.text, &mut file);
    }
}

impl Drop for OwnSource {
    fn drop(&mut self) {
        self.source.destroy();
        wipe(&mut self.text);
    }
}

/// What a paste of `mime` gets: the text in one write for a text type, nothing for the marker or
/// any other type. A write that fails or stops short is never retried (a stalled reader must not
/// hold the loop) and never logged.
pub(crate) fn serve(mime: &str, text: &str, out: &mut impl Write) {
    if TEXT_TYPES.contains(&mime) {
        let _ = out.write(text.as_bytes());
    }
}

/// The pending read: its loop source, what it has so far and when it started (the deadline).
pub(crate) struct Reading {
    pub(crate) token: RegistrationToken,
    pub(crate) buffer: ReadBuffer,
    pub(crate) started: Instant,
    /// Which read this is: a source of an older one that still wakes is ignored.
    pub(crate) generation: u64,
}

pub(crate) struct Control {
    manager: Manager,
    device: Option<Device>,
    pub(crate) reading: Slot<Reading>,
    next_generation: u64,
    /// The entry last put back on the clipboard, while its source lives.
    pub(crate) own: Option<OwnSource>,
}

pub(crate) const EXT_NAME: &str = "ext-data-control-v1";

/// `ext-data-control-v1` at version 1, else `wlr-data-control-unstable-v1` at version 1; the
/// name is the start line's.
pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<(Control, &'static str)> {
    let (manager, name) = if let Ok(m) = globals.bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ()) {
        (Manager::Ext(m), EXT_NAME)
    } else {
        let m = globals.bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=1, ()).ok()?;
        (Manager::Wlr(m), "wlr-data-control-unstable-v1")
    };
    Some((Control { manager, device: None, reading: Slot::default(), next_generation: 0, own: None }, name))
}

impl Control {
    /// The device of the bar's seat, made once.
    pub(crate) fn attach(&mut self, seat: &WlSeat, qh: &QueueHandle<App>) {
        if self.device.is_some() {
            return;
        }
        self.device = Some(match &self.manager {
            Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        });
    }

    /// Whether `id` is the live device's.
    pub(crate) fn is_device(&self, id: &ObjectId) -> bool {
        same_device(self.device.as_ref().map(Device::id), id)
    }

    /// The seat is gone, or the compositor finished the device: it is destroyed (a finished
    /// device is inert, its destroy request is still due).
    pub(crate) fn detach(&mut self) {
        // Its own source goes with the device (destroyed, its text wiped).
        self.own = None;
        if let Some(device) = self.device.take() {
            device.destroy();
        }
    }

    /// Makes `text` the clipboard: a new source offering `own_offer_types()` (the marker last)
    /// becomes the selection, the previous own source is destroyed and wiped. The only place the
    /// daemon sets a selection, never the primary one, never to none. False without a device.
    pub(crate) fn own_selection(&mut self, text: &str, qh: &QueueHandle<App>) -> bool {
        let Some(device) = self.device.as_ref() else { return false };
        let source = match (&self.manager, device) {
            (Manager::Ext(m), Device::Ext(d)) => {
                let source = SourceProxy::Ext(m.create_data_source(qh, ()));
                own_offer_types().for_each(|mime| source.offer(mime));
                if let SourceProxy::Ext(s) = &source {
                    d.set_selection(Some(s));
                }
                source
            }
            (Manager::Wlr(m), Device::Wlr(d)) => {
                let source = SourceProxy::Wlr(m.create_data_source(qh, ()));
                own_offer_types().for_each(|mime| source.offer(mime));
                if let SourceProxy::Wlr(s) = &source {
                    d.set_selection(Some(s));
                }
                source
            }
            _ => return false,
        };
        self.own = Some(OwnSource { source, text: text.to_string() });
        true
    }
}

/// Puts `new` in the slot; the read it replaces, if any, is handed to `remove` (its loop source
/// goes), so two sources never feed one buffer.
pub(crate) fn replace_and_remove<T>(slot: &mut Slot<T>, new: T, remove: impl FnOnce(T)) {
    if let Some(old) = slot.replace(new) {
        remove(old);
    }
}

/// Whether a wake-up from the source of `generation` belongs to the pending read.
pub(crate) fn is_current(pending: Option<u64>, generation: u64) -> bool {
    pending == Some(generation)
}

/// One 4 KiB read per wake-up (the pipe is level-triggered): the step the loop takes next.
pub(crate) fn read_once(file: &mut impl Read, buffer: &mut ReadBuffer) -> Step {
    let mut chunk = [0u8; 4096];
    let read = loop {
        match file.read(&mut chunk) {
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            other => break other,
        }
    };
    let step = step(read, &chunk, buffer);
    chunk.fill(0);
    std::hint::black_box(&mut chunk);
    step
}

/// Asks `offer` for `mime` on a fresh pipe read on the loop, a previous read dropped first.
/// The offer is destroyed at once. The deadline is armed here and by every way the read ends.
pub(crate) fn start_read(app: &mut App, offer: Offer, mime: &'static str) {
    app.clipboard_stop_read();
    // The loop handle exists before any device does: seat events (SCTK's `new_capability`) run
    // only in dispatch, after startup stored it. Without one, the offer is simply dropped.
    let Some(handle) = app.loop_handle.clone() else {
        offer.destroy();
        return;
    };
    let Ok((reader, writer)) = UnixStream::pair() else {
        offer.destroy();
        return;
    };
    offer.receive(mime, writer.as_fd());
    drop(writer);
    offer.destroy();
    let pipe = ReadPipe::from(OwnedFd::from(reader));
    let generation = app.clipboard.as_mut().map_or(0, |c| {
        c.control.next_generation += 1;
        c.control.next_generation
    });
    let inserted = handle.insert_source(pipe, move |(), file, app: &mut App| {
        let mut pipe: &std::fs::File = file.as_ref();
        app.clipboard_guarded("read", |app| app.clipboard_read_wake(generation, &mut pipe)).unwrap_or(PostAction::Remove)
    });
    if let (Ok(token), Some(c)) = (inserted, app.clipboard.as_mut()) {
        let reading = Reading { token, buffer: ReadBuffer::default(), started: Instant::now(), generation };
        replace_and_remove(&mut c.control.reading, reading, |old| handle.remove(old.token));
    }
    app.clipboard_timer_sync();
}

macro_rules! data_control_family {
    ($manager:ty, $device:ty, $device_mod:ident, $offer:ty, $offer_mod:ident, $source:ty, $source_mod:ident, $wrap:expr, $manager_mod:ident) => {
        impl Dispatch<$manager, ()> for App {
            fn event(_: &mut App, _: &$manager, _: $manager_mod::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {}
        }

        impl Dispatch<$device, ()> for App {
            fn event(state: &mut App, device: &$device, event: $device_mod::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
                use $device_mod::Event as E;
                // A queued event of a device already replaced or removed: its offers go, nothing else.
                if !state.clipboard.as_ref().is_some_and(|c| c.control.is_device(&device.id())) {
                    match event {
                        E::DataOffer { id } | E::Selection { id: Some(id) } | E::PrimarySelection { id: Some(id) } => id.destroy(),
                        _ => {}
                    }
                    return;
                }
                match event {
                    E::Selection { id } => state.clipboard_selection(id.map($wrap)),
                    // The primary selection is never read: its offer goes at once.
                    E::PrimarySelection { id: Some(offer) } => offer.destroy(),
                    E::Finished => state.clipboard_device_finished(),
                    _ => {}
                }
            }

            event_created_child!(App, $device, [
                $device_mod::EVT_DATA_OFFER_OPCODE => ($offer, OfferData::default()),
            ]);
        }

        impl Dispatch<$offer, OfferData> for App {
            fn event(_: &mut App, _: &$offer, event: $offer_mod::Event, data: &OfferData, _: &Connection, _: &QueueHandle<App>) {
                if let $offer_mod::Event::Offer { mime_type } = event {
                    data.push(mime_type);
                }
            }
        }

        // The source is the daemon's own: a paste is served from it, a replaced one is dropped.
        impl Dispatch<$source, ()> for App {
            fn event(state: &mut App, source: &$source, event: $source_mod::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
                let id = source.id();
                match event {
                    $source_mod::Event::Send { mime_type, fd } => {
                        state.clipboard_guarded("send", |app| app.clipboard_source_send(&id, &mime_type, fd));
                    }
                    $source_mod::Event::Cancelled => {
                        state.clipboard_guarded("cancelled", |app| app.clipboard_source_cancelled(&id));
                    }
                    _ => {}
                }
            }
        }
    };
}

data_control_family!(
    ExtDataControlManagerV1,
    ExtDataControlDeviceV1,
    ext_data_control_device_v1,
    ExtDataControlOfferV1,
    ext_data_control_offer_v1,
    ExtDataControlSourceV1,
    ext_data_control_source_v1,
    Offer::Ext,
    ext_data_control_manager_v1
);
data_control_family!(
    ZwlrDataControlManagerV1,
    ZwlrDataControlDeviceV1,
    zwlr_data_control_device_v1,
    ZwlrDataControlOfferV1,
    zwlr_data_control_offer_v1,
    ZwlrDataControlSourceV1,
    zwlr_data_control_source_v1,
    Offer::Wlr,
    zwlr_data_control_manager_v1
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_live_devices_events_count() {
        assert!(same_device(Some(7), &7));
        assert!(!same_device(Some(8), &7), "a queued event of the device replaced by another");
        assert!(!same_device(None, &7), "no device at all (detached): everything is stale");
    }

    #[test]
    fn a_new_read_replaces_the_pending_one() {
        let mut slot: Slot<u32> = Slot::default();
        assert_eq!(slot.replace(1), None);
        assert_eq!(slot.replace(2), Some(1), "the old one comes back to be removed");
        assert_eq!(slot.get(), Some(&2));
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.get(), None);
    }

    #[test]
    fn replacing_a_read_removes_the_old_source() {
        let mut slot: Slot<u32> = Slot::default();
        let mut removed = Vec::new();
        replace_and_remove(&mut slot, 1, |old| removed.push(old));
        assert!(removed.is_empty());
        replace_and_remove(&mut slot, 2, |old| removed.push(old));
        assert_eq!(removed, [1]);
        assert_eq!(slot.get(), Some(&2));
    }

    /// A writer that counts its calls and fails each one with `kind`.
    struct Failing {
        calls: u32,
        kind: ErrorKind,
    }

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            assert!(self.calls < 2, "a failed write was retried");
            Err(self.kind.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn serve_writes_the_text_for_a_text_type_and_nothing_for_the_marker() {
        for mime in TEXT_TYPES {
            let mut out = Vec::new();
            serve(mime, "héllo", &mut out);
            assert_eq!(out, "héllo".as_bytes(), "{mime}");
        }
        let mut out = Vec::new();
        serve(crate::clipboard::offer::MARKER, "héllo", &mut out);
        serve("image/png", "héllo", &mut out);
        assert!(out.is_empty(), "the marker and other types get nothing");
    }

    #[test]
    fn serve_never_retries_a_blocked_write() {
        let mut out = Failing { calls: 0, kind: ErrorKind::WouldBlock };
        serve("text/plain", "x", &mut out);
        assert_eq!(out.calls, 1, "one call, then it gives up");
        let mut out = Failing { calls: 0, kind: ErrorKind::BrokenPipe };
        serve("UTF8_STRING", "x", &mut out);
        assert_eq!(out.calls, 1, "a closed reader is not retried either");
    }

    #[test]
    fn a_password_hint_past_the_type_cap_is_still_seen() {
        use crate::clipboard::offer::{Skip, Verdict, verdict};
        let data = OfferData::default();
        for i in 0..MAX_TYPES {
            data.push(format!("application/x-filler-{i}"));
        }
        data.push("text/plain".to_string());
        data.push("x-kde-passwordManagerHint".to_string());
        let types = data.types();
        assert!(types.len() <= MAX_TYPES + 2, "still bounded");
        assert_eq!(verdict(types.iter().map(String::as_str), false), Verdict::Skip(Skip::Sensitive));
        let data = OfferData::default();
        for i in 0..MAX_TYPES {
            data.push(format!("application/x-filler-{i}"));
        }
        data.push(MARKER.to_string());
        let types = data.types();
        assert_eq!(verdict(types.iter().map(String::as_str), false), Verdict::Skip(Skip::Own));
    }

    #[test]
    fn a_stale_source_is_not_current() {
        assert!(is_current(Some(3), 3));
        assert!(!is_current(Some(3), 2));
        assert!(!is_current(None, 3));
    }
}
