//! The bar's popups (spec § 1 Menus and Calendar, § 2): an `xdg_popup`
//! attached to the bar's own layer surface on one output
//! (`zwlr_layer_surface_v1.get_popup`, issued before the popup's first
//! commit), placed by an `xdg_positioner` under the cells it hangs from,
//! drawn as rows of cells into a pool of its own. One at a time; everything
//! lives in `OpenPopup` and is dropped whole on close (nothing kept). What a
//! popup shows (`PopupBody`) and how it reacts is filled by the tooltip, the
//! menu and the calendar (tasks 16–18); this module is the Wayland side.
//!
//! The keyboard is shared with the launcher: bound while the launcher or a
//! grabbing popup needs it, released once neither does. Headless sway 1.12
//! (fact check 5) gives no keyboard focus to a grabbing popup whose parent
//! layer surface has `KeyboardInteractivity::None`: the bar's layer surface
//! on that output is made `Exclusive` (committed before the popup is
//! created) while a grabbing popup is open, and keys arrive on the bar's
//! surface — so they are routed by "which popup is open", never by surface.

use crate::actions::Button;
use crate::calendar::Calendar;
use crate::grid::Grid;
use crate::keys::{self, Key, Mods};
use crate::log::write as log;
use crate::painter;
use crate::tray::menu::Menu;
use crate::wayland::{App, pick_buffer};
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{Anchor as Edge, ConstraintAdjustment, Gravity};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity;
use smithay_client_toolkit::shell::xdg::XdgPositioner;
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};

/// `xdg_popup.reposition` exists from `xdg_wm_base` version 3.
const REPOSITION_VERSION: u32 = 3;

/// Logical px size of a box of cells: the width rounded UP (never cut the
/// last column), whole rows of `row_height` physical px.
pub fn box_size(columns: u16, rows: u16, advance_phys: f32, row_height: u32, scale: i32) -> (u32, u32) {
    let e = scale.max(1) as u32;
    let width_phys = (columns as f32 * advance_phys).ceil() as u32;
    (width_phys.div_ceil(e), (rows as u32 * row_height).div_ceil(e))
}

/// Pointer position (logical px) → (column, row).
pub fn cell_under(x: f64, y: f64, scale: i32, advance_phys: f32, row_height: u32) -> (u16, u16) {
    let column = painter::column_under(x, scale, advance_phys);
    let row =
        if row_height == 0 || y <= 0.0 { 0 } else { ((y * scale.max(1) as f64) / row_height as f64).floor().min(u16::MAX as f64) as u16 };
    (column, row)
}

/// Where a popup hangs: the bar of `output`, under cells [start, end) of its grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub output: String,
    pub start: u16,
    pub end: u16,
}

/// xdg_positioner parameters (pure, tested): anchor rect from the cells, size in logical px,
/// anchor bottom-left, gravity bottom-right, constraint adjustment slide-x + flip-y + resize-y.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub anchor_x: i32,
    pub anchor_width: i32,
    pub bar_height: i32,
    pub width: i32,
    pub height: i32,
}

/// `advance_logical`: a cell's width in logical px (the physical advance ÷ scale); `row_height`: a
/// row's height in logical px (the bar's height); `bar_height`: the bar's, the anchor rect's height.
/// The anchor rect stays the icon's cells even near an edge: sliding the popup is the compositor's.
pub fn placement(anchor: &Anchor, columns: u16, rows: u16, advance_logical: f32, row_height: u32, bar_height: u32) -> Placement {
    let left = (anchor.start as f32 * advance_logical).floor() as i32;
    let right = (anchor.end.max(anchor.start + 1) as f32 * advance_logical).ceil() as i32;
    Placement {
        anchor_x: left,
        anchor_width: (right - left).max(1),
        bar_height: bar_height.max(1) as i32,
        width: ((columns as f32 * advance_logical).ceil() as i32).max(1),
        height: (rows as u32 * row_height).max(1) as i32,
    }
}

