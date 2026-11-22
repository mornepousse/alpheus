//! Wayland: one layer-shell surface (Top layer, anchored at the top,
//! exclusive zone) per screen, tracked live; a pointer for clicks and the
//! wheel. Nothing else is asked of the compositor. Redraw only when a
//! screen's grid changes. The tray's session-bus callbacks (`tray_output`) live in
//! `tray_bridge.rs`.

use crate::actions::{self, Button, Wheel};
use crate::config::ModuleId;
use crate::deadline::DeadlineTimer;
use crate::grid::{Grid, Target};
use crate::launcher::surface::{CloseReason, OpenLauncher};
use crate::painter::{self, Background, BarGeometry, Fonts, Rect};
use crate::placement::{Placement, input_rect};
use crate::popup::{Anchor as PopupAnchor, Input, ModulePopup, OpenPopup, PopupBody, PopupClose, PopupPointer};
use crate::session_bus::ItemKey;
use crate::session_bus::hover::{self, Hover, HoverChange};
use crate::session_bus::router::Tray;
use crate::startup::Mail;
use crate::state::Bar;
use alpheus_commun::exec::Executor;
use alpheus_commun::theme::Roles;
use alpheus_commun::theme::settings::ThemeSettings;
use smithay_client_toolkit::reexports::calloop::{LoopHandle, channel};
use smithay_client_toolkit::reexports::client::delegate_noop;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shm::{
    Shm, ShmHandler,
    slot::{Buffer, SlotPool},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    data_device_manager::DataDeviceManagerState,
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_popup, delegate_xdg_shell,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::log::write as log;

/// Starting pool size (`App::new`): never taken back below this, so as not
/// to keep recreating the pool around a tiny screen or when it's already at
/// its floor size.
const POOL_FLOOR: usize = 64 * 1024;
/// A pool that exceeds what the remaining surfaces need by this factor (or
/// more) is judged clearly oversized.
const POOL_OVERSIZE_FACTOR: usize = 4;

/// Bytes needed for a surface's buffer, `width` logical cells wide, at
/// `scale`, for a bar `height` logical px tall (same computation as `draw`:
/// ARGB8888, 4 bytes per physical pixel).
fn buffer_size(width: u32, height: u32, scale: i32) -> usize {
    let e = scale.max(1) as usize;
    (width as usize * e) * (height as usize * e) * 4
}

/// Should the pool (currently `current` bytes) be recreated for the
/// remaining surfaces, whose needs (in bytes) are `needs`? Returns the
/// target size if so, `None` otherwise. `SlotPool::resize` (wl_shm) can only
/// grow, never shrink: the only way to return a large screen's memory once
/// it's unplugged is to recreate the pool, smaller. This is only done when
/// clearly worthwhile (`POOL_OVERSIZE_FACTOR`), so as not to recreate it at
/// the slightest size fluctuation.
///
/// The need is the SUM of `needs`, not its maximum: the pool carries every
/// surface's buffer AT THE SAME TIME (one per screen), not a single buffer
/// reused in rotation — a pool sized for the largest surface alone would be
/// too small as soon as a second one also needs room.
fn target_pool(current: usize, needs: &[usize]) -> Option<usize> {
    if needs.is_empty() {
        // No surface left at all: at minimum, return to the floor.
        return (current > POOL_FLOOR).then_some(POOL_FLOOR);
    }
    let required = needs.iter().copied().sum::<usize>().max(POOL_FLOOR);
    if current > required.saturating_mul(POOL_OVERSIZE_FACTOR) {
        // Margin (double) so as not to recreate it again right away.
        Some(required.saturating_mul(2))
    } else {
        None
    }
}

/// Text logged for an output: its Wayland name if known, otherwise a
/// readable fallback with its protocol id — a `wl_output` older than
/// protocol version 4 does not announce a name, and "bar placed on " (empty)
/// says nothing to whoever reads the log. Pure function (the rest,
/// `log_name`, derives the id from the real `wl_output`): testable with no
/// Wayland connection.
fn log_text(name: &str, id: u32) -> String {
    if name.is_empty() { format!("an output with no name (id {id})") } else { name.to_string() }
}

fn log_name(name: &str, output: &wl_output::WlOutput) -> String {
    log_text(name, output.id().protocol_id())
}

/// Whether `seat` is the one the bar tracks (`App::seat`): the same check
/// repeated in `new_capability`, `remove_capability` and `remove_seat` — on
/// a multi-seat session, only a change on OUR seat is ours to react to, a
/// capability lost or gained on another seat is not. Generic over `PartialEq`, like `should_restore`
/// below: a `wl_seat::WlSeat` cannot be instantiated outside a real
/// connection, so this stays testable without Wayland.
pub(crate) fn tracks_seat<T: PartialEq>(tracked: &Option<T>, seat: &T) -> bool {
    tracked.as_ref() == Some(seat)
}

/// Should the bar be restored on `output` after the compositor closed its
/// surface (`LayerShellHandler::closed`)? Yes if this output is still among
/// `known`: some compositors close the SURFACE without ever destroying the
/// OUTPUT (`output_destroyed`), expecting it to be restored — unlike a real
/// unplugging, where both happen. Generic over `T: PartialEq` rather than
/// over `wl_output::WlOutput` directly: a `wl_output::WlOutput` cannot be
/// instantiated outside a real connection, so this form stays testable
/// without Wayland.
fn should_restore<T: PartialEq>(output: &T, known: impl Iterator<Item = T>) -> bool {
    known.into_iter().any(|s| s == *output)
}

/// Bounded variant of `should_restore`: refuses to restore the SAME output a
/// second time as long as the first restore hasn't received its first
/// `configure` (it is then in `pending`). Without this bound, a compositor
/// that closed the surface in a loop (a bug, a protocol violation) would
/// restore it endlessly — `place`/`remove` on every `closed`, never a
/// `configure` in between to settle things down.
fn should_restore_once<T: PartialEq>(output: &T, pending: &[T], known: impl Iterator<Item = T>) -> bool {
    !pending.iter().any(|s| s == output) && should_restore(output, known)
}

/// Which of the outputs marked for restoration (`closed` events handled
/// during the batch `loop.dispatch()` just processed) should actually be
/// restored now, once the WHOLE batch has been dispatched. Called once from
/// the after-dispatch step, never from inside `closed` itself.
///
/// This is what fixes the hot-plug race: a compositor can send a surface's
/// `closed` (the output still listed, so `closed` alone looks worth
/// restoring) and the matching `output_destroyed` (the output really gone)
/// as two events of the SAME batch, in that order. Deciding inside `closed`
/// sees the output as still known — it hasn't been removed yet — and
/// restores it, only for `output_destroyed` to tear it straight back down:
/// an extra remove+place pair that serves no purpose. Deferring the
/// decision to here, after the whole batch (hence any paired
/// `output_destroyed`) has been processed, sees the true end state: `known`
/// no longer lists it, so it is correctly left alone.
fn pending_restores<T: PartialEq + Clone>(pending: &[T], known: impl Iterator<Item = T>) -> Vec<T> {
    let known: Vec<T> = known.collect();
    pending.iter().filter(|o| known.contains(o)).cloned().collect()
}

/// What `draw` should do about a surface's persisted SHM buffers, given
/// each previous buffer's physical size and whether the compositor still
/// has it (`busy`, i.e. not yet released), and the size now `needed`.
///
/// wlroots compositors (sway, Hyprland) keep the surface's CURRENTLY
/// DISPLAYED buffer locked until a newer one is committed — only then do
/// they release the previous one. A single persisted buffer is therefore
/// busy right up to the moment a new one replaces it: checked right
/// before that replacement, it is always busy, never reusable. Keeping
/// two buffers breaks the deadlock: whichever one isn't currently on
/// screen is free, so in steady state the daemon alternates between the
/// two, painting into whichever was just released, with no further
/// allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufferPlan {
    /// A previous buffer (at this index) already matches `needed` and is
    /// free: paint into it again.
    Reuse(usize),
    /// No previous buffer matches `needed` at all (none yet, or a resize):
    /// every one of them is useless now, whether busy or not.
    Resize,
    /// Every previous buffer matches `needed`, but all are still busy:
    /// allocate one more rather than paint into a busy buffer. In steady
    /// state this only ever happens once (growing from one buffer to two);
    /// a compositor slow enough to need a third is handled (the caller
    /// prunes down to two again), just not optimized for.
    Grow,
}

/// Pure decision, testable with no Wayland connection at all: a resize
/// always discards every previous buffer, whether free or busy, since a
/// buffer of the wrong size is useless either way.
fn plan_buffer(previous: &[((u32, u32), bool)], needed: (u32, u32)) -> BufferPlan {
    if previous.is_empty() || previous.iter().any(|(size, _)| *size != needed) {
        return BufferPlan::Resize;
    }
    match previous.iter().position(|(_, busy)| !busy) {
        Some(i) => BufferPlan::Reuse(i),
        None => BufferPlan::Grow,
    }
}

