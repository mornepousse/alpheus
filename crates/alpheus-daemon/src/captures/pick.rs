//! The colour picker's model: the frozen frame read under the pointer, the lens, the value's text and
//! what the pointer and the keys make of a pick.
//!
//! This is the pure half of the picker. A `Shot` holds the frame captured once at open and reads it
//! the way the output shows it: a pointer position in logical pixels is scaled to the frame's own
//! pixels (the scale read off the frame, so a fractional scale is exact) and turned back through the
//! output's rotation (`screencopy::buffer_pixel`). `Shot::lens` gives the 9 × 9 pixels around the
//! pointer, `Shot::paint` the frozen frame into the surface's buffer. A `Picker` is told about the
//! pointer and the keys (`PickInput`) and answers with `PickState`. `value_text` is the text copied.
//! The other half, `pick_app.rs`, owns the layer surface and draws.
//!
//! Never:
//! - read a clock, a file or the Wayland connection, or log: the caller hands in every event;
//! - read outside the frame: a position past the output is clamped to its edge;
//! - change its answer once it has given `Picked` or `Cancelled` (the end is latched).
//!
//! See: docs/features/captures.md and docs/decisions.md ("A capture never touches the clipboard
//! unless it succeeded").

use super::screencopy::{Frame, buffer_pixel, pixel, turned_a_quarter};
use alpheus_common::theme::Rgb;
use smithay_client_toolkit::reexports::client::protocol::wl_output::Transform;

/// The lens's side, in source pixels: the pointer's and four on each side.
pub const LENS: usize = 9;
/// Logical px per source pixel in the lens: 9 pixels make a 90 px lens.
pub const ZOOM: u32 = 10;
/// Logical px between the pointer and the lens, so the pixel being picked stays in view.
pub const LENS_GAP: i32 = 24;
/// The value's text when `[captures] pick_format` is left alone: `#RRGGBB`.
pub const DEFAULT_FORMAT: &str = "{hex}";

/// The frame captured at open, and how the output shows it.
pub struct Shot {
    frame: Frame,
    transform: Transform,
    /// The output's size in shown pixels: the frame's, turned a quarter for a rotated output.
    shown: (u32, u32),
    /// Shown pixels per logical pixel.
    scale: f64,
}

impl Shot {
    /// The shot of an output of `logical_size` (None: the compositor gave no xdg-output, and the
    /// integer `scale_factor` stands in, below 1 read as 1), turned by `transform`.
    pub fn new(frame: Frame, logical_size: Option<(u32, u32)>, scale_factor: i32, transform: Transform) -> Shot {
        let shown = if turned_a_quarter(transform) { (frame.height, frame.width) } else { (frame.width, frame.height) };
        let scale = match logical_size {
            Some((w, _)) if w > 0 && shown.0 > 0 => f64::from(shown.0) / f64::from(w),
            _ => f64::from(scale_factor.max(1)),
        };
        Shot { frame, transform, shown, scale }
    }

    /// The colour of the shown pixel (x, y), clamped to the output; black past a frame too short.
    fn shown_pixel(&self, x: i64, y: i64) -> Rgb {
        let x = x.clamp(0, i64::from(self.shown.0.max(1)) - 1);
        let y = y.clamp(0, i64::from(self.shown.1.max(1)) - 1);
        let (bx, by) = buffer_pixel(self.transform, (x, y), self.shown);
        match pixel(&self.frame, bx, by) {
            Some([r, g, b, _]) => Rgb(r, g, b),
            None => Rgb(0, 0, 0),
        }
    }

    /// The shown pixel under a logical position.
    pub fn under(&self, (x, y): (f64, f64)) -> (i64, i64) {
        ((x * self.scale).floor() as i64, (y * self.scale).floor() as i64)
    }

    /// The colour under the logical position `at`, clamped to the output.
    pub fn colour_at(&self, at: (f64, f64)) -> Rgb {
        let (x, y) = self.under(at);
        self.shown_pixel(x, y)
    }

    /// The 9 × 9 pixels around the logical position `at`, row by row, the pointer's at the centre;
    /// past an edge, the edge's pixels repeat.
    pub fn lens(&self, at: (f64, f64)) -> [Rgb; LENS * LENS] {
        let (x, y) = self.under(at);
        let half = (LENS / 2) as i64;
        let mut cells = [Rgb(0, 0, 0); LENS * LENS];
        for (i, cell) in cells.iter_mut().enumerate() {
            let (col, row) = ((i % LENS) as i64, (i / LENS) as i64);
            *cell = self.shown_pixel(x + col - half, y + row - half);
        }
        cells
    }

