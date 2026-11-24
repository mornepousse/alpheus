//! The area selector's layer surface: a live dim over the focused output, the rubber band, a size
//! label and the keys.
//!
//! This is the Wayland half of the selector (the model is `select.rs`). One `Overlay` layer
//! surface covers the output, over the bar too, and takes the keyboard exclusively. It is dimmed
//! to a quarter, with the selection punched clear and a 2 px border in the `accent` role around
//! it; a `W×H` card in the launcher's font and card colours (`popup_bg`, `fg`, the frame) follows
//! the corner. The pointer is a crosshair, through SCTK's cursor-shape support. The pointer's
//! presses, motions and releases and the keys become `SelInput`; when the model answers `Done` or
//! `Cancelled` the surface is destroyed and `select_finished` is told. Everything lives in
//! `OpenSelect`, dropped whole on close: nothing of the selector stays resident.
//!
//! Never:
//! - capture anything: the selector's answer is an `Option<Rect>` in the output's logical pixels;
//! - stay alive after its answer, or draw when nothing changed;
//! - put a geometry, an output name or a key in a log line (`the_selector_logs_fixed_words_only`);
//! - keep a timer or a thread: it is driven by Wayland events alone.
//!
//! See: docs/features/captures.md and docs/decisions.md ("A capture never touches the clipboard
//! unless it succeeded").

use super::select::{SelInput, SelKey, SelState, Selection, label_origin};
use super::{Rect, SelectorAnswer};
use crate::actions::{self, Button};
use crate::grid::{Grid, Style, text_width};
use crate::keys::Mods;
use crate::launcher::surface::{CloseReason, FALLBACK_OUTPUT_SIZE, pool_size};
use crate::list;
use crate::log::write as log;
use crate::painter::FrameInk;
use crate::popup::{PopupClose, box_size};
use crate::state::Feed;
use crate::wayland::{App, Pick, any_buffer_free, pick_buffer_or_wait};
use alpheus_common::theme::Rgb;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use xkeysym::key;

const NAMESPACE: &str = "alpheus-selector";
/// Opacity of the dim, out of 255: a quarter.
const DIM_ALPHA: u8 = 64;
/// Logical px thickness of the border around the selection.
const BORDER: u32 = 2;

/// Why the selector closed, for the log: fixed words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectClose {
    /// The selection, as the model gave it.
    Done(Rect),
    Cancelled,
    FocusLost,
    OutputGone,
    Replaced,
    Error,
}

impl SelectClose {
    fn text(self) -> &'static str {
        match self {
            SelectClose::Done(_) => "done",
            SelectClose::Cancelled => "cancelled",
            SelectClose::FocusLost => "focus lost",
            SelectClose::OutputGone => "output gone",
            SelectClose::Replaced => "replaced",
            SelectClose::Error => "error",
        }
    }

    /// What this close tells a capture: only the user's gestures are a silent cancel.
    pub(crate) fn answer(self) -> SelectorAnswer {
        match self {
            SelectClose::Done(rect) => SelectorAnswer::Area(rect),
            SelectClose::Cancelled | SelectClose::FocusLost | SelectClose::Replaced => SelectorAnswer::Cancelled,
            SelectClose::OutputGone | SelectClose::Error => SelectorAnswer::Failed,
        }
    }
}

/// Everything one open selector holds; dropped whole on close.
pub(crate) struct OpenSelect {
    layer: LayerSurface,
    /// The selector's own pool, two frames of the output: dropped with it (a pool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// The output it covers: the focused one, or the compositor's choice.
    output: Option<wl_output::WlOutput>,
    /// Logical size of the surface: the output's, then the configure's.
    size: (u32, u32),
    scale: i32,
    configured: bool,
    selection: Selection,
    /// The rectangle and label last drawn: an unchanged frame is not repainted.
    last: Option<Option<Rect>>,
    /// A draw found both buffers held: drawn again after the release (`select_after_dispatch`).
    waiting: bool,
    shown_logged: bool,
    /// The generation of the capture that opened it (`Step::Select`); None for the debug verb's.
    /// Its answer goes to that capture only: a selector left open by a capture that was reset is
    /// never taken for the next one's.
    capture: Option<u64>,
    /// The label's card, painted aside then laid over the frame; kept to be reused.
    card: Vec<u8>,
}