/// Whether a draw opting in (`pick_buffer_or_wait`: the launcher's box) must wait for a release
/// rather than allocate: `plan_buffer` says Grow while two buffers already exist. Back-to-back
/// redraws (a key held, steps sent in a burst) otherwise allocate a third buffer each time while
/// the dropped busy ones still hold their slots, and the SlotPool doubles (it never shrinks).
fn waits_for_release(previous: &[((u32, u32), bool)], needed: (u32, u32)) -> bool {
    previous.len() >= 2 && plan_buffer(previous, needed) == BufferPlan::Grow
}

/// What `pick_buffer_or_wait` found.
pub(crate) enum Pick {
    Buffer(usize),
    /// Both buffers still held by the compositor: draw again once one is released.
    Wait,
    /// An allocation failure (logged).
    Failed,
}

/// `pick_buffer`, except that with two buffers both busy it allocates nothing and says `Wait`:
/// the caller redraws after the next release (the launcher, `launcher_after_dispatch`). The other
/// surfaces (bar, popups, bubbles, OSD) keep `pick_buffer`'s third buffer: they redraw on model
/// changes that do not come in bursts of keys.
pub(crate) fn pick_buffer_or_wait(buffers: &mut Vec<(Buffer, u32, u32)>, pool: &mut SlotPool, w: u32, h: u32) -> Pick {
    let states: Vec<((u32, u32), bool)> = buffers.iter().map(|(b, bw, bh)| ((*bw, *bh), b.slot().has_active_buffers())).collect();
    if waits_for_release(&states, (w, h)) {
        return Pick::Wait;
    }
    match pick_buffer(buffers, pool, w, h) {
        Some(i) => Pick::Buffer(i),
        None => Pick::Failed,
    }
}

/// Whether one of `buffers` is free (released by the compositor).
pub(crate) fn any_buffer_free(buffers: &[(Buffer, u32, u32)]) -> bool {
    buffers.iter().any(|(b, _, _)| !b.slot().has_active_buffers())
}

/// Whether a surface may carry the tray's frame request (`tray_sync`):
/// configured, and currently holding at least one attached buffer from the
/// live pool — never one `draw()` skipped (not yet sized, an allocation
/// failure) or whose buffers a pool shrink (`shrink_pool_if_needed`) just
/// forgot (2026-10-06: `s.last` alone can stay stale across a
/// shrink, while `s.buffers` correctly empties with it). Picking an
/// ineligible surface would mean the compositor never fires a frame
/// callback (nothing attached to repaint), blocking `tray_frame_pending`
/// forever. Nor a bar waiting for the configure of a new placement (`Surface.placing`): its
/// attached buffer has the old size, and the commit would show it under the new placement.
pub(crate) fn eligible_for_frame(configured: bool, has_buffer: bool, placing: bool) -> bool {
    configured && has_buffer && !placing
}

/// Whether bar `draw` may paint: configured, with a width, and not waiting for the configure of a
/// new placement.
fn drawable(configured: bool, width: u32, placing: bool) -> bool {
    configured && width != 0 && !placing
}

/// Whether a bar is still waiting for its placement after a configure `height` logical px tall.
/// Only a configure that clearly answers the PREVIOUS placement (`previous`, its surface height)
/// keeps the hold: two quick restyles, or a configure already in flight. The placements differ by
/// floating's shadow, so their heights tell them apart. Any other configure ends it: the latest
/// placement's height, 0 (the size left to the client), or a height the compositor clamped or
/// rounded. A bar must never freeze waiting for an exact echo: only the FIRST configure after a
/// placement (`answered`, those already received since it, is 0) can keep it; the second ends it
/// whatever its height, so a compositor that refuses the resize cannot hold the bar.
fn still_placing(placing: bool, height: u32, previous: u32, answered: u8) -> bool {
    placing && answered == 0 && height == previous
}

/// Applies `p` to `layer`, uncommitted: margin, size, exclusive zone, and the input region (the
/// bar only under floating; the default whole surface otherwise).
fn apply_placement(compositor: &CompositorState, layer: &LayerSurface, p: &Placement) {
    let (top, right, bottom, left) = p.margin;
    layer.set_margin(top, right, bottom, left);
    layer.set_size(0, p.surface_height);
    layer.set_exclusive_zone(p.exclusive_zone);
    match input_rect(p) {
        None => layer.set_input_region(None),
        Some((x, y, w, h)) => match Region::new(compositor) {
            Ok(region) => {
                region.add(x, y, w, h);
                // Copied by the request: the region may be destroyed (dropped) right after.
                layer.set_input_region(Some(region.wl_region()));
            }
            Err(e) => log(&format!("input region: {e}")),
        },
    }
}

/// Allocates a fresh ARGB8888 buffer `width` × `height` physical px in
/// `pool` (same format and stride computation everywhere a buffer is
/// created, whether on first draw, resize, or growing to a second buffer).
fn new_buffer(pool: &mut SlotPool, width: u32, height: u32) -> Result<Buffer, String> {
    pool.create_buffer(width as i32, height as i32, width as i32 * 4, wl_shm::Format::Argb8888)
        .map(|(buffer, _)| buffer)
        .map_err(|e| e.to_string())
}

/// Position of the first of `items` whose `key` projection equals `target` — generic over any
/// `PartialEq` identity so every lookup that must go by surface IDENTITY (never a bar's `name`,
/// which `update_output` can change after the fact, or which two bars could briefly share) is
/// unit-testable without a live Wayland connection: a `wl_surface::WlSurface`, like
/// `wl_output::WlOutput`, cannot be built outside one (see `should_restore`'s own comment) — tests
/// stand in with plain integers instead, same idiom. Shared by `index`, `popup_close`'s and
/// `hover_fire`'s own parent lookups (popup.rs).
pub(crate) fn position_by<T, U: PartialEq>(items: &[T], key: impl Fn(&T) -> &U, target: &U) -> Option<usize> {
    items.iter().position(|i| key(i) == target)
}

/// Which of `buffers` to paint a `w` × `h` physical frame into, allocating
/// from `pool` when `plan_buffer` says so; None on an allocation failure
/// (logged). Shared by the bar's surfaces and the launcher's box.
pub(crate) fn pick_buffer(buffers: &mut Vec<(Buffer, u32, u32)>, pool: &mut SlotPool, w: u32, h: u32) -> Option<usize> {
    let states: Vec<((u32, u32), bool)> = buffers.iter().map(|(b, bw, bh)| ((*bw, *bh), b.slot().has_active_buffers())).collect();
    match plan_buffer(&states, (w, h)) {
        BufferPlan::Resize => {
            buffers.clear();
            match new_buffer(pool, w, h) {
                Ok(buffer) => buffers.push((buffer, w, h)),
                Err(err) => {
                    log(&format!("buffer {w}×{h}: {err}"));
                    return None;
                }
            }
            Some(0)
        }
        BufferPlan::Grow => {
            match new_buffer(pool, w, h) {
                Ok(buffer) => buffers.push((buffer, w, h)),
                Err(err) => {
                    log(&format!("buffer {w}×{h}: {err}"));
                    return None;
                }
            }
            // Two buffers are enough to alternate forever in steady state
            // (see `BufferPlan`'s doc): a third was only ever allocated
            // because neither existing one was free yet. Drop the oldest
            // rather than let the list grow without bound in a pathological
            // case (a compositor slower still to release) — safe even if
            // it's still busy (`Buffer`'s own drop defers its destruction to
            // the eventual release).
            while buffers.len() > 2 {
                buffers.remove(0);
            }
            Some(buffers.len() - 1)
        }
        BufferPlan::Reuse(i) => Some(i),
    }
}

pub(crate) struct Surface {
    output: wl_output::WlOutput,
    pub(crate) name: String,
    pub(crate) layer: LayerSurface,
    /// Logical width given by the compositor (0 before the first configure).
    width: u32,
    pub(crate) scale: i32,
    pub(crate) configured: bool,
    /// Where this bar's layer surface is placed (margin, size, exclusive zone, input region): the
    /// style's `Placement` it was last given.
    pub(crate) placement: Placement,
    /// A new placement was asked and its configure has not come yet: nothing is painted meanwhile
    /// (spec § 4), since a buffer of the old size would not match.
    pub(crate) placing: bool,
    /// The surface height of the placement before the latest one: a configure of that height
    /// answers an earlier request, and keeps the hold (`still_placing`).
    placed_from: u32,
    /// Configures received since the latest placement (saturating): only the first can keep the
    /// hold (`still_placing`).
    answered: u8,
    /// Last grid drawn: no redraw if nothing changed.
    pub(crate) last: Option<Grid>,
    /// SHM buffers reused across redraws, each with the physical size it
    /// was created at — `plan_buffer` needs it to decide whether a buffer
    /// still fits. At most two in steady state: the compositor keeps the
    /// currently displayed one locked until a newer one is committed (see
    /// `BufferPlan`'s doc), so one persisted buffer is never enough to
    /// avoid reallocating on every redraw. Empty before the first draw,
    /// after a resize whose old buffers no longer match, or once
    /// invalidated by a pool recreation (`shrink_pool_if_needed`: a buffer
    /// only belongs to the pool that created it).
    pub(crate) buffers: Vec<(Buffer, u32, u32)>,
    /// The opaque region last sent (logical px, `painter::opaque_rects`): sent again only when it
    /// changes. Empty before the first draw.
    opaque: Vec<Rect>,
    /// The last attempt to send the opaque region failed: retried on every frame, logged once
    /// (`first_failure`).
    opaque_failing: bool,
}

