//! The only file that speaks `zwlr_screencopy_manager_v1`: one output captured into one shm buffer,
//! handed over as a `Frame`, and the pure conversions around it (pixels to RGBA, a logical area to
//! buffer pixels).
//!
//! `Screencopy` is the bound manager; `request` asks it for one frame of one output and the
//! result comes back through a `CaptureDone` closure (the daemon's glue sends it to its mail).
//! Everything a capture needs lives in the frame's user data (`FrameData`): its own short-lived
//! `SlotPool`, dropped the moment the compositor says `ready` or `failed`, so nothing is held at
//! rest and the bar's own pool never grows by a screenshot.
//!
//! Never:
//! - capture anything but the output it is given, or more than once per `request` call;
//! - keep a pool, a buffer or a pixel once the capture is answered;
//! - log a size, a format or an output name.
//!
//! See: docs/features/captures.md (when it lands), docs/decisions.md.

use super::Rect;
use crate::wayland::App;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use smithay_client_toolkit::reexports::protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{Event, Flags, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};
use smithay_client_toolkit::shm::{
    Shm,
    slot::{Buffer, SlotPool},
};
use std::sync::Mutex;

/// The widest or tallest capture taken (a compositor announcing more is refused).
const MAX_SIDE: u32 = 16384;
/// The most bytes one capture buffer may hold (256 MiB: an 8K by 8K output).
const MAX_BYTES: u64 = 256 << 20;

/// The pixel layouts a capture is read in (little-endian shm words). `wl_shm` guarantees the first
/// two; the others are what wlroots sometimes announces first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Bytes B, G, R, A.
    Argb8888,
    /// Bytes B, G, R, unused.
    Xrgb8888,
    /// Bytes R, G, B, A.
    Abgr8888,
    /// Bytes R, G, B, unused.
    Xbgr8888,
}

impl Format {
    fn from_shm(f: wl_shm::Format) -> Option<Format> {
        match f {
            wl_shm::Format::Argb8888 => Some(Format::Argb8888),
            wl_shm::Format::Xrgb8888 => Some(Format::Xrgb8888),
            wl_shm::Format::Abgr8888 => Some(Format::Abgr8888),
            wl_shm::Format::Xbgr8888 => Some(Format::Xbgr8888),
            _ => None,
        }
    }

    fn to_shm(self) -> wl_shm::Format {
        match self {
            Format::Argb8888 => wl_shm::Format::Argb8888,
            Format::Xrgb8888 => wl_shm::Format::Xrgb8888,
            Format::Abgr8888 => wl_shm::Format::Abgr8888,
            Format::Xbgr8888 => wl_shm::Format::Xbgr8888,
        }
    }

    /// One pixel's four bytes as RGBA. An `X` format has no alpha: opaque.
    fn rgba(self, p: [u8; 4]) -> [u8; 4] {
        match self {
            Format::Argb8888 => [p[2], p[1], p[0], p[3]],
            Format::Xrgb8888 => [p[2], p[1], p[0], 255],
            Format::Abgr8888 => p,
            Format::Xbgr8888 => [p[0], p[1], p[2], 255],
        }
    }
}

/// One captured output: the compositor's pixels, copied out of the shm buffer.
#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Bytes between two rows.
    pub stride: u32,
    pub format: Format,
    /// The rows are stored bottom to top (the compositor's `y_invert` flag).
    pub y_invert: bool,
    pub data: Vec<u8>,
}

/// `area` of `frame` as RGBA, row-major, top row first. `area` is in buffer pixels (see
/// `map_area`); the part of it outside the frame is cut, so the result may be smaller (empty when
/// nothing is inside). A frame whose `data` is too short for its rows gives opaque black rows
/// rather than a panic.
pub fn to_rgba(frame: &Frame, area: Rect) -> Vec<u8> {
    let x0 = i64::from(area.x).clamp(0, i64::from(frame.width));
    let y0 = i64::from(area.y).clamp(0, i64::from(frame.height));
    let x1 = (i64::from(area.x) + i64::from(area.w)).clamp(x0, i64::from(frame.width));
    let y1 = (i64::from(area.y) + i64::from(area.h)).clamp(y0, i64::from(frame.height));
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y0..y1 {
        let src = if frame.y_invert { i64::from(frame.height) - 1 - row } else { row } as usize;
        let start = src * frame.stride as usize + x0 as usize * 4;
        match frame.data.get(start..start + w * 4) {
            Some(bytes) => {
                for p in bytes.as_chunks::<4>().0 {
                    out.extend_from_slice(&frame.format.rgba(*p));
                }
            }
            None => out.extend(std::iter::repeat_n([0, 0, 0, 255], w).flatten()),
        }
    }
    out
}

