//! The colour picker's layer surface: the focused output frozen as it was captured, a zoom lens that
//! follows the pointer, a swatch and the `#RRGGBB` of the pixel under it.
//!
//! This is the Wayland half of the picker (the model is `pick.rs`). One `Overlay` layer surface
//! covers the output, over the bar too, and takes the keyboard exclusively. It shows the frame the
//! capture took at open (`Shot`), so moving content stands still while picking. Beside the pointer
//! go the lens (9 × 9 pixels, each `ZOOM` logical px wide, in a 2 px `accent` frame, the pointer's
//! pixel outlined), a swatch of its colour and a card in the launcher's font and card colours with
//! its `#RRGGBB`. The pointer is a crosshair, through SCTK's cursor-shape support. A left click or
//! Enter picks, Escape or a right click cancels; on either the surface is destroyed and the capture
//! is told (`capture_picked`). Everything, the frame included, lives in `OpenPick`, dropped whole on
//! close: nothing of the picker stays resident.
//!
//! Each draw repaints from the frame only what the last overlay of that buffer covered, then draws
//! the new overlay: a motion costs the lens's pixels, not the output's.
//!
//! Never:
//! - capture anything: the frame comes with the open, from the capture's one screencopy;
//! - stay alive after its answer, or draw when the pixel under the pointer did not change;
//! - put a colour, a position, an output name or a key in a log line (`the_picker_logs_fixed_words_only`);
//! - keep a timer or a thread: it is driven by Wayland events alone.
//!
//! See: docs/features/captures.md and docs/decisions.md ("A capture never touches the clipboard
//! unless it succeeded").

use super::PickAnswer;
use super::pick::{DEFAULT_FORMAT, LENS, PickInput, PickState, Picker, Shot, ZOOM, lens_origin, value_text};
use super::screencopy::Frame;
use super::select::LABEL_GAP;
use super::select_app::{fill_rect, guarded, lay_over};
use crate::actions::{self, Button};
use crate::grid::{Grid, Style, text_width};
use crate::launcher::surface::{CloseReason, FALLBACK_OUTPUT_SIZE, pool_size};
use crate::list;
use crate::log::write as log;
use crate::painter::FrameInk;
use crate::popup::{PopupClose, box_size};
use crate::wayland::{App, Pick, any_buffer_free, pick_buffer_or_wait};
use alpheus_common::theme::Rgb;
use smithay_client_toolkit::reexports::client::Connection;
use smithay_client_toolkit::reexports::client::protocol::wl_output::{self, Transform};
use smithay_client_toolkit::reexports::client::protocol::{wl_buffer, wl_surface};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use xkeysym::key;

const NAMESPACE: &str = "alpheus-picker";
/// Logical px thickness of the lens's and the swatch's frames.
const BORDER: u32 = 2;

/// An area of a buffer, in physical px: left, top, right, bottom (the right and bottom excluded).
type Area = (i32, i32, i32, i32);

/// Why the picker closed, for the log: fixed words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickClose {
    /// The colour picked, as the frame holds it.
    Picked(Rgb),
    Cancelled,
    FocusLost,
    OutputGone,
    Replaced,
    Error,
}

impl PickClose {
    fn text(self) -> &'static str {
        match self {
            PickClose::Picked(_) => "picked",
            PickClose::Cancelled => "cancelled",
            PickClose::FocusLost => "focus lost",
            PickClose::OutputGone => "output gone",
            PickClose::Replaced => "replaced",
            PickClose::Error => "error",
        }
    }

    /// What this close tells the capture: only the user's gestures are a silent cancel.
    pub(crate) fn answer(self) -> PickAnswer {
        match self {
            PickClose::Picked(colour) => PickAnswer::Colour(colour),
            PickClose::Cancelled | PickClose::FocusLost | PickClose::Replaced => PickAnswer::Cancelled,
            PickClose::OutputGone | PickClose::Error => PickAnswer::Failed,
        }
    }
}