    /// The frozen frame painted into `px`, a premultiplied ARGB buffer of `width` × `height` px for a
    /// surface drawn at buffer scale `buffer_scale`, over the region (x0, y0, x1, y1) of the buffer
    /// (clipped to it). Opaque: bytes B, G, R, 255. A buffer too small for its size is left alone.
    pub fn paint(&self, px: &mut [u8], width: u32, height: u32, buffer_scale: u32, region: (i32, i32, i32, i32)) {
        if px.len() < width as usize * height as usize * 4 {
            return;
        }
        let (x0, x1) = (region.0.clamp(0, width as i32), region.2.clamp(0, width as i32));
        let (y0, y1) = (region.1.clamp(0, height as i32), region.3.clamp(0, height as i32));
        // A buffer pixel's centre, in shown pixels: the same pixel for an integer scale, the nearest
        // for a fractional one.
        let ratio = self.scale / f64::from(buffer_scale.max(1));
        let shown = |p: i32| ((f64::from(p) + 0.5) * ratio).floor() as i64;
        for row in y0..y1 {
            let sy = shown(row);
            let start = (row as usize * width as usize + x0 as usize) * 4;
            let end = start + (x1 - x0).max(0) as usize * 4;
            let Some(line) = px.get_mut(start..end) else { return };
            for (k, out) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let Rgb(r, g, b) = self.shown_pixel(shown(x0 + k as i32), sy);
                *out = [b, g, r, 255];
            }
        }
    }
}

/// The value's text: `format` with `{r}`, `{g}` and `{b}` (0 to 255) and `{hex}` (`#RRGGBB`, upper
/// case) filled in; anything else is kept as written.
pub fn value_text(colour: Rgb, format: &str) -> String {
    let Rgb(r, g, b) = colour;
    format
        .replace("{hex}", &format!("#{r:02X}{g:02X}{b:02X}"))
        .replace("{r}", &r.to_string())
        .replace("{g}", &g.to_string())
        .replace("{b}", &b.to_string())
}

/// What the surface tells the model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickInput {
    /// The pointer is at this logical position on the surface.
    Motion(f64, f64),
    /// A left click or Enter: the pixel under the pointer.
    Pick,
    /// Escape or a right click.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickState {
    Picking,
    /// Picked at this logical position.
    Picked((f64, f64)),
    Cancelled,
}

/// One pick in progress: where the pointer is, and how it ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Picker {
    pointer: Option<(f64, f64)>,
    end: PickState,
}

impl Default for Picker {
    fn default() -> Picker {
        Picker { pointer: None, end: PickState::Picking }
    }
}

impl Picker {
    /// Where the pointer is, once it has come over the surface.
    pub fn pointer(&self) -> Option<(f64, f64)> {
        self.pointer
    }

    /// Applies one input and gives the state after it. A pick before the pointer ever came over the
    /// surface has nothing to pick and is ignored.
    pub fn apply(&mut self, input: PickInput) -> PickState {
        if self.end != PickState::Picking {
            return self.end;
        }
        match input {
            PickInput::Motion(x, y) => self.pointer = Some((x, y)),
            PickInput::Pick => {
                if let Some(at) = self.pointer {
                    self.end = PickState::Picked(at);
                }
            }
            PickInput::Cancel => self.end = PickState::Cancelled,
        }
        self.end
    }
}

/// Where the lens block (`block`: width, height, logical px) goes for a pointer at `pointer` on an
/// output of `bounds`: right of and below the pointer, past `LENS_GAP`; left of it or above it when
/// it would leave the output; kept on the output as far as its size allows.
pub fn lens_origin(pointer: (i32, i32), block: (u32, u32), bounds: (u32, u32)) -> (i32, i32) {
    let (bw, bh) = (block.0 as i32, block.1 as i32);
    let (ow, oh) = (bounds.0 as i32, bounds.1 as i32);
    let place = |at: i32, size: i32, room: i32| {
        let after = at + LENS_GAP;
        let spot = if after + size <= room { after } else { at - LENS_GAP - size };
        spot.min(room - size).max(0)
    };
    (place(pointer.0, bw, ow), place(pointer.1, bh, oh))
}

#[cfg(test)]
mod tests;