/// A logical area of an output (`logical_size` big, its own origin at 0, 0) as buffer pixels:
/// scaled, and turned back through the output's `transform`. wlroots hands the buffer in the
/// output's native orientation, as `grim` reads it, so a rotated output is un-rotated here:
/// the compositor shows `transform(buffer)`.
pub fn map_area(logical: Rect, scale: f64, transform: Transform, logical_size: (u32, u32)) -> Rect {
    let (dw, dh) = (f64::from(logical_size.0), f64::from(logical_size.1));
    // Continuous coordinates: the two corners map, and the box between them is the answer.
    let corner = |dx: f64, dy: f64| -> (f64, f64) {
        match transform {
            Transform::_90 => (dh - dy, dx),
            Transform::_180 => (dw - dx, dh - dy),
            Transform::_270 => (dy, dw - dx),
            Transform::Flipped => (dw - dx, dy),
            Transform::Flipped90 => (dy, dx),
            Transform::Flipped180 => (dx, dh - dy),
            Transform::Flipped270 => (dh - dy, dw - dx),
            _ => (dx, dy),
        }
    };
    let (x, y) = (f64::from(logical.x), f64::from(logical.y));
    let a = corner(x, y);
    let b = corner(x + f64::from(logical.w), y + f64::from(logical.h));
    let (left, right) = (a.0.min(b.0) * scale, a.0.max(b.0) * scale);
    let (top, bottom) = (a.1.min(b.1) * scale, a.1.max(b.1) * scale);
    // The edges round, not the size: two areas side by side stay side by side.
    let (left, right, top, bottom) = (left.round(), right.round(), top.round(), bottom.round());
    Rect { x: left as i32, y: top as i32, w: (right - left) as u32, h: (bottom - top) as u32 }
}

/// Why a capture gave no frame. Never carries text: the glue logs a fixed word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// The compositor said `failed` (the output went away, a mode change, a refusal).
    Failed,
    /// No pixel format it offered is one we read, or the size it announced is absurd.
    Unsupported,
    /// The shared buffer could not be made.
    Buffer,
}

/// What a capture's answer is handed to: called exactly once, on the daemon's thread, from the
/// Wayland dispatch. The glue sends the result to its mail.
pub type CaptureDone = Box<dyn FnOnce(Result<Frame, CaptureError>) + Send>;

/// The bound screencopy manager. Binding it costs nothing at rest; absent, the captures are off.
pub struct Screencopy {
    manager: ZwlrScreencopyManagerV1,
}

impl Screencopy {
    /// Binds the manager (versions 1 to 3); `None` when the compositor has none.
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Option<Screencopy>
    where
        D: Dispatch<ZwlrScreencopyManagerV1, ()> + 'static,
    {
        let manager = globals.bind::<ZwlrScreencopyManagerV1, _, _>(qh, 1..=3, ()).ok()?;
        Some(Screencopy { manager })
    }

    /// Asks for one frame of `output`, cursor not drawn. `done` is called once with the frame or
    /// the error; `shm` only lends its `wl_shm` to the frame's own pool.
    pub fn request<D>(&self, output: &WlOutput, shm: &Shm, qh: &QueueHandle<D>, done: CaptureDone)
    where
        D: Dispatch<ZwlrScreencopyFrameV1, FrameData> + 'static,
    {
        let grab = Grab { shm: Shm::from(shm.wl_shm().clone()), done: Some(done), shape: None, held: None, y_invert: false };
        self.manager.capture_output(0, output, qh, FrameData(Mutex::new(grab)));
    }
}

impl App {
    /// Asks for one frame of `output`; `false` (and `done` never called) when the compositor has
    /// no screencopy. The one capture at a time rule is the caller's.
    pub fn capture_output(&self, output: &WlOutput, qh: &QueueHandle<App>, done: CaptureDone) -> bool {
        match &self.screencopy {
            Some(s) => {
                s.request(output, &self.shm, qh, done);
                true
            }
            None => false,
        }
    }
}

// The manager has no events.
delegate_noop!(App: ignore ZwlrScreencopyManagerV1);

// Any state with `delegate_dispatch2!` dispatches the frame's events here.
impl<D> Dispatch2<ZwlrScreencopyFrameV1, D> for FrameData {
    fn event(&self, _: &mut D, frame: &ZwlrScreencopyFrameV1, event: Event, _: &Connection, _: &QueueHandle<D>) {
        self.handle(frame, event);
    }
}

/// The user data of one frame: the state of its capture.
pub struct FrameData(Mutex<Grab>);

/// What the compositor announced for the buffer.
struct Shape {
    format: Format,
    width: u32,
    height: u32,
    stride: u32,
}

