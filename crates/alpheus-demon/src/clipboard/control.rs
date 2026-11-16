//! The only file that names a data-control protocol (clipboard spec): ext-data-control-v1 first,
//! wlr-data-control-unstable-v1 (version 1) as the fallback, both through one macro. The manager,
//! the device, the offers and the one pending read on the loop. Never logs, and nothing here shows
//! an offer's types or text.

use crate::clipboard::offer::{ReadBuffer, Step, step};
use crate::wayland::App;
use smithay_client_toolkit::data_device_manager::ReadPipe;
use smithay_client_toolkit::reexports::calloop::{PostAction, RegistrationToken};
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
use std::io::{ErrorKind, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Instant;

/// An offer announcing more types than this has the rest ignored.
const MAX_TYPES: usize = 64;

/// The types an offer announced so far, kept on its proxy as they arrive. No `Debug`: a type list
/// is never printed.
#[derive(Default)]
pub(crate) struct OfferData(Mutex<Vec<String>>);

impl OfferData {
    fn push(&self, mime: String) {
        let mut types = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if types.len() < MAX_TYPES {
            types.push(mime);
        }
    }

    fn types(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
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

impl Device {
    fn destroy(&self) {
        match self {
            Device::Ext(d) => d.destroy(),
            Device::Wlr(d) => d.destroy(),
        }
    }
}

/// The pending read: its loop source, what it has so far and when it started (the deadline).
pub(crate) struct Reading {
    pub(crate) token: RegistrationToken,
    pub(crate) buffer: ReadBuffer,
    pub(crate) started: Instant,
}

pub(crate) struct Control {
    manager: Manager,
    device: Option<Device>,
    pub(crate) reading: Slot<Reading>,
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
    Some((Control { manager, device: None, reading: Slot::default() }, name))
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

    /// The seat is gone, or the compositor finished the device: it is released.
    pub(crate) fn detach(&mut self) {
        if let Some(device) = self.device.take() {
            device.destroy();
        }
    }

    /// The compositor already destroyed the device (`finished`): only forgotten.
    pub(crate) fn device_finished(&mut self) {
        self.device = None;
    }
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
    step
}

/// Asks `offer` for `mime` on a fresh pipe read on the loop, a previous read dropped first.
/// The offer is destroyed at once. The deadline is armed here and by every way the read ends.
pub(crate) fn start_read(app: &mut App, offer: Offer, mime: &'static str) {
    app.clipboard_stop_read();
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
    let inserted = handle.insert_source(pipe, move |(), file, app: &mut App| {
        let mut pipe: &std::fs::File = file.as_ref();
        app.clipboard_guarded("read", |app| app.clipboard_read_wake(&mut pipe)).unwrap_or(PostAction::Remove)
    });
    if let (Ok(token), Some(c)) = (inserted, app.clipboard.as_mut()) {
        c.control.reading.replace(Reading { token, buffer: ReadBuffer::default(), started: Instant::now() });
    }
    app.clipboard_timer_sync();
}

macro_rules! data_control_family {
    ($manager:ty, $device:ty, $device_mod:ident, $offer:ty, $offer_mod:ident, $source:ty, $source_mod:ident, $wrap:expr, $manager_mod:ident) => {
        impl Dispatch<$manager, ()> for App {
            fn event(_: &mut App, _: &$manager, _: $manager_mod::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {}
        }

        impl Dispatch<$device, ()> for App {
            fn event(state: &mut App, _: &$device, event: $device_mod::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
                use $device_mod::Event as E;
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

        // The source is the daemon's own (a later task offers it); a request for data is closed.
        impl Dispatch<$source, ()> for App {
            fn event(_: &mut App, _: &$source, event: $source_mod::Event, _: &(), _: &Connection, _: &QueueHandle<App>) {
                if let $source_mod::Event::Send { fd, .. } = event {
                    drop(fd);
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
    fn a_new_read_replaces_the_pending_one() {
        let mut slot: Slot<u32> = Slot::default();
        assert_eq!(slot.replace(1), None);
        assert_eq!(slot.replace(2), Some(1), "the old one comes back to be removed");
        assert_eq!(slot.get(), Some(&2));
        assert_eq!(slot.take(), Some(2));
        assert_eq!(slot.get(), None);
    }
}