pub struct App {
    registry: RegistryState,
    pub(crate) outputs: OutputState,
    pub(crate) seats: SeatState,
    pub(crate) shm: Shm,
    pub(crate) compositor: CompositorState,
    pub(crate) layers: LayerShell,
    pool: SlotPool,
    pub(crate) surfaces: Vec<Surface>,
    pub(crate) pointer: Option<ThemedPointer>,
    pub(crate) wheel: Wheel,
    pub bar: Bar,
    /// The 16 colour roles every surface paints with (themes spec § 1).
    pub(crate) roles: Roles,
    /// theme.toml's settings, the portal's word, the config watch and the mode file (appearance.rs).
    pub(crate) theme: crate::appearance::ThemeState,
    pub(crate) fonts: Fonts,
    pub(crate) exec: Arc<dyn Executor>,
    pub stop: bool,
    /// Raised by a message that changed the display (channel, clock):
    /// redrawn exactly once after `loop.dispatch()` returns, not on every
    /// message — see `startup::on_message`.
    pub(crate) dirty: bool,
    /// Outputs restored since their last surface was closed (`closed`),
    /// with no successful `configure` since (see `should_restore_once`): at
    /// most one restore per output as long as the previous one hasn't
    /// succeeded.
    awaiting_configure: Vec<wl_output::WlOutput>,
    /// Outputs `closed` marked for restoration, not yet placed: decided for
    /// real only after the whole dispatch batch has been processed (see
    /// `pending_restores`), so a paired `output_destroyed` of the SAME
    /// batch is taken into account instead of racing it.
    pending_restore: Vec<wl_output::WlOutput>,
    /// First seat announced: the launcher binds its keyboard on it while open.
    pub(crate) seat: Option<wl_seat::WlSeat>,
    /// The data-device manager, bound once at start: the quick notepad's paste reads the selection
    /// through it (`notes/paste.rs`). None when the compositor has none.
    pub(crate) data_devices: Option<DataDeviceManagerState>,
    /// wp_viewporter, if the compositor has it: scales the launcher's 1×1
    /// scrims to their output (no full-output buffer).
    pub(crate) viewporter: Option<WpViewporter>,
    /// The screens through the output management protocol (outputs_app.rs); None without `displays` in
    /// `modules` or without the manager.
    pub(crate) displays: Option<crate::outputs_app::OutputsApp>,
    /// The clipboard history (clipboard spec); None with `[clipboard] enabled` off (nothing bound) or
    /// without a data-control protocol.
    pub(crate) clipboard: Option<crate::clipboard::app::ClipboardApp>,
    /// The event loop's handle (startup.rs sets it): keyboard repeat, socket
    /// connections, the dmenu client watch, the tray's deadline timer.
    pub loop_handle: Option<LoopHandle<'static, App>>,
    /// For the tray's frame requests, made outside any Wayland callback, and the bubbles' surface.
    pub(crate) qh: QueueHandle<App>,
    /// The tray while its session bus lives; None without the module, without
    /// a session bus, or once the bus closed (never reconnected).
    pub(crate) tray: Option<Tray>,
    /// Armed only while tray calls are pending (2 s timeouts).
    pub(crate) tray_timer: DeadlineTimer,
    /// The bar surface whose frame callback was asked for the tray's dirty
    /// properties (None: none asked).
    pub(crate) tray_frame_pending: Option<wl_surface::WlSurface>,
    /// The bar's own session link, its timer and its bookkeeping (link.rs).
    pub(crate) link: crate::link::LinkState,
    /// Do-not-disturb: loaded at startup, changed only by `dnd_apply`.
    /// Mirrors `dnd_timed.model.on`; `dnd_apply` and `dnd_load` set both together.
    pub(crate) dnd: bool,
    /// Its timed model and the timer of its end (notify_app.rs).
    pub(crate) dnd_timed: crate::notify_app::DndTimed,
    /// An idle callback is queued to route what a void tray call's flush drained
    /// (`Tray::take_deferred`): `open_menu`, `menu_submenu`, `menu_click`, `close_menu` return no
    /// output, so it is routed right after the current dispatch, never re-entrantly.
    pub(crate) tray_deferred_queued: bool,
    /// An idle callback is queued to route what waits in a bus's `ready` with no fd event to
    /// announce it: answers a flush drained, or that the settle rounds left after a burst.
    /// One per connection (the link's is `link.ready_queued`).
    pub(crate) tray_ready_queued: bool,
    /// The horizontal wheel, for tray icons only.
    pub(crate) hwheel: Wheel,
    /// The last button press (serial, time, surface-local position): Activate's
    /// coordinates, the popup grab.
    pub(crate) last_press: Option<(u32, u32, (f64, f64))>,
    /// The last press's time ON THE OPEN POPUP's surface:
    /// never `last_press`, which is the bar's own — a mouse click inside a menu running an entry
    /// sends dbusmenu's `Event` with THIS time, not the click that asked the popup to open.
    pub(crate) popup_press_time: u32,
    /// The launcher while open; None at rest (nothing of it resident).
    pub(crate) launcher: Option<OpenLauncher>,
    /// Bumped at each open: a scan result from an older open is ignored.
    pub(crate) launcher_generation: u64,
    /// The quick notepad: its pad while open (None at rest) and its save timer.
    pub(crate) notes: crate::notes::app::NotesApp,
    /// The loop's mailbox (startup.rs sets it): a scan posts its catalog there.
    pub(crate) mail: Option<channel::Sender<Mail>>,
    /// xdg_wm_base, for the popups; None: no popups (logged once at startup).
    pub(crate) xdg: Option<XdgShell>,
    /// The open popup (menu, tooltip, calendar); None at rest.
    pub(crate) popup: Option<OpenPopup>,
    /// The bound keyboard and what follows its keys (popup.rs).
    pub(crate) keyboard: crate::popup::Keyboard,
    /// The tray icon the pointer rests on (tooltips), and its timer: armed only while it rests on one.
    pub(crate) hover: Hover,
    pub(crate) hover_timer: DeadlineTimer,
    /// The media popup's redraw tick (spec § 2): armed only while that popup is open and its player
    /// plays, up to the track's end; never at rest.
    pub(crate) media_timer: DeadlineTimer,
    /// The notification bubbles' surface, expiry timer and close memory (bubbles_surface.rs).
    pub(crate) bubbles: crate::bubbles_surface::BubbleSurface,
    /// Low-battery alerts (battery_alert.rs): only with the battery and notifications modules and
    /// a threshold in `[battery] warn`.
    pub(crate) battery_alerts: Option<crate::battery_alert::BatteryAlerts>,
    /// The event hooks (hooks_app.rs): None when `[hooks]` sets none.
    pub(crate) hooks: Option<crate::hooks_app::HooksApp>,
    /// The OSD's trigger memory, surface and hide timer (osd_surface.rs).
    pub(crate) osd: crate::osd_surface::OsdSurface,
    /// The quick panel's timer, taps, acting Bluetooth connection and followed Wi-Fi attempt
    /// (quick_app.rs).
    pub(crate) quick: crate::quick_app::QuickApp,
    /// The power menu's timer (power spec § 4): armed only while it is open and the reboot or
    /// power-off question (3 s) is pending.
    pub(crate) power_timer: DeadlineTimer,
    /// The Tools menu's question timer and reader schedule (tools_app.rs); nothing armed or
    /// running while the menu is closed.
    pub(crate) tools: crate::tools_app::ToolsApp,
    /// When `alpheus-launch --power` opened the menu (its re-fired press is ignored); None for a click.
    pub(crate) power_verb_opened: Option<Instant>,
    /// The focus timer's model, lengths and deadline (timer_app.rs); the deadline is armed only
    /// while a timer runs.
    pub(crate) timer: crate::timer_app::TimerApp,
    /// The privacy indicators' off-delay (privacy spec § 4): the bar's, armed only while an
    /// indicator is pending off, popup open or not.
    pub(crate) privacy_timer: DeadlineTimer,
    /// The camera devices' watch and scan state (privacy delivery 2, camera_app.rs).
    pub(crate) camera: crate::camera_app::Camera,
    /// The tray menus' and tooltip's state (menu_app.rs).
    pub(crate) menu: crate::menu_app::TrayMenu,
    /// The module popup last closed by `done`, and when (the calendar on Hyprland and niri; the media
    /// popup): a left press on that module within `RECENT_CLOSE_WINDOW` only closes.
    pub(crate) recently_closed_popup: Option<(ModulePopup, Instant)>,
    /// Whether the bar's layer surface goes `Exclusive` while a grabbing popup is open
    /// (`popup::bar_exclusive_during_grab`): decided once at startup from the compositor.
    pub(crate) exclusive_during_grab: bool,
    /// The bar surface `hover.over`'s icon was last seen on, set by `hover_moved` (which knows the
    /// exact surface `i`); `hover_fire` locates it by this IDENTITY, never by re-deriving the bar
    /// from `Anchor::output` — a name that `update_output` can change, or that two bars could
    /// briefly share, in the ≤ ½ s between arming and firing. Cleared whenever `Hover` itself cancels (`hover_apply`'s `Cancel` arm).
    hover_surface: Option<wl_surface::WlSurface>,
    /// Raised by a `Msg::Compositor` message, consumed once per
    /// `dispatch()` pass (`startup::WindowsFeed`): an open launcher's windows
    /// are fed at most once per pass, never once per message — a burst
    /// (e.g. niri) can carry several in the same pass, and `set_windows` is
    /// not cheap.
    windows_dirty: bool,
    /// When the scan `spawn_scan` currently allows (at most one at a time)
    /// was started; `None` once it has delivered. Lets a refusal while
    /// reopening (`host::launcher_open_apps`) tell a quick reopen (wait
    /// quietly, adopt that scan's result) from one that has run
    /// suspiciously long (show the message after all).
    pub(crate) scan_started: Option<Instant>,
    /// `draw`'s room for a bar's opaque rects, swapped with the surface's when they differ: no
    /// allocation once both have held a bar's rects.
    opaque_scratch: Vec<Rect>,
}