struct Grab {
    shm: Shm,
    /// Taken at the answer, so it runs once.
    done: Option<CaptureDone>,
    shape: Option<Shape>,
    /// The pool and buffer between `copy` and the answer.
    held: Option<(SlotPool, Buffer)>,
    y_invert: bool,
}

impl Grab {
    /// Makes the buffer announced and asks the compositor to fill it.
    fn copy(&mut self, frame: &ZwlrScreencopyFrameV1) -> Result<(), CaptureError> {
        if self.held.is_some() {
            return Ok(());
        }
        let s = self.shape.as_ref().ok_or(CaptureError::Unsupported)?;
        let len = u64::from(s.stride) * u64::from(s.height);
        let mut pool = SlotPool::new(len as usize, &self.shm).map_err(|_| CaptureError::Buffer)?;
        let (buffer, _) =
            pool.create_buffer(s.width as i32, s.height as i32, s.stride as i32, s.format.to_shm()).map_err(|_| CaptureError::Buffer)?;
        frame.copy(buffer.wl_buffer());
        self.held = Some((pool, buffer));
        Ok(())
    }

    /// The frame, read out of the buffer, which is dropped with its pool.
    fn frame(&mut self) -> Result<Frame, CaptureError> {
        let (mut pool, buffer) = self.held.take().ok_or(CaptureError::Failed)?;
        let s = self.shape.as_ref().ok_or(CaptureError::Failed)?;
        let len = (u64::from(s.stride) * u64::from(s.height)) as usize;
        let canvas = buffer.canvas(&mut pool).ok_or(CaptureError::Buffer)?;
        let data = canvas.get(..len).ok_or(CaptureError::Buffer)?.to_vec();
        Ok(Frame { width: s.width, height: s.height, stride: s.stride, format: s.format, y_invert: self.y_invert, data })
    }
}

