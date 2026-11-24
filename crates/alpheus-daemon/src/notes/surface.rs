//! The pad's Wayland side, kept thin and shaped like the launcher's (launcher/surface.rs): one
//! centred `Overlay` layer surface with exclusive keyboard focus (the box), one transparent scrim
//! per output on the same layer to catch a click outside, the App's keyboard, buffers from a pool
//! of its own coalesced while the compositor holds both. Everything lives in `OpenNotes`, dropped
//! whole on close: nothing of the pad stays resident.

use crate::actions::{self, Button};
use crate::launcher::host::read_config;
use crate::launcher::scan::ScanEnv;
use crate::launcher::surface::{
    CloseReason, FALLBACK_OUTPUT_SIZE, Redraw, Scrim, adds_scrim, output_cells, owns_surface, plan_configure, plan_redraw, pool_size,
};
use crate::list::Boxed;
use crate::log::write as log;
use crate::notes::app::{Fingerprint, opening_model, repeat_ignored};
use crate::notes::keys;
use crate::notes::model::{Notes, NotesOutcome};
use crate::notes::store;
use crate::notes::view::{self, NotesLayout};
use crate::painter::{self, Frame, FrameInk};
use crate::popup::{PopupClose, box_size, cell_under_fitted};
use crate::state::Feed;
use crate::wayland::{App, Pick, any_buffer_free, pick_buffer_or_wait};
use alpheus_common::paths;
use smithay_client_toolkit::data_device_manager::data_device::DataDevice;
use smithay_client_toolkit::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{KeyboardInteractivity, Layer, LayerSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use std::path::PathBuf;
use std::time::Instant;

const NAMESPACE: &str = "alpheus-notes";
const SCRIM_NAMESPACE: &str = "alpheus-notes-scrim";
/// Rows the wheel scrolls per notch.
const WHEEL_ROWS: i32 = 3;

/// The box's logical size: the layout's cells without its ring of one blank cell, plus the pixel
/// frame's room around them (as the launcher's `layout_size`).
pub(crate) fn notes_layout_size(layout: &NotesLayout, advance_phys: f32, row_height: u32, scale: i32, frame: &Frame) -> (u32, u32) {
    box_size(layout.columns.saturating_sub(2), layout.rows.saturating_sub(2), advance_phys, row_height, scale, frame)
}

/// The layout refitted inside a size the compositor imposed (logical px), as the launcher's
/// `layout_within`: the cells the pixel frame leaves, `view::layout`'s ring added back.
pub(crate) fn notes_layout_within(size: (u32, u32), advance: f32, row_height: u32, scale: i32, frame: &Frame) -> NotesLayout {
    let e = scale.max(1) as u32;
    let (extra_w, extra_h) = frame.extra();
    let columns = painter::columns((size.0 * e).saturating_sub(extra_w), advance);
    let rows = ((size.1 * e).saturating_sub(extra_h)).checked_div(row_height).map_or(0, |r| r.min(u32::from(u16::MAX)) as u16);
    view::layout(columns.saturating_add(4), rows.saturating_add(2))
}

/// The layout for a scale change: inside the compositor's imposed size if there is one, else fitted
/// to the whole output; with the logical size to show it at.
fn notes_resized_layout(
    imposed: Option<(u32, u32)>,
    output_size: (u32, u32),
    advance: f32,
    row_height: u32,
    scale: i32,
    frame: &Frame,
) -> (NotesLayout, (u32, u32)) {
    match imposed {
        Some(size) => (notes_layout_within(size, advance, row_height, scale, frame), size),
        None => {
            let (columns, rows) = output_cells(output_size.0, output_size.1, advance, row_height, scale);
            let layout = view::layout(columns, rows);
            (layout, notes_layout_size(&layout, advance, row_height, scale, frame))
        }
    }
}

/// Bytes the pad's pool carries: two frames of its own box (`pool_size`) and a scrim share per
/// output.
pub(crate) fn notes_pool_bytes(size: (u32, u32), scale: i32, outputs: usize) -> usize {
    let e = scale.max(1) as usize;
    pool_size(size.0 as usize * e * size.1 as usize * e * 4, outputs)
}

/// The layout's cell (column, row), ring included (`view::hit`'s coordinates), under a pointer at
/// `position` (logical px) on the box; None in the frame's margin.
pub(crate) fn notes_hit_cell(
    position: (f64, f64),
    scale: i32,
    advance: f32,
    row_height: u32,
    frame: &Frame,
    layout: &NotesLayout,
    size: (u32, u32),
) -> Option<(u16, u16)> {
    let cells = (layout.columns.saturating_sub(2), layout.rows.saturating_sub(2));
    cell_under_fitted(position.0, position.1, scale, advance, row_height, frame, cells, size).map(|(column, row)| (column + 1, row + 1))
}

/// Everything one open pad holds; dropped whole on close.
pub struct OpenNotes {
    pub model: Notes,
    pub requested_at: Instant,
    /// The first unsaved edit: the save is due `SAVE_AFTER` later (`store::save_deadline`).
    pub dirty_since: Option<Instant>,
    /// The file as last loaded or saved (`Fingerprint`), kept if a close cannot save.
    pub(crate) seen: Fingerprint,
    /// launcher.toml's `copy`, read at open.
    pub copy: Vec<String>,
    /// `$PATH`, read at open: the copy program is looked up there.
    pub path_dirs: Vec<PathBuf>,
    /// The seat's data device, for the selection offer Ctrl+V reads (created before the first
    /// commit, so the compositor's selection reaches the box with its keyboard focus).
    pub(crate) data_device: Option<DataDevice>,
    /// A paste being read: its pipe is a source of the loop until it ends (`notes/paste.rs`).
    pub(crate) paste: Option<(LoopHandle<'static, App>, RegistrationToken)>,
    box_layer: LayerSurface,
    scrims: Vec<Scrim>,
    /// The pad's own pool: dropped with it (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// Decided at open; changed only by a scale change or an imposed configure size.
    layout: NotesLayout,
    /// Logical size of the box (asked, or imposed by a configure).
    size: (u32, u32),
    /// The compositor's own last-imposed size, kept across scale changes.
    imposed_size: Option<(u32, u32)>,
    /// Some: the focused output, found by name; None: the compositor's choice.
    output: Option<wl_output::WlOutput>,
    output_name: Option<String>,
    /// Logical size of the output the layout is computed for.
    output_size: (u32, u32),
    scale: i32,
    configured: bool,
    shown_logged: bool,
    /// Rows last committed: an unchanged frame is not repainted.
    last: Option<Boxed>,
    /// A draw found both buffers held: drawn again after the release (`notes_after_dispatch`).
    waiting: bool,
}

impl Drop for OpenNotes {
    fn drop(&mut self) {
        self.stop_paste();
        // SCTK's `DataDevice::drop` only releases the device: the offers it still holds would leak.
        // `release` exists from `wl_data_device` v2: on a compositor offering only v1 the device
        // object stays on the server, one per open, until the daemon disconnects (wlroots
        // compositors, Sway and Mango among them, offer v3).
        if let Some(device) = self.data_device.as_ref() {
            if let Some(offer) = device.data().selection_offer() {
                offer.destroy();
            }
            if let Some(offer) = device.data().drag_offer() {
                offer.destroy();
            }
        }
    }
}

impl OpenNotes {
    /// Drops a paste still being read: its pipe leaves the loop and closes.
    pub(crate) fn stop_paste(&mut self) {
        if let Some((handle, token)) = self.paste.take() {
            handle.remove(token);
        }
    }

    /// The first edit since the last save starts the save's clock; later ones never push it back.
    pub(crate) fn stamp_dirty(&mut self) {
        if self.model.dirty && self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    /// The model's wrap and rows follow the layout drawn (the `set_area` contract).
    fn fit_model(&mut self) {
        self.model.set_area(view::wrap_width(&self.layout), usize::from(self.layout.text_rows));
    }
}

impl App {
    /// Creates the box on the focused output (by name among the bar's outputs; unknown → the
    /// compositor's choice) and the scrims; reads the notes file; logs "notes shown on <output>
    /// (<w>×<h> px) in <µs> µs" at the first commit.
    pub(crate) fn notes_open(&mut self, qh: &QueueHandle<App>, requested_at: Instant) {
        // One of the two at a time: an open launcher goes (a waiting dmenu client: cancelled).
        if self.launcher_is_open() {
            self.launcher_guarded("replaced", |app| app.launcher_close(CloseReason::Replaced));
        }
        self.select_close(crate::captures::select_app::SelectClose::Replaced);
        self.popup_close(PopupClose::Launcher);
        self.menu.last_icon = None;
        let c = self.hover.cancel();
        self.hover_apply(c);
        let wanted = match &self.bar.compositor {
            Feed::Ready(c) => c.focused_output().map(str::to_owned),
            _ => None,
        };
        let named = wanted
            .as_deref()
            .and_then(|n| self.outputs.outputs().find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(n)));
        let output_name = named.as_ref().and(wanted);
        let info = named.clone().or_else(|| self.outputs.outputs().next()).and_then(|o| self.outputs.info(&o));
        let scale = info.as_ref().map_or(1, |i| i.scale_factor).max(1);
        let output_size =
            info.as_ref().and_then(|i| i.logical_size).map_or(FALLBACK_OUTPUT_SIZE, |(w, h)| (w.max(1) as u32, h.max(1) as u32));
        let row_height = self.bar.config.height * scale as u32;
        let advance = self.fonts.advance(scale);
        let (columns, rows) = output_cells(output_size.0, output_size.1, advance, row_height, scale);
        let layout = view::layout(columns, rows);
        let size = notes_layout_size(&layout, advance, row_height, scale, &self.fonts.frame(scale));
        let outputs = self.outputs.outputs().count();
        let pool = match SlotPool::new(notes_pool_bytes(size, scale, outputs).max(4096), &self.shm) {
            Ok(p) => p,
            Err(e) => {
                log(&format!("notes: no shared buffer ({word}), not opened", word = crate::log::error_word(&e)));
                crate::memory::release_memory();
                return;
            }
        };
        let copy = read_config(&paths::config_dir(), &mut |m| log(m)).copy;
        let path_dirs = ScanEnv::from_env(|k| std::env::var_os(k)).path_dirs;
        // The text a failed close kept comes first while the file is as that pad last saw it (taken
        // before the file is read: a change after it differs at the next compare); else the file.
        // The kept text is released only at the end (`unsaved = None` below), so a failed open
        // keeps it (M-2).
        let seen = Fingerprint::of(&paths::state_dir());
        let model = opening_model(self.notes.unsaved.as_ref(), seen, || store::load(&paths::state_dir()));
        // Scrims BEFORE the box, on the box's own Overlay layer: created last, the box stacks above
        // them (the launcher's rule, launcher/surface.rs).
        let scrims = self.notes_scrims(qh);
        let data_device = self.new_data_device(qh);
        let surface = self.compositor.create_surface(qh);
        let box_layer = self.layers.create_layer_surface(qh, surface, Layer::Overlay, Some(NAMESPACE), named.as_ref());
        box_layer.set_size(size.0, size.1);
        box_layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        box_layer.commit();
        let mut open = OpenNotes {
            model,
            requested_at,
            dirty_since: None,
            seen,
            copy,
            path_dirs,
            data_device,
            paste: None,
            box_layer,
            scrims,
            pool,
            buffers: Vec::new(),
            layout,
            size,
            imposed_size: None,
            output: named,
            output_name,
            output_size,
            scale,
            configured: false,
            shown_logged: false,
            last: None,
            waiting: false,
        };
        open.fit_model();
        self.notes.unsaved = None;
        self.notes.open = Some(open);
        self.keyboard_acquire(qh);
    }

    /// The seat's data device, when the manager and the seat both exist.
    fn new_data_device(&self, qh: &QueueHandle<App>) -> Option<DataDevice> {
        match (&self.data_devices, &self.seat) {
            (Some(manager), Some(seat)) => Some(manager.get_data_device(qh, seat)),
            _ => None,
        }
    }

    /// A seat that appears while the pad is open (a keyboard plugged in, a virtual one: the seat
    /// is tracked from its first capability): the data device is made then, before the keyboard
    /// is bound, so the selection reaches the box with its focus.
    pub(crate) fn notes_attach_data_device(&mut self, qh: &QueueHandle<App>) {
        if self.notes.open.as_ref().is_none_or(|o| o.data_device.is_some()) {
            return;
        }
        let device = self.new_data_device(qh);
        if let Some(open) = self.notes.open.as_mut() {
            open.data_device = device;
        }
    }

    fn notes_scrims(&mut self, qh: &QueueHandle<App>) -> Vec<Scrim> {
        if self.viewporter.is_none() {
            log("notes: no wp_viewporter, a click outside will not close it");
            return Vec::new();
        }
        let outputs: Vec<_> = self.outputs.outputs().collect();
        outputs.into_iter().filter_map(|output| self.make_scrim(qh, output, SCRIM_NAMESPACE)).collect()
    }

    /// A new output while the pad is open: a scrim of its own.
    pub(crate) fn notes_output_added(&mut self, qh: &QueueHandle<App>, output: wl_output::WlOutput) {
        if !adds_scrim(self.notes_is_open(), self.viewporter.is_some()) {
            return;
        }
        let Some(scrim) = self.make_scrim(qh, output, SCRIM_NAMESPACE) else { return };
        if let Some(open) = self.notes.open.as_mut() {
            open.scrims.push(scrim);
        }
    }

    /// Re-renders after a model change. Never resizes.
    pub(crate) fn notes_redraw(&mut self) {
        self.notes_draw();
    }

    /// After a scale change: the layout recomputed, the box resized if its logical size moved.
    fn notes_resize(&mut self) {
        let height = self.bar.config.height;
        let App { notes, fonts, .. } = self;
        let Some(open) = notes.open.as_mut() else { return };
        let e = open.scale.max(1);
        let frame = fonts.frame(e);
        let (layout, wanted) = notes_resized_layout(open.imposed_size, open.output_size, fonts.advance(e), height * e as u32, e, &frame);
        open.layout = layout;
        open.fit_model();
        match plan_redraw(open.size, wanted, open.configured) {
            Redraw::Wait => {}
            Redraw::Resize(w, h) => {
                open.size = (w, h);
                open.box_layer.set_size(w, h);
                open.box_layer.commit();
                open.last = None;
            }
            Redraw::Draw => self.notes_draw(),
        }
    }

    /// Once per pass of the loop: a draw that waited for a buffer is done once one is free.
    pub(crate) fn notes_after_dispatch(&mut self) {
        if self.notes.open.as_ref().is_some_and(|o| o.waiting && any_buffer_free(&o.buffers)) {
            self.notes_draw();
        }
    }

    /// The box redrawn even if its rows did not change (a new theme).
    pub(crate) fn notes_repaint(&mut self) {
        if let Some(open) = self.notes.open.as_mut() {
            open.fit_model();
            open.last = None;
        }
        self.notes_draw();
    }

    fn notes_draw(&mut self) {
        let height = self.bar.config.height;
        let App { notes, fonts, roles, .. } = self;
        let Some(open) = notes.open.as_mut() else { return };
        if !open.configured {
            return;
        }
        let e = open.scale.max(1);
        let row_height = height * e as u32;
        let rows = view::render(&open.model, &open.layout);
        if open.last.as_ref() == Some(&rows) {
            open.waiting = false;
            return;
        }
        let (w, h) = (open.size.0 * e as u32, open.size.1 * e as u32);
        let OpenNotes { buffers, pool, box_layer, waiting, .. } = open;
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
            log(&format!("notes buffer {w}×{h}: unexpectedly busy"));
            return;
        };
        fonts.paint_boxed(&rows, roles, px, w, h, row_height, e, FrameInk::Border);
        let surface = box_layer.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffer.attach_to(surface).is_err() {
            return;
        }
        box_layer.commit();
        open.last = Some(rows);
        if !open.shown_logged {
            open.shown_logged = true;
            let place = open.output_name.as_deref().unwrap_or("the compositor's choice of output");
            log(&format!("notes shown on {place} ({w}×{h} px) in {} µs", open.requested_at.elapsed().as_micros()));
        }
    }

    /// A key: translated with the App's modifiers, applied, the save's clock started at the first
    /// edit; a held Copy, Cut, Paste or Escape does not repeat.
    pub(crate) fn notes_key(&mut self, event: KeyEvent) {
        let repeat = self.keyboard.repeat;
        let Some(open) = self.notes.open.as_mut() else { return };
        let Some(key) = keys::translate(event.keysym, event.utf8.as_deref(), self.keyboard.mods) else { return };
        if repeat && repeat_ignored(key) {
            return;
        }
        let outcome = open.model.key(key);
        open.stamp_dirty();
        self.notes_timer_sync();
        self.notes_outcome(outcome);
    }

    /// Pointer events on the pad's surfaces; false: not ours.
    pub(crate) fn notes_pointer(&mut self, conn: &Connection, ev: &PointerEvent) -> bool {
        let height = self.bar.config.height;
        let Some(open) = self.notes.open.as_ref() else { return false };
        let on_box = open.box_layer.wl_surface() == &ev.surface;
        if !on_box && !open.scrims.iter().any(|s| s.layer.wl_surface() == &ev.surface) {
            return false;
        }
        let scale = open.scale.max(1);
        match &ev.kind {
            PointerEventKind::Enter { .. } => {
                if let Some(p) = &self.pointer {
                    let _ = p.set_cursor(conn, CursorIcon::Default);
                }
            }
            PointerEventKind::Leave { .. } => self.wheel.stop(),
            PointerEventKind::Press { .. } if !on_box => self.notes_close(CloseReason::ClickOutside),
            PointerEventKind::Press { button, .. } => {
                let advance = self.fonts.advance(scale);
                let frame = self.fonts.frame(scale);
                let outcome = match (self.notes.open.as_mut(), actions::button(*button)) {
                    (Some(open), Some(Button::Left)) => {
                        let cell = notes_hit_cell(ev.position, scale, advance, height * scale as u32, &frame, &open.layout, open.size);
                        match cell.and_then(|(column, row)| view::hit(&open.layout, column, row)) {
                            Some((row, column)) => open.model.click(row, column),
                            None => NotesOutcome::Nothing,
                        }
                    }
                    _ => NotesOutcome::Nothing,
                };
                self.notes_outcome(outcome);
            }
            PointerEventKind::Axis { vertical, .. } if on_box => {
                if let Some(b) = self.wheel.add(vertical.absolute, vertical.discrete, vertical.value120) {
                    let rows = if b == Button::WheelUp { -WHEEL_ROWS } else { WHEEL_ROWS };
                    let outcome = self.notes.open.as_mut().map_or(NotesOutcome::Nothing, |o| o.model.scroll(rows));
                    self.notes_outcome(outcome);
                }
                if vertical.stop {
                    self.wheel.stop();
                }
            }
            _ => {}
        }
        true
    }

    /// Whether `surface` is the open pad's box: checked outside any guard.
    pub(crate) fn notes_owns_box(&self, surface: &wl_surface::WlSurface) -> bool {
        let is_box = self.notes.open.as_ref().is_some_and(|o| o.box_layer.wl_surface() == surface);
        owns_surface(self.notes.open.is_some(), is_box, false)
    }

    /// Whether `surface` is the open pad's box or one of its scrims: checked outside any guard.
    pub(crate) fn notes_owns_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        let Some(open) = self.notes.open.as_ref() else { return false };
        let is_box = open.box_layer.wl_surface() == surface;
        let is_scrim = open.scrims.iter().any(|s| s.layer.wl_surface() == surface);
        owns_surface(true, is_box, is_scrim)
    }

    /// A configure for a pad surface (the caller checked `notes_owns_surface`).
    pub(crate) fn notes_configured(&mut self, surface: &wl_surface::WlSurface, size: (u32, u32)) -> bool {
        let height = self.bar.config.height;
        let App { notes, fonts, .. } = self;
        let Some(open) = notes.open.as_mut() else { return false };
        if open.box_layer.wl_surface() == surface {
            if let Some(imposed) = plan_configure(open.size, size) {
                // The compositor chose another size: obeyed, the layout refitted inside.
                let e = open.scale.max(1);
                let frame = fonts.frame(e);
                open.layout = notes_layout_within(imposed, fonts.advance(e), height * e as u32, e, &frame);
                open.fit_model();
                open.size = imposed;
                open.imposed_size = Some(imposed);
            }
            open.configured = true;
            open.last = None;
            self.notes_draw();
            return true;
        }
        let OpenNotes { scrims, pool, .. } = open;
        let Some(s) = scrims.iter_mut().find(|s| s.layer.wl_surface() == surface) else { return false };
        s.show(pool, size);
        true
    }

    /// The compositor closed a pad surface (the caller checked `notes_owns_surface`).
    pub(crate) fn notes_layer_closed(&mut self, surface: &wl_surface::WlSurface) -> bool {
        let Some(open) = self.notes.open.as_mut() else { return false };
        if open.box_layer.wl_surface() == surface {
            self.notes_close(CloseReason::OutputGone);
            return true;
        }
        let before = open.scrims.len();
        open.scrims.retain(|s| s.layer.wl_surface() != surface);
        open.scrims.len() != before
    }

    /// An output went away: the box's → close; another one → its scrim only.
    pub(crate) fn notes_output_gone(&mut self, output: &wl_output::WlOutput) {
        let Some(open) = self.notes.open.as_mut() else { return };
        if open.output.as_ref() == Some(output) {
            self.notes_close(CloseReason::OutputGone);
        } else {
            open.scrims.retain(|s| s.output != *output);
        }
    }

    /// Scale change of the box (the caller checked `notes_owns_box`).
    pub(crate) fn notes_scale(&mut self, surface: &wl_surface::WlSurface, factor: i32) -> bool {
        let Some(open) = self.notes.open.as_mut() else { return false };
        if open.box_layer.wl_surface() != surface {
            return false;
        }
        if open.scale != factor {
            open.scale = factor;
            open.last = None;
            self.notes_resize();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame as the workstation's font measures it at scale 1 (8.25 px cells, 22 px rows).
    fn frame1() -> Frame {
        Frame { side: 4, top: 7, bottom: 6, stroke: 1, radius: 6 }
    }

    #[test]
    fn notes_layout_within_fits_an_imposed_size() {
        let full = view::layout(232, 49);
        let size = notes_layout_size(&full, 8.25, 22, 1, &frame1());
        assert_eq!(notes_layout_within(size, 8.25, 22, 1, &frame1()), full, "its own size gives the same layout back");
        for imposed in [(400, 250), (300, 180), (495, 330)] {
            let b = notes_layout_within(imposed, 8.25, 22, 1, &frame1());
            let (w, h) = notes_layout_size(&b, 8.25, 22, 1, &frame1());
            assert!(w <= imposed.0 && h <= imposed.1, "never past {imposed:?}: {w} × {h}");
            assert!(b.text_rows >= view::MIN_TEXT_ROWS);
        }
        // Too small for the minimum: the minimum still (the compositor clips it).
        let tiny = notes_layout_within((300, 160), 8.25, 22, 1, &frame1());
        assert_eq!(tiny.text_rows, view::MIN_TEXT_ROWS, "at least {} text rows", view::MIN_TEXT_ROWS);
    }

    #[test]
    fn the_pool_carries_two_frames() {
        assert_eq!(notes_pool_bytes((100, 50), 1, 0), pool_size(100 * 50 * 4, 0));
        assert_eq!(notes_pool_bytes((100, 50), 2, 3), pool_size(200 * 100 * 4, 3), "physical pixels, a scrim share per output");
        assert_eq!(notes_pool_bytes((100, 50), 2, 3), 2 * 80_000 + 192);
    }

    #[test]
    fn a_scale_change_keeps_a_compositor_imposed_size_untouched() {
        // Without an imposed size: the whole output.
        let (free, free_size) = notes_resized_layout(None, (1920, 1080), 8.25, 22, 1, &frame1());
        assert_eq!(free, view::layout(232, 49));
        assert_eq!(free_size, notes_layout_size(&free, 8.25, 22, 1, &frame1()), "no constraint: sized from the output alone");
        // With one: the imposed logical size wins, whatever the output is.
        let (within, wanted) = notes_resized_layout(Some((400, 250)), (1920, 1080), 8.25, 22, 1, &frame1());
        assert_eq!(wanted, (400, 250), "the compositor's own size, unaffected by the output");
        assert_eq!(within, notes_layout_within((400, 250), 8.25, 22, 1, &frame1()), "the same refit a configure uses");
        assert_ne!(within, free);
    }

    #[test]
    fn a_press_maps_to_the_text_cells() {
        let full = view::layout(232, 49);
        let size = notes_layout_size(&full, 8.25, 22, 1, &frame1());
        let hit = |x: f64, y: f64| notes_hit_cell((x, y), 1, 8.25, 22, &frame1(), &full, size);
        // The first cell inside the frame is the layout's (1, 1): the ring is the frame.
        assert_eq!(hit(4.5, 7.5), Some((1, 1)));
        // Third text row (ring row 5), sixth text column (ring column 7).
        let on_text = hit(4.0 + 8.25 * 6.5, 7.0 + 22.0 * 4.5).and_then(|(c, r)| view::hit(&full, c, r));
        assert_eq!(on_text, Some((2, 5)));
        assert_eq!(hit(1.0, 1.0), None, "the frame's margin");
    }
}