/// Sends `wanted` as a surface's opaque region when it differs from the one last `sent`, then
/// records it: `set` makes and sets the region. When that fails, `clear` withdraws the surface's
/// claim, so a stale region never covers what is now transparent, and nothing is recorded as sent:
/// the next frame tries again.
fn send_opaque<E>(
    sent: &mut Vec<Rect>,
    wanted: &mut Vec<Rect>,
    set: impl FnOnce(&[Rect]) -> Result<(), E>,
    clear: impl FnOnce(),
) -> Result<(), E> {
    if *wanted == *sent {
        return Ok(());
    }
    match set(wanted) {
        Ok(()) => {
            std::mem::swap(sent, wanted);
            Ok(())
        }
        Err(e) => {
            clear();
            sent.clear();
            Err(e)
        }
    }
}

/// Records whether the last attempt `failed` in `failing`; true only for the first failure after a
/// success (or ever): what is logged, so a region that keeps failing never floods the log.
fn first_failure(failing: &mut bool, failed: bool) -> bool {
    let first = failed && !*failing;
    *failing = failed;
    first
}

/// The background a bar paints with: the bar's style and module colours (set by the wiring from
/// theme.toml), the theme's opacity, the grid's blocks.
pub(crate) fn bar_background<'a>(bar: &Bar, settings: &ThemeSettings, grid: &'a Grid) -> Background<'a> {
    Background { style: bar.style, opacity: settings.opacity, module_colors: bar.module_colors, groups: grid.groups() }
}

