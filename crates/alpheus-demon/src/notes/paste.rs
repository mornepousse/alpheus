//! Paste (quick notes spec § 4): Ctrl+V reads the compositor's selection through the data device
//! the pad creates when it opens. No process, no thread, no timer: the pipe is a source of the
//! event loop only while a paste is pending, bounded by `CAP` (a clipboard owner that streams
//! forever is cut at the first byte past it). The pad never owns a selection and accepts no drop.

use crate::log::write as log;
use crate::notes::text::CAP;
use crate::wayland::App;
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::data_device_manager::data_device::DataDeviceHandler;
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferError, DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::reexports::calloop::PostAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use std::io::{ErrorKind, Read};

/// The text types asked for, best first.
pub const MIMES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"];

/// The first of `MIMES` the offer carries.
pub fn pick_mime<'a>(offered: impl IntoIterator<Item = &'a str>) -> Option<&'static str> {
    let offered: Vec<&str> = offered.into_iter().collect();
    MIMES.into_iter().find(|m| offered.contains(m))
}

/// What a paste has read so far: at most `CAP + 1` bytes (one past the cap tells "too much").
#[derive(Debug, Default)]
pub struct PasteBuffer {
    bytes: Vec<u8>,
    over: bool,
}

impl PasteBuffer {
    pub fn push(&mut self, chunk: &[u8]) {
        let room = (CAP + 1).saturating_sub(self.bytes.len());
        let take = chunk.len().min(room);
        self.bytes.extend_from_slice(chunk.get(..take).unwrap_or_default());
        self.over = self.bytes.len() > CAP;
    }

    /// The text (lossy UTF-8 of the first `CAP` bytes, cut on a scalar boundary) and whether more
    /// came than fits.
    pub fn finish(mut self) -> (String, bool) {
        self.bytes.truncate(CAP);
        if let Err(e) = std::str::from_utf8(&self.bytes) {
            // A character cut by the cap is dropped whole; an invalid byte stays for the lossy pass.
            if e.error_len().is_none() {
                self.bytes.truncate(e.valid_up_to());
            }
        }
        (String::from_utf8_lossy(&self.bytes).into_owned(), self.over)
    }
}

/// Whether the reader has all it will use: past the cap, the pipe is dropped whatever the owner
/// still sends.
pub fn done(buffer: &PasteBuffer) -> bool {
    buffer.over
}

/// What one read tells the loop to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    /// The end of the data, or the cap: the text goes in.
    Stop,
    Failed(ErrorKind),
}

/// The pure part of one read wake-up: `chunk` is the buffer the read filled.
pub fn step(read: std::io::Result<usize>, chunk: &[u8], buffer: &mut PasteBuffer) -> Step {
    match read {
        Ok(n) if n > 0 => {
            buffer.push(chunk.get(..n).unwrap_or_default());
            if done(buffer) { Step::Stop } else { Step::Continue }
        }
        Ok(_) => Step::Stop,
        Err(e) if e.kind() == ErrorKind::WouldBlock => Step::Continue,
        Err(e) => Step::Failed(e.kind()),
    }
}

impl App {
    /// Ctrl+V: asks the selection offer for text and reads it on the loop; nothing to paste (no
    /// manager, device, offer or text type) is a message. A second Ctrl+V drops the first read.
    pub(crate) fn notes_paste(&mut self) {
        let Some(open) = self.notes.open.as_mut() else { return };
        open.stop_paste();
        if open.model.read_only.is_some() {
            // No pipe for a pad that refuses the text (the model's own refusal message).
            let out = open.model.paste("");
            self.notes_outcome(out);
            return;
        }
        let offer = open.data_device.as_ref().and_then(|d| d.data().selection_offer());
        let mime = offer.as_ref().and_then(|o| o.with_mime_types(|m| pick_mime(m.iter().map(String::as_str))));
        let (Some(offer), Some(mime), Some(handle)) = (offer, mime, self.loop_handle.clone()) else {
            return self.notes_say("nothing to paste");
        };
        let pipe = match offer.receive(mime.to_string()) {
            Ok(p) => p,
            Err(e) => {
                let kind = match e {
                    DataOfferError::Io(e) => e.kind(),
                    _ => ErrorKind::Other,
                };
                log(&format!("notes: paste failed ({kind:?})"));
                return self.notes_say("paste failed");
            }
        };
        let mut buffer = PasteBuffer::default();
        let inserted = handle.insert_source(pipe, move |(), file, app: &mut App| {
            let mut pipe: &std::fs::File = file.as_ref();
            app.notes_guarded("paste", |app| app.notes_paste_read(&mut pipe, &mut buffer)).unwrap_or(PostAction::Remove)
        });
        match inserted {
            Ok(token) => {
                if let Some(open) = self.notes.open.as_mut() {
                    open.paste = Some((handle, token));
                }
            }
            Err(_) => {
                log("notes: paste failed (no loop)");
                self.notes_say("paste failed");
            }
        }
    }

