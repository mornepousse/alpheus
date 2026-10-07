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

use crate::actions::{Button, Wheel};
use crate::calendar::Calendar;
use crate::keys::{self, Key, Mods};
use crate::list::{self, Boxed};
use crate::log::write as log;
use crate::painter::Frame;
use crate::tray::codec::Layout;
use crate::tray::menu::{Menu, MenuAction};
use crate::tray::menu_session::MenuSession;
use crate::wayland::{App, pick_buffer, position_by};
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
use std::time::Instant;

/// `xdg_popup.reposition` exists from `xdg_wm_base` version 3.
const REPOSITION_VERSION: u32 = 3;

/// Logical px size of a box of cells and its frame: the width rounded UP (never cut the last
/// column), whole rows of `row_height` physical px.
pub fn box_size(columns: u16, rows: u16, advance_phys: f32, row_height: u32, scale: i32, frame: &Frame) -> (u32, u32) {
    let e = scale.max(1) as u32;
    let (extra_w, extra_h) = frame.extra();
    let width_phys = (columns as f32 * advance_phys).ceil() as u32 + extra_w;
    (width_phys.div_ceil(e), (rows as u32 * row_height + extra_h).div_ceil(e))
}

/// Pointer position (logical px) → the (column, row) of the `cells` box's cells under it; None in
/// the frame's margin (and past the last cell), where no cell is.
pub fn cell_under(x: f64, y: f64, scale: i32, advance_phys: f32, row_height: u32, frame: &Frame, cells: (u16, u16)) -> Option<(u16, u16)> {
    let e = scale.max(1) as f64;
    let (px, py) = (x * e - frame.side as f64, y * e - frame.top as f64);
    if px < 0.0 || py < 0.0 || advance_phys <= 0.0 || row_height == 0 {
        return None;
    }
    let column = (px / advance_phys as f64).floor();
    let row = (py / row_height as f64).floor();
    (column < cells.0 as f64 && row < cells.1 as f64).then_some((column as u16, row as u16))
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

/// `size`: the box's logical px (`box_size`: cells and frame); `advance_logical`: a cell's width in
/// logical px (the physical advance ÷ scale), the anchor rect's unit; `bar_height`: the bar's, the
/// anchor rect's height. The anchor rect stays the icon's cells even near an edge: sliding the popup
/// is the compositor's.
pub fn placement(anchor: &Anchor, size: (u32, u32), advance_logical: f32, bar_height: u32) -> Placement {
    let left = (anchor.start as f32 * advance_logical).floor() as i32;
    let right = (anchor.end.max(anchor.start + 1) as f32 * advance_logical).ceil() as i32;
    Placement {
        anchor_x: left,
        anchor_width: (right - left).max(1),
        bar_height: bar_height.max(1) as i32,
        width: (size.0 as i32).max(1),
        height: (size.1 as i32).max(1),
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
    /// A press on the bar itself while the popup is open (delivered to the bar, same client).
    OutsideClick,
    /// The compositor ended the grab (`xdg_popup.done`): a click elsewhere, or its own reasons.
    Dismissed,
    Activated,
    OutputGone,
    Replaced,
    ItemGone,
    Timeout,
    /// Its item's `Menu` path moved under it (a duplicate registration, fix round 2026-10-06):
    /// never a real timeout, so it gets its own, honestly-named reason.
    MenuMoved,
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
            PopupClose::Dismissed => "dismissed",
            PopupClose::Activated => "activated",
            PopupClose::OutputGone => "output gone",
            PopupClose::Replaced => "replaced",
            PopupClose::ItemGone => "item gone",
            PopupClose::Timeout => "timeout",
            PopupClose::MenuMoved => "menu moved",
            PopupClose::Launcher => "launcher",
            PopupClose::HoverEnded => "hover ended",
            PopupClose::Error => "error",
        }
    }
}

/// Whether the compositor is Hyprland, from the interfaces it advertises: it alone has
/// `hyprland_*` protocols.
pub fn is_hyprland<'a>(mut interfaces: impl Iterator<Item = &'a str>) -> bool {
    interfaces.any(|i| i.starts_with("hyprland_"))
}