/// A cell's width in logical px: the font's physical advance at `scale`, divided by it (scale 0
/// treated as 1). The positioner speaks logical px; `Fonts::advance` is physical.
pub(crate) fn logical_advance(advance_phys: f32, scale: i32) -> f32 {
    advance_phys / scale.max(1) as f32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupClose {
    Escape,
    OutsideClick,
    Activated,
    OutputGone,
    Replaced,
    ItemGone,
    Timeout,
    Launcher,
    /// A tooltip: the pointer left its icon (task 16).
    HoverEnded,
    Error,
}

impl PopupClose {
    /// For the log: "popup closed (<text>)".
    pub fn text(self) -> &'static str {
        match self {
            PopupClose::Escape => "escape",
            PopupClose::OutsideClick => "outside click",
            PopupClose::Activated => "activated",
            PopupClose::OutputGone => "output gone",
            PopupClose::Replaced => "replaced",
            PopupClose::ItemGone => "item gone",
            PopupClose::Timeout => "timeout",
            PopupClose::Launcher => "launcher",
            PopupClose::HoverEnded => "hover ended",
            PopupClose::Error => "error",
        }
    }
}

/// What a popup shows (filled by tasks 16–18); dropped whole on close.
pub enum PopupBody {
    Menu(Menu),
    Calendar(Calendar),
    /// One row of text (task 16).
    Tooltip(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupPointer {
    Motion,
    Press(Button),
    Scroll(i32),
    Leave,
}

/// Everything one open popup holds; dropped whole on close.
pub struct OpenPopup {
    pub anchor: Anchor,
    pub body: PopupBody,
    /// The keyboard and an outside click (menus, the calendar); false for a tooltip.
    pub grab: bool,
    popup: Popup,
    /// Its own pool: dropped with it (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// Logical size asked (the positioner's), then the configured one.
    size: (u32, u32),
    /// Cells of the rows painted (columns, rows).
    cells: (u16, u16),
    scale: i32,
    configured: bool,
    /// The rows to show (painted at the first configure, repainted on a scale change).
    rows: Vec<Grid>,
    /// Rows last committed: an unchanged frame is not repainted.
    last: Option<Vec<Grid>>,
    /// Reposition requests sent (their token).
    token: u32,
}

impl OpenPopup {
    pub(crate) fn wl_surface(&self) -> &wl_surface::WlSurface {
        self.popup.wl_surface()
    }
}

/// Who gets a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyTo {
    Launcher,
    Popup,
    Nobody,
}

/// The launcher first (it never coexists with a popup, but if it did, typing goes to it), then a
/// grabbing popup; a tooltip never takes keys.
pub(crate) fn key_to(launcher_open: bool, grabbing_popup: bool) -> KeyTo {
    if launcher_open {
        KeyTo::Launcher
    } else if grabbing_popup {
        KeyTo::Popup
    } else {
        KeyTo::Nobody
    }
}

fn release(k: wl_keyboard::WlKeyboard) {
    if k.version() >= 3 {
        k.release();
    }
}

impl App {
    /// Closes any open popup (Replaced) and the launcher (it closes with ClickOutside first, decision of
    /// the launcher plan), then opens `body` under `anchor`, `columns` × `rows` cells. `grab`: take the
    /// keyboard and close on an outside click (menus, calendar; uses `last_press`'s serial); false for a
    /// tooltip. false when the output's bar is not configured.
    pub fn popup_open(&mut self, qh: &QueueHandle<App>, anchor: Anchor, body: PopupBody, columns: u16, rows: u16, grab: bool) -> bool {
        // Every refusal first: a popup that cannot open closes neither the launcher nor the
        // popup already shown.
        if self.xdg.is_none() {
            return false;
        }
        let Some(i) = self.bar_surface(&anchor.output) else { return false };
        let serial = match (grab, self.last_press) {
            (true, Some((serial, _, _))) => Some(serial),
            (true, None) => return false, // a grab needs the press that asked for it
            (false, _) => None,
        };
        if self.launcher_is_open() {
            if !grab {
                return false; // a tooltip never closes the launcher
            }
            self.launcher_guarded("popup", |app| app.launcher_close(crate::launcher::surface::CloseReason::ClickOutside));
        }
        if self.popup.is_some() {
            self.popup_close(PopupClose::Replaced);
        }
        // Closing the launcher or a popup never touches `surfaces`: `i` still points at the bar.
        let Some(xdg) = self.xdg.as_ref() else { return false };
        let scale = self.surfaces[i].scale.max(1);
        let height = self.bar.config.height;
        let advance = self.fonts.advance(scale);
        let p = placement(&anchor, columns, rows, logical_advance(advance, scale), height, height);
        let bytes = (p.width as usize * scale as usize) * (p.height as usize * scale as usize) * 4;
        let pool = match SlotPool::new(bytes.max(4096), &self.shm) {
            Ok(pool) => pool,
            Err(e) => {
                log(&format!("popup: no shared buffer ({e})"));
                return false;
            }
        };
        let Ok(positioner) = XdgPositioner::new(xdg) else { return false };
        positioner.set_size(p.width, p.height);
        positioner.set_anchor_rect(p.anchor_x, 0, p.anchor_width, p.bar_height);
        positioner.set_anchor(Edge::BottomLeft);
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::FlipY | ConstraintAdjustment::ResizeY);
        let layer = self.surfaces[i].layer.clone();
        if grab {
            // Fact check 5: committed before the popup exists, so the bar's surface holds the
            // keyboard focus while the grab lasts (back to None on close).
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
        }
        let surface = self.compositor.create_surface(qh);
        let popup = match Popup::from_surface(None, &positioner, qh, surface, xdg) {
            Ok(popup) => popup,
            Err(e) => {
                log(&format!("popup: {e}"));
                if grab {
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.commit();
                }
                return false;
            }
        };
        // Before the popup's first commit: else invalid_popup_parent.
        layer.get_popup(popup.xdg_popup());
        if let (Some(serial), Some(seat)) = (serial, self.seat.as_ref()) {
            popup.xdg_popup().grab(seat, serial);
        }
        popup.wl_surface().commit();
        self.popup = Some(OpenPopup {
            anchor,
            body,
            grab,
            popup,
            pool,
            buffers: Vec::new(),
            size: (p.width as u32, p.height as u32),
            cells: (columns, rows),
            scale,
            configured: false,
            rows: Vec::new(),
            last: None,
            token: 0,
        });
        if grab {
            self.keyboard_acquire(qh);
        }
        true
    }