impl App {
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        bar: Bar,
        theme_settings: ThemeSettings,
        roles: Roles,
        fonts: Fonts,
        exec: Arc<dyn Executor>,
    ) -> Result<App, String> {
        let compositor = CompositorState::bind(globals, qh).map_err(|e| format!("wl_compositor: {e}"))?;
        let layers = LayerShell::bind(globals, qh).map_err(|e| format!("layer-shell unavailable: {e}"))?;
        let shm = Shm::bind(globals, qh).map_err(|e| format!("wl_shm: {e}"))?;
        let pool = SlotPool::new(64 * 1024, &shm).map_err(|e| format!("shared buffer: {e}"))?;
        // Optional: without it, the launcher has no click-outside scrim (logged at open).
        let viewporter = globals.bind::<WpViewporter, _, _>(qh, 1..=1, ()).ok();
        // Only with the module: otherwise the global is not bound at all.
        let displays = bar
            .config
            .modules
            .contains(&ModuleId::Displays)
            .then(|| crate::outputs_app::OutputsApp::bind(globals, qh, &bar.config.displays))
            .flatten();
        // Only when switched on: otherwise no manager is bound and nothing is allocated.
        let clipboard = bar.config.clipboard.enabled.then(|| crate::clipboard::app::ClipboardApp::bind(globals, qh)).flatten();
        let timer = crate::timer_app::TimerApp::new(&bar.config);
        let battery_alerts = crate::battery_alert::alerts_wanted(&bar.config.modules, &bar.config.battery.warn)
            .then(|| crate::battery_alert::BatteryAlerts::new(&bar.config.battery.warn));
        let hooks = crate::hooks_app::HooksApp::new(&bar.config.hooks);
        // Optional: without it, no popup (menus, tooltips, calendar); the bar is unaffected.
        let xdg = XdgShell::bind(globals, qh).map_err(|e| log(&format!("popups unavailable: {e}"))).ok();
        Ok(App {
            registry: RegistryState::new(globals),
            outputs: OutputState::new(globals, qh),
            seats: SeatState::new(globals, qh),
            shm,
            compositor,
            layers,
            pool,
            surfaces: Vec::new(),
            pointer: None,
            wheel: Wheel::default(),
            bar,
            theme: crate::appearance::ThemeState::new(theme_settings),
            roles,
            fonts,
            exec,
            stop: false,
            dirty: false,
            awaiting_configure: Vec::new(),
            pending_restore: Vec::new(),
            seat: None,
            data_devices: DataDeviceManagerState::bind(globals, qh).ok(),
            viewporter,
            displays,
            clipboard,
            loop_handle: None,
            qh: qh.clone(),
            tray: None,
            tray_timer: DeadlineTimer::new(),
            tray_frame_pending: None,
            link: crate::link::LinkState::new(),
            dnd: false,
            dnd_timed: crate::notify_app::DndTimed::new(),
            tray_deferred_queued: false,
            tray_ready_queued: false,
            hwheel: Wheel::default(),
            last_press: None,
            popup_press_time: 0,
            launcher: None,
            launcher_generation: 0,
            notes: crate::notes::app::NotesApp::new(),
            mail: None,
            xdg,
            popup: None,
            keyboard: Default::default(),
            hover: Hover::new(),
            hover_timer: DeadlineTimer::new(),
            media_timer: DeadlineTimer::new(),
            bubbles: crate::bubbles_surface::BubbleSurface::new(),
            battery_alerts,
            hooks,
            osd: crate::osd_surface::OsdSurface::new(),
            quick: crate::quick_app::QuickApp::new(),
            power_timer: DeadlineTimer::new(),
            tools: crate::tools_app::ToolsApp::new(),
            power_verb_opened: None,
            timer,
            privacy_timer: DeadlineTimer::new(),
            camera: crate::camera_app::Camera::new(),
            menu: Default::default(),
            recently_closed_popup: None,
            // Asked of the compositor actually on the other end of the connection (its own
            // `hyprland_*` globals), never of the environment: an inherited
            // HYPRLAND_INSTANCE_SIGNATURE would mistake sway for Hyprland.
            exclusive_during_grab: crate::popup::bar_exclusive_during_grab(
                globals.contents().with_list(|l| crate::popup::is_hyprland(l.iter().map(|g| g.interface.as_str()))),
            ),
            hover_surface: None,
            windows_dirty: false,
            scan_started: None,
            opaque_scratch: Vec::new(),
        })
    }

    fn place(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let info = self.outputs.info(&output);
        let name = info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default();
        let scale = info.map(|i| i.scale_factor).unwrap_or(1);
        // The bar's style: set from theme.toml at startup and on reload.
        let placement = Placement::for_style(self.bar.style, self.bar.config.height);
        let surface = self.compositor.create_surface(qh);
        let layer = self.layers.create_layer_surface(qh, surface, Layer::Top, Some("alpheus-bar"), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        apply_placement(&self.compositor, &layer, &placement);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        log(&format!("bar placed on {}", log_name(&name, &output)));
        self.surfaces.push(Surface {
            output,
            name,
            layer,
            width: 0,
            scale,
            configured: false,
            placement,
            placing: false,
            placed_from: placement.surface_height,
            answered: 0,
            last: None,
            buffers: Vec::new(),
            opaque: Vec::new(),
            opaque_failing: false,
        });
    }

    /// Returns the removed surface's output, so the caller can decide to
    /// restore the bar onto it (see `closed`) if it still exists.
    fn remove(&mut self, i: usize) -> wl_output::WlOutput {
        // A popup hanging from this bar goes first (its parent must outlive it): by this surface's
        // IDENTITY, never its `name` — `update_output` can rename it,
        // or another bar could share the name for a moment, and the wrong popup (or none) would
        // react, leaving a bar stuck `Exclusive`.
        let surface = self.surfaces[i].layer.wl_surface().clone();
        self.popup_output_gone(&surface);
        let s = self.surfaces.remove(i); // the surface is destroyed along with `s`
        log(&format!("bar removed from {}", log_name(&s.name, &s.output)));
        self.shrink_pool_if_needed();
        // A frame asked on this surface never comes now: ask it on another bar.
        if self.tray_frame_pending.as_ref() == Some(s.layer.wl_surface()) {
            self.tray_frame_pending = None;
            self.tray_sync();
        }
        // The focus going with this bar (a popup closed by Escape, the key still down, a release
        // deferred on it): the key-up and leave that would end the wait may never come. Not by
        // `is_alive`: `closed` holds the layer surface (and so its wl_surface) until it returns.
        if self.keyboard.focus.as_ref() == Some(&surface) {
            self.keyboard.focus = None;
            self.keyboard_release_deferred();
        }
        s.output
    }

    /// Recreates the pool at a size fitted to the remaining surfaces, if
    /// clearly oversized (see `target_pool`): without this, the peak
    /// footprint of a large unplugged screen stays in memory forever
    /// ("running on a potato").
    fn shrink_pool_if_needed(&mut self) {
        let needs: Vec<usize> = self.surfaces.iter().map(|s| buffer_size(s.width, s.placement.surface_height, s.scale)).collect();
        if let Some(size) = target_pool(self.pool.len(), &needs) {
            match SlotPool::new(size, &self.shm) {
                Ok(p) => {
                    self.pool = p;
                    // Every surviving surface's persisted buffers belong to
                    // the pool just replaced (`Buffer::canvas` matches it by
                    // identity): drop them so the next redraw allocates
                    // fresh ones from the new pool, rather than an orphaned
                    // buffer silently failing to be reused forever.
                    for s in &mut self.surfaces {
                        s.buffers.clear();
                    }
                }
                Err(e) => log(&format!("pool {size}: {e}")),
            }
        }
    }

    fn index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        position_by(&self.surfaces, |s| s.layer.wl_surface(), surface)
    }

    pub fn redraw_all(&mut self) {
        for i in 0..self.surfaces.len() {
            self.draw(i);
        }
    }

    /// Every bar redrawn even if its grid did not change (a new theme): `draw` skips an unchanged grid.
    pub(crate) fn bars_repaint(&mut self) {
        for s in &mut self.surfaces {
            s.last = None;
        }
        self.redraw_all();
    }

    /// Re-places every bar for the bar's current `style` (themes delivery 2, spec § 4). Called after
    /// a theme reload. A bar whose `Placement` changes (a switch to or from floating) gets
    /// its new margin, size, exclusive zone and input region, and paints again only on the
    /// compositor's configure (its size always changes, so that configure always comes). A no-op for
    /// a bar already placed right, so it can be called on every reload.
    ///
    /// A popup (menu, tooltip, calendar) is anchored to its bar's surface at the old
    /// size; it is closed before that bar moves, never left hanging from a stale geometry. The
    /// launcher is its own layer surface, not anchored to a bar: it stays open.
    pub(crate) fn apply_bar_placement(&mut self) {
        let wanted = Placement::for_style(self.bar.style, self.bar.config.height);
        let moving: Vec<wl_surface::WlSurface> =
            self.surfaces.iter().filter(|s| s.placement != wanted).map(|s| s.layer.wl_surface().clone()).collect();
        for parent in &moving {
            self.popup_close_if_parent(parent, PopupClose::Restyled);
        }
        let App { surfaces, compositor, .. } = self;
        for s in surfaces.iter_mut().filter(|s| s.placement != wanted) {
            apply_placement(compositor, &s.layer, &wanted);
            s.placed_from = s.placement.surface_height;
            s.placement = wanted;
            // Assumption: a configure always follows this change. The surface's size changes with
            // the placement (floating's margin and shadow), and a compositor answers a size change
            // with a configure; until then `placing` holds the bar's paint.
            s.placing = true;
            s.answered = 0;
            s.last = None;
            s.layer.commit();
        }
    }

    /// A reload changed the bar's shape (shapes spec § 4).
    /// - To or from `floating`, every bar is re-placed first (`apply_bar_placement`): margin,
    ///   size, exclusive zone, input region, an open popup on a moved bar closed; each moved bar
    ///   holds its paint until its configure (`Surface.placing`).
    /// - Then every surface is repainted once: the bars that may paint, an open popup, the
    ///   launcher box.
    pub(crate) fn restyle(&mut self, placement: bool) {
        if placement {
            self.apply_bar_placement();
        }
        self.repaint_all();
    }

    /// Called once per `loop.dispatch()` pass, right after `after_dispatch`
    /// (`startup::run`): places the bar back on every output `closed` marked
    /// for restoration this batch and that is still known now — see
    /// `pending_restores`'s doc for why this can't be decided synchronously
    /// inside `closed` itself.
    pub fn restore_pending(&mut self, qh: &QueueHandle<Self>) {
        let marked = std::mem::take(&mut self.pending_restore);
        for output in pending_restores(&marked, self.outputs.outputs()) {
            self.awaiting_configure.push(output.clone());
            self.place(qh, output);
        }
    }

    fn draw(&mut self, i: usize) {
        let App { surfaces, fonts, bar, roles, pool, theme, compositor, opaque_scratch, .. } = self;
        let s = &mut surfaces[i];
        if !drawable(s.configured, s.width, s.placing) {
            return;
        }
        let e = s.scale.max(1);
        // The surface: the bar, plus floating's shadow below it (`Placement::surface_height`).
        let (w, h) = (s.width * e as u32, s.placement.surface_height * e as u32);
        let grid = bar.grid(&s.name, painter::columns(w, fonts.advance(e)));
        if s.last.as_ref() == Some(&grid) {
            return; // nothing changed on this screen
        }
        let Some(used) = pick_buffer(&mut s.buffers, pool, w, h) else { return };
        // Expected to succeed (`plan_buffer` already checked free-ness for
        // `Reuse`, and a buffer fresh out of `new_buffer` is always
        // inactive): `None` here is defensive, never seen in practice.
        let Some(pixels) = s.buffers[used].0.canvas(pool) else {
            log(&format!("buffer {w}×{h}: unexpectedly busy right after allocation or reuse"));
            return;
        };
        fonts.paint_bar(&grid, roles, &bar_background(bar, &theme.settings, &grid), pixels, w, h, e);
        let surface = s.layer.wl_surface();
        // The compositor skips blending what the style paints opaque (shapes spec § 3): told again only
        // when it changes (a new width, style or block layout).
        let geometry = BarGeometry { width: s.width, height: bar.config.height, advance: fonts.advance(e), scale: e };
        painter::opaque_rects(bar.style, grid.groups(), &geometry, opaque_scratch);
        let sent = send_opaque(
            &mut s.opaque,
            opaque_scratch,
            |rects| {
                let region = Region::new(&*compositor)?;
                for r in rects {
                    region.add(r.x, r.y, r.width, r.height);
                }
                surface.set_opaque_region(Some(region.wl_region()));
                Ok::<(), smithay_client_toolkit::error::GlobalError>(())
            },
            || surface.set_opaque_region(None),
        );
        if first_failure(&mut s.opaque_failing, sent.is_err())
            && let Err(e) = sent
        {
            log(&format!("opaque region: {e}"));
        }
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if s.buffers[used].0.attach_to(surface).is_err() {
            return;
        }
        s.layer.commit();
        s.last = Some(grid);
    }

    /// The module popup a press of `b` at `x` on bar `i` toggles, its module's cells (its anchor),
    /// and the zone's target (the quick panel's section).
    fn module_popup_at(&mut self, i: usize, x: f64, b: Button) -> Option<(ModulePopup, PopupAnchor, Target)> {
        let e = self.surfaces[i].scale;
        let column = painter::column_under(x, e, self.fonts.advance(e));
        let quick = self.bar.config.modules.contains(&ModuleId::Quick);
        let s = &self.surfaces[i];
        let zone = s.last.as_ref()?.zone_at(column)?;
        let popup = ModulePopup::of(&zone.target, b, quick)?;
        Some((popup, PopupAnchor { output: s.name.clone(), start: zone.start, end: zone.end }, zone.target.clone()))
    }

    /// `launcher_was_open`: whether the launcher was open right before this
    /// press (`pointer_frame` already closed it, as a click outside, before
    /// calling here): the same press must not reopen it (`toggles_launcher`).
    fn click(&mut self, qh: &QueueHandle<Self>, i: usize, x: f64, b: Button, launcher_was_open: bool) {
        if let Some((popup, anchor, target)) = self.module_popup_at(i, x, b) {
            (popup.ops().toggle)(self, qh, anchor, &target);
            return;
        }
        let e = self.surfaces[i].scale;
        let advance = self.fonts.advance(e);
        let (toggles, media, dnd, timer) = {
            let Some(grid) = &self.surfaces[i].last else { return };
            let target = grid.target_at(painter::column_under(x, e, advance));
            (
                target.is_some_and(|t| actions::toggles_launcher(t, b)),
                target.and_then(|t| actions::media_control(t, b)),
                target.is_some_and(|t| actions::toggles_dnd(t, b)),
                target.is_some_and(|t| actions::timer_middle(t, b)),
            )
        };
        // The bell's middle button: do-not-disturb flips inside the daemon (notifications spec § 4),
        // through the one path that persists it.
        if dnd {
            self.set_dnd(!self.dnd);
            return;
        }
        // The timer chip's middle button: pause or resume, or dismiss `done` (timer spec decision 16).
        if timer {
            self.timer_middle_click();
            return;
        }
        // The media module's middle button and wheel: D-Bus, never a command (spec § 1).
        if let Some(c) = media {
            self.mpris_do(move |m, out| m.control(out, c));
            return;
        }
        match decide_click(toggles, launcher_was_open) {
            ClickDecision::ToggleLauncher => self.launcher_toggle(qh),
            ClickDecision::Nothing => {}
            ClickDecision::RunBarAction => {
                let s = &self.surfaces[i];
                let Some(grid) = &s.last else { return };
                if let Some(argv) = actions::click(&self.bar, &s.name, grid, x, e, advance, b) {
                    actions::run(self.exec.as_ref(), &argv);
                }
            }
        }
    }
}