/// Everything one open picker holds, the frozen frame included; dropped whole on close.
pub(crate) struct OpenPick {
    layer: LayerSurface,
    /// The picker's own pool, two frames of the output: dropped with it (a pool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// For each buffer drawn into, the area its overlay covers (None: none yet); a buffer not listed
    /// holds no frame yet and is painted whole.
    overlays: Vec<(wl_buffer::WlBuffer, Option<Area>)>,
    /// The output it covers: the one the frame was taken of.
    output: Option<wl_output::WlOutput>,
    /// Logical size of the surface: the output's, then the configure's.
    size: (u32, u32),
    scale: i32,
    configured: bool,
    picker: Picker,
    /// The frame the capture took at open.
    shot: Shot,
    /// What the last draw showed (the pointer's logical pixel); an unchanged frame is not repainted.
    last: Option<Option<(i32, i32)>>,
    /// A draw found both buffers held: drawn again after the release (`pick_after_dispatch`).
    waiting: bool,
    shown_logged: bool,
    /// The generation of the capture that opened it: its answer goes to that capture only.
    capture: u64,
    /// The value's card, painted aside then laid over the frame; kept to be reused.
    card: Vec<u8>,
}

/// The lens painted into `px` (premultiplied ARGB, `width` × `height` physical px): `cells` (row by
/// row, `LENS` a side), each a square of `cell` px, its top left at (x0, y0), framed `border` px
/// outside in `accent`; the centre cell outlined in white or black, whichever stands out from it.
/// Returns the area it covers, frame included. A buffer too small for its size is left alone.
pub(crate) fn paint_lens(
    px: &mut [u8],
    width: u32,
    height: u32,
    (x0, y0, cell): (i32, i32, u32),
    cells: &[Rgb; LENS * LENS],
    border: u32,
    accent: Rgb,
) -> Area {
    let (c, b, side) = (cell as i32, border as i32, (cell as usize * LENS) as i32);
    let area = (x0 - b, y0 - b, x0 + side + b, y0 + side + b);
    if px.len() < width as usize * height as usize * 4 {
        return area;
    }
    fill_rect(px, width, height, area, ink(accent));
    for (i, colour) in cells.iter().enumerate() {
        let (cx, cy) = (x0 + (i % LENS) as i32 * c, y0 + (i / LENS) as i32 * c);
        fill_rect(px, width, height, (cx, cy, cx + c, cy + c), ink(*colour));
    }
    // The centre cell: an outline a tenth of the cell thick, at least 1 px, inside it.
    let centre = cells[LENS * LENS / 2];
    let mark = if luminance(centre) > 127 { [0, 0, 0, 255] } else { [255, 255, 255, 255] };
    let (cx, cy) = (x0 + (LENS / 2) as i32 * c, y0 + (LENS / 2) as i32 * c);
    let t = (c / 10).max(1);
    for strip in [(cx, cy, cx + c, cy + t), (cx, cy + c - t, cx + c, cy + c), (cx, cy, cx + t, cy + c), (cx + c - t, cy, cx + c, cy + c)] {
        fill_rect(px, width, height, strip, mark);
    }
    area
}

/// The swatch: a square of `colour`, (x, y, side) in physical px, framed `border` px outside in
/// `accent`. Returns the area it covers.
pub(crate) fn paint_swatch(
    px: &mut [u8],
    width: u32,
    height: u32,
    (x, y, side): (i32, i32, u32),
    colour: Rgb,
    border: u32,
    accent: Rgb,
) -> Area {
    let (s, b) = (side as i32, border as i32);
    let area = (x - b, y - b, x + s + b, y + s + b);
    fill_rect(px, width, height, area, ink(accent));
    fill_rect(px, width, height, (x, y, x + s, y + s), ink(colour));
    area
}

/// An opaque colour in the buffer's byte order.
fn ink(Rgb(r, g, b): Rgb) -> [u8; 4] {
    [b, g, r, 255]
}

/// The colour's brightness, 0 to 255 (Rec. 601 weights, in integers).
fn luminance(Rgb(r, g, b): Rgb) -> u32 {
    (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000
}

/// The smallest area holding both; None when neither is there.
pub(crate) fn union(a: Option<Area>, b: Option<Area>) -> Option<Area> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (one, None) | (None, one) => one,
    }
}

