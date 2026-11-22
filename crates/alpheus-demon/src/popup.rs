//! The bar's popups (spec § 1 Menus and Calendar, § 2): an `xdg_popup`
//! attached to the bar's own layer surface on one output
//! (`zwlr_layer_surface_v1.get_popup`, issued before the popup's first
//! commit), placed by an `xdg_positioner` under the cells it hangs from,
//! drawn as rows of cells into a pool of its own. One at a time; everything
//! lives in `OpenPopup` and is dropped whole on close (nothing kept). What a
//! popup shows (`PopupBody`) and how it reacts is filled by the tooltip, the
//! menu and the calendar; this module is the Wayland side.
//!
//! The keyboard is shared with the launcher: bound while the launcher or a
//! grabbing popup needs it, released once neither does. Headless sway 1.12
//! gives no keyboard focus to a grabbing popup whose parent
//! layer surface has `KeyboardInteractivity::None`: the bar's layer surface
//! on that output is made `Exclusive` (committed before the popup is
//! created) while a grabbing popup is open, and keys arrive on the bar's
//! surface — so they are routed by "which popup is open", never by surface.

use crate::actions::{Button, Wheel};
use crate::calendar::Calendar;
use crate::grid::Target;
use crate::keys::{self, Key, Mods};
use crate::list::Boxed;
use crate::log::write as log;
use crate::media_popup::MediaPopup;
use crate::painter::{Frame, FrameInk};
use crate::panel::Panel;
use crate::power::Power;
use crate::quick::Quick;
use crate::resources_popup::ResourcesPopup;
use crate::session_bus::codec::Layout;
use crate::session_bus::menu::{Menu, MenuAction};
use crate::session_bus::menu_session::MenuSession;
use crate::timer_popup::TimerPopup;
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

/// `cell_under` on the frame as it is PAINTED: `paint_boxed` refits `frame` inside the surface
/// (`size`, logical px) so the room the cells leave over is shared evenly between the sides, and the
/// pointer must be resolved against that same refitted frame, never the raw one (the two drift at
/// scale 3, where the logical rounding leaves px over). The popups and the launcher box both hit-test
/// through here.
#[allow(clippy::too_many_arguments)]
pub fn cell_under_fitted(
    x: f64,
    y: f64,
    scale: i32,
    advance_phys: f32,
    row_height: u32,
    frame: &Frame,
    cells: (u16, u16),
    size: (u32, u32),
) -> Option<(u16, u16)> {
    let e = scale.max(1) as u32;
    let frame = frame.fitted(cells, advance_phys, row_height, (size.0 * e, size.1 * e));
    cell_under(x, y, scale, advance_phys, row_height, &frame, cells)
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
    /// Its item's `Menu` path moved under it (a duplicate registration):
    /// never a real timeout, so it gets its own, honestly-named reason.
    MenuMoved,
    Launcher,
    /// A tooltip: the pointer left its icon.
    HoverEnded,
    /// Its bar was re-placed for a new style (to or from floating): the popup's anchor and size no
    /// longer match.
    Restyled,
    /// The media popup: no player left to show (spec § 2), or the connection hosting them went.
    PlayerGone,
    /// The quick panel: `alpheus-launch --quick` again.
    Verb,
    /// The privacy popup: every indicator went off, the off-delay passed (privacy spec § 5).
    NothingCaptured,
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
            PopupClose::Restyled => "restyled",
            PopupClose::PlayerGone => "player gone",
            PopupClose::Verb => "verb",
            PopupClose::NothingCaptured => "nothing captured",
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
/// sway gives a grabbing popup of a layer surface no keyboard unless that layer
/// surface is `Exclusive`. Hyprland is the other way round: it hands the grab the keyboard by
/// itself, and before 0.56.2 (hyprwm/Hyprland#15729) an `Exclusive` layer surface swallows the
/// hit test of its own popups, so every click inside the menu is taken for a click outside and
/// ends the grab (`xdg_popup.done`) — Mae, Hyprland 0.55.4, 2026-10-06.
pub fn bar_exclusive_during_grab(on_hyprland: bool) -> bool {
    !on_hyprland
}

/// How a popup takes input (quick spec § 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// A tooltip: neither a grab nor the keyboard.
    Tooltip,
    /// Menus, the calendar, the media popup, the panels opened by a click: an xdg grab with the
    /// press's serial, the keyboard, closed by a click elsewhere.
    Grab,
    /// The quick panel opened by `alpheus-launch --quick`: no press to grab with, so no grab; the
    /// bar's layer surface goes `Exclusive` on every compositor, so the keys reach it.
    Keys,
}

/// What a popup shows: a tray item's menu, its hover tooltip, or one of the module popups
/// (`ModulePopup`); dropped whole on close.
pub enum PopupBody {
    Menu(Menu),
    Calendar(Calendar),
    /// One row of text.
    Tooltip(String),
    /// The media module's popup (title, time, buttons, players).
    Media(MediaPopup),
    /// The notifications panel under the bell (the media card, the history, DND, Clear all).
    Panel(Panel),
    /// The quick settings panel (quick spec), boxed: it is several times the other bodies' size.
    Quick(Box<Quick>),
    /// The ⏻ power menu (power spec § 2).
    Power(Power),
    /// The privacy popup (privacy spec § 5): its rows are drawn from `Bar.privacy`, nothing kept here.
    Privacy,
    /// The resources popup (resources popup spec § 2): figures and graphs per item, `Monitor…`.
    Resources(ResourcesPopup),
    /// The timer popup (timer spec § 3).
    Timer(TimerPopup),
}