    /// The configured bar surface of `output`, by name.
    fn bar_surface(&self, output: &str) -> Option<usize> {
        self.surfaces.iter().position(|s| s.name == output && s.configured)
    }

    /// Paints `rows` (resizing when their size changed: reposition when xdg_wm_base ≥ 3, else a new buffer size).
    pub fn popup_paint(&mut self, rows: &[Grid]) {
        let height = self.bar.config.height;
        let reposition = self.xdg.as_ref().is_some_and(|x| x.xdg_wm_base().version() >= REPOSITION_VERSION);
        let App { popup, fonts, xdg, .. } = self;
        let Some(open) = popup.as_mut() else { return };
        let cells = (rows.first().map_or(0, Grid::width), rows.len().min(u16::MAX as usize) as u16);
        open.rows = rows.to_vec();
        if cells != open.cells {
            open.cells = cells;
            let e = open.scale.max(1);
            let p = placement(&open.anchor, cells.0, cells.1, logical_advance(fonts.advance(e), e), height, height);
            open.size = (p.width as u32, p.height as u32);
            if let (true, Some(xdg)) = (reposition, xdg.as_ref()) {
                if let Ok(positioner) = XdgPositioner::new(xdg) {
                    positioner.set_size(p.width, p.height);
                    positioner.set_anchor_rect(p.anchor_x, 0, p.anchor_width, p.bar_height);
                    positioner.set_anchor(Edge::BottomLeft);
                    positioner.set_gravity(Gravity::BottomRight);
                    positioner.set_constraint_adjustment(
                        ConstraintAdjustment::SlideX | ConstraintAdjustment::FlipY | ConstraintAdjustment::ResizeY,
                    );
                    open.token += 1;
                    open.popup.reposition(&positioner, open.token);
                    open.configured = false; // its configure paints
                    return;
                }
            }
            // xdg_wm_base < 3: a buffer of the new size, placed anew at the next open.
        }
        self.popup_draw();
    }