/// The model's input for a key; None for any key the picker does not answer to.
pub(crate) fn pick_key(sym: Keysym) -> Option<PickInput> {
    match sym.raw() {
        key::Escape => Some(PickInput::Cancel),
        key::Return | key::KP_Enter => Some(PickInput::Pick),
        _ => None,
    }
}

/// The model's input for a pointer event on the surface; None for events it has no use for.
pub(crate) fn pick_pointer_input(kind: &PointerEventKind, position: (f64, f64)) -> Option<PickInput> {
    match kind {
        PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => Some(PickInput::Motion(position.0, position.1)),
        PointerEventKind::Press { button, .. } => match actions::button(*button) {
            Some(Button::Left) => Some(PickInput::Pick),
            Some(Button::Right) => Some(PickInput::Cancel),
            _ => None,
        },
        _ => None,
    }
}

impl App {
    /// Opens the picker over the output capture `generation` took `frame` of. The launcher, the pad,
    /// the selector and any popup go first. A picker that cannot open fails the capture (critical);
    /// the frame goes with it.
    pub(crate) fn pick_open(&mut self, frame: Frame, generation: u64) {
        if self.launcher_is_open() {
            self.launcher_guarded("replaced", |app| app.launcher_close(CloseReason::Replaced));
        }
        if self.notes_is_open() {
            self.notes_guarded("replaced", |app| app.notes_close(CloseReason::Replaced));
        }
        self.select_close(super::select_app::SelectClose::Replaced);
        self.popup_close(PopupClose::Launcher);
        let c = self.hover.cancel();
        self.hover_apply(c);
        let output = self.capture.output();
        let info = output.as_ref().and_then(|o| self.outputs.info(o));
        let scale = info.as_ref().map_or(1, |i| i.scale_factor).max(1);
        let logical = info.as_ref().and_then(|i| i.logical_size).map(|(w, h)| (w.max(1) as u32, h.max(1) as u32));
        let transform = info.as_ref().map_or(Transform::Normal, |i| i.transform);
        let size = logical.unwrap_or(FALLBACK_OUTPUT_SIZE);
        let bytes = (size.0 * scale as u32) as usize * (size.1 * scale as u32) as usize * 4;
        let Ok(pool) = SlotPool::new(pool_size(bytes, 0).max(4096), &self.shm) else {
            log("capture: no shared buffer, picker not opened");
            drop(frame);
            self.capture_picked(generation, PickAnswer::Failed);
            return;
        };
        let shot = Shot::new(frame, logical, scale, transform);
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let layer = self.layers.create_layer_surface(&qh, surface, Layer::Overlay, Some(NAMESPACE), output.as_ref());
        // Anchored on the four edges with no size: the output's own. Exclusive zone -1: over the
        // bar, which is in the frame and may be picked from.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.pick = Some(OpenPick {
            layer,
            pool,
            buffers: Vec::new(),
            overlays: Vec::new(),
            output,
            size,
            scale,
            configured: false,
            picker: Picker::default(),
            shot,
            last: None,
            waiting: false,
            shown_logged: false,
            capture: generation,
            card: Vec::new(),
        });
        self.keyboard_acquire(&qh);
        log("capture: picker opened");
    }

    pub(crate) fn pick_is_open(&self) -> bool {
        self.pick.is_some()
    }

    /// Runs a picker entry point; a panic closes it (a failure of its capture), the bar goes on.
    pub(crate) fn pick_guarded<R>(&mut self, what: &str, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = guarded(what, || f(self));
        if r.is_none() {
            let _ = guarded("close", || self.pick_close(PickClose::Error));
        }
        r
    }

    /// Drops the surface and the frame, gives the keyboard back and the memory they held, then tells
    /// the capture what came of it (`PickClose::answer`): a colour, a cancel or a failure.
    pub(crate) fn pick_close(&mut self, reason: PickClose) {
        let Some(open) = self.pick.take() else { return };
        let capture = open.capture;
        drop(open);
        self.keyboard_release();
        log(&format!("capture: picker closed ({})", reason.text()));
        crate::memory::release_memory();
        self.capture_picked(capture, reason.answer());
    }

    /// A key press or repeat while the picker is open.
    pub(crate) fn pick_key(&mut self, event: KeyEvent) {
        if let Some(input) = pick_key(event.keysym) {
            self.pick_input(input);
        }
    }

    /// Feeds the model, closes on its answer, else redraws.
    fn pick_input(&mut self, input: PickInput) {
        let Some(open) = self.pick.as_mut() else { return };
        match open.picker.apply(input) {
            PickState::Picking => self.pick_draw(),
            PickState::Picked(at) => {
                let colour = open.shot.colour_at(at);
                self.pick_close(PickClose::Picked(colour));
            }
            PickState::Cancelled => self.pick_close(PickClose::Cancelled),
        }
    }

    /// Pointer events on the picker's surface; false: not ours.
    pub(crate) fn pick_pointer(&mut self, conn: &Connection, ev: &PointerEvent) -> bool {
        let Some(open) = self.pick.as_ref() else { return false };
        if open.layer.wl_surface() != &ev.surface {
            return false;
        }
        if matches!(ev.kind, PointerEventKind::Enter { .. })
            && let Some(p) = &self.pointer
        {
            let _ = p.set_cursor(conn, CursorIcon::Crosshair);
        }
        if let Some(input) = pick_pointer_input(&ev.kind, ev.position) {
            self.pick_input(input);
        }
        true
    }

    /// Whether `surface` is the open picker's: checked outside any guard.
    pub(crate) fn pick_owns_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        self.pick.as_ref().is_some_and(|o| o.layer.wl_surface() == surface)
    }

    /// A configure for the picker's surface (the caller checked `pick_owns_surface`).
    pub(crate) fn pick_configured(&mut self, size: (u32, u32)) {
        let Some(open) = self.pick.as_mut() else { return };
        open.size = (if size.0 == 0 { open.size.0 } else { size.0 }, if size.1 == 0 { open.size.1 } else { size.1 });
        open.configured = true;
        open.last = None;
        self.pick_draw();
    }

    /// The compositor closed the picker's surface: a failure, nothing picked.
    pub(crate) fn pick_layer_closed(&mut self) {
        self.pick_close(PickClose::OutputGone);
    }

    /// An output went away: the picker's → close.
    pub(crate) fn pick_output_gone(&mut self, output: &wl_output::WlOutput) {
        if self.pick.as_ref().is_some_and(|o| o.output.as_ref() == Some(output)) {
            self.pick_close(PickClose::OutputGone);
        }
    }

    /// The keyboard focus left the picker's surface: it is closed, nothing picked.
    pub(crate) fn pick_focus_lost(&mut self) {
        self.pick_close(PickClose::FocusLost);
    }

    /// Scale change of the surface (the caller checked `pick_owns_surface`).
    pub(crate) fn pick_scale(&mut self, factor: i32) {
        let Some(open) = self.pick.as_mut() else { return };
        if open.scale != factor {
            open.scale = factor;
            open.last = None;
            self.pick_draw();
        }
    }

    /// Once per pass of the loop: a draw that waited for a buffer is done once one is free.
    pub(crate) fn pick_after_dispatch(&mut self) {
        if self.pick.as_ref().is_some_and(|o| o.waiting && any_buffer_free(&o.buffers)) {
            self.pick_draw();
        }
    }

    /// The surface redrawn even if nothing changed (a new theme).
    pub(crate) fn pick_repaint(&mut self) {
        if let Some(open) = self.pick.as_mut() {
            open.last = None;
        }
        self.pick_draw();
    }

    fn pick_draw(&mut self) {
        let height = self.bar.config.height;
        let App { pick, fonts, roles, .. } = self;
        let Some(open) = pick.as_mut() else { return };
        if !open.configured {
            return;
        }
        let pointer = open.picker.pointer();
        let shown = pointer.map(|(x, y)| (x.floor() as i32, y.floor() as i32));
        if open.last == Some(shown) {
            open.waiting = false;
            return;
        }
        let e = open.scale.max(1) as u32;
        let (w, h) = (open.size.0 * e, open.size.1 * e);
        let OpenPick { buffers, pool, layer, waiting, card, shot, size, overlays, .. } = open;
        let used = match pick_buffer_or_wait(buffers, pool, w, h) {
            Pick::Buffer(i) => i,
            Pick::Wait => {
                *waiting = true;
                return;
            }
            Pick::Failed => return,
        };
        *waiting = false;
        // The overlays of buffers gone (a resize, a third one dropped) are forgotten with them.
        overlays.retain(|(b, _)| buffers.iter().any(|(kept, _, _)| kept.wl_buffer() == b));
        let Some((buffer, _, _)) = buffers.get(used) else { return };
        let Some(px) = buffer.canvas(pool) else {
            log("capture: picker buffer unexpectedly busy");
            return;
        };
        let whole = (0, 0, w as i32, h as i32);
        // What this buffer showed before: the frame and an overlay to wipe, or nothing yet.
        let before = overlays.iter().find(|(b, _)| b == buffer.wl_buffer()).map(|(_, area)| *area);
        match before {
            None => shot.paint(px, w, h, e, whole),
            Some(Some(area)) => shot.paint(px, w, h, e, area),
            Some(None) => {}
        }
        let mut drawn = None;
        if let (Some(at), Some((px_x, px_y))) = (pointer, shown) {
            let colour = shot.colour_at(at);
            let cells = shot.lens(at);
            // The card: the launcher's frame and colours around the `#RRGGBB` text, painted aside.
            let text = value_text(colour, DEFAULT_FORMAT);
            let columns = text_width(&text) + 2;
            let mut grid = Grid::empty(text_width(&text));
            grid.write(0, text_width(&text), &text, Style::NORMAL);
            let boxed = list::padded(&[grid]);
            let scale = e as i32;
            let advance = fonts.advance(scale);
            let frame = fonts.frame(scale);
            let (lw, lh) = box_size(columns, 1, advance, height * e, scale, &frame);
            let (cw, ch) = (lw * e, lh * e);
            card.clear();
            card.resize(cw as usize * ch as usize * 4, 0);
            fonts.paint_boxed(&boxed, roles, card, cw, ch, height * e, scale, FrameInk::Border);
            // The block: the lens, and under it the swatch (as tall as the card) and the card.
            let lens = LENS as u32 * ZOOM;
            let gap = LABEL_GAP as u32;
            let block = (lens.max(lh + gap + lw), lens + gap + lh);
            let (ox, oy) = lens_origin((px_x, px_y), block, *size);
            let (pe, ie) = (e as i32, (lens + gap) as i32);
            let lens_area = paint_lens(px, w, h, (ox * pe, oy * pe, ZOOM * e), &cells, BORDER * e, roles.accent);
            let swatch = paint_swatch(px, w, h, (ox * pe, (oy + ie) * pe, lh * e), colour, e, roles.accent);
            let card_at = ((ox + (lh + gap) as i32) * pe, (oy + ie) * pe);
            lay_over(px, w, h, card, cw, ch, card_at);
            let card_area = (card_at.0, card_at.1, card_at.0 + cw as i32, card_at.1 + ch as i32);
            drawn = union(union(Some(lens_area), Some(swatch)), Some(card_area));
        }
        let damage = match before {
            None => Some(whole),
            Some(area) => union(area, drawn),
        };
        let surface = layer.wl_surface();
        surface.set_buffer_scale(e as i32);
        if let Some((x0, y0, x1, y1)) = damage {
            surface.damage_buffer(x0, y0, x1 - x0, y1 - y0);
        }
        let mark = buffer.wl_buffer().clone();
        if buffer.attach_to(surface).is_err() {
            return;
        }
        layer.commit();
        overlays.retain(|(b, _)| b != &mark);
        overlays.push((mark, drawn));
        open.last = Some(shown);
        if !open.shown_logged {
            open.shown_logged = true;
            log("capture: picker shown");
        }
    }
}

#[cfg(test)]
mod tests;