/// A bar module whose left click toggles a popup of its own: the clock's calendar, the media
/// popup, the notifications panel, the quick settings panel, the ⏻ power menu, the privacy
/// popup. It is the anchor's and the press race's single source of truth (wayland.rs
/// `module_popup_at`, `closed_by_done`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModulePopup {
    Calendar,
    /// The media module's popup.
    Media,
    /// The bell's panel.
    Panel,
    /// The quick settings panel: the volume, network and Bluetooth modules.
    Quick,
    /// The ⏻ power menu: the power button (power spec § 1).
    Power,
    /// The privacy popup: the privacy module (privacy spec § 5).
    Privacy,
    /// The resources popup: the resources module (resources popup spec § 1).
    Resources,
    /// The timer chip's popup.
    Timer,
}

impl ModulePopup {
    /// The popup a press of `b` on `target` toggles: a left press on its module's zone. `quick`:
    /// the quick module is in `modules` (without it, those three modules keep their own clicks).
    pub fn of(target: &Target, b: Button, quick: bool) -> Option<ModulePopup> {
        let (Target::Module(m), Button::Left) = (target, b) else { return None };
        let spec = m.spec();
        spec.popup.filter(|_| quick || !spec.popup_needs_quick)
    }

    /// This popup's row of `POPUPS`.
    pub(crate) fn ops(self) -> &'static PopupOps {
        let row = &POPUPS[self as usize];
        debug_assert_eq!(row.kind, self, "POPUPS out of variant order");
        row
    }
}

/// What the generic popup code calls for one module popup; each row lives in its `*_app.rs`.
pub(crate) struct PopupOps {
    /// The popup this row is for: its index in `POPUPS`.
    pub kind: ModulePopup,
    /// A press on its module (`click`, wayland.rs): opens it under `anchor`, or closes it when it
    /// is open. The target is the zone pressed (the quick panel's section).
    pub toggle: fn(&mut App, &QueueHandle<App>, Anchor, &Target),
    /// A key while it is open.
    pub key: fn(&mut App, Key),
    /// The pointer on it, at a cell (column, row) of its rows; None: in the frame's margin.
    pub pointer: fn(&mut App, Option<(u16, u16)>, PopupPointer),
    /// `popup_close`, before its body goes: what still needs the body. `no_hook` when nothing does.
    pub flush: fn(&mut App),
    /// `popup_close`, once its body is dropped, whatever the reason: its timers and taps let go.
    /// `no_hook` when it holds none.
    pub closed: fn(&mut App),
}

/// The module popups, one row per `ModulePopup` variant, in variant order (a test checks it). A new
/// popup: its variant, its body in `PopupBody` and `module_popup`, an `OPS` row in its `*_app.rs`,
/// listed here, and the module's `popup` in `modules.rs`.
static POPUPS: [PopupOps; 8] = [
    crate::calendar_app::OPS,
    crate::media_app::OPS,
    crate::panel_app::OPS,
    crate::quick_app::OPS,
    crate::power_app::OPS,
    crate::privacy_app::OPS,
    crate::resources_app::OPS,
    crate::timer_app::OPS,
];

/// A row's hook for a popup with nothing to do there.
pub(crate) fn no_hook(_: &mut App) {}

impl PopupBody {
    /// The module whose click opened this popup, if any.
    pub fn module_popup(&self) -> Option<ModulePopup> {
        match self {
            PopupBody::Calendar(_) => Some(ModulePopup::Calendar),
            PopupBody::Media(_) => Some(ModulePopup::Media),
            PopupBody::Panel(_) => Some(ModulePopup::Panel),
            PopupBody::Quick(_) => Some(ModulePopup::Quick),
            PopupBody::Power(_) => Some(ModulePopup::Power),
            PopupBody::Privacy => Some(ModulePopup::Privacy),
            PopupBody::Resources(_) => Some(ModulePopup::Resources),
            PopupBody::Timer(_) => Some(ModulePopup::Timer),
            PopupBody::Menu(_) | PopupBody::Tooltip(_) => None,
        }
    }
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
    /// The keyboard and closing on a click on the bar (menus, the calendar, the panels, a keyed
    /// quick panel too); false for a tooltip.
    pub grab: bool,
    /// The bar's `wl_surface` this popup was opened under (cloned from its `layer.wl_surface()`
    /// at `popup_open`): `popup_close`/`popup_output_gone` reset THIS surface's keyboard
    /// interactivity, by identity — never by `anchor.output` or a bar's `name`, which
    /// `update_output` can change after the fact, or which two bars could briefly share.
    parent: wl_surface::WlSurface,
    /// Opened without an xdg grab (`Input::Keys`): the bar went `Exclusive` on every compositor,
    /// and goes back on close.
    keyed: bool,
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

    /// The bar's surface this popup hangs under (compared by identity, never by output name).
    pub(crate) fn parent_surface(&self) -> &wl_surface::WlSurface {
        &self.parent
    }
}

/// Who gets a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyTo {
    Launcher,
    Notes,
    Popup,
    Nobody,
}

/// The launcher first (it never coexists with a popup, but if it did, typing goes to it), then the
/// quick notepad (never open with the launcher either), then a grabbing popup; a tooltip never
/// takes keys.
pub(crate) fn key_to(launcher_open: bool, notes_open: bool, grabbing_popup: bool) -> KeyTo {
    if launcher_open {
        KeyTo::Launcher
    } else if notes_open {
        KeyTo::Notes
    } else if grabbing_popup {
        KeyTo::Popup
    } else {
        KeyTo::Nobody
    }
}

/// Most keys `KeysDown` follows at once (more than a hand can hold).
pub(crate) const MAX_KEYS_DOWN: usize = 16;