    fn popup_draw(&mut self) {
        let height = self.bar.config.height;
        let App { popup, fonts, palette, .. } = self;
        let Some(open) = popup.as_mut() else { return };
        if !open.configured || open.last.as_ref() == Some(&open.rows) {
            return;
        }
        let e = open.scale.max(1);
        let (w, h) = (open.size.0 * e as u32, open.size.1 * e as u32);
        let OpenPopup { buffers, pool, popup, rows, .. } = open;
        let Some(used) = pick_buffer(buffers, pool, w, h) else { return };
        let Some(px) = buffers[used].0.canvas(pool) else { return };
        px.fill(0);
        fonts.paint_rows(rows, palette, px, w, height * e as u32, e);
        let surface = popup.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffers[used].0.attach_to(surface).is_err() {
            return;
        }
        surface.commit();
        open.last = Some(open.rows.clone());
    }

    /// Drops everything the popup holds; the keyboard goes back if nothing else needs it.
    pub fn popup_close(&mut self, why: PopupClose) {
        let Some(open) = self.popup.take() else { return };
        let (grab, output) = (open.grab, open.anchor.output.clone());
        drop(open);
        if grab {
            if let Some(i) = self.surfaces.iter().position(|s| s.name == output) {
                let layer = &self.surfaces[i].layer;
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                layer.commit();
            }
            self.keyboard_release();
        }
        log(&format!("popup closed ({})", why.text()));
        // The popup's pool and whatever its body built: give the pages back.
        crate::memory::release_memory();
    }

    pub fn popup(&self) -> Option<&OpenPopup> {
        self.popup.as_ref()
    }

    pub fn popup_mut(&mut self) -> Option<&mut OpenPopup> {
        self.popup.as_mut()
    }

    /// A key while a grabbing popup is open (the launcher is closed).
    pub(crate) fn popup_key(&mut self, k: Key) {
        let _ = k; // read by the bodies' arms (tasks 17, 18)
        let Some(open) = self.popup.as_ref() else { return };
        match open.body {
            PopupBody::Menu(_) => {}     // task 17
            PopupBody::Calendar(_) => {} // task 18
            PopupBody::Tooltip(_) => {}
        }
    }

    /// The pointer on the popup, at cell (`column`, `row`) of its rows.
    pub(crate) fn popup_pointer(&mut self, column: u16, row: u16, kind: PopupPointer) {
        let _ = (column, row); // read by the bodies' arms (tasks 17, 18)
        let Some(open) = self.popup.as_ref() else { return };
        match open.body {
            PopupBody::Menu(_) => {}     // task 17
            PopupBody::Calendar(_) => {} // task 18
            // A press on the tooltip dismisses it (nothing in it is clickable).
            PopupBody::Tooltip(_) => {
                if let PopupPointer::Press(_) = kind {
                    self.popup_close(PopupClose::HoverEnded);
                }
            }
        }
    }

    /// Pointer events on the popup's surface; false: not ours.
    pub(crate) fn popup_pointer_event(&mut self, surface: &wl_surface::WlSurface, position: (f64, f64), kind: PopupPointer) -> bool {
        let height = self.bar.config.height;
        let Some(open) = self.popup.as_ref() else { return false };
        if open.wl_surface() != surface {
            return false;
        }
        let e = open.scale.max(1);
        let (column, row) = cell_under(position.0, position.1, e, self.fonts.advance(e), height * e as u32);
        self.popup_pointer(column, row, kind);
        true
    }

    /// The anchor's bar goes (output unplugged, surface closed): the popup goes first.
    pub(crate) fn popup_output_gone(&mut self, output: &str) {
        if self.popup.as_ref().is_some_and(|p| p.anchor.output == output) {
            self.popup_close(PopupClose::OutputGone);
        }
    }

    /// Scale change of the popup: repainted at the new scale; false: not ours.
    pub(crate) fn popup_scale(&mut self, surface: &wl_surface::WlSurface, factor: i32) -> bool {
        let Some(open) = self.popup.as_mut() else { return false };
        if open.wl_surface() != surface {
            return false;
        }
        if open.scale != factor {
            open.scale = factor;
            open.last = None;
            self.popup_draw();
        }
        true
    }