/// What a press on the bar does, decided purely (tested without Wayland):
/// the launcher button toggles the launcher, UNLESS this very press just
/// closed it as a click outside (`pointer_frame` closes before calling
/// `click`) — then it is swallowed instead of reopening it. Anywhere else,
/// the bar's own action (`actions::click`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickDecision {
    ToggleLauncher,
    Nothing,
    RunBarAction,
}

fn decide_click(toggles_launcher: bool, launcher_was_open: bool) -> ClickDecision {
    match (toggles_launcher, launcher_was_open) {
        (true, false) => ClickDecision::ToggleLauncher,
        (true, true) => ClickDecision::Nothing,
        (false, _) => ClickDecision::RunBarAction,
    }
}

/// Mae (2026-10-05), generalised 2026-10-06: a click on the icon whose menu is shown, with the
/// button that opened it, only closes that menu and does nothing else; a right click always
/// closes it, whatever opened it — the press that closed it never reopens it (like `decide_click`
/// for the launcher). Steam's menu, opened by a left click (a failed Activate, no `Menu` of its
/// own on the press), is closed the same way: the same left click again, no new Activate, no
/// flicker.
fn closes_own_menu(menu_shown_for: Option<&ItemKey>, pressed_icon: Option<&ItemKey>, b: Option<Button>, opened_by: Option<Button>) -> bool {
    menu_shown_for.is_some() && menu_shown_for == pressed_icon && b.is_some() && (b == Some(Button::Right) || b == opened_by)
}

/// A compositor may deliver `xdg_
/// popup.done` (closing the popup: `PopupClose::Dismissed`) before the press that follows it reaches
/// the bar — by then `closes_own_menu` sees no menu open at all and would let this very press
/// reopen what it just closed. `record` (the icon whose menu this was, the button that opened it,
/// and when it closed) makes that press still only-closing: the same icon, a button `closes_own_
/// menu` would also have accepted (its own opening button, or a right click), within `
/// RECENT_CLOSE_WINDOW` of the close.
const RECENT_CLOSE_WINDOW: Duration = Duration::from_millis(300);

fn closed_recently(record: Option<&(ItemKey, Button, Instant)>, pressed_icon: Option<&ItemKey>, b: Option<Button>, now: Instant) -> bool {
    let Some((key, opened_by, at)) = record else { return false };
    b.is_some()
        && pressed_icon == Some(key)
        && (b == Some(Button::Right) || b == Some(*opened_by))
        && now.duration_since(*at) < RECENT_CLOSE_WINDOW
}

