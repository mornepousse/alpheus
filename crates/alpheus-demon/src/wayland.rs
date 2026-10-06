//! Wayland: one layer-shell surface (Top layer, anchored at the top,
//! exclusive zone) per screen, tracked live; a pointer for clicks and the
//! wheel. Nothing else is asked of the compositor. Redraw only when a
//! screen's grid changes. The tray's session-bus callbacks land here too
//! (`tray_output`): its icons, its gestures, its frame-coalesced re-reads.

use crate::actions::{self, Button, Wheel};
use crate::deadline::DeadlineTimer;
use crate::grid::{Grid, Target};
use crate::keys::Mods;
use crate::launch::Mail;
use crate::launcher::surface::{CloseReason, OpenLauncher};
use crate::painter::{self, Fonts};
use crate::popup::{Anchor as PopupAnchor, OpenPopup, PopupBody, PopupClose, PopupPointer};
use crate::state::{Bar, Msg};
use crate::tray::hover::{self, Hover, HoverChange};
use crate::tray::router::{Tray, TrayOutput};
use crate::tray::{Gesture, ItemKey, icons};
use alpheus_commun::exec::Executor;
use alpheus_commun::theme::Palette;
use smithay_client_toolkit::reexports::calloop::{LoopHandle, channel};
use smithay_client_toolkit::reexports::client::delegate_noop;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
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
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm, delegate_xdg_popup, delegate_xdg_shell,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use std::sync::Arc;
use std::time::Instant;

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
/// capability lost or gained on another seat is not (review items M3/M4,
/// final review slice 3). Generic over `PartialEq`, like `should_restore`
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