/// The keyboard feature's state, one field of `App`: the keyboard while bound and what follows its keys.
#[derive(Default)]
pub(crate) struct Keyboard {
    /// The keyboard, bound only while the launcher or a grabbing popup needs it.
    pub(crate) device: Option<wl_keyboard::WlKeyboard>,
    pub(crate) mods: Mods,
    /// Keys down on `device` (`KeysDown`): it is never released while one is down on a
    /// live focus (`release_decision`).
    pub(crate) keys_down: KeysDown,
    /// The surface `device` entered, until its leave: alive or not decides whether a release waits.
    pub(crate) focus: Option<wl_surface::WlSurface>,
    /// `keyboard_release` was asked while a key was down: done at its release or a leave.
    pub(crate) release_deferred: bool,
    /// Released keyboards still held elsewhere (`retired`): dropped after a `dispatch`, never in one.
    pub(crate) retired: crate::retired::Retired<wl_keyboard::WlKeyboard>,
    /// The key being handled is SCTK's repeat, not a press (the panel repeats the arrows only).
    pub(crate) repeat: bool,
    /// The last key's time (dbusmenu Event's timestamp).
    pub(crate) time: u32,
}

/// The keys down on the bound keyboard, by raw code: those `enter` reports already held, then each
/// press until its release; a leave clears them. A set, so a release of a key never counted (held
/// before the keyboard was bound and not reported) cannot make another one look up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KeysDown {
    codes: Vec<u32>,
}

impl KeysDown {
    pub(crate) fn entered(&mut self, held: &[u32]) {
        self.codes.clear();
        held.iter().for_each(|&c| self.pressed(c));
    }

    /// Full, the oldest code goes: the key just pressed is the one whose release matters.
    pub(crate) fn pressed(&mut self, code: u32) {
        if self.codes.contains(&code) {
            return;
        }
        if self.codes.len() >= MAX_KEYS_DOWN {
            self.codes.remove(0);
        }
        self.codes.push(code);
    }

    pub(crate) fn released(&mut self, code: u32) {
        self.codes.retain(|&c| c != code);
    }

    pub(crate) fn clear(&mut self) {
        self.codes.clear();
    }

    pub(crate) fn any(&self) -> bool {
        !self.codes.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.codes.len()
    }
}

/// What `keyboard_release` does with the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    /// The launcher or a grabbing popup still needs it.
    Keep,
    /// A key is down on a live focus: kept until every key is up or the focus leaves.
    Defer,
    Now,
}

/// SCTK repeats a held key on a calloop timer that stops only on that key's release, a leave, or
/// the focused surface dying. Under a grab on sway or niri the focus is the bar's own layer surface
/// (`exclusive_during_grab`), which lives on: a keyboard released while the key that closed the
/// popup is still down gets neither event, and its timer repeats that key forever — so the release
/// waits. A dead focus (the launcher's box, a popup on Hyprland) already stops the repeat, and may
/// never bring the key-up nor a leave: released at once.
fn release_decision(wanted: bool, keys: &KeysDown, focus_alive: bool) -> Release {
    if wanted {
        Release::Keep
    } else if keys.any() && focus_alive {
        Release::Defer
    } else {
        Release::Now
    }
}

/// "2026-10": the shown month, for the log.
pub(crate) fn calendar_month(c: &Calendar) -> String {
    format!("{}-{:02}", c.shown.year, c.shown.month)
}