    /// Whether the keyboard is needed: the launcher, or a grabbing popup.
    pub(crate) fn keyboard_wanted(&self) -> bool {
        self.launcher_is_open() || self.popup.as_ref().is_some_and(|p| p.grab)
    }

    /// Binds the keyboard (with repeat on the event loop) on the seat if none: the launcher is open, or a
    /// grabbing popup. (The guardrail counts the binding call: never name it in a comment.)
    pub(crate) fn keyboard_acquire(&mut self, qh: &QueueHandle<App>) {
        if self.keyboard.is_some() || !self.keyboard_wanted() {
            return;
        }
        let (Some(handle), Some(seat)) = (self.loop_handle.clone(), self.seat.clone()) else { return };
        let repeat = Box::new(|app: &mut App, _: &wl_keyboard::WlKeyboard, event: KeyEvent| app.key_event(event));
        match self.seats.get_keyboard_with_repeat(qh, &seat, None, handle, repeat) {
            Ok(k) => self.keyboard = Some(k),
            // UnsupportedCapability just means the seat has no keyboard yet
            // (retried once the capability appears, new_capability): routine,
            // not worth a line every time.
            Err(e) if crate::launcher::surface::should_log_keyboard_error(&e) => log(&format!("keyboard: {e}")),
            Err(_) => {}
        }
    }

    /// release() once neither the launcher nor a grabbing popup needs it.
    pub(crate) fn keyboard_release(&mut self) {
        if self.keyboard_wanted() {
            return;
        }
        if let Some(k) = self.keyboard.take() {
            release(k);
        }
        self.mods = Mods::default();
    }

    /// The seat lost its keyboard: the object goes now, whoever wanted it, and with it the
    /// modifiers it reported (a Ctrl held then must not stick to the next keyboard).
    pub(crate) fn keyboard_lost(&mut self) {
        if let Some(k) = self.keyboard.take() {
            release(k);
        }
        self.mods = Mods::default();
    }

    /// A key (press or repeat): to the launcher when it is open, else to a grabbing popup.
    pub(crate) fn key_event(&mut self, event: KeyEvent) {
        match key_to(self.launcher_is_open(), self.popup.as_ref().is_some_and(|p| p.grab)) {
            KeyTo::Launcher => {
                self.launcher_guarded("key", move |app| app.launcher_key(event));
            }
            KeyTo::Popup => {
                if let Some(k) = keys::translate(event.keysym, event.utf8.as_deref(), self.mods) {
                    self.popup_key(k);
                }
            }
            KeyTo::Nobody => {}
        }
    }
}

impl KeyboardHandler for App {
    #[allow(clippy::too_many_arguments)]
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &wl_surface::WlSurface, _: u32) {
        if self.launcher_owns_box(surface) {
            self.launcher_guarded("focus", |app| app.launcher_close(crate::launcher::surface::CloseReason::FocusLost));
        }
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key_event(event);
    }

    /// wl_keyboard v10 repeats (compositor-side); calloop repeats go through
    /// the callback given at bind time, to the same place.
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.key_event(event);
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    #[allow(clippy::too_many_arguments)]
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        m: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.mods = Mods { ctrl: m.ctrl, shift: m.shift, alt: m.alt, logo: m.logo };
    }
}

impl PopupHandler for App {
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup, config: PopupConfigure) {
        let Some(open) = self.popup.as_mut().filter(|o| o.popup == *popup) else { return };
        // The compositor's size (resize-y may have shortened it); never zero.
        open.size = (config.width.max(1) as u32, config.height.max(1) as u32);
        open.configured = true;
        open.last = None;
        self.popup_draw();
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        if self.popup.as_ref().is_some_and(|o| o.popup == *popup) {
            self.popup_close(PopupClose::OutsideClick);
        }
    }
}