/// Whether a surface may carry the tray's frame request (`tray_sync`):
/// configured, and currently holding at least one attached buffer from the
/// live pool — never one `draw()` skipped (not yet sized, an allocation
/// failure) or whose buffers a pool shrink (`shrink_pool_if_needed`) just
/// forgot (fix round, 2026-10-06: `s.last` alone can stay stale across a
/// shrink, while `s.buffers` correctly empties with it). Picking an
/// ineligible surface would mean the compositor never fires a frame
/// callback (nothing attached to repaint), blocking `tray_frame_pending`
/// forever.
fn eligible_for_frame(configured: bool, has_buffer: bool) -> bool {
    configured && has_buffer
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
/// `hover_fire`'s own parent lookups (popup.rs; task 15 fix round, CRITICAL).
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
    /// Last grid drawn: no redraw if nothing changed.
    last: Option<Grid>,
    /// SHM buffers reused across redraws, each with the physical size it
    /// was created at — `plan_buffer` needs it to decide whether a buffer
    /// still fits. At most two in steady state: the compositor keeps the
    /// currently displayed one locked until a newer one is committed (see
    /// `BufferPlan`'s doc), so one persisted buffer is never enough to
    /// avoid reallocating on every redraw. Empty before the first draw,
    /// after a resize whose old buffers no longer match, or once
    /// invalidated by a pool recreation (`shrink_pool_if_needed`: a buffer
    /// only belongs to the pool that created it).
    buffers: Vec<(Buffer, u32, u32)>,
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
    pub(crate) palette: Palette,
    pub(crate) fonts: Fonts,
    pub(crate) exec: Arc<dyn Executor>,
    pub stop: bool,
    /// Raised by a message that changed the display (channel, clock):
    /// redrawn exactly once after `loop.dispatch()` returns, not on every
    /// message — see `launch::on_message`.
    dirty: bool,
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
    /// wp_viewporter, if the compositor has it: scales the launcher's 1×1
    /// scrims to their output (no full-output buffer).
    pub(crate) viewporter: Option<WpViewporter>,
    /// The event loop's handle (launch.rs sets it): keyboard repeat, socket
    /// connections, the dmenu client watch, the tray's deadline timer.
    pub loop_handle: Option<LoopHandle<'static, App>>,
    /// For the tray's frame requests, made outside any Wayland callback.
    qh: QueueHandle<App>,
    /// The tray while its session bus lives; None without the module, without
    /// a session bus, or once the bus closed (never reconnected).
    pub(crate) tray: Option<Tray>,
    /// Armed only while tray calls are pending (2 s timeouts).
    pub(crate) tray_timer: DeadlineTimer,
    /// The bar surface whose frame callback was asked for the tray's dirty
    /// properties (None: none asked).
    pub(crate) tray_frame_pending: Option<wl_surface::WlSurface>,
    /// The horizontal wheel, for tray icons only.
    pub(crate) hwheel: Wheel,
    /// The last button press (serial, time, surface-local position): Activate's
    /// coordinates, the popup grab (task 15).
    pub(crate) last_press: Option<(u32, u32, (f64, f64))>,
    /// The launcher while open; None at rest (nothing of it resident).
    pub(crate) launcher: Option<OpenLauncher>,
    /// Bumped at each open: a scan result from an older open is ignored.
    pub(crate) launcher_generation: u64,
    /// The loop's mailbox (launch.rs sets it): a scan posts its catalog there.
    pub(crate) mail: Option<channel::Sender<Mail>>,
    /// xdg_wm_base, for the popups; None: no popups (logged once at startup).
    pub(crate) xdg: Option<XdgShell>,
    /// The open popup (menu, tooltip, calendar); None at rest.
    pub(crate) popup: Option<OpenPopup>,
    /// The keyboard, bound only while the launcher or a grabbing popup needs it.
    pub(crate) keyboard: Option<wl_keyboard::WlKeyboard>,
    pub(crate) mods: Mods,
    /// The tray icon the pointer rests on (tooltips), and its timer: armed only while it rests on one.
    pub(crate) hover: Hover,
    pub(crate) hover_timer: DeadlineTimer,
    /// The item whose tooltip is shown (it leaving closes the tooltip: ItemGone).
    pub(crate) tooltip_item: Option<ItemKey>,
    /// The bar surface `hover.over`'s icon was last seen on, set by `hover_moved` (which knows the
    /// exact surface `i`); `hover_fire` locates it by this IDENTITY, never by re-deriving the bar
    /// from `Anchor::output` — a name that `update_output` can change, or that two bars could
    /// briefly share, in the ≤ ½ s between arming and firing (task 15 fix round, from the task 16
    /// review). Cleared whenever `Hover` itself cancels (`hover_apply`'s `Cancel` arm).
    hover_surface: Option<wl_surface::WlSurface>,
    /// Raised by a `Msg::Compositor` message, consumed once per
    /// `dispatch()` pass (`launch::WindowsFeed`): an open launcher's windows
    /// are fed at most once per pass, never once per message — a burst
    /// (e.g. niri) can carry several in the same pass, and `set_windows` is
    /// not cheap.
    windows_dirty: bool,
    /// When the scan `spawn_scan` currently allows (at most one at a time)
    /// was started; `None` once it has delivered. Lets a refusal while
    /// reopening (`host::launcher_open_apps`) tell a quick reopen (wait
    /// quietly, adopt that scan's result) from one that has run
    /// suspiciously long (show the message after all) — task 13 fix round,
    /// review item 1.
    pub(crate) scan_started: Option<Instant>,
}