    /// One read per wake-up (the pipe is level-triggered): more data, the end, the cap or an error.
    fn notes_paste_read(&mut self, file: &mut impl Read, buffer: &mut PasteBuffer) -> PostAction {
        let mut chunk = [0u8; 16 * 1024];
        let read = loop {
            match file.read(&mut chunk) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                other => break other,
            }
        };
        match step(read, &chunk, buffer) {
            Step::Continue => return PostAction::Continue,
            Step::Failed(kind) => {
                log(&format!("notes: paste failed ({kind:?})"));
                self.paste_finished();
                self.notes_say("paste failed");
                return PostAction::Remove;
            }
            Step::Stop => {}
        }
        // The end, or the cap: the source goes (it is removed by the return), the text goes in.
        self.paste_finished();
        let (text, over) = std::mem::take(buffer).finish();
        let out = self.notes.open.as_mut().map(|open| {
            let out = open.model.paste(&text);
            if over {
                open.model.show_message("the clipboard was too large: the pad is full (64 KiB)".to_string());
            }
            open.stamp_dirty();
            out
        });
        self.notes_timer_sync();
        if let Some(out) = out {
            self.notes_outcome(out);
        }
        self.notes_redraw();
        PostAction::Remove
    }

    /// The pending read ended by itself: forget its token (the loop removes the source on return).
    fn paste_finished(&mut self) {
        if let Some(open) = self.notes.open.as_mut() {
            open.paste = None;
        }
    }

    fn notes_say(&mut self, message: &str) {
        if let Some(open) = self.notes.open.as_mut() {
            open.model.show_message(message.to_string());
        }
        self.notes_redraw();
    }
}

// The pad takes part in the data-device protocol only as far as SCTK needs: it never owns a
// selection and accepts no drag and drop; the selection offer is read on Ctrl+V alone.
impl DataDeviceHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for App {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: String, _: WritePipe) {}
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_mime_prefers_utf8_plain_text() {
        assert_eq!(pick_mime(["STRING", "text/plain;charset=utf-8"]), Some("text/plain;charset=utf-8"));
        assert_eq!(pick_mime(["image/png"]), None);
        assert_eq!(pick_mime(["TEXT", "UTF8_STRING", "STRING"]), Some("UTF8_STRING"));
    }

    #[test]
    fn paste_reader_caps_and_cleans() {
        let mut b = PasteBuffer::default();
        let chunk = [b'a'; 4096];
        let mut stopped_at = None;
        for i in 0..256 {
            b.push(&chunk);
            if done(&b) && stopped_at.is_none() {
                stopped_at = Some(i);
            }
        }
        assert_eq!(b.bytes.len(), CAP + 1, "one byte past the cap is kept, no more");
        assert_eq!(stopped_at, Some(CAP / 4096), "the stop rule fires on the chunk that passes the cap");
        let (text, over) = b.finish();
        assert_eq!((text.len(), over), (CAP, true));
        let mut b = PasteBuffer::default();
        b.push(&[0xff, b'a']);
        assert_eq!(b.finish(), ("\u{fffd}a".to_string(), false));
    }

    #[test]
    fn a_character_cut_by_the_cap_is_dropped_whole() {
        let mut b = PasteBuffer::default();
        b.push(&vec![b'a'; CAP - 1]);
        b.push("é".as_bytes()); // two bytes: one fits under the cap, one is the extra
        let (text, over) = b.finish();
        assert_eq!((text.len(), over), (CAP - 1, true), "no replacement character at the cut");
    }

    #[test]
    fn step_stops_at_the_cap_and_the_end_and_waits_on_would_block() {
        let mut b = PasteBuffer::default();
        let chunk = [b'a'; 4096];
        assert_eq!(step(Ok(10), &chunk, &mut b), Step::Continue);
        assert_eq!(step(Err(std::io::ErrorKind::WouldBlock.into()), &chunk, &mut b), Step::Continue);
        assert_eq!(step(Ok(0), &chunk, &mut b), Step::Stop, "EOF");
        let mut b = PasteBuffer::default();
        let mut last = Step::Continue;
        for _ in 0..=CAP / 4096 {
            last = step(Ok(4096), &chunk, &mut b);
        }
        assert_eq!(last, Step::Stop, "the cap");
        assert_eq!(
            step(Err(std::io::ErrorKind::BrokenPipe.into()), &chunk, &mut PasteBuffer::default()),
            Step::Failed(ErrorKind::BrokenPipe)
        );
    }

    #[test]
    fn exactly_the_cap_is_not_over() {
        let mut b = PasteBuffer::default();
        b.push(&vec![b'a'; CAP]);
        assert!(!done(&b));
        assert!(!b.finish().1);
    }
}