/// Dim, selection, border and label painted into `px`, a premultiplied ARGB buffer of
/// `width` × `height` physical px. `area` is the selection in physical px (x, y, width, height).
pub(crate) fn paint_dim(px: &mut [u8], width: u32, height: u32, area: Option<(i32, i32, u32, u32)>, border: u32, accent: Rgb) {
    if px.len() < width as usize * height as usize * 4 {
        return; // inconsistent buffer: do nothing rather than panic
    }
    // Black at a quarter: premultiplied, so only the alpha byte is not zero.
    for pixel in px.as_chunks_mut::<4>().0 {
        *pixel = [0, 0, 0, DIM_ALPHA];
    }
    let Some((x, y, w, h)) = area else { return };
    let (x1, y1) = (x.saturating_add(w as i32), y.saturating_add(h as i32));
    let b = border as i32;
    let Rgb(r, g, bl) = accent;
    let ink = [bl, g, r, 0xff];
    // Four strips just outside the selection, so the captured pixels carry no ink of ours.
    let strips = [(x - b, y - b, x1 + b, y), (x - b, y1, x1 + b, y1 + b), (x - b, y, x, y1), (x1, y, x1 + b, y1)];
    fill_rect(px, width, height, (x, y, x1, y1), [0; 4]);
    for strip in strips {
        fill_rect(px, width, height, strip, ink);
    }
}

/// `value` over the pixels of the rectangle (x0, y0, x1, y1), clipped to the buffer.
pub(crate) fn fill_rect(px: &mut [u8], width: u32, height: u32, (x0, y0, x1, y1): (i32, i32, i32, i32), value: [u8; 4]) {
    let (x0, x1) = (x0.clamp(0, width as i32) as usize, x1.clamp(0, width as i32) as usize);
    let (y0, y1) = (y0.clamp(0, height as i32) as usize, y1.clamp(0, height as i32) as usize);
    if x0 >= x1 {
        return;
    }
    for row in y0..y1 {
        let start = (row * width as usize + x0) * 4;
        for pixel in px[start..start + (x1 - x0) * 4].as_chunks_mut::<4>().0 {
            *pixel = value;
        }
    }
}

/// `src` (`sw` × `sh`, premultiplied) laid over `dst` (`dw` × `dh`) with its top left at (x, y),
/// clipped to `dst`: the "over" operator, which keeps an opaque source exactly as it is.
pub(crate) fn lay_over(dst: &mut [u8], dw: u32, dh: u32, src: &[u8], sw: u32, sh: u32, (x, y): (i32, i32)) {
    if dst.len() < dw as usize * dh as usize * 4 || src.len() < sw as usize * sh as usize * 4 {
        return;
    }
    for sy in 0..sh as i32 {
        let ty = y + sy;
        if ty < 0 || ty >= dh as i32 {
            continue;
        }
        for sx in 0..sw as i32 {
            let tx = x + sx;
            if tx < 0 || tx >= dw as i32 {
                continue;
            }
            let s = (sy as usize * sw as usize + sx as usize) * 4;
            let d = (ty as usize * dw as usize + tx as usize) * 4;
            let inverse = 255 - u32::from(src[s + 3]);
            for k in 0..4 {
                let over = u32::from(src[s + k]) + (u32::from(dst[d + k]) * inverse + 127) / 255;
                dst[d + k] = over.min(255) as u8;
            }
        }
    }
}

/// The key a keysym is to the selector; None for any other key.
pub(crate) fn sel_key(sym: Keysym) -> Option<SelKey> {
    match sym.raw() {
        key::Escape => Some(SelKey::Escape),
        key::Return | key::KP_Enter => Some(SelKey::Enter),
        key::Left => Some(SelKey::Left),
        key::Right => Some(SelKey::Right),
        key::Up => Some(SelKey::Up),
        key::Down => Some(SelKey::Down),
        key::space => Some(SelKey::SpaceDown),
        _ => None,
    }
}