impl App {
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        bar: Bar,
        palette: Palette,
        fonts: Fonts,
        exec: Arc<dyn Executor>,
    ) -> Result<App, String> {
        let compositor = CompositorState::bind(globals, qh).map_err(|e| format!("wl_compositor: {e}"))?;
        let layers = LayerShell::bind(globals, qh).map_err(|e| format!("layer-shell unavailable: {e}"))?;
        let shm = Shm::bind(globals, qh).map_err(|e| format!("wl_shm: {e}"))?;
        let pool = SlotPool::new(64 * 1024, &shm).map_err(|e| format!("shared buffer: {e}"))?;
        // Optional: without it, the launcher has no click-outside scrim (logged at open).
        let viewporter = globals.bind::<WpViewporter, _, _>(qh, 1..=1, ()).ok();
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
            palette,
            fonts,
            exec,
            stop: false,
            dirty: false,
            awaiting_configure: Vec::new(),
            pending_restore: Vec::new(),
            seat: None,
            viewporter,
            loop_handle: None,
            qh: qh.clone(),
            tray: None,
            tray_timer: DeadlineTimer::new(),
            tray_frame_pending: None,
            hwheel: Wheel::default(),
            last_press: None,
            launcher: None,
            launcher_generation: 0,
            mail: None,
            xdg,
            popup: None,
            keyboard: None,
            mods: Mods::default(),
            hover: Hover::new(),
            hover_timer: DeadlineTimer::new(),
            tooltip_item: None,
            hover_surface: None,
            windows_dirty: false,
            scan_started: None,
        })
    }

    fn place(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let info = self.outputs.info(&output);
        let name = info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default();
        let scale = info.map(|i| i.scale_factor).unwrap_or(1);
        let height = self.bar.config.height;
        let surface = self.compositor.create_surface(qh);
        let layer = self.layers.create_layer_surface(qh, surface, Layer::Top, Some("alpheus-bar"), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, height);
        layer.set_exclusive_zone(height as i32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        log(&format!("bar placed on {}", log_name(&name, &output)));
        self.surfaces.push(Surface { output, name, layer, width: 0, scale, configured: false, last: None, buffers: Vec::new() });
    }

    /// Returns the removed surface's output, so the caller can decide to
    /// restore the bar onto it (see `closed`) if it still exists.
    fn remove(&mut self, i: usize) -> wl_output::WlOutput {
        // A popup hanging from this bar goes first (its parent must outlive it): by this surface's
        // IDENTITY (task 15 fix round, CRITICAL), never its `name` — `update_output` can rename it,
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
        s.output
    }

    /// Recreates the pool at a size fitted to the remaining surfaces, if
    /// clearly oversized (see `target_pool`): without this, the peak
    /// footprint of a large unplugged screen stays in memory forever
    /// ("running on a potato").
    fn shrink_pool_if_needed(&mut self) {
        let height = self.bar.config.height;
        let needs: Vec<usize> = self.surfaces.iter().map(|s| buffer_size(s.width, height, s.scale)).collect();
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

    /// Called once per `loop.dispatch()` pass, right after `after_dispatch`
    /// (`launch::run`): places the bar back on every output `closed` marked
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
        let App { surfaces, fonts, bar, palette, pool, .. } = self;
        let s = &mut surfaces[i];
        if !s.configured || s.width == 0 {
            return;
        }
        let e = s.scale.max(1);
        let (w, h) = (s.width * e as u32, bar.config.height * e as u32);
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
        fonts.paint(&grid, palette, pixels, w, h, e);
        let surface = s.layer.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if s.buffers[used].0.attach_to(surface).is_err() {
            return;
        }
        s.layer.commit();
        s.last = Some(grid);
    }

    /// Applies a tray callback's output: redraw (Msg::Tray rebuilt from the
    /// tray's items and `[tray.icons]`), bus closed (logged once, the tray
    /// dropped and emptied, never retried), a menu asked (logged until task 17
    /// opens it); then the frame request and the deadline timer.
    pub(crate) fn tray_output(&mut self, out: TrayOutput) {
        if out.bus_closed {
            log("tray: the session bus closed; the tray stays empty");
            self.tray = None;
        }
        if out.redraw || out.bus_closed {
            // The hovered item leaving the tray closes its tooltip outright (ItemGone), whatever
            // else changed; checked first, independent of the grid below.
            self.tooltip_item_gone();
            let shown = match &self.tray {
                Some(t) => icons::icons(t.items(), &self.bar.config.tray.icons),
                None => Vec::new(),
            };
            if shown.len() != self.bar.tray.len() {
                log(&format!("tray: icons shown: {}", shown.len()));
            }
            // Fix round 2026-10-06 (main fix): `out.redraw` fires for ANY property of ANY item
            // changing (a messenger's unread counter, an icon animating faster than ½ s) — most of
            // which leave the drawn grid untouched. Only an actual display change (`bar.apply`
            // returns true) can possibly have moved or dropped the hovered icon's own zone, so the
            // hover (armed, or already shown as a tooltip) is cancelled then, and only if
            // `hover_target_moved` confirms THAT zone is the one that changed — an unrelated icon
            // changing elsewhere leaves it alone.
            let changed = self.bar.apply(Msg::Tray(shown));
            if changed {
                self.dirty = true;
                if self.hover_target_moved() {
                    let c = self.hover.cancel();
                    self.hover_apply(c);
                }
            }
        }
        if let Some(key) = &out.open_menu {
            log(&format!("tray: menu asked for {}{}", key.owner, key.path));
        }
        self.tray_sync();
    }

    /// After any tray callback: the deadline timer follows the pending calls
    /// (none: no timer), and dirty properties ask for one frame callback on
    /// the first configured bar (a commit with no new buffer and one pixel
    /// of damage), where they are re-read once (controller decision 10).
    fn tray_sync(&mut self) {
        let deadline = self.tray.as_ref().and_then(Tray::next_deadline);
        if let Some(handle) = &self.loop_handle {
            self.tray_timer.sync(handle, deadline, App::tray_deadline);
        }
        if self.tray_frame_pending.is_some() || !self.tray.as_ref().is_some_and(Tray::wants_frame) {
            return;
        }
        let Some(surface) =
            self.surfaces.iter().find(|s| eligible_for_frame(s.configured, !s.buffers.is_empty())).map(|s| s.layer.wl_surface().clone())
        else {
            return;
        };
        surface.frame(&self.qh, surface.clone());
        // 1×1 of buffer damage on every compositor (controller decision 10):
        // an undamaged commit may get no frame callback at all (Hyprland) or a
        // throttled one (niri, ≈ 1 s). The attached buffer is not re-sent.
        surface.damage_buffer(0, 0, 1, 1);
        surface.commit();
        self.tray_frame_pending = Some(surface);
    }

    /// The session bus's fd is readable (calloop callback).
    pub(crate) fn tray_readable(&mut self) {
        let Some(tray) = self.tray.as_mut() else { return };
        let out = tray.on_readable();
        self.tray_output(out);
    }

    /// The deadline timer fired (calloop callback; its source is dropped).
    pub(crate) fn tray_deadline(app: &mut App, now: Instant) {
        app.tray_timer.fired();
        let Some(tray) = app.tray.as_mut() else { return };
        let out = tray.on_deadline(now);
        app.tray_output(out);
    }

    /// The tray icon under `x` on bar `i`, if any.
    fn tray_key_at(&mut self, i: usize, x: f64) -> Option<ItemKey> {
        let e = self.surfaces[i].scale;
        let col = painter::column_under(x, e, self.fonts.advance(e));
        match self.surfaces[i].last.as_ref()?.target_at(col)? {
            Target::TrayItem(key) => Some(key.clone()),
            _ => None,
        }
    }

    /// A gesture at `position` (surface-local, logical) on bar `i`: sent to
    /// the tray icon there, with that position (decision 4); false when no
    /// icon is there (the bar's own click handling then applies).
    fn tray_gesture_at(&mut self, i: usize, position: (f64, f64), g: Gesture) -> bool {
        let Some(key) = self.tray_key_at(i, position.0) else { return false };
        if let Some(tray) = self.tray.as_mut() {
            let out = tray.gesture(&key, g, (position.0 as i32, position.1 as i32));
            self.tray_output(out);
        }
        true
    }

    /// `launcher_was_open`: whether the launcher was open right before this
    /// press (`pointer_frame` already closed it, as a click outside, before
    /// calling here): the same press must not reopen it (`toggles_launcher`).
    fn click(&mut self, qh: &QueueHandle<Self>, i: usize, x: f64, b: Button, launcher_was_open: bool) {
        let e = self.surfaces[i].scale;
        let advance = self.fonts.advance(e);
        let toggles = {
            let Some(grid) = &self.surfaces[i].last else { return };
            grid.target_at(painter::column_under(x, e, advance)).is_some_and(|t| actions::toggles_launcher(t, b))
        };
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

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, f: i32) {
        // Ownership decided first, outside any guard (task 13 fix round,
        // review item 2): a panic inside `launcher_scale` must never make a
        // BAR surface's own scale change look handled.
        if self.launcher_owns_box(surface) {
            self.launcher_guarded("scale", |app| app.launcher_scale(surface, f));
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
        // Ownership decided first, outside any guard (task 13 fix round,
        // review item 2): same reasoning as `scale_factor_changed`. Checked
        // before the bar's own restore logic below even gets a chance to
        // read: a launcher surface never goes through it.
        if self.launcher_owns_surface(layer.wl_surface()) {
            self.launcher_guarded("closed", |app| app.launcher_layer_closed(layer.wl_surface()));
            return; // a launcher surface: the bar's restore logic is not involved
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
        // batch has been dispatched (`launch::run`), sees the true end
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
        // Ownership decided first, outside any guard (task 13 fix round,
        // review item 2): same reasoning as `scale_factor_changed`.
        if self.launcher_owns_surface(layer.wl_surface()) {
            self.launcher_guarded("configure", |app| app.launcher_configured(layer.wl_surface(), c.new_size));
            return;
        }
        let Some(i) = self.index(layer.wl_surface()) else { return };
        // First configure of THIS surface: the restore (if there was one
        // pending) succeeded, the output can again be restored in the
        // future if the compositor closes its surface again.
        let first_configure = !self.surfaces[i].configured;
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
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, c: Capability) {
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
        }
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

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if tracks_seat(&self.seat, &seat) {
            self.seat = None;
            // The seat itself is gone, not just one of its capabilities: any
            // keyboard bound through it (the launcher's, a grabbing popup's,
            // while open) goes with it — `remove_capability` alone never
            // fires for this case (review item M3, final review slice 3).
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
            if self.popup.as_ref().is_some_and(|p| p.wl_surface() == &ev.surface) {
                if let (PointerEventKind::Enter { .. }, Some(p)) = (&ev.kind, &self.pointer) {
                    let _ = p.set_cursor(conn, CursorIcon::Default);
                }
                // The popup's OWN wheel accumulator (task 15 fix round), never the bar's
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
                    let was_open = self.launcher_is_open();
                    if was_open {
                        self.launcher_guarded("click outside", |app| app.launcher_close(CloseReason::ClickOutside));
                    }
                    // A press on the bar is delivered to it even while one of its popups grabs
                    // the pointer (same client, fact check 5): it is a click outside that popup.
                    if self.popup.is_some() {
                        self.popup_close(PopupClose::OutsideClick);
                    }
                    if let Some(b) = actions::button(*button) {
                        if !self.tray_gesture_at(i, ev.position, actions::tray_gesture(b)) {
                            self.click(qh, i, ev.position.0, b, was_open);
                        }
                    }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    if let Some(b) = self.wheel.add(vertical.absolute, vertical.discrete, vertical.value120) {
                        if !self.tray_gesture_at(i, ev.position, actions::tray_gesture(b)) {
                            self.click(qh, i, ev.position.0, b, false);
                        }
                    }
                    // The horizontal axis only scrolls tray icons.
                    let h = self.hwheel.add(horizontal.absolute, horizontal.discrete, horizontal.value120);
                    if let Some(g) = h.and_then(actions::tray_horizontal) {
                        self.tray_gesture_at(i, ev.position, g);
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
delegate_noop!(App: ignore WpViewporter);
delegate_noop!(App: ignore WpViewport);
delegate_registry!(App);

impl App {
    /// The pointer moved on bar `i` at `x`: over a tray icon, the tooltip's timer (only with tooltips on,
    /// and never while a grabbing popup or the launcher is open: no tooltip could show then).
    fn hover_moved(&mut self, i: usize, x: f64) {
        let busy = self.popup.as_ref().is_some_and(|p| p.grab) || self.launcher_is_open();
        let tooltips = self.bar.config.tray.tooltips && !busy;
        let e = self.surfaces[i].scale;
        let column = painter::column_under(x, e, self.fonts.advance(e));
        // Borrowed from `self.surfaces[i]` (fix round 2026-10-06, item 2): resting on the same
        // icon allocates nothing, `Hover::moved` only clones the key and the output name on Arm.
        let found = self.surfaces[i].last.as_ref().and_then(|g| hover::over(tooltips, g, column));
        let c = self.hover.moved(found, &self.surfaces[i].name, Instant::now());
        if matches!(c, HoverChange::Arm(_)) {
            // This surface's own identity, known here precisely (task 15 fix round, from the task
            // 16 review): `hover_fire`, run later by its timer, must find the SAME bar back by
            // this, never by re-deriving it from a name that could have changed meanwhile.
            self.hover_surface = Some(self.surfaces[i].layer.wl_surface().clone());
        }
        self.hover_apply(c);
    }

    /// Arm: the timer at that instant (a tooltip shown for another icon goes); Cancel: no timer, no tooltip.
    pub(crate) fn hover_apply(&mut self, c: HoverChange) {
        let tooltip_shown = matches!(self.popup.as_ref().map(|p| &p.body), Some(PopupBody::Tooltip(_)));
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
        if let Some(handle) = self.loop_handle.clone() {
            self.hover_timer.sync(&handle, deadline, App::hover_fire);
        }
    }

    /// The tooltip's item left the tray: its tooltip goes.
    fn tooltip_item_gone(&mut self) {
        let Some(open) = self.popup.as_ref() else { return };
        if !matches!(open.body, PopupBody::Tooltip(_)) {
            return;
        }
        // A Tooltip popup is always paired with `tooltip_item` (set together in `hover_fire`,
        // cleared together in `popup_close`). Should that ever break, the tooltip closes rather
        // than the daemon panicking. `self.tray` can be None: the session bus just closed,
        // taking every item with it.
        let gone = match (&self.tray, self.tooltip_item.as_ref()) {
            (Some(tray), Some(key)) => tray.item(key).is_none(),
            _ => true,
        };
        if gone {
            self.popup_close(PopupClose::ItemGone);
        }
    }

    /// True when the item `Hover` is tracking (armed, or already shown as a tooltip) lost its
    /// zone, or moved to different columns, in the grid `self.bar` would now paint for the
    /// surface it was armed on (fix round 2026-10-06, main fix) — found by `hover_surface`'s
    /// IDENTITY, never `anchor.output`'s name (it can change meanwhile, see the task 15 fix
    /// round). An unrelated icon changing elsewhere never moves THIS one's zone, so this stays
    /// false then — the whole point of only cancelling here, not on every redraw. Nothing
    /// tracked: never "moved".
    fn hover_target_moved(&self) -> bool {
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
        if app.popup.is_some() || app.launcher_is_open() {
            return;
        }
        let Some(text) = app.tray.as_ref().and_then(|t| t.item(&key)).and_then(|i| i.tooltip_text()).map(str::to_owned) else { return };
        // By `hover_surface`'s IDENTITY (task 15 fix round, from the task 16 review), never by
        // re-deriving the bar from `anchor.output`: up to ½ s has passed since `hover_moved` armed
        // this, time enough for `update_output` to rename it, or for the name to be briefly shared.
        let Some(surface) = app.hover_surface.clone() else { return };
        let Some(i) = position_by(&app.surfaces, |s| s.layer.wl_surface(), &surface) else { return };
        let Some(columns) = app.surfaces[i].last.as_ref().map(Grid::width) else { return };
        // Refreshed to this surface's CURRENT name: `popup_open` (through `bar_surface`) still
        // picks the bar by name, but now from a name read right now, never a stale one.
        let anchor = PopupAnchor { output: app.surfaces[i].name.clone(), ..anchor };
        let row = hover::tooltip_row(&text, columns.saturating_sub(2));
        let qh = app.qh.clone();
        if app.popup_open(&qh, anchor, PopupBody::Tooltip(text), row.width(), 1, false) {
            app.tooltip_item = Some(key);
            app.popup_paint(&[row]);
            log("tooltip shown");
        }
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

impl crate::launch::DeferredRedraw for App {
    fn bar_mut(&mut self) -> &mut Bar {
        &mut self.bar
    }
    fn dirty_mut(&mut self) -> &mut bool {
        &mut self.dirty
    }
    fn redraw_now(&mut self) {
        self.redraw_all();
    }
}

impl crate::launch::WindowsFeed for App {
    fn windows_dirty_mut(&mut self) -> &mut bool {
        &mut self.windows_dirty
    }
    fn feed_windows_now(&mut self) {
        self.launcher_windows_changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launcher_button_click_toggles_unless_this_press_just_closed_it() {
        assert_eq!(decide_click(true, false), ClickDecision::ToggleLauncher);
        assert_eq!(decide_click(true, true), ClickDecision::Nothing, "the same press that closed it as a click outside must not reopen it");
        assert_eq!(decide_click(false, true), ClickDecision::RunBarAction, "elsewhere on the bar: its own action");
        assert_eq!(decide_click(false, false), ClickDecision::RunBarAction);
    }

    #[test]
    fn buffer_size_accounts_for_scale() {
        assert_eq!(buffer_size(100, 22, 1), 100 * 22 * 4);
        assert_eq!(buffer_size(100, 22, 2), 200 * 44 * 4);
        assert_eq!(buffer_size(100, 22, 0), 100 * 22 * 4, "scale 0 treated as 1");
    }

    #[test]
    fn pool_already_at_floor_without_surfaces_nothing_to_do() {
        assert_eq!(target_pool(POOL_FLOOR, &[]), None);
        assert_eq!(target_pool(POOL_FLOOR / 2, &[]), None, "already under the floor: no needless recreation");
    }

    #[test]
    fn no_more_surfaces_returns_to_floor() {
        // A large screen (3840 logical px, scale 2) grew the pool well past
        // the floor; once unplugged, no surface remains: the pool must
        // return to the floor, not stay at its peak size forever.
        assert_eq!(target_pool(4 * 1024 * 1024, &[]), Some(POOL_FLOOR));
    }

    #[test]
    fn slight_oversize_leaves_the_pool_as_is() {
        // A pool that just doubled to fit a screen must not be recreated at
        // the slightest reduction (churn): only a clear gap
        // (POOL_OVERSIZE_FACTOR) triggers a recreation.
        let need = 676_480; // ~3840×44×4 (wide screen, scale 2)
        assert_eq!(target_pool(need * 2, &[need]), None);
        assert_eq!(target_pool(need * POOL_OVERSIZE_FACTOR, &[need]), None, "right at the factor: not clear yet");
    }

    #[test]
    fn clear_oversize_recreates_smaller() {
        let need = 676_480;
        let target = target_pool(need * POOL_OVERSIZE_FACTOR + 1, &[need]);
        assert_eq!(target, Some(need * 2), "{target:?}");
        assert!(target.unwrap() < need * POOL_OVERSIZE_FACTOR, "clearly smaller than before");
    }

    #[test]
    fn several_surfaces_the_sum_decides_not_the_largest() {
        // The pool holds EVERY surface's buffer at once (one per screen): it
        // needs room for their SUM, not just the largest — otherwise three
        // equally large screens would never all fit their buffers at the
        // same time in a pool sized for just one.
        let (small, large) = (10_000, 676_480);
        let sum = small + large;
        assert_eq!(target_pool(sum * POOL_OVERSIZE_FACTOR, &[small, large]), None, "right at the sum's factor: not clear yet");
        let target = target_pool(sum * POOL_OVERSIZE_FACTOR + 1, &[small, large]);
        assert_eq!(target, Some(sum * 2));
        assert!(target.unwrap() >= sum, "the recreated pool must hold every buffer at once, at least");
    }

    /// Fix round, 2026-10-06 (reviewer findings 2/3 on task 11): a surface
    /// `draw()` skipped (never sized, an allocation failure) or whose
    /// buffers a pool shrink just forgot must never carry the tray's frame
    /// request — the compositor would never fire its callback, blocking
    /// every later re-read.
    #[test]
    fn tray_sync_never_picks_a_surface_with_no_attached_buffer() {
        assert!(!eligible_for_frame(true, false), "configured, but nothing attached (draw() skipped it, or a pool shrink forgot it)");
        assert!(!eligible_for_frame(false, true), "not configured yet: no commit belongs to it either");
        assert!(eligible_for_frame(true, true), "configured and holding an attached buffer");
    }

    /// Review items M3/M4, final review slice 3: the same check used by
    /// `new_capability`, `remove_capability` and `remove_seat` to tell our
    /// seat apart from any other on a multi-seat session.
    #[test]
    fn tracks_seat_only_matches_the_tracked_one() {
        assert!(tracks_seat(&Some(1), &1), "the tracked seat");
        assert!(!tracks_seat(&Some(1), &2), "a different seat");
        assert!(!tracks_seat(&None, &1), "no seat tracked at all");
    }

    /// Task 15 fix round, CRITICAL: the shared lookup `popup_close`/`popup_output_gone`/`hover_fire`
    /// (popup.rs) now use to find their bar by surface IDENTITY, never by a `name` that
    /// `update_output` can change, or that two bars could briefly share — generic so this is
    /// testable without a live Wayland connection, a plain integer standing in for a
    /// `wl_surface::WlSurface` (see `should_restore`'s own comment, same reasoning for
    /// `wl_output::WlOutput`).
    #[test]
    fn position_by_goes_by_the_projected_identity_never_a_shared_name() {
        // Two entries share the "name" "": only the identity (the second element) tells them apart.
        let items = [("", 10), ("", 20), ("other", 30)];
        assert_eq!(position_by(&items, |(_, id)| id, &20), Some(1));
        assert_eq!(position_by(&items, |(_, id)| id, &10), Some(0));
        assert_eq!(position_by(&items, |(_, id)| id, &99), None, "no match: already gone");
    }

    #[test]
    fn log_text_readable_fallback_for_an_unnamed_output() {
        assert_eq!(log_text("HDMI-A-1", 7), "HDMI-A-1");
        assert_eq!(log_text("", 7), "an output with no name (id 7)");
    }

    #[test]
    fn should_restore_if_the_output_is_still_known() {
        // wl_output::WlOutput cannot be instantiated outside a real Wayland
        // connection (see the comment on `should_restore`): plain integers
        // stand in for output ids in this test, the function being generic
        // over `PartialEq`.
        assert!(should_restore(&2, [1, 2, 3].into_iter()), "still in the list: to be restored");
        assert!(!should_restore(&5, [1, 2, 3].into_iter()), "no longer in the list: really unplugged");
        assert!(!should_restore(&1, std::iter::empty::<i32>()), "no output known at all anymore");
    }

    #[test]
    fn plan_buffer_resizes_when_none_yet() {
        assert_eq!(plan_buffer(&[], (100, 22)), BufferPlan::Resize);
    }

    #[test]
    fn plan_buffer_reuses_the_one_free_buffer() {
        assert_eq!(plan_buffer(&[((100, 22), false)], (100, 22)), BufferPlan::Reuse(0));
    }

    #[test]
    fn plan_buffer_grows_when_the_only_buffer_is_busy() {
        // The whole point of double-buffering: a single buffer is always
        // busy right up to the moment a new one replaces it (see
        // `BufferPlan`'s doc) — this must grow to a second, not stall.
        assert_eq!(plan_buffer(&[((100, 22), true)], (100, 22)), BufferPlan::Grow);
    }

    #[test]
    fn plan_buffer_reuses_whichever_of_two_is_free() {
        assert_eq!(plan_buffer(&[((100, 22), true), ((100, 22), false)], (100, 22)), BufferPlan::Reuse(1));
        assert_eq!(plan_buffer(&[((100, 22), false), ((100, 22), true)], (100, 22)), BufferPlan::Reuse(0));
    }

    #[test]
    fn plan_buffer_grows_when_both_of_two_are_busy() {
        assert_eq!(plan_buffer(&[((100, 22), true), ((100, 22), true)], (100, 22)), BufferPlan::Grow);
    }

    #[test]
    fn plan_buffer_resizes_on_a_size_change_whatever_the_busy_state() {
        // Wrong size: every previous buffer is useless either way, free or busy.
        assert_eq!(plan_buffer(&[((100, 22), false)], (120, 22)), BufferPlan::Resize);
        assert_eq!(plan_buffer(&[((100, 22), true)], (120, 22)), BufferPlan::Resize);
        assert_eq!(plan_buffer(&[((100, 22), false), ((100, 22), true)], (120, 22)), BufferPlan::Resize);
    }

    #[test]
    fn should_restore_once_bounds_repeated_closures() {
        // Still known, never yet restored: to be restored.
        assert!(should_restore_once(&2, &[], [1, 2, 3].into_iter()));
        // Already restored (in `pending`), no configure received since: a
        // compositor that closed its surface in a loop must NOT be restored
        // a second time as long as the first one hasn't succeeded.
        assert!(!should_restore_once(&2, &[2], [1, 2, 3].into_iter()));
        // No longer known at all: never to be restored, even absent from `pending`.
        assert!(!should_restore_once(&5, &[], [1, 2, 3].into_iter()));
    }

    #[test]
    fn pending_restores_keeps_only_still_known_outputs() {
        // 2 is still known at after-dispatch time: restored. 1 no longer
        // is (its `output_destroyed`, in the same batch, already ran): the
        // hot-plug race this fixes — left alone, no extra remove+place.
        assert_eq!(pending_restores(&[1, 2], [2, 3].into_iter()), vec![2]);
    }

    #[test]
    fn pending_restores_empty_when_nothing_marked() {
        assert_eq!(pending_restores::<i32>(&[], [1, 2, 3].into_iter()), Vec::<i32>::new());
    }

    #[test]
    fn pending_restores_empty_when_none_of_the_marked_outputs_survived() {
        // Every output marked for restore by `closed` turned out to be
        // truly gone by the time the batch finished: nothing restored.
        assert_eq!(pending_restores(&[5, 6], [1, 2, 3].into_iter()), Vec::<i32>::new());
    }

    #[test]
    fn pending_restores_preserves_the_marked_order() {
        assert_eq!(pending_restores(&[3, 1, 2], [1, 2, 3].into_iter()), vec![3, 1, 2]);
    }
}