/// Whether the bar's layer surface must go `Exclusive` while one of its popups grabs.
///
/// sway (fact check 5) gives a grabbing popup of a layer surface no keyboard unless that layer
/// surface is `Exclusive`. Hyprland is the other way round: it hands the grab the keyboard by
/// itself, and before 0.56.2 (hyprwm/Hyprland#15729) an `Exclusive` layer surface swallows the
/// hit test of its own popups, so every click inside the menu is taken for a click outside and
/// ends the grab (`xdg_popup.done`) — Mae, Hyprland 0.55.4, 2026-10-06.
pub fn bar_exclusive_during_grab(on_hyprland: bool) -> bool {
    !on_hyprland
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
    /// The bar's `wl_surface` this popup was opened under (cloned from its `layer.wl_surface()`
    /// at `popup_open`): `popup_close`/`popup_output_gone` reset THIS surface's keyboard
    /// interactivity, by identity — never by `anchor.output` or a bar's `name`, which
    /// `update_output` can change after the fact, or which two bars could briefly share.
    parent: wl_surface::WlSurface,
    popup: Popup,
    /// Its own pool: dropped with it (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// Logical size asked (the positioner's), then the configured one.
    size: (u32, u32),
    /// Cells of the rows painted (columns, rows), the frame's margin excluded.
    cells: (u16, u16),
    scale: i32,
    configured: bool,
    /// What to show (painted at the first configure, repainted on a scale change).
    rows: Boxed,
    /// What was last committed: an unchanged frame is not repainted.
    last: Option<Boxed>,
    /// Reposition requests sent (their token).
    token: u32,
    /// This popup's own wheel accumulator (menus, the calendar): never the bar's `self.wheel`,
    /// reset fresh at every open, else a notch half-accumulated on the bar would carry into it
    /// (or the other way around) and cost or save a notch that was never really scrolled here.
    pub(crate) wheel: Wheel,
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

/// "2026-10": the shown month, for the log.
pub(crate) fn calendar_month(c: &Calendar) -> String {
    format!("{}-{:02}", c.shown.year, c.shown.month)
}

impl App {
    /// Closes any open popup (Replaced) and the launcher (it closes with ClickOutside first, decision of
    /// the launcher plan), then opens `body` under `anchor`, `columns` × `rows` cells (the frame's margin
    /// comes on top: `Fonts::frame`). `grab`: take the
    /// keyboard and close on an outside click (menus, calendar; uses `last_press`'s serial); false for a
    /// tooltip. false when the output's bar is not configured.
    pub fn popup_open(&mut self, qh: &QueueHandle<App>, anchor: Anchor, body: PopupBody, columns: u16, rows: u16, grab: bool) -> bool {
        // Every refusal first (moved above the launcher/popup closes below, task 15 fix round:
        // `SlotPool::new` and `XdgPositioner::new` can fail too, and must refuse here, before
        // anything already shown is torn down): a popup that cannot open closes neither the
        // launcher nor the popup already shown.
        if self.xdg.is_none() {
            return false;
        }
        let Some(i) = self.bar_surface(&anchor.output) else { return false };
        let serial = match (grab, self.last_press) {
            (true, Some((serial, _, _))) => Some(serial),
            (true, None) => return false, // a grab needs the press that asked for it
            (false, _) => None,
        };
        let scale = self.surfaces[i].scale.max(1);
        let height = self.bar.config.height;
        let advance = self.fonts.advance(scale);
        let frame = self.fonts.frame(scale);
        let size = box_size(columns, rows, advance, height * scale as u32, scale, &frame);
        let p = placement(&anchor, size, logical_advance(advance, scale), height);
        let bytes = (p.width as usize * scale as usize) * (p.height as usize * scale as usize) * 4;
        let pool = match SlotPool::new(bytes.max(4096), &self.shm) {
            Ok(pool) => pool,
            Err(e) => {
                log(&format!("popup: no shared buffer ({e})"));
                return false;
            }
        };
        let Some(xdg) = self.xdg.as_ref() else { return false }; // checked above: never None here
        let Ok(positioner) = XdgPositioner::new(xdg) else { return false };
        positioner.set_size(p.width, p.height);
        positioner.set_anchor_rect(p.anchor_x, 0, p.anchor_width, p.bar_height);
        positioner.set_anchor(Edge::BottomLeft);
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::FlipY | ConstraintAdjustment::ResizeY);
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
        let layer = self.surfaces[i].layer.clone();
        let parent = layer.wl_surface().clone();
        if grab && self.exclusive_during_grab {
            // Fact check 5: committed before the popup exists, so the bar's surface holds the
            // keyboard focus while the grab lasts (back to None on close).
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
        }
        let surface = self.compositor.create_surface(qh);
        // Re-borrowed (the closes above took `&mut self`): confirmed non-None just above.
        let Some(xdg) = self.xdg.as_ref() else { return false };
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
            parent,
            popup,
            pool,
            buffers: Vec::new(),
            size: (p.width as u32, p.height as u32),
            cells: (columns, rows),
            scale,
            configured: false,
            rows: Boxed::default(),
            last: None,
            token: 0,
            wheel: Wheel::default(),
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
    pub fn popup_paint(&mut self, rows: Boxed) {
        let Some(open) = self.popup.as_mut() else { return };
        let cells = rows.cells();
        open.rows = rows;
        if cells != open.cells && self.popup_reposition(cells) {
            return; // its configure paints
        }
        self.popup_draw();
    }

    /// Recomputes the positioner's size and anchor rect for `cells` at the popup's current scale,
    /// and asks the compositor to reposition it (xdg_wm_base ≥ 3) or just updates `size` for the
    /// next buffer (older compositors); true: a reposition was requested, its `configure` paints —
    /// the caller must not draw now. Shared by `popup_paint` (the cell count changed) and
    /// `popup_scale` (task 15 fix round: the SAME cell count can still occupy a slightly different
    /// pixel size at a different scale — the font's physical advance does not always scale
    /// exactly — so a scale change must recompute the placement too, not just redraw in place).
    fn popup_reposition(&mut self, cells: (u16, u16)) -> bool {
        let height = self.bar.config.height;
        let reposition = self.xdg.as_ref().is_some_and(|x| x.xdg_wm_base().version() >= REPOSITION_VERSION);
        let App { popup, fonts, xdg, .. } = self;
        let Some(open) = popup.as_mut() else { return false };
        open.cells = cells;
        let e = open.scale.max(1);
        let advance = fonts.advance(e);
        let size = box_size(cells.0, cells.1, advance, height * e as u32, e, &fonts.frame(e));
        let p = placement(&open.anchor, size, logical_advance(advance, e), height);
        open.size = (p.width as u32, p.height as u32);
        if let (true, Some(xdg)) = (reposition, xdg.as_ref()) {
            if let Ok(positioner) = XdgPositioner::new(xdg) {
                positioner.set_size(p.width, p.height);
                positioner.set_anchor_rect(p.anchor_x, 0, p.anchor_width, p.bar_height);
                positioner.set_anchor(Edge::BottomLeft);
                positioner.set_gravity(Gravity::BottomRight);
                positioner
                    .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::FlipY | ConstraintAdjustment::ResizeY);
                open.token += 1;
                open.popup.reposition(&positioner, open.token);
                open.configured = false; // its configure paints
                return true;
            }
        }
        // xdg_wm_base < 3, or the positioner failed: a buffer of the new size, drawn now.
        false
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
        fonts.paint_boxed(rows, palette, px, w, h, height * e as u32, e);
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
        // A menu's D-Bus session ends with its popup — except when a new menu replaces it (the tray
        // already swapped the session); a menu still being fetched dies with the launcher opening.
        let menu = matches!(self.popup.as_ref().map(|p| &p.body), Some(PopupBody::Menu(_)));
        // Fix round 2026-10-06 (hardening, requirement 3): the compositor may deliver `xdg_popup.
        // done` (routed here as OutsideClick) before the press that follows it reaches the bar — a
        // record of what just closed, by whom it was opened, and when, lets that next press still
        // count as only-closing (`closed_recently`, wayland.rs) even though this popup is already
        // gone by the time it arrives.
        if menu && why == PopupClose::Dismissed {
            if let (Some((key, _)), Some(button)) = (self.menu_for.clone(), self.menu_opened_by) {
                self.recently_closed_menu = Some((key, button, Instant::now()));
            }
        }
        // The same race for the calendar (a left press on the clock only closes, wayland.rs).
        if why == PopupClose::Dismissed && matches!(self.popup.as_ref().map(|p| &p.body), Some(PopupBody::Calendar(_))) {
            self.recently_closed_calendar = Some(Instant::now());
        }
        if (menu && why != PopupClose::Replaced) || (why == PopupClose::Launcher && self.menu_for.is_some()) {
            self.menu_end();
        }
        let Some(open) = self.popup.take() else { return };
        let (grab, parent) = (open.grab, open.parent.clone());
        // Whatever the reason: a closed tooltip is never left pairing `tooltip_item` with a popup
        // that no longer exists (fix round 2026-10-06, item 3) — `tooltip_item_gone` (wayland.rs)
        // relies on the two always being set and cleared together.
        if matches!(open.body, PopupBody::Tooltip(_)) {
            self.tooltip_item = None;
        }
        drop(open);
        if grab {
            // By the parent's surface IDENTITY, never by `anchor.output` or a bar's `name` (task
            // 15 fix round, CRITICAL): a name can change after the fact (`update_output`) or be
            // shared for a moment by two bars, and the wrong bar — or none — would stay stuck
            // `Exclusive`, holding Mae's keyboard.
            if let Some(i) = position_by(&self.surfaces, |s| s.layer.wl_surface(), &parent) {
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
        let Some(open) = self.popup.as_ref() else { return };
        match open.body {
            PopupBody::Menu(_) => {
                let action = self.menu_with(|m, layout| m.key(k, layout));
                self.menu_action(action, self.key_time);
            }
            // ← → change month, Escape closes (spec § 2).
            PopupBody::Calendar(_) => match k {
                Key::Left => self.calendar_scroll(-1),
                Key::Right => self.calendar_scroll(1),
                Key::Escape => self.popup_close(PopupClose::Escape),
                _ => {}
            },
            PopupBody::Tooltip(_) => {}
        }
    }

    /// The calendar shows `months` later (earlier when negative), repainted the same size.
    fn calendar_scroll(&mut self, months: i32) {
        let Some(PopupBody::Calendar(c)) = self.popup.as_mut().map(|p| &mut p.body) else { return };
        if !c.scroll(months) {
            return;
        }
        let rows = list::padded(&c.render());
        let month = calendar_month(c);
        self.popup_paint(rows);
        log(&format!("calendar shows {month}"));
    }

    /// Runs `f` on the open menu and its session's layout; Nothing when either is missing — the
    /// session ended under it (its item gone, its path moved, a call timed out), which `tray_
    /// output` has already closed and logged with the right reason (fix round 2026-10-06,
    /// requirement 5): a fabricated `MenuAction::Close` here would make `menu_action` log it again,
    /// wrongly, as "escape".
    fn menu_with(&mut self, f: impl FnOnce(&mut Menu, &Layout) -> MenuAction) -> MenuAction {
        let App { tray, popup, .. } = self;
        match (tray.as_ref().and_then(|t| t.menu()).and_then(MenuSession::layout), popup.as_mut().map(|p| &mut p.body)) {
            (Some(layout), Some(PopupBody::Menu(m))) => f(m, layout),
            _ => MenuAction::Nothing,
        }
    }

    /// The pointer on the popup, at `cell` (column, row) of its rows; None: in the frame's margin,
    /// where no cell is.
    pub(crate) fn popup_pointer(&mut self, cell: Option<(u16, u16)>, kind: PopupPointer) {
        let Some(open) = self.popup.as_ref() else { return };
        match open.body {
            // One entry per row: the column never matters.
            PopupBody::Menu(_) => {
                let content = cell.map(|(_, row)| row);
                let action = match (kind, content) {
                    (PopupPointer::Motion, Some(r)) => self.menu_with(|m, l| m.hover(r, l)),
                    (PopupPointer::Press(Button::Left), Some(r)) => self.menu_with(|m, l| m.press(r, l)),
                    (PopupPointer::Scroll(steps), _) => self.menu_with(|m, l| m.scroll(steps, l)),
                    _ => MenuAction::Nothing,
                };
                // Fix round 2026-10-06, requirement 6: the popup's OWN last press, never the
                // bar's `last_press` — that one is the click that asked the popup to open (a
                // right click, say), not necessarily the one running an entry inside it.
                self.menu_action(action, self.popup_press_time);
            }
            // The wheel changes month.
            PopupBody::Calendar(_) => {
                if let PopupPointer::Scroll(steps) = kind {
                    self.calendar_scroll(steps);
                }
            }
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
        let cells = open.cells;
        let frame = self.fonts.frame(e);
        let cell = cell_under(position.0, position.1, e, self.fonts.advance(e), height * e as u32, &frame, cells);
        self.popup_pointer(cell, kind);
        true
    }

    /// The popup's parent bar surface goes (output unplugged, surface closed): the popup goes
    /// first — compared by surface IDENTITY (task 15 fix round, CRITICAL), never by `anchor.output`
    /// or a bar's `name`, both of which can go stale (see `popup_close`).
    pub(crate) fn popup_output_gone(&mut self, parent: &wl_surface::WlSurface) {
        if self.popup.as_ref().is_some_and(|p| &p.parent == parent) {
            self.popup_close(PopupClose::OutputGone);
        }
        // A tooltip's hover tracking must not survive the bar it was armed on going away (task 15
        // fix round, from the task 16 review): left alone, `Hover.over` would still name the same
        // icon once the bar (or another showing it) comes back, and `Hover::moved` — keyed only on
        // the item, never the surface — would then wrongly see "still on it", never re-arming.
        let c = self.hover.cancel();
        self.hover_apply(c);
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
            let cells = open.cells;
            // Through `popup_paint`'s own resize path (task 15 fix round): the placement must be
            // recomputed at the new scale, not just redrawn at the old one.
            if !self.popup_reposition(cells) {
                self.popup_draw();
            }
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
        self.key_time = event.time;
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
            self.popup_close(PopupClose::Dismissed);
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

    /// A frame as the first font of the workstation measures it at scale 1 (8.25 px cells, 22 px
    /// rows), written out so the geometry below is plain arithmetic.
    fn frame1() -> Frame {
        Frame { side: 4, top: 7, bottom: 6, stroke: 1, radius: 6 }
    }

    #[test]
    fn placement_under_the_icon_cells() {
        // An icon at cells 40..41, 8.25 px cells, 22 px rows: the rect is that cell, the popup
        // 20 × 8 cells and its frame hangs below the bar.
        let size = box_size(20, 8, 8.25, 22, 1, &frame1());
        assert_eq!(size, (173, 189), "165 + 2 × 4 px, 176 + 7 + 6 px");
        let p = placement(&under("HDMI-A-1", 40, 41), size, 8.25, 22);
        assert_eq!(p, Placement { anchor_x: 330, anchor_width: 9, bar_height: 22, width: 173, height: 189 });
        let p = placement(&under("HDMI-A-1", 10, 13), (187, 240), 8.125, 24);
        assert_eq!((p.anchor_x, p.anchor_width), (81, 25), "floor the left edge, ceil the right: the cells are covered");
        assert_eq!((p.width, p.height), (187, 240), "the size is the caller's, never recomputed here");
        let p = placement(&under("HDMI-A-1", 7, 8), (1, 1), 8.125, 22);
        assert_eq!((p.anchor_x, p.anchor_width), (56, 9), "56.875 px: the rect starts on the cell's first pixel");
    }

    #[test]
    fn placement_at_scale_2() {
        // popup_open's conversion: the physical advance at scale 2 (16.5 px) is 8.25 logical px,
        // so the popup takes the same logical place and size as at scale 1 — never twice as wide.
        assert_eq!(logical_advance(16.5, 2), 8.25);
        assert_eq!(logical_advance(8.25, 0), 8.25, "scale 0 treated as 1");
        let frame2 = Frame { side: 8, top: 14, bottom: 12, stroke: 2, radius: 12 };
        let one = placement(&under("eDP-1", 40, 41), box_size(20, 8, 8.25, 22, 1, &frame1()), logical_advance(8.25, 1), 22);
        let two = placement(&under("eDP-1", 40, 41), box_size(20, 8, 16.5, 44, 2, &frame2), logical_advance(16.5, 2), 22);
        assert_eq!(one, two);
        assert_eq!((two.anchor_x, two.width), (330, 173), "logical px, not the physical 660 and 346");
    }

    #[test]
    fn placement_near_the_right_edge_keeps_the_anchor() {
        // The last cell of a 160-cell bar: the rect stays the icon's (the compositor slides the
        // popup back inside, slide-x), never moved here.
        let p = placement(&under("HDMI-A-1", 159, 160), (248, 100), 8.0, 22);
        assert_eq!((p.anchor_x, p.anchor_width), (1272, 8));
        assert_eq!(p.width, 248);
        let empty = placement(&under("HDMI-A-1", 5, 5), (1, 1), 8.0, 22);
        assert_eq!(empty.anchor_width, 8, "an empty span still anchors on one cell");
    }

    #[test]
    fn box_size_adds_the_frame_to_the_cells() {
        let f = frame1();
        assert_eq!(box_size(60, 15, 8.25, 22, 1, &f), (503, 343), "495 + 8, 330 + 13");
        assert_eq!(box_size(61, 15, 8.125, 22, 1, &f), (504, 343), "495.625 px: the last column is never cut");
        let f2 = Frame { side: 8, top: 14, bottom: 12, stroke: 2, radius: 12 };
        assert_eq!(box_size(60, 15, 16.5, 44, 2, &f2), (503, 343), "logical px at scale 2");
        assert_eq!(box_size(60, 15, 8.25, 22, 0, &f), (503, 343), "scale 0 treated as 1");
        assert_eq!(box_size(0, 0, 8.25, 22, 1, &f), (8, 13), "no cells: the frame alone");
    }

    #[test]
    fn cell_under_counts_from_the_frames_inner_margin() {
        let f = frame1();
        let cells = (60, 15);
        assert_eq!(cell_under(104.0, 52.0, 1, 8.25, 22, &f, cells), Some((12, 2)), "(104 - 4) / 8.25, (52 - 7) / 22");
        assert_eq!(cell_under(4.0, 7.0, 1, 8.25, 22, &f, cells), Some((0, 0)), "the very first pixel of the first cell");
        assert_eq!(cell_under(3.5, 30.0, 1, 8.25, 22, &f, cells), None, "left margin: no cell");
        assert_eq!(cell_under(30.0, 6.5, 1, 8.25, 22, &f, cells), None, "top margin: no cell");
        assert_eq!(cell_under(-3.0, -1.0, 1, 8.25, 22, &f, cells), None, "outside the surface");
        assert_eq!(cell_under(4.0 + 60.0 * 8.25, 30.0, 1, 8.25, 22, &f, cells), None, "right margin: past the last column");
        assert_eq!(cell_under(100.0, 7.0 + 15.0 * 22.0, 1, 8.25, 22, &f, cells), None, "bottom margin: past the last row");
        assert_eq!(cell_under(4.0 + 59.9 * 8.25, 7.0 + 14.9 * 22.0, 1, 8.25, 22, &f, cells), Some((59, 14)), "the last cell");
        let f2 = Frame { side: 8, top: 14, bottom: 12, stroke: 2, radius: 12 };
        assert_eq!(cell_under(104.0, 52.0, 2, 16.5, 44, &f2, cells), Some((12, 2)), "scale 2: physical cells, logical pointer");
        assert_eq!(cell_under(100.0, 45.0, 1, 0.0, 22, &f, cells), None, "no advance: no cell, no division by zero");
        assert_eq!(cell_under(100.0, 45.0, 1, 8.25, 0, &f, cells), None, "no row height: same");
    }

    #[test]
    fn keys_go_to_the_launcher_first() {
        assert_eq!(key_to(true, true), KeyTo::Launcher);
        assert_eq!(key_to(true, false), KeyTo::Launcher);
        assert_eq!(key_to(false, true), KeyTo::Popup);
        assert_eq!(key_to(false, false), KeyTo::Nobody, "a tooltip, or nothing open: the key goes nowhere");
    }

    #[test]
    fn hyprland_is_told_by_its_own_globals() {
        assert!(is_hyprland(["wl_compositor", "hyprland_focus_grab_manager_v1", "xdg_wm_base"].into_iter()));
        assert!(!is_hyprland(["wl_compositor", "zwlr_layer_shell_v1", "xdg_wm_base"].into_iter()), "sway");
        assert!(!is_hyprland(std::iter::empty()));
    }

    #[test]
    fn the_bar_goes_exclusive_during_a_grab_except_on_hyprland() {
        assert!(bar_exclusive_during_grab(false), "sway, niri: the popup gets the keyboard through the bar");
        assert!(!bar_exclusive_during_grab(true), "Hyprland: Exclusive would make every click in the menu end it");
    }

    #[test]
    fn close_reasons_have_texts() {
        use PopupClose::*;
        let all =
            [Escape, OutsideClick, Dismissed, Activated, OutputGone, Replaced, ItemGone, Timeout, MenuMoved, Launcher, HoverEnded, Error];
        assert_eq!(
            all.map(PopupClose::text),
            [
                "escape",
                "outside click",
                "dismissed",
                "activated",
                "output gone",
                "replaced",
                "item gone",
                "timeout",
                "menu moved",
                "launcher",
                "hover ended",
                "error"
            ]
        );
    }
}