/// The module popups' version of the race above. `record` holds the popup a compositor `done` closed,
/// and when. A press that would toggle that same popup within `RECENT_CLOSE_WINDOW` only closes
/// (returns true: do not reopen). Any press, this one included, consumes the record.
fn closed_by_done(record: &mut Option<(ModulePopup, Instant)>, pressed: Option<ModulePopup>, now: Instant) -> bool {
    match (record.take(), pressed) {
        (Some((closed, at)), Some(p)) => closed == p && now.duration_since(at) < RECENT_CLOSE_WINDOW,
        _ => false,
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, f: i32) {
        // Ownership decided first, outside any guard: a panic inside `launcher_scale` must never make a
        // BAR surface's own scale change look handled.
        if self.launcher_owns_box(surface) {
            self.launcher_guarded("scale", |app| app.launcher_scale(surface, f));
            return;
        }
        if self.notes_owns_box(surface) {
            self.notes_guarded("scale", |app| app.notes_scale(surface, f));
            return;
        }
        if self.bubbles_scale(surface, f) {
            return;
        }
        if self.osd_scale(surface, f) {
            return;
        }
        if self.popup_scale(surface, f) {
            return;
        }
        if let Some(i) = self.index(surface) {
            if self.surfaces[i].scale != f {
                self.surfaces[i].scale = f;
                self.surfaces[i].last = None;
                self.draw(i);
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        // Only the tray asks for frames: its dirty properties, re-read once.
        if self.tray_frame_pending.as_ref() != Some(surface) {
            return;
        }
        self.tray_frame_pending = None;
        let Some(tray) = self.tray.as_mut() else { return };
        let out = tray.on_frame();
        self.tray_output(out);
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // Hot-plugged while the launcher is open: it gets a scrim too, so a
        // click on it closes the launcher like any output already shown had.
        self.launcher_guarded("output added", |app| app.launcher_output_added(qh, output.clone()));
        self.notes_guarded("output added", |app| app.notes_output_added(qh, output.clone()));
        self.place(qh, output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        // Name announced or changed afterwards: the shown workspaces depend on it.
        let name = self.outputs.info(&output).and_then(|i| i.name);
        if let Some(i) = self.surfaces.iter().position(|s| s.output == output) {
            if let Some(n) = name.filter(|n| *n != self.surfaces[i].name) {
                self.surfaces[i].name = n;
                self.surfaces[i].last = None;
                self.draw(i);
            }
        }
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.launcher_guarded("output", |app| app.launcher_output_gone(&output));
        self.notes_guarded("output", |app| app.notes_output_gone(&output));
        self.bubbles_output_gone(&output);
        self.osd_output_gone(&output);
        if let Some(i) = self.surfaces.iter().position(|s| s.output == output) {
            self.remove(i);
        }
        // A truly destroyed output will never be restored again: any
        // pending restore for it (if `closed` had just restored it with no
        // configure yet succeeding, or marked it for restoration this same
        // batch) would serve no more purpose — bounded memory, not an
        // output forgotten per session that piles up. `restore_pending`
        // would filter it out anyway (`pending_restores`), but dropping it
        // here too keeps both lists free of outputs already known gone,
        // rather than relying on that one later check alone.
        self.awaiting_configure.retain(|s| *s != output);
        self.pending_restore.retain(|s| *s != output);
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        // Ownership decided first, outside any guard: same reasoning as `scale_factor_changed`. Checked
        // before the bar's own restore logic below even gets a chance to
        // read: a launcher surface never goes through it.
        if self.launcher_owns_surface(layer.wl_surface()) {
            self.launcher_guarded("closed", |app| app.launcher_layer_closed(layer.wl_surface()));
            return; // a launcher surface: the bar's restore logic is not involved
        }
        if self.notes_owns_surface(layer.wl_surface()) {
            self.notes_guarded("closed", |app| app.notes_layer_closed(layer.wl_surface()));
            return; // the pad's surface: the bar's restore logic is not involved
        }
        if self.bubbles_owns(layer.wl_surface()) {
            self.bubbles_layer_closed();
            return; // the bubbles' surface: the bar's restore logic is not involved
        }
        if self.osd_owns(layer.wl_surface()) {
            self.osd_layer_closed();
            return; // the pill's surface: the bar's restore logic is not involved
        }
        // The compositor closed the surface. Usually (screen unplugged),
        // `output_destroyed` follows: we forget this bar, the others carry
        // on, never a stop here. But if the output still exists (see
        // `should_restore`), the compositor expects it to be restored —
        // otherwise this screen would stay without a bar until the next
        // output change.
        //
        // This only MARKS the output (`pending_restore`); it does not place
        // it back right away. Some compositors send `closed` and the
        // matching `output_destroyed` as two events of the very same
        // dispatch batch, `closed` first: deciding here, synchronously,
        // would see the output as still known (its `output_destroyed`
        // hasn't run yet) and restore it, only for `output_destroyed` a
        // moment later to tear it right back down — an extra remove+place
        // pair with no purpose. `restore_pending`, called once the whole
        // batch has been dispatched (`startup::run`), sees the true end
        // state instead.
        //
        // `should_restore_once` (bounding restores to one per output as
        // long as a `configure` hasn't succeeded — see `configure`, further
        // below) is checked against BOTH `awaiting_configure` (already
        // placed, awaiting its first `configure`) and `pending_restore`
        // (already marked this batch, not yet placed): without the second
        // check, a compositor that closed the surface in a loop within one
        // batch (a bug, a protocol violation) could mark the same output
        // more than once before `restore_pending` ever runs.
        if let Some(i) = self.index(layer.wl_surface()) {
            let output = self.remove(i);
            let already_pending = self.pending_restore.contains(&output);
            if !already_pending && should_restore_once(&output, &self.awaiting_configure, self.outputs.outputs()) {
                self.pending_restore.push(output);
            }
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        // Ownership decided first, outside any guard: same reasoning as `scale_factor_changed`.
        if self.launcher_owns_surface(layer.wl_surface()) {
            self.launcher_guarded("configure", |app| app.launcher_configured(layer.wl_surface(), c.new_size));
            return;
        }
        if self.notes_owns_surface(layer.wl_surface()) {
            self.notes_guarded("configure", |app| app.notes_configured(layer.wl_surface(), c.new_size));
            return;
        }
        if self.bubbles_owns(layer.wl_surface()) {
            self.bubbles_configured(c.new_size);
            return;
        }
        if self.osd_owns(layer.wl_surface()) {
            self.osd_configured(c.new_size);
            return;
        }
        let Some(i) = self.index(layer.wl_surface()) else { return };
        // First configure of THIS surface: the restore (if there was one
        // pending) succeeded, the output can again be restored in the
        // future if the compositor closes its surface again.
        let first_configure = !self.surfaces[i].configured;
        // Any configure but the answer to an earlier placement ends a style switch's hold: paint
        // again, at the new size.
        let s = &mut self.surfaces[i];
        let held = s.placing;
        s.placing = still_placing(s.placing, c.new_size.1, s.placed_from, s.answered);
        s.answered = s.answered.saturating_add(1);
        if held && !s.placing {
            s.last = None;
        }
        if self.surfaces[i].width != c.new_size.0 {
            self.surfaces[i].last = None;
        }
        self.surfaces[i].width = c.new_size.0;
        self.surfaces[i].configured = true;
        if first_configure {
            let output = self.surfaces[i].output.clone();
            self.awaiting_configure.retain(|s| *s != output);
        }
        self.draw(i);
        // Signals that came before any bar was configured asked no frame yet:
        // ask it now, on a surface that has a buffer (drawn just above).
        self.tray_sync();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
        self.clipboard_seat(qh);
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, c: Capability) {
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
        }
        self.clipboard_seat(qh);
        if c == Capability::Pointer && self.pointer.is_none() {
            let surface = self.compositor.create_surface(qh);
            match self.seats.get_pointer_with_theme(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::default()) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => log(&format!("pointer unavailable: {e}")),
            }
        }
        // A keyboard appearing while the launcher or a grabbing popup is open
        // (virtual keyboard, a keyboard plugged in): bind it now. At rest:
        // nothing is bound (keyboard_acquire checks). On a multi-seat
        // session, only the seat the bar already tracks (`self.seat`) is
        // ours — a keyboard on another seat is not.
        if c == Capability::Keyboard && tracks_seat(&self.seat, &seat) {
            self.notes_attach_data_device(qh);
            self.keyboard_acquire(qh);
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat, c: Capability) {
        if c == Capability::Pointer {
            self.pointer = None;
        }
        // Same multi-seat check as `new_capability`: another seat losing its
        // keyboard must not forget about ours.
        if c == Capability::Keyboard && tracks_seat(&self.seat, &seat) {
            self.keyboard_lost();
        }
    }

    fn remove_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if tracks_seat(&self.seat, &seat) {
            self.seat = None;
            self.clipboard_seat(qh);
            self.clipboard_seat_removed(&seat);
            // The seat itself is gone, not just one of its capabilities: any
            // keyboard bound through it (the launcher's, a grabbing popup's,
            // while open) goes with it — `remove_capability` alone never
            // fires for this case.
            self.keyboard_lost();
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            if self.launcher_guarded("pointer", |app| app.launcher_pointer(conn, ev)).unwrap_or(true) {
                continue;
            }
            if self.notes_guarded("pointer", |app| app.notes_pointer(conn, ev)).unwrap_or(true) {
                continue;
            }
            if self.bubbles_pointer(conn, ev) {
                continue;
            }
            if self.osd_owns(&ev.surface) {
                // Its input region is empty (OSD spec § 3): an event here is a bug, never the bar's.
                log("osd: a pointer event reached the pill");
                continue;
            }
            if self.popup.as_ref().is_some_and(|p| p.wl_surface() == &ev.surface) {
                if let (PointerEventKind::Enter { .. }, Some(p)) = (&ev.kind, &self.pointer) {
                    let _ = p.set_cursor(conn, CursorIcon::Default);
                }
                // THIS press's own time — never the bar's
                // `last_press`, the click that asked the popup to open — so a mouse click running
                // a menu entry sends dbusmenu's `Event` with the click's own timestamp.
                if let PointerEventKind::Press { time, .. } = &ev.kind {
                    self.popup_press_time = *time;
                }
                // The popup's OWN wheel accumulator, never the bar's
                // `self.wheel`: a notch half-scrolled on the bar must not carry into a menu, or
                // the other way around.
                let kind = self.popup.as_mut().and_then(|open| popup_pointer_kind(&mut open.wheel, &ev.kind));
                if let Some(kind) = kind {
                    self.popup_pointer_event(&ev.surface, ev.position, kind);
                }
                continue;
            }
            let Some(i) = self.index(&ev.surface) else { continue };
            match &ev.kind {
                PointerEventKind::Enter { .. } => {
                    if let Some(p) = &self.pointer {
                        let _ = p.set_cursor(conn, CursorIcon::Default);
                    }
                    self.hover_moved(i, ev.position.0);
                }
                PointerEventKind::Motion { .. } => self.hover_moved(i, ev.position.0),
                PointerEventKind::Press { button, serial, time } => {
                    self.last_press = Some((*serial, *time, ev.position));
                    let c = self.hover.cancel();
                    self.hover_apply(c);
                    // A click on the bar while the launcher is open is a click
                    // outside it: close first, then the bar's own action — but
                    // the same press must not then reopen it through the
                    // launcher button (`click`'s `launcher_was_open`).
                    let launcher_was_open = self.launcher_is_open();
                    if launcher_was_open {
                        self.launcher_guarded("click outside", |app| app.launcher_close(CloseReason::ClickOutside));
                    }
                    // The quick notepad too: a click on the bar is a click outside it. The launcher
                    // button then opens the launcher (opening one closes the other): only the
                    // launcher's own close swallows its button.
                    if self.notes_is_open() {
                        self.notes_guarded("click outside", |app| app.notes_close(CloseReason::ClickOutside));
                    }
                    let pressed_icon = self.tray_key_at(i, ev.position.0);
                    // A press that hits no icon drops
                    // `menu.last_icon` — a failed Activate answered late, for whatever icon was
                    // pressed before this one, must not open a menu once the user has pressed
                    // elsewhere on the bar meanwhile.
                    if pressed_icon.is_none() {
                        self.menu.last_icon = None;
                    }
                    let b = actions::button(*button);
                    let menu_shown_for = match self.popup_body() {
                        Some(PopupBody::Menu(_)) => self.menu.target.as_ref().map(|(k, _)| k),
                        _ => None,
                    };
                    let only_closes = closes_own_menu(menu_shown_for, pressed_icon.as_ref(), b, self.menu.opened_by)
                        || closed_recently(self.menu.recently_closed.as_ref(), pressed_icon.as_ref(), b, Instant::now());
                    // A press on the bar is delivered to it even while one of its popups grabs
                    // the pointer (same client): it is a click outside that popup —
                    // except a module popup's own second click: its toggle closes it.
                    let pressed_popup = b.and_then(|b| self.module_popup_at(i, ev.position.0, b)).map(|(p, _, _)| p);
                    let toggles_own_popup = pressed_popup.is_some() && pressed_popup == self.popup_body().and_then(PopupBody::module_popup);
                    if self.popup.is_some() {
                        // Where the compositor delivered it: a press meant for the popup but handed
                        // to the bar (seen on Hyprland) shows here, at a row below the bar's own.
                        log(&format!("popup: press on the bar at ({:.0}, {:.0})", ev.position.0, ev.position.1));
                        if !toggles_own_popup {
                            self.popup_close(PopupClose::OutsideClick);
                        }
                    }
                    // A menu asked for but not shown yet dies with any press on the bar (a menu
                    // shown has just ended with its popup).
                    if self.menu.target.is_some() {
                        self.menu_end();
                    }
                    // Consumed by a matching press, and just as irrelevant to any
                    // other one — never reused by a later, unrelated press. Cleared AFTER the
                    // close above: a close this press performs itself needs no record (only a
                    // compositor `done` arriving ahead of the press does), else a quick reopen
                    // on the same icon would be swallowed.
                    self.menu.recently_closed = None;
                    let popup_only_closes = closed_by_done(&mut self.recently_closed_popup, pressed_popup, Instant::now());
                    if only_closes || popup_only_closes {
                        continue; // the menu's own icon: closed, never reopened (Mae)
                    }
                    if let Some(b) = b {
                        if !self.tray_gesture_at(i, ev.position, actions::tray_gesture(b), b) {
                            self.click(qh, i, ev.position.0, b, launcher_was_open);
                        }
                    }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    if let Some(b) = self.wheel.add(vertical.absolute, vertical.discrete, vertical.value120) {
                        if !self.tray_gesture_at(i, ev.position, actions::tray_gesture(b), b) {
                            self.click(qh, i, ev.position.0, b, false);
                        }
                    }
                    // The horizontal axis only scrolls tray icons.
                    let h = self.hwheel.add(horizontal.absolute, horizontal.discrete, horizontal.value120);
                    if let (Some(b), Some(g)) = (h, h.and_then(actions::tray_horizontal)) {
                        self.tray_gesture_at(i, ev.position, g, b);
                    }
                    // End of gesture (notched wheel released, touchpad
                    // lifted): nothing to carry over to the next gesture.
                    if vertical.stop {
                        self.wheel.stop();
                    }
                    if horizontal.stop {
                        self.hwheel.stop();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.wheel.stop();
                    self.hwheel.stop();
                    let c = self.hover.cancel();
                    self.hover_apply(c);
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_keyboard!(App);
delegate_xdg_shell!(App);
delegate_xdg_popup!(App);
// wp_viewporter and wp_viewport have no events.
delegate_data_device!(App);
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);
delegate_registry!(App);

impl App {
    /// The pointer moved on bar `i` at `x`: over a tray icon, the tooltip's timer (only with tooltips on,
    /// and never while a grabbing popup or the launcher is open: no tooltip could show then).
    fn hover_moved(&mut self, i: usize, x: f64) {
        let busy = self.popup.as_ref().is_some_and(|p| p.grab) || self.launcher_is_open() || self.notes_is_open();
        let tooltips = self.bar.config.tray.tooltips && !busy;
        let e = self.surfaces[i].scale;
        let column = painter::column_under(x, e, self.fonts.advance(e));
        // Borrowed from `self.surfaces[i]`: resting on the same
        // icon allocates nothing, `Hover::moved` only clones the key and the output name on Arm.
        let found = self.surfaces[i].last.as_ref().and_then(|g| hover::over(tooltips, g, column));
        let c = self.hover.moved(found, &self.surfaces[i].name, Instant::now());
        if matches!(c, HoverChange::Arm(_)) {
            // This surface's own identity, known here precisely (from the task
            // 16 review): `hover_fire`, run later by its timer, must find the SAME bar back by
            // this, never by re-deriving it from a name that could have changed meanwhile.
            self.hover_surface = Some(self.surfaces[i].layer.wl_surface().clone());
        }
        self.hover_apply(c);
    }

    /// Arm: the timer at that instant (a tooltip shown for another icon goes); Cancel: no timer, no tooltip.
    pub(crate) fn hover_apply(&mut self, c: HoverChange) {
        let tooltip_shown = matches!(self.popup_body(), Some(PopupBody::Tooltip(_)));
        let deadline = match c {
            HoverChange::Nothing => return,
            HoverChange::Arm(at) => Some(at),
            HoverChange::Cancel => {
                self.hover_surface = None;
                None
            }
        };
        if tooltip_shown {
            self.popup_close(PopupClose::HoverEnded);
        }
        self.hover_timer.sync_opt(self.loop_handle.as_ref(), deadline, App::hover_fire);
    }

    /// The tooltip's item left the tray: its tooltip goes.
    pub(crate) fn tooltip_item_gone(&mut self) {
        let Some(open) = self.popup.as_ref() else { return };
        if !matches!(open.body, PopupBody::Tooltip(_)) {
            return;
        }
        // A Tooltip popup is always paired with `menu.tooltip_item` (set together in `hover_fire`,
        // cleared together in `popup_close`). Should that ever break, the tooltip closes rather
        // than the daemon panicking. `self.tray` can be None: the session bus just closed,
        // taking every item with it.
        let gone = match (&self.tray, self.menu.tooltip_item.as_ref()) {
            (Some(tray), Some(key)) => tray.item(key).is_none(),
            _ => true,
        };
        if gone {
            self.popup_close(PopupClose::ItemGone);
        }
    }

    /// True when the item `Hover` is tracking (armed, or already shown as a tooltip) lost its
    /// zone, or moved to different columns, in the grid `self.bar` would now paint for the
    /// surface it was armed on — found by `hover_surface`'s
    /// IDENTITY, never `anchor.output`'s name (it can change meanwhile). An unrelated icon changing elsewhere never moves THIS one's
    /// zone, so this stays
    /// false then — the whole point of only cancelling here, not on every redraw. Nothing
    /// tracked: never "moved".
    pub(crate) fn hover_target_moved(&self) -> bool {
        let Some((key, anchor)) = self.hover.target() else { return false };
        let Some(surface) = self.hover_surface.as_ref() else { return false };
        let Some(i) = self.index(surface) else { return true }; // the bar surface itself is gone
        let Some(columns) = self.surfaces[i].last.as_ref().map(Grid::width) else { return true };
        let grid = self.bar.grid(&self.surfaces[i].name, columns);
        match grid.zones().iter().find(|z| matches!(&z.target, Target::TrayItem(k) if k == key)) {
            Some(z) => (z.start, z.end) != (anchor.start, anchor.end),
            None => true, // dropped from the grid: gone
        }
    }

    /// The hover timer fired: the tooltip, unless another popup or the launcher is open.
    pub(crate) fn hover_fire(app: &mut App, now: Instant) {
        app.hover_timer.fired();
        let Some((key, anchor)) = app.hover.fire(now) else { return };
        if app.popup.is_some() || app.launcher_is_open() || app.notes_is_open() {
            return;
        }
        let Some(text) = app.tray.as_ref().and_then(|t| t.item(&key)).and_then(|i| i.tooltip_text()).map(str::to_owned) else { return };
        // By `hover_surface`'s IDENTITY, never by
        // re-deriving the bar from `anchor.output`: up to ½ s has passed since `hover_moved` armed
        // this, time enough for `update_output` to rename it, or for the name to be briefly shared.
        let Some(surface) = app.hover_surface.clone() else { return };
        let Some(i) = position_by(&app.surfaces, |s| s.layer.wl_surface(), &surface) else { return };
        let Some(columns) = app.surfaces[i].last.as_ref().map(Grid::width) else { return };
        // Refreshed to this surface's CURRENT name: `popup_open` (through `bar_surface`) still
        // picks the bar by name, but now from a name read right now, never a stale one.
        let anchor = PopupAnchor { output: app.surfaces[i].name.clone(), ..anchor };
        // Room for the pixel frame's real extra width, not a flat two columns: a tall bar's frame
        // is wider, and a long tooltip must still fit the output.
        let e = app.surfaces[i].scale.max(1);
        let room = hover::tooltip_columns(columns, app.fonts.frame(e).extra().0, app.fonts.advance(e));
        let rows = crate::list::padded(&[hover::tooltip_row(&text, room)]);
        let qh = app.qh.clone();
        let (columns, height) = rows.cells();
        if app.popup_open(&qh, anchor, PopupBody::Tooltip(text), columns, height, Input::Tooltip) {
            app.menu.tooltip_item = Some(key);
            app.popup_paint(rows);
            log("tooltip shown");
        }
    }
}

impl App {
    /// Cells a popup may take under bar `i`: its columns, and the output's rows below the bar.
    pub(crate) fn popup_room(&self, i: usize) -> (u16, u16) {
        let s = &self.surfaces[i];
        let columns = s.last.as_ref().map_or(0, Grid::width);
        let height = self.bar.config.height.max(1);
        let output_height = self.outputs.info(&s.output).and_then(|o| o.logical_size).map_or(720, |(_, h)| h.max(1) as u32);
        let rows = (output_height / height).saturating_sub(1).min(u16::MAX as u32) as u16;
        (columns, rows)
    }
}

/// A pointer event as the popup sees it; the wheel goes through the bar's `Wheel` (notches).
fn popup_pointer_kind(wheel: &mut Wheel, kind: &PointerEventKind) -> Option<PopupPointer> {
    match kind {
        PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => Some(PopupPointer::Motion),
        PointerEventKind::Leave { .. } => Some(PopupPointer::Leave),
        PointerEventKind::Press { button, .. } => actions::button(*button).map(PopupPointer::Press),
        PointerEventKind::Axis { vertical, .. } => {
            let step = wheel.add(vertical.absolute, vertical.discrete, vertical.value120);
            if vertical.stop {
                wheel.stop();
            }
            match step {
                Some(Button::WheelUp) => Some(PopupPointer::Scroll(-1)),
                Some(Button::WheelDown) => Some(PopupPointer::Scroll(1)),
                _ => None,
            }
        }
        _ => None,
    }
}

impl crate::startup::DeferredRedraw for App {
    fn bar_mut(&mut self) -> &mut Bar {
        &mut self.bar
    }
    fn dirty_mut(&mut self) -> &mut bool {
        &mut self.dirty
    }
    fn redraw_now(&mut self) {
        self.redraw_all();
    }
    fn flush_osd(&mut self) {
        self.osd_draw();
    }
}

impl crate::startup::WindowsFeed for App {
    fn windows_dirty_mut(&mut self) -> &mut bool {
        &mut self.windows_dirty
    }
    fn feed_windows_now(&mut self) {
        self.launcher_windows_changed();
    }
}

#[cfg(test)]
mod tests;