/// The model's input for a pointer event on the surface; None for events it has no use for.
/// A press of the right button is an Escape: the selector's gesture to cancel.
pub(crate) fn pointer_input(kind: &PointerEventKind, position: (f64, f64)) -> Option<SelInput> {
    let at = (position.0.floor() as i32, position.1.floor() as i32);
    match kind {
        PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => Some(SelInput::Motion(at.0, at.1)),
        PointerEventKind::Press { button, .. } => match actions::button(*button) {
            Some(Button::Left) => Some(SelInput::Press(at.0, at.1)),
            Some(Button::Right) => Some(SelInput::Key(SelKey::Escape, Mods::default())),
            _ => None,
        },
        PointerEventKind::Release { button, .. } if actions::button(*button) == Some(Button::Left) => Some(SelInput::Release),
        _ => None,
    }
}

/// Runs `f`, catching a panic: logs "capture error in <what>" — never the panic's message — and
/// returns None.
pub(super) fn guarded<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            log(&format!("capture error in {what}"));
            None
        }
    }
}

impl App {
    /// Opens the selector on the focused output (by name among the bar's outputs; unknown → the
    /// compositor's choice). Another selector, the launcher, the pad and any popup go first.
    /// `capture`: the generation of the capture that asked for it, whose flow gets its answer; the
    /// debug verb's selector (None) answers nobody.
    pub(crate) fn select_open(&mut self, qh: &QueueHandle<App>, capture: Option<u64>) {
        if self.select.is_some() {
            self.select_close(SelectClose::Replaced);
        }
        if self.launcher_is_open() {
            self.launcher_guarded("replaced", |app| app.launcher_close(CloseReason::Replaced));
        }
        if self.notes_is_open() {
            self.notes_guarded("replaced", |app| app.notes_close(CloseReason::Replaced));
        }
        self.popup_close(PopupClose::Launcher);
        let c = self.hover.cancel();
        self.hover_apply(c);
        // The same choice the whole-screen capture makes: the focused output, else the first one.
        let named = self.focused_output();
        let info = named.as_ref().and_then(|o| self.outputs.info(o));
        let scale = info.as_ref().map_or(1, |i| i.scale_factor).max(1);
        let size = info.as_ref().and_then(|i| i.logical_size).map_or(FALLBACK_OUTPUT_SIZE, |(w, h)| (w.max(1) as u32, h.max(1) as u32));
        let bytes = (size.0 * scale as u32) as usize * (size.1 * scale as u32) as usize * 4;
        let Ok(pool) = SlotPool::new(pool_size(bytes, 0).max(4096), &self.shm) else {
            log("capture: no shared buffer, selector not opened");
            self.select_finished(None, SelectorAnswer::Failed, capture);
            return;
        };
        let surface = self.compositor.create_surface(qh);
        let layer = self.layers.create_layer_surface(qh, surface, Layer::Overlay, Some(NAMESPACE), named.as_ref());
        // Anchored on the four edges with no size: the output's own. Exclusive zone -1: over the
        // bar, which a selection may include.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.select = Some(OpenSelect {
            layer,
            pool,
            buffers: Vec::new(),
            output: named,
            size,
            scale,
            configured: false,
            selection: Selection::new(size.0, size.1),
            last: None,
            waiting: false,
            shown_logged: false,
            capture,
            card: Vec::new(),
        });
        self.keyboard_acquire(qh);
        log("capture: selector opened");
    }

    /// The output the captures work on: the focused one (by name among the bar's outputs), else the
    /// first one the compositor listed; None with no output at all. The selector and the whole-screen
    /// capture share this choice, so "the screen" is the one the selector would cover.
    pub(crate) fn focused_output(&self) -> Option<wl_output::WlOutput> {
        let wanted = match &self.bar.compositor {
            Feed::Ready(c) => c.focused_output().map(str::to_owned),
            _ => None,
        };
        let named = wanted
            .as_deref()
            .and_then(|n| self.outputs.outputs().find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(n)));
        named.or_else(|| self.outputs.outputs().next())
    }

    /// `alpheus-launch --capture select`, the debug verb: opens the selector, or cancels it when it
    /// is already open. Nothing is captured: the selection is the surface's own.
    pub(crate) fn select_toggle(&mut self) {
        let qh = self.qh.clone();
        self.select_guarded("verb", |app| {
            if app.select_is_open() {
                app.select_close(SelectClose::Cancelled);
            } else {
                app.select_open(&qh, None);
            }
        });
    }

    pub(crate) fn select_is_open(&self) -> bool {
        self.select.is_some()
    }

    /// Runs a selector entry point; a panic closes it, the bar goes on.
    pub(crate) fn select_guarded<R>(&mut self, what: &str, f: impl FnOnce(&mut App) -> R) -> Option<R> {
        let r = guarded(what, || f(self));
        if r.is_none() {
            let _ = guarded("close", || self.select_close(SelectClose::Error));
        }
        r
    }

    /// Drops the surface, gives the keyboard back and the memory the frames held, then tells
    /// `select_finished` what came of it (`SelectClose::answer`): an area, a cancel or a failure.
    pub(crate) fn select_close(&mut self, reason: SelectClose) {
        let Some(open) = self.select.take() else { return };
        let output = open.output.clone();
        let capture = open.capture;
        drop(open);
        self.keyboard_release();
        log(&format!("capture: selector closed ({})", reason.text()));
        crate::memory::release_memory();
        self.select_finished(output, reason.answer(), capture);
    }

    /// The selector's answer, said in fixed words, then handed to the capture flow with the
    /// generation of the capture that opened it. The debug verb's selector, even one a capture's
    /// replaces, answers nobody; an earlier capture's, left open by a reset, carries its own
    /// generation, so its close is never taken for the new capture's cancel.
    fn select_finished(&mut self, output: Option<wl_output::WlOutput>, answer: SelectorAnswer, capture: Option<u64>) {
        match answer {
            SelectorAnswer::Area(_) => log("capture: area selected"),
            SelectorAnswer::Cancelled | SelectorAnswer::Failed => log("capture: nothing selected"),
        }
        if let Some(generation) = capture {
            self.capture_selected(output, generation, answer);
        }
    }

    /// A key press or repeat while the selector is open.
    pub(crate) fn select_key(&mut self, event: KeyEvent) {
        let Some(key) = sel_key(event.keysym) else { return };
        let mods = self.keyboard.mods;
        self.select_input(SelInput::Key(key, mods));
    }

    /// A key release: only Space's matters, the move it held ends.
    pub(crate) fn select_key_released(&mut self, event: KeyEvent) {
        if sel_key(event.keysym) == Some(SelKey::SpaceDown) {
            self.select_input(SelInput::Key(SelKey::SpaceUp, Mods::default()));
        }
    }

    /// Feeds the model, closes on its answer, else redraws.
    fn select_input(&mut self, input: SelInput) {
        let Some(open) = self.select.as_mut() else { return };
        match open.selection.apply(input) {
            SelState::Selecting => self.select_draw(),
            SelState::Done(rect) => self.select_close(SelectClose::Done(rect)),
            SelState::Cancelled => self.select_close(SelectClose::Cancelled),
        }
    }

    /// Pointer events on the selector's surface; false: not ours.
    pub(crate) fn select_pointer(&mut self, conn: &Connection, ev: &PointerEvent) -> bool {
        let Some(open) = self.select.as_ref() else { return false };
        if open.layer.wl_surface() != &ev.surface {
            return false;
        }
        if matches!(ev.kind, PointerEventKind::Enter { .. })
            && let Some(p) = &self.pointer
        {
            let _ = p.set_cursor(conn, CursorIcon::Crosshair);
        }
        if let Some(input) = pointer_input(&ev.kind, ev.position) {
            self.select_input(input);
        }
        true
    }

    /// Whether `surface` is the open selector's: checked outside any guard.
    pub(crate) fn select_owns_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        self.select.as_ref().is_some_and(|o| o.layer.wl_surface() == surface)
    }

    /// A configure for the selector's surface (the caller checked `select_owns_surface`): the size
    /// the compositor gave is the output's.
    pub(crate) fn select_configured(&mut self, size: (u32, u32)) {
        let Some(open) = self.select.as_mut() else { return };
        let granted = (if size.0 == 0 { open.size.0 } else { size.0 }, if size.1 == 0 { open.size.1 } else { size.1 });
        if granted != open.size {
            open.size = granted;
            open.selection = Selection::new(granted.0, granted.1);
        }
        open.configured = true;
        open.last = None;
        self.select_draw();
    }

    /// The compositor closed the selector's surface: nothing is selected.
    pub(crate) fn select_layer_closed(&mut self) {
        self.select_close(SelectClose::OutputGone);
    }

    /// An output went away: the selector's → close.
    pub(crate) fn select_output_gone(&mut self, output: &wl_output::WlOutput) {
        if self.select.as_ref().is_some_and(|o| o.output.as_ref() == Some(output)) {
            self.select_close(SelectClose::OutputGone);
        }
    }

    /// The keyboard focus left the selector's surface: it is closed, nothing selected.
    pub(crate) fn select_focus_lost(&mut self) {
        self.select_close(SelectClose::FocusLost);
    }

    /// Scale change of the surface (the caller checked `select_owns_surface`).
    pub(crate) fn select_scale(&mut self, factor: i32) {
        let Some(open) = self.select.as_mut() else { return };
        if open.scale != factor {
            open.scale = factor;
            open.last = None;
            self.select_draw();
        }
    }

    /// Once per pass of the loop: a draw that waited for a buffer is done once one is free.
    pub(crate) fn select_after_dispatch(&mut self) {
        if self.select.as_ref().is_some_and(|o| o.waiting && any_buffer_free(&o.buffers)) {
            self.select_draw();
        }
    }

    /// The surface redrawn even if nothing changed (a new theme).
    pub(crate) fn select_repaint(&mut self) {
        if let Some(open) = self.select.as_mut() {
            open.last = None;
        }
        self.select_draw();
    }

    fn select_draw(&mut self) {
        let height = self.bar.config.height;
        let App { select, fonts, roles, .. } = self;
        let Some(open) = select.as_mut() else { return };
        if !open.configured {
            return;
        }
        let rect = open.selection.rect();
        if open.last == Some(rect) {
            open.waiting = false;
            return;
        }
        let e = open.scale.max(1) as u32;
        let (w, h) = (open.size.0 * e, open.size.1 * e);
        let OpenSelect { buffers, pool, layer, waiting, card, selection, size, .. } = open;
        let used = match pick_buffer_or_wait(buffers, pool, w, h) {
            Pick::Buffer(i) => i,
            Pick::Wait => {
                *waiting = true;
                return;
            }
            Pick::Failed => return,
        };
        *waiting = false;
        let Some((buffer, _, _)) = buffers.get(used) else { return };
        let Some(px) = buffer.canvas(pool) else {
            log("capture: selector buffer unexpectedly busy");
            return;
        };
        let physical = rect.map(|r| (r.x * e as i32, r.y * e as i32, r.width * e, r.height * e));
        paint_dim(px, w, h, physical, BORDER * e, roles.accent);
        if let (Some(r), Some(text)) = (rect, selection.label()) {
            // The card: the launcher's frame and colours around the `W×H` text, painted aside.
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
            let (ox, oy) = label_origin(r, (lw, lh), *size);
            lay_over(px, w, h, card, cw, ch, (ox * e as i32, oy * e as i32));
        }
        let surface = layer.wl_surface();
        surface.set_buffer_scale(e as i32);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_err() {
            return;
        }
        layer.commit();
        open.last = Some(rect);
        if !open.shown_logged {
            open.shown_logged = true;
            log("capture: selector shown");
        }
    }
}

#[cfg(test)]
mod tests;