impl FrameData {
    /// One event of the frame. Every path ends in one `done` call and the frame's destruction.
    pub fn handle(&self, frame: &ZwlrScreencopyFrameV1, event: Event) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let answer = match event {
            Event::Buffer { format, width, height, stride } => {
                // Version 3 announces several buffers: the first shm one we read is kept.
                if g.shape.is_none()
                    && let WEnum::Value(f) = format
                    && let Some(format) = Format::from_shm(f)
                    && width > 0
                    && height > 0
                    && width <= MAX_SIDE
                    && height <= MAX_SIDE
                    && stride >= width.saturating_mul(4)
                    && u64::from(stride) * u64::from(height) <= MAX_BYTES
                {
                    g.shape = Some(Shape { format, width, height, stride });
                }
                // Before version 3 there is no `buffer_done`: the one `buffer` event is the cue.
                if frame.version() < 3 { g.copy(frame).err().map(Err) } else { None }
            }
            Event::BufferDone => g.copy(frame).err().map(Err),
            Event::Flags { flags } => {
                g.y_invert = matches!(flags, WEnum::Value(f) if f.contains(Flags::YInvert));
                None
            }
            Event::Ready { .. } => Some(g.frame()),
            Event::Failed => Some(Err(CaptureError::Failed)),
            _ => None,
        };
        if let Some(result) = answer {
            g.held = None;
            let done = g.done.take();
            drop(g);
            frame.destroy();
            if let Some(done) = done {
                done(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(format: Format, width: u32, height: u32, stride: u32, y_invert: bool, data: Vec<u8>) -> Frame {
        Frame { width, height, stride, format, y_invert, data }
    }

    fn whole(f: &Frame) -> Rect {
        Rect { x: 0, y: 0, w: f.width, h: f.height }
    }

    #[test]
    fn xrgb_and_argb_become_rgba() {
        // Bytes in memory are B, G, R, X for the little-endian XRGB word.
        let xrgb = frame(Format::Xrgb8888, 2, 1, 8, false, vec![10, 20, 30, 0, 1, 2, 3, 77]);
        assert_eq!(to_rgba(&xrgb, whole(&xrgb)), [30, 20, 10, 255, 3, 2, 1, 255]);
        let argb = frame(Format::Argb8888, 2, 1, 8, false, vec![10, 20, 30, 128, 1, 2, 3, 4]);
        assert_eq!(to_rgba(&argb, whole(&argb)), [30, 20, 10, 128, 3, 2, 1, 4]);
        let xbgr = frame(Format::Xbgr8888, 1, 1, 4, false, vec![10, 20, 30, 0]);
        assert_eq!(to_rgba(&xbgr, whole(&xbgr)), [10, 20, 30, 255]);
        let abgr = frame(Format::Abgr8888, 1, 1, 4, false, vec![10, 20, 30, 40]);
        assert_eq!(to_rgba(&abgr, whole(&abgr)), [10, 20, 30, 40]);
    }

    #[test]
    fn y_invert_flips_rows() {
        // Two rows, the first stored one is the bottom row of the image.
        let data = vec![1, 1, 1, 0, 2, 2, 2, 0];
        let upright = frame(Format::Xbgr8888, 1, 2, 4, false, data.clone());
        assert_eq!(to_rgba(&upright, whole(&upright)), [1, 1, 1, 255, 2, 2, 2, 255]);
        let inverted = frame(Format::Xbgr8888, 1, 2, 4, true, data);
        assert_eq!(to_rgba(&inverted, whole(&inverted)), [2, 2, 2, 255, 1, 1, 1, 255]);
        // A crop of the top row of the image is the last stored row.
        assert_eq!(to_rgba(&inverted, Rect { x: 0, y: 0, w: 1, h: 1 }), [2, 2, 2, 255]);
    }

    #[test]
    fn an_area_crops_through_the_stride_and_is_cut_at_the_frame() {
        // 3 by 2, stride 16 (4 padding bytes a row); pixel value = its index.
        let mut data = vec![0xEE; 32];
        for (i, p) in [(0, 0), (1, 1), (2, 2), (4, 3), (5, 4), (6, 5)].iter().map(|&(i, v)| (i, v as u8)) {
            data[i * 4..i * 4 + 3].copy_from_slice(&[p, p, p]);
        }
        let f = frame(Format::Xbgr8888, 3, 2, 16, false, data);
        assert_eq!(to_rgba(&f, Rect { x: 1, y: 1, w: 2, h: 1 }), [4, 4, 4, 255, 5, 5, 5, 255]);
        // Past the right and bottom edges, and with a negative origin: cut to what is inside.
        assert_eq!(to_rgba(&f, Rect { x: 2, y: 1, w: 9, h: 9 }), [5, 5, 5, 255]);
        assert_eq!(to_rgba(&f, Rect { x: -5, y: 0, w: 6, h: 1 }), [0, 0, 0, 255]);
        assert!(to_rgba(&f, Rect { x: 9, y: 9, w: 2, h: 2 }).is_empty());
    }

    #[test]
    fn a_short_buffer_does_not_panic() {
        let f = frame(Format::Xrgb8888, 2, 2, 8, false, vec![0; 4]);
        assert_eq!(to_rgba(&f, whole(&f)).len(), 16);
    }

    #[test]
    fn a_scaled_or_rotated_output_maps_the_area() {
        let area = Rect { x: 10, y: 20, w: 30, h: 40 };
        // Scale 2, no transform: everything doubles.
        assert_eq!(map_area(area, 2.0, Transform::Normal, (100, 200)), Rect { x: 20, y: 40, w: 60, h: 80 });
        // 90 degrees on a 100 by 200 logical output (buffer 200 by 100): x = 200 - (y + h), y = x.
        assert_eq!(map_area(area, 1.0, Transform::_90, (100, 200)), Rect { x: 140, y: 10, w: 40, h: 30 });
        // Both at once.
        assert_eq!(map_area(area, 2.0, Transform::_90, (100, 200)), Rect { x: 280, y: 20, w: 80, h: 60 });
        // 180 degrees: the area mirrors through the centre.
        assert_eq!(map_area(area, 1.0, Transform::_180, (100, 200)), Rect { x: 60, y: 140, w: 30, h: 40 });
        // 270 degrees: x = y, y = 100 - (x + w).
        assert_eq!(map_area(area, 1.0, Transform::_270, (100, 200)), Rect { x: 20, y: 60, w: 40, h: 30 });
        // Flipped: mirrored left to right.
        assert_eq!(map_area(area, 1.0, Transform::Flipped, (100, 200)), Rect { x: 60, y: 20, w: 30, h: 40 });
        // A fractional scale rounds the edges, not the size.
        assert_eq!(map_area(Rect { x: 1, y: 1, w: 1, h: 1 }, 1.5, Transform::Normal, (10, 10)), Rect { x: 2, y: 2, w: 1, h: 1 });
    }

    #[test]
    fn the_whole_output_maps_to_the_whole_buffer_under_every_transform() {
        let whole_logical = Rect { x: 0, y: 0, w: 100, h: 200 };
        for t in [Transform::Normal, Transform::_180, Transform::Flipped, Transform::Flipped180] {
            assert_eq!(map_area(whole_logical, 2.0, t, (100, 200)), Rect { x: 0, y: 0, w: 200, h: 400 }, "{t:?}");
        }
        for t in [Transform::_90, Transform::_270, Transform::Flipped90, Transform::Flipped270] {
            assert_eq!(map_area(whole_logical, 2.0, t, (100, 200)), Rect { x: 0, y: 0, w: 400, h: 200 }, "{t:?}");
        }
    }
}