/// `delegate_xdg_shell!` needs it; the daemon creates no xdg toplevel.
impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, _: WindowConfigure, _: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn under(output: &str, start: u16, end: u16) -> Anchor {
        Anchor { output: output.into(), start, end }
    }

    #[test]
    fn placement_under_the_icon_cells() {
        // An icon at cells 40..41, 8.25 px cells, 22 px rows: the rect is that cell, the popup
        // 20 × 8 cells hangs below the bar.
        let p = placement(&under("HDMI-A-1", 40, 41), 20, 8, 8.25, 22, 22);
        assert_eq!(p, Placement { anchor_x: 330, anchor_width: 9, bar_height: 22, width: 165, height: 176 });
        let p = placement(&under("HDMI-A-1", 10, 13), 23, 10, 8.125, 24, 24);
        assert_eq!((p.anchor_x, p.anchor_width), (81, 25), "floor the left edge, ceil the right: the cells are covered");
        assert_eq!((p.width, p.height), (187, 240), "186.875 px: the last column is never cut");
        let p = placement(&under("HDMI-A-1", 7, 8), 1, 1, 8.125, 22, 22);
        assert_eq!((p.anchor_x, p.anchor_width), (56, 9), "56.875 px: the rect starts on the cell's first pixel");
    }

    #[test]
    fn placement_at_scale_2() {
        // popup_open's conversion: the physical advance at scale 2 (16.5 px) is 8.25 logical px,
        // so the popup takes the same logical place and size as at scale 1 — never twice as wide.
        assert_eq!(logical_advance(16.5, 2), 8.25);
        assert_eq!(logical_advance(8.25, 0), 8.25, "scale 0 treated as 1");
        let one = placement(&under("eDP-1", 40, 41), 20, 8, logical_advance(8.25, 1), 22, 22);
        let two = placement(&under("eDP-1", 40, 41), 20, 8, logical_advance(16.5, 2), 22, 22);
        assert_eq!(one, two);
        assert_eq!((two.anchor_x, two.width), (330, 165), "logical px, not the physical 660 and 330");
    }

    #[test]
    fn placement_near_the_right_edge_keeps_the_anchor() {
        // The last cell of a 160-cell bar: the rect stays the icon's (the compositor slides the
        // popup back inside, slide-x), never moved here.
        let p = placement(&under("HDMI-A-1", 159, 160), 30, 5, 8.0, 22, 22);
        assert_eq!((p.anchor_x, p.anchor_width), (1272, 8));
        assert_eq!(p.width, 240);
        let empty = placement(&under("HDMI-A-1", 5, 5), 1, 1, 8.0, 22, 22);
        assert_eq!(empty.anchor_width, 8, "an empty span still anchors on one cell");
    }

    /// The launcher's own tests, moved with the functions.
    #[test]
    fn box_size_and_cell_under_unchanged_after_the_move() {
        assert_eq!(box_size(60, 15, 8.25, 22, 1), (495, 330));
        assert_eq!(box_size(61, 15, 8.125, 22, 1), (496, 330), "495.625 px: the last column is never cut");
        assert_eq!(box_size(60, 15, 16.5, 44, 2), (495, 330), "logical px at scale 2");
        assert_eq!(box_size(60, 15, 8.25, 22, 0), (495, 330), "scale 0 treated as 1");
        assert_eq!(cell_under(100.0, 45.0, 1, 8.25, 22), (12, 2));
        assert_eq!(cell_under(100.0, 45.0, 2, 16.5, 44), (12, 2), "scale 2: physical cells, logical pointer");
        assert_eq!(cell_under(-3.0, -1.0, 1, 8.25, 22), (0, 0), "a pointer at the very edge");
    }

    #[test]
    fn keys_go_to_the_launcher_first() {
        assert_eq!(key_to(true, true), KeyTo::Launcher);
        assert_eq!(key_to(true, false), KeyTo::Launcher);
        assert_eq!(key_to(false, true), KeyTo::Popup);
        assert_eq!(key_to(false, false), KeyTo::Nobody, "a tooltip, or nothing open: the key goes nowhere");
    }

    #[test]
    fn close_reasons_have_texts() {
        use PopupClose::*;
        let all = [Escape, OutsideClick, Activated, OutputGone, Replaced, ItemGone, Timeout, Launcher, HoverEnded, Error];
        assert_eq!(
            all.map(PopupClose::text),
            ["escape", "outside click", "activated", "output gone", "replaced", "item gone", "timeout", "launcher", "hover ended", "error"]
        );
    }
}