impl App {
    /// Closes any open popup (Replaced) and the launcher (it closes with ClickOutside first, decision of
    /// the launcher plan), then opens `body` under `anchor`, `columns` × `rows` cells (the frame's margin
    /// comes on top: `Fonts::frame`). `input`: `Grab` takes the keyboard and closes on an outside
    /// click (uses `last_press`'s serial); `Keys` takes the keyboard without a grab (the quick verb);
    /// `Tooltip` neither. false when the output's bar is not configured.
    pub fn popup_open(&mut self, qh: &QueueHandle<App>, anchor: Anchor, body: PopupBody, columns: u16, rows: u16, input: Input) -> bool {
        let (grab, keyed) = (input != Input::Tooltip, input == Input::Keys);
        // Every refusal first (moved above the launcher/popup closes below:
        // `SlotPool::new` and `XdgPositioner::new` can fail too, and must refuse here, before
        // anything already shown is torn down): a popup that cannot open closes neither the
        // launcher nor the popup already shown.
        if self.xdg.is_none() {
            return false;
        }
        let Some(i) = self.bar_surface(&anchor.output) else { return false };
        let serial = match (input, self.last_press) {
            (Input::Grab, Some((serial, _, _))) => Some(serial),
            (Input::Grab, None) => return false, // a grab needs the press that asked for it
            _ => None,
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
        // A grabbing popup closes the quick notepad the same way (it saves); a tooltip never does.
        if self.notes_is_open() {
            if !grab {
                return false;
            }
            self.notes_guarded("popup", |app| app.notes_close(crate::launcher::surface::CloseReason::ClickOutside));
        }
        if self.popup.is_some() {
            self.popup_close(PopupClose::Replaced);
        }
        // Closing the launcher or a popup never touches `surfaces`: `i` still points at the bar.
        let layer = self.surfaces[i].layer.clone();
        let parent = layer.wl_surface().clone();
        if grab && (self.exclusive_during_grab || keyed) {
            // Fact check 5: committed before the popup exists, so the bar's surface holds the
            // keyboard focus while the grab lasts (back to None on close). A keyed popup has no
            // grab to hand it the keyboard: the bar goes Exclusive on every compositor.
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
                // Only what was set: on Hyprland nothing was (`exclusive_during_grab`), and a
                // needless commit on the bar is a needless roundtrip of its own.
                if grab && (self.exclusive_during_grab || keyed) {
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
            keyed,
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
    /// `popup_scale` (the SAME cell count can still occupy a slightly different
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

    /// The open popup redrawn even if its rows did not change (a new theme).
    pub(crate) fn popup_repaint(&mut self) {
        if let Some(open) = self.popup.as_mut() {
            open.last = None;
        }
        self.popup_draw();
    }

    fn popup_draw(&mut self) {
        let height = self.bar.config.height;
        let App { popup, fonts, roles, .. } = self;
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
        fonts.paint_boxed(rows, roles, px, w, h, height * e as u32, e, FrameInk::Border);
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
        let menu = matches!(self.popup_body(), Some(PopupBody::Menu(_)));
        // A module popup's own hooks (its `POPUPS` row): `flush` before its body goes, `closed` after.
        let kind = self.popup_body().and_then(PopupBody::module_popup);
        // The compositor may deliver `xdg_popup.
        // done` (routed here as Dismissed) before the press that follows it reaches the bar — a
        // record of what just closed, by whom it was opened, and when, lets that next press still
        // count as only-closing (`closed_recently`, wayland.rs) even though this popup is already
        // gone by the time it arrives.
        if menu && why == PopupClose::Dismissed {
            if let (Some((key, _)), Some(button)) = (self.menu.target.clone(), self.menu.opened_by) {
                self.menu.recently_closed = Some((key, button, Instant::now()));
            }
        }
        // The same race for a module's popup (the calendar; the media popup): a left press on
        // its module right after a compositor `done` only closes (wayland.rs `closed_by_done`).
        if why == PopupClose::Dismissed
            && let Some(p) = kind
        {
            self.recently_closed_popup = Some((p, Instant::now()));
        }
        if (menu && why != PopupClose::Replaced) || (why == PopupClose::Launcher && self.menu.target.is_some()) {
            self.menu_end();
        }
        if let Some(p) = kind {
            (p.ops().flush)(self);
        }
        let Some(open) = self.popup.take() else { return };
        let (grab, keyed, parent) = (open.grab, open.keyed, open.parent.clone());
        // Whatever the reason: a closed tooltip is never left pairing `menu.tooltip_item` with a popup
        // that no longer exists — `tooltip_item_gone` (wayland.rs)
        // relies on the two always being set and cleared together.
        if matches!(open.body, PopupBody::Tooltip(_)) {
            self.menu.tooltip_item = None;
        }
        drop(open);
        if let Some(p) = kind {
            // Whatever the reason (Escape, outside click, restyle, output gone, replaced).
            (p.ops().closed)(self);
        }
        if grab {
            // By the parent's surface IDENTITY, never by `anchor.output` or a bar's `name`:
            // a name can change after the fact (`update_output`) or be
            // shared for a moment by two bars, and the wrong bar — or none — would stay stuck
            // `Exclusive`, holding Mae's keyboard.
            // Only if `popup_open` set it (`exclusive_during_grab`: not on Hyprland, unless keyed).
            let bar =
                if self.exclusive_during_grab || keyed { position_by(&self.surfaces, |s| s.layer.wl_surface(), &parent) } else { None };
            if let Some(i) = bar {
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

    /// The open popup's body, if any.
    pub(crate) fn popup_body(&self) -> Option<&PopupBody> {
        self.popup.as_ref().map(|p| &p.body)
    }

    /// The open popup's body, mutably.
    pub(crate) fn popup_body_mut(&mut self) -> Option<&mut PopupBody> {
        self.popup.as_mut().map(|p| &mut p.body)
    }

    /// A key while a grabbing popup is open (the launcher is closed).
    pub(crate) fn popup_key(&mut self, k: Key) {
        let Some(body) = self.popup_body() else { return };
        match body {
            PopupBody::Menu(_) => {
                let action = self.menu_with(|m, layout| m.key(k, layout));
                self.menu_action(action, self.keyboard.time);
            }
            PopupBody::Tooltip(_) => {}
            // The module popups: their own row (`POPUPS`).
            PopupBody::Calendar(_)
            | PopupBody::Media(_)
            | PopupBody::Panel(_)
            | PopupBody::Quick(_)
            | PopupBody::Power(_)
            | PopupBody::Privacy
            | PopupBody::Resources(_)
            | PopupBody::Timer(_) => {
                if let Some(p) = body.module_popup() {
                    (p.ops().key)(self, k);
                }
            }
        }
    }

    /// Runs `f` on the open menu and its session's layout; Nothing when either is missing — the
    /// session ended under it (its item gone, its path moved, a call timed out), which `tray_
    /// output` has already closed and logged with the right reason: a fabricated `MenuAction::Close` here would make `menu_action`
    /// log it again,
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
        let Some(body) = self.popup_body() else { return };
        match body {
            // One entry per row: the column never matters.
            PopupBody::Menu(_) => {
                let content = cell.map(|(_, row)| row);
                let action = match (kind, content) {
                    (PopupPointer::Motion, Some(r)) => self.menu_with(|m, l| m.hover(r, l)),
                    (PopupPointer::Press(Button::Left), Some(r)) => self.menu_with(|m, l| m.press(r, l)),
                    (PopupPointer::Scroll(steps), _) => self.menu_with(|m, l| m.scroll(steps, l)),
                    _ => MenuAction::Nothing,
                };
                // The popup's OWN last press, never the
                // bar's `last_press` — that one is the click that asked the popup to open (a
                // right click, say), not necessarily the one running an entry inside it.
                self.menu_action(action, self.popup_press_time);
            }
            // A press on the tooltip dismisses it (nothing in it is clickable).
            PopupBody::Tooltip(_) => {
                if let PopupPointer::Press(_) = kind {
                    self.popup_close(PopupClose::HoverEnded);
                }
            }
            // The module popups: their own row (`POPUPS`).
            PopupBody::Calendar(_)
            | PopupBody::Media(_)
            | PopupBody::Panel(_)
            | PopupBody::Quick(_)
            | PopupBody::Power(_)
            | PopupBody::Privacy
            | PopupBody::Resources(_)
            | PopupBody::Timer(_) => {
                if let Some(p) = body.module_popup() {
                    (p.ops().pointer)(self, cell, kind);
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
        let cell = cell_under_fitted(position.0, position.1, e, self.fonts.advance(e), height * e as u32, &frame, cells, open.size);
        self.popup_pointer(cell, kind);
        true
    }

    /// Closes the open popup if it hangs from `parent` (a bar about to be re-placed).
    pub(crate) fn popup_close_if_parent(&mut self, parent: &wl_surface::WlSurface, why: PopupClose) {
        if self.popup.as_ref().is_some_and(|p| &p.parent == parent) {
            self.popup_close(why);
        }
    }

    /// The popup's parent bar surface goes (output unplugged, surface closed): the popup goes
    /// first — compared by surface IDENTITY, never by `anchor.output`
    /// or a bar's `name`, both of which can go stale (see `popup_close`).
    pub(crate) fn popup_output_gone(&mut self, parent: &wl_surface::WlSurface) {
        if self.popup.as_ref().is_some_and(|p| &p.parent == parent) {
            self.popup_close(PopupClose::OutputGone);
        }
        // A tooltip's hover tracking must not survive the bar it was armed on going away:
        // left alone, `Hover.over` would still name the same
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
            // Through `popup_paint`'s own resize path: the placement must be
            // recomputed at the new scale, not just redrawn at the old one.
            if !self.popup_reposition(cells) {
                self.popup_draw();
            }
        }
        true
    }

    /// Whether the keyboard is needed: the launcher, the quick notepad, or a grabbing popup.
    pub(crate) fn keyboard_wanted(&self) -> bool {
        self.launcher_is_open() || self.notes_is_open() || self.popup.as_ref().is_some_and(|p| p.grab)
    }

    /// Binds the keyboard (with repeat on the event loop) on the seat if none: the launcher is open, or a
    /// grabbing popup. (The guardrail counts the binding call: never name it in a comment.)
    pub(crate) fn keyboard_acquire(&mut self, qh: &QueueHandle<App>) {
        if self.keyboard_wanted() {
            // Wanted again before a deferred release happened: kept.
            self.keyboard.release_deferred = false;
        }
        if self.keyboard.device.is_some() || !self.keyboard_wanted() {
            return;
        }
        let (Some(handle), Some(seat)) = (self.loop_handle.clone(), self.seat.clone()) else { return };
        let repeat = Box::new(|app: &mut App, _: &wl_keyboard::WlKeyboard, event: KeyEvent| {
            // SCTK's own repeat: the quick panel lets only the arrows repeat (quick spec § 3).
            app.keyboard.repeat = true;
            app.key_event(event);
            app.keyboard.repeat = false;
        });
        match self.seats.get_keyboard_with_repeat(qh, &seat, None, handle, repeat) {
            Ok(k) => self.keyboard.device = Some(k),
            // UnsupportedCapability just means the seat has no keyboard yet
            // (retried once the capability appears, new_capability): routine,
            // not worth a line every time.
            Err(e) if crate::launcher::surface::should_log_keyboard_error(&e) => log(&format!("keyboard: {e}")),
            Err(_) => {}
        }
    }

    /// release() once neither the launcher nor a grabbing popup needs it, and no key is down
    /// (`release_decision`): else at that key's release or the focus leaving.
    pub(crate) fn keyboard_release(&mut self) {
        let focus_alive = self.keyboard.focus.as_ref().is_some_and(Proxy::is_alive);
        match release_decision(self.keyboard_wanted(), &self.keyboard.keys_down, focus_alive) {
            Release::Keep => return,
            Release::Defer => {
                self.keyboard.release_deferred = true;
                return;
            }
            Release::Now => {}
        }
        self.keyboard.release_deferred = false;
        if let Some(k) = self.keyboard.device.take() {
            // The keyboards released before and still held: 0 unless one is still in use elsewhere.
            let earlier = self.keyboard.retired.len();
            self.keyboard_retire(k);
            log(&format!("keyboard released ({earlier} retired)"));
        }
        // What the released keyboard reported is gone with it.
        self.keyboard.keys_down.clear();
        self.keyboard.focus = None;
        self.keyboard.mods = Mods::default();
    }

    /// A key went up or the focus left: the release `keyboard_release` deferred, if any, now.
    pub(crate) fn keyboard_release_deferred(&mut self) {
        if self.keyboard.release_deferred {
            self.keyboard_release();
        }
    }

    /// The seat lost its keyboard: the object goes now, whoever wanted it, and with it the
    /// modifiers it reported (a Ctrl held then must not stick to the next keyboard).
    /// A key repeating then, with no `leave` first, keeps repeating until its surface dies: SCTK
    /// gives no way to stop it (`retired`'s module doc).
    pub(crate) fn keyboard_lost(&mut self) {
        if let Some(k) = self.keyboard.device.take() {
            self.keyboard_retire(k);
        }
        (self.keyboard.keys_down, self.keyboard.focus, self.keyboard.release_deferred) = (KeysDown::default(), None, false);
        self.keyboard.mods = Mods::default();
    }

    /// Releases `k` and keeps it until no one else holds it (`retired`): wayland-backend drops its own
    /// reference when it reads the compositor's `delete_id`, inside calloop's dispatch, and the last
    /// one dropped there would remove SCTK's repeat token from the loop while calloop borrows it.
    /// Before version 3 there is no release: the backend keeps the object to the end, never ours.
    fn keyboard_retire(&mut self, k: wl_keyboard::WlKeyboard) {
        if k.version() >= 3 {
            k.release();
            self.keyboard.retired.hold(k);
        }
    }

    /// A key (press or repeat): to the launcher when it is open, else to a grabbing popup.
    pub(crate) fn key_event(&mut self, event: KeyEvent) {
        self.keyboard.time = event.time;
        match key_to(self.launcher_is_open(), self.notes_is_open(), self.popup.as_ref().is_some_and(|p| p.grab)) {
            KeyTo::Launcher => {
                self.launcher_guarded("key", move |app| app.launcher_key(event));
            }
            KeyTo::Notes => {
                self.notes_guarded("key", move |app| app.notes_key(event));
            }
            KeyTo::Popup => {
                if let Some(k) = keys::translate(event.keysym, event.utf8.as_deref(), self.keyboard.mods) {
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
        surface: &wl_surface::WlSurface,
        _: u32,
        held: &[u32],
        _: &[Keysym],
    ) {
        // The keys already down count: their release must not make another key look up.
        self.keyboard.keys_down.entered(held);
        self.keyboard.focus = Some(surface.clone());
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &wl_surface::WlSurface, _: u32) {
        // A leave ends every repeat (SCTK): no key counts as down any more.
        self.keyboard.keys_down.clear();
        self.keyboard.focus = None;
        if self.launcher_owns_box(surface) {
            self.launcher_guarded("focus", |app| app.launcher_close(crate::launcher::surface::CloseReason::FocusLost));
        }
        if self.notes_owns_box(surface) {
            self.notes_guarded("focus", |app| app.notes_close(crate::launcher::surface::CloseReason::FocusLost));
        }
        self.keyboard_release_deferred();
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.keyboard.keys_down.pressed(event.raw_code);
        self.key_event(event);
    }

    /// wl_keyboard v10 repeats (compositor-side); calloop repeats go through
    /// the callback given at bind time, to the same place.
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.keyboard.repeat = true;
        self.key_event(event);
        self.keyboard.repeat = false;
    }

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.keyboard.keys_down.released(event.raw_code);
        self.keyboard_release_deferred();
    }

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
        self.keyboard.mods = Mods { ctrl: m.ctrl, shift: m.shift, alt: m.alt, logo: m.logo };
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

    /// Which hooks run at a close hang on this map: every module body names its popup.
    #[test]
    fn every_module_body_names_its_popup_and_the_others_none() {
        use crate::calendar::Calendar;
        use crate::clock::LocalTime;
        use crate::config::{NotificationsConfig, QuickConfig};
        use crate::media::Players;
        use crate::notify::Center;
        use crate::quick::Live;
        let today = LocalTime { year: 2026, month: 10, day: 5, weekday: 1, hour: 9, minute: 30, second: 0 };
        let config = QuickConfig::default();
        let live = Live { sound: None, dnd: None, dnd_end: None, profile: None, config: &config, bluetooth: None, shield: None };
        let center = Center::new(&NotificationsConfig::default(), false);
        let bodies = [
            (PopupBody::Calendar(Calendar::new(&today, 1, false)), Some(ModulePopup::Calendar)),
            (PopupBody::Media(MediaPopup::new(&Players::default())), Some(ModulePopup::Media)),
            (PopupBody::Panel(Panel::new(&center, None, 40)), Some(ModulePopup::Panel)),
            (
                PopupBody::Quick(Box::new(Quick::new(crate::quick::Focus::Top, &live, Vec::new(), None, [None; 2], 40))),
                Some(ModulePopup::Quick),
            ),
            (PopupBody::Power(Power::new(&config).unwrap()), Some(ModulePopup::Power)),
            (PopupBody::Privacy, Some(ModulePopup::Privacy)),
            (PopupBody::Resources(ResourcesPopup::default()), Some(ModulePopup::Resources)),
            (PopupBody::Timer(TimerPopup::new(10, &crate::timer::Timer::Idle { done: false })), Some(ModulePopup::Timer)),
            (PopupBody::Menu(Menu::new()), None),
            (PopupBody::Tooltip("x".into()), None),
        ];
        for (body, popup) in bodies {
            assert_eq!(body.module_popup(), popup);
        }
    }

    #[test]
    fn only_a_left_press_on_the_clock_toggles_its_popup() {
        use crate::config::ModuleId;
        use crate::grid::Target;
        let clock = Target::Module(ModuleId::Clock);
        assert_eq!(ModulePopup::of(&clock, Button::Left, false), Some(ModulePopup::Calendar));
        for b in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
            assert_eq!(ModulePopup::of(&clock, b, false), None, "{b:?}");
        }
        assert_eq!(ModulePopup::of(&Target::Module(ModuleId::Volume), Button::Left, false), None);
        assert_eq!(PopupBody::Tooltip("x".into()).module_popup(), None, "a tooltip belongs to no module");
        // The media module's left press toggles its own popup; its middle button and
        // wheel are controls, never a popup.
        let media = Target::Module(ModuleId::Media);
        assert_eq!(ModulePopup::of(&media, Button::Left, false), Some(ModulePopup::Media));
        for b in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
            assert_eq!(ModulePopup::of(&media, b, false), None, "{b:?}");
        }
        let body = PopupBody::Media(MediaPopup { selected: 0, button: 1, chosen: None });
        assert_eq!(body.module_popup(), Some(ModulePopup::Media), "a second press on the module only closes it");
    }

    /// Quick spec § 1: with the module, a left press on the volume, the network or Bluetooth
    /// toggles the panel (one popup for the three); the right press is the TUI tab's;
    /// the battery keeps its click.
    #[test]
    fn a_left_press_on_volume_network_or_bluetooth_toggles_the_quick_panel() {
        use crate::config::ModuleId;
        use crate::grid::Target;
        for m in [ModuleId::Volume, ModuleId::Network, ModuleId::Bluetooth] {
            let t = Target::Module(m);
            assert_eq!(ModulePopup::of(&t, Button::Left, true), Some(ModulePopup::Quick), "{m:?}");
            assert_eq!(ModulePopup::of(&t, Button::Left, false), None, "{m:?}: without the module, today's click");
            assert_eq!(ModulePopup::of(&t, Button::Right, true), None, "{m:?}");
        }
        assert_eq!(ModulePopup::of(&Target::Module(ModuleId::Battery), Button::Left, true), None, "the battery keeps its click");
    }

    /// Power spec § 1: a left press on the power button toggles its menu, with or without the quick
    /// module; the other buttons do nothing; a second press on it only closes the menu.
    #[test]
    fn a_left_press_on_the_power_button_toggles_its_menu() {
        use crate::config::{ModuleId, QuickConfig};
        use crate::grid::Target;
        let power = Target::Module(ModuleId::Power);
        for quick in [false, true] {
            assert_eq!(ModulePopup::of(&power, Button::Left, quick), Some(ModulePopup::Power), "quick: {quick}");
            for b in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
                assert_eq!(ModulePopup::of(&power, b, quick), None, "{b:?}");
            }
        }
        let body = PopupBody::Power(Power::new(&QuickConfig::default()).expect("the default entries"));
        assert_eq!(body.module_popup(), Some(ModulePopup::Power), "a second press on ⏻ only closes it");
    }

    /// Privacy spec § 5: a left press on the privacy module toggles its popup; the middle click
    /// mutes the source (actions.rs), the other buttons do nothing here.
    #[test]
    fn a_left_press_on_the_privacy_module_toggles_its_popup() {
        use crate::config::ModuleId;
        use crate::grid::Target;
        let privacy = Target::Module(ModuleId::Privacy);
        for quick in [false, true] {
            assert_eq!(ModulePopup::of(&privacy, Button::Left, quick), Some(ModulePopup::Privacy), "quick: {quick}");
            for b in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
                assert_eq!(ModulePopup::of(&privacy, b, quick), None, "{b:?}");
            }
        }
        assert_eq!(PopupBody::Privacy.module_popup(), Some(ModulePopup::Privacy), "a second press only closes it");
    }

    /// Timer spec § 3: a left press on the timer chip toggles its popup, with or without the quick
    /// module; the middle click is the chip's own (pause or resume), the others do nothing.
    #[test]
    fn a_left_press_on_the_timer_chip_toggles_its_popup() {
        use crate::config::ModuleId;
        use crate::grid::Target;
        let timer = Target::Module(ModuleId::Timer);
        for quick in [false, true] {
            assert_eq!(ModulePopup::of(&timer, Button::Left, quick), Some(ModulePopup::Timer), "quick: {quick}");
            for b in [Button::Right, Button::Middle, Button::WheelUp, Button::WheelDown] {
                assert_eq!(ModulePopup::of(&timer, b, quick), None, "{b:?}");
            }
        }
    }

    /// Every variant with the row it must have. The `match` is exhaustive: a variant added to
    /// `ModulePopup` stops this file compiling until it is listed here, and listed, its row must exist.
    macro_rules! rows {
        ($($variant:ident => $index:expr),* $(,)?) => {
            #[allow(dead_code)]
            fn row_of(p: ModulePopup) -> usize {
                match p {
                    $(ModulePopup::$variant => $index),*
                }
            }
            const LISTED: &[(ModulePopup, usize)] = &[$((ModulePopup::$variant, $index)),*];
        };
    }

    rows! { Calendar => 0, Media => 1, Panel => 2, Quick => 3, Power => 4, Privacy => 5, Resources => 6, Timer => 7 }

    /// `POPUPS` is indexed by `ModulePopup as usize`: one row per variant, in variant order.
    #[test]
    fn one_popup_row_per_variant_in_variant_order() {
        for &(p, i) in LISTED {
            assert_eq!(p as usize, i, "{p:?}'s discriminant is not its row");
            assert_eq!(POPUPS.get(i).map(|r| r.kind), Some(p), "{p:?} has no row at {i}");
            assert_eq!(p.ops().kind, p, "{p:?}");
        }
        assert_eq!(LISTED.len(), POPUPS.len(), "a row for no variant, or a variant for no row");
        for (i, row) in POPUPS.iter().enumerate() {
            assert_eq!(row.kind as usize, i, "{:?} is not at its own index", row.kind);
        }
    }

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
    fn the_fitted_hit_test_follows_the_painted_frame_at_scale_3() {
        // Scale 3: 20 columns of 24.75 px (495 px) + 2 x 12 = 519 px of cells and frame, in a
        // 174-logical-px (522 px) surface: 3 px over, 1 more on the left than the raw frame says.
        let f = Frame { side: 12, top: 21, bottom: 18, stroke: 3, radius: 18 };
        let (cells, advance, row) = ((20, 4), 24.75_f32, 66);
        let size = (174, 130); // 390 px: 4 rows of 66 = 264 + 39 = 303, 87 px over
        let painted = f.fitted(cells, advance, row, (size.0 * 3, size.1 * 3));
        assert!(painted.side > f.side && painted.top > f.top, "the surface leaves room over: {painted:?}");
        // Half a physical pixel into the painted first cell: cell (0, 0).
        let (x, y) = ((painted.side as f64 + 0.5) / 3.0, (painted.top as f64 + 0.5) / 3.0);
        assert_eq!(cell_under_fitted(x, y, 3, advance, row, &f, cells, size), Some((0, 0)));
        // Half a pixel into the RAW frame's first cell: the painted margin, so no cell; the raw
        // `cell_under` would answer (0, 0), which is the drift this guards.
        let (x, y) = ((f.side as f64 + 0.5) / 3.0, (f.top as f64 + 0.5) / 3.0);
        assert_eq!(cell_under_fitted(x, y, 3, advance, row, &f, cells, size), None, "still in the painted margin");
        assert_eq!(cell_under(x, y, 3, advance, row, &f, cells), Some((0, 0)), "the raw frame disagrees");
    }

    #[test]
    fn keys_go_to_the_launcher_first() {
        assert_eq!(key_to(true, false, true), KeyTo::Launcher);
        assert_eq!(key_to(true, false, false), KeyTo::Launcher);
        assert_eq!(key_to(false, false, true), KeyTo::Popup);
        assert_eq!(key_to(false, false, false), KeyTo::Nobody, "a tooltip, or nothing open: the key goes nowhere");
        // Quick notes: the pad after the launcher (never open together), before a popup.
        assert_eq!(key_to(false, true, false), KeyTo::Notes);
        assert_eq!(key_to(false, true, true), KeyTo::Notes, "the pad takes the keys over a popup");
        assert_eq!(key_to(true, true, false), KeyTo::Launcher);
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

    /// A popup closed by a key press (Escape, Enter on an entry) must not release the keyboard
    /// while that key is down: SCTK's repeat timer stops only on its release or a leave, and on
    /// sway or niri the focused surface is the bar's own, which never dies — released early, the
    /// key repeated at the compositor's rate forever.
    #[test]
    fn the_keyboard_goes_only_once_its_keys_are_up() {
        const SHIFT: u32 = 42;
        const ESCAPE: u32 = 1;
        let mut k = KeysDown::default();
        assert_eq!(release_decision(false, &k, true), Release::Now, "nothing down");
        // Shift already held when the grab gives the bar the keyboard: `enter` lists it.
        k.entered(&[SHIFT]);
        assert_eq!(release_decision(false, &k, true), Release::Defer, "Shift, held since before the keyboard came, counts");
        k.pressed(ESCAPE); // closes the popup
        assert_eq!(release_decision(true, &k, true), Release::Keep, "still wanted: the launcher or a grabbing popup");
        assert_eq!(release_decision(false, &k, true), Release::Defer, "Escape and Shift down");
        k.released(SHIFT);
        assert_eq!(release_decision(false, &k, true), Release::Defer, "Shift up, Escape still down: its repeat still runs");
        k.released(ESCAPE);
        assert_eq!(release_decision(false, &k, true), Release::Now, "every key up");
        k.released(ESCAPE);
        k.released(SHIFT);
        assert!(!k.any(), "a release of a key not down changes nothing");
        // A press seen twice (a missed release) is still one key: one release clears it.
        k.pressed(ESCAPE);
        k.pressed(ESCAPE);
        k.released(ESCAPE);
        assert!(!k.any());
        // The focused surface is gone (the launcher's box, a popup on Hyprland): SCTK's repeat
        // stops by itself, and neither the key-up nor a leave may ever come to us.
        k.pressed(ESCAPE);
        assert_eq!(release_decision(false, &k, false), Release::Now, "a dead focus never defers");
        // A leave ends every repeat: nothing counts as down.
        k.clear();
        assert!(!k.any());
        assert_eq!(release_decision(false, &k, true), Release::Now);
        // A release deferred on a live focus goes once that focus dies (the bar's surface removed
        // with its output mid-Escape on sway: no key-up nor leave may come to it any more).
        k.pressed(ESCAPE);
        assert_eq!(release_decision(false, &k, true), Release::Defer);
        assert_eq!(release_decision(false, &k, false), Release::Now, "its focus gone: released");
        k.clear();
        // Bounded: a flood of presses keeps at most MAX_KEYS_DOWN codes.
        (0..1000).for_each(|c| k.pressed(c));
        assert_eq!(k.len(), MAX_KEYS_DOWN);
        // Full: a new press drops the oldest code, never the new key (its release must still be
        // waited for, or its repeat runs on).
        k.clear();
        (0..MAX_KEYS_DOWN as u32).for_each(|c| k.pressed(c));
        k.pressed(ESCAPE + 100);
        (0..MAX_KEYS_DOWN as u32).for_each(|c| k.released(c));
        assert!(k.any(), "the newest key is still down");
        assert_eq!(release_decision(false, &k, true), Release::Defer);
        k.released(ESCAPE + 100);
        assert!(!k.any());
        k.entered(&[SHIFT, SHIFT]);
        assert_eq!(k.len(), 1, "enter starts afresh, each key once");
    }

    #[test]
    fn close_reasons_have_texts() {
        use PopupClose::*;
        let all = [
            Escape,
            OutsideClick,
            Dismissed,
            Activated,
            OutputGone,
            Replaced,
            ItemGone,
            Timeout,
            MenuMoved,
            Launcher,
            HoverEnded,
            PlayerGone,
            NothingCaptured,
            Error,
        ];
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
                "player gone",
                "nothing captured",
                "error"
            ]
        );
    }
}
