//! The launcher's Wayland side, kept thin. One centred `Overlay` layer
//! surface with exclusive keyboard focus (the box), one transparent 1×1
//! "scrim" per output scaled by wp_viewporter to catch a click outside, a
//! keyboard that is the App's (shared with the popups, popup.rs), buffers
//! from a pool of its own. Everything lives in `OpenLauncher`, dropped whole
//! on close: nothing of the launcher stays resident (spec § 3).

use crate::actions::{self, Button};
use crate::keys;
use crate::launcher::config::LauncherConfig;
use crate::launcher::history;
use crate::launcher::model::{Launcher, Outcome};
use crate::launcher::run::{self, Done, RunContext, Spawner};
use crate::launcher::scan::ScanEnv;
use crate::launcher::socket::Pending;
use crate::launcher::view::{self, BoxLayout, Click};
use crate::list::Boxed;
use crate::log::write as log;
use crate::painter::{self, Frame};
use crate::popup::{PopupClose, box_size, cell_under};
use crate::state::Feed;
use crate::wayland::{App, pick_buffer};
use alpheus_commun::launcher_ipc::Reply;
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::seat::SeatError;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardError};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

const NAMESPACE: &str = "alpheus-launcher";
const SCRIM_NAMESPACE: &str = "alpheus-launcher-scrim";
/// Logical size assumed when no output announced one (never seen on a real
/// compositor; keeps the box sane rather than zero-sized).
const FALLBACK_OUTPUT_SIZE: (u32, u32) = (1280, 720);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Escape,
    Toggle,
    Close,
    FocusLost,
    ClickOutside,
    Activated,
    OutputGone,
    Replaced,
    Error,
}

impl CloseReason {
    /// For the log: "launcher closed (<text>)".
    pub fn text(self) -> &'static str {
        match self {
            CloseReason::Escape => "escape",
            CloseReason::Toggle => "toggle",
            CloseReason::Close => "close",
            CloseReason::FocusLost => "focus lost",
            CloseReason::ClickOutside => "click outside",
            CloseReason::Activated => "activated",
            CloseReason::OutputGone => "output gone",
            CloseReason::Replaced => "replaced",
            CloseReason::Error => "error",
        }
    }
}

/// Output grid in cells (for view::layout) from its logical size.
pub fn output_cells(width: u32, height: u32, advance_phys: f32, row_height: u32, scale: i32) -> (u16, u16) {
    let e = scale.max(1) as u32;
    let columns = painter::columns(width * e, advance_phys);
    let rows = (height * e).checked_div(row_height).map_or(0, |r| r.min(u16::MAX as u32) as u16);
    (columns, rows)
}

/// The box's logical size: the layout's cells without the ring of one blank cell its numbers count
/// (`view`), plus the pixel frame's room around them.
pub(crate) fn layout_size(layout: &BoxLayout, advance_phys: f32, row_height: u32, scale: i32, frame: &Frame) -> (u32, u32) {
    box_size(layout.columns.saturating_sub(2), layout.rows.saturating_sub(2), advance_phys, row_height, scale, frame)
}

/// The layout's cell (column, row), in `view::hit`'s coordinates (the layout's ring of one blank
/// cell included), under a pointer at `position` (logical px) on the box; None in the frame's
/// margin, where no cell is.
pub(crate) fn hit_cell(
    position: (f64, f64),
    scale: i32,
    advance_phys: f32,
    row_height: u32,
    frame: &Frame,
    layout: &BoxLayout,
) -> Option<(u16, u16)> {
    let cells = (layout.columns.saturating_sub(2), layout.rows.saturating_sub(2));
    cell_under(position.0, position.1, scale, advance_phys, row_height, frame, cells).map(|(column, row)| (column + 1, row + 1))
}

/// What a redraw does with the box (pure decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Redraw {
    /// New logical size: set_size + commit; the compositor's configure redraws.
    Resize(u32, u32),
    Draw,
    /// Not configured yet: the first configure draws.
    Wait,
}

pub(crate) fn plan_redraw(current: (u32, u32), wanted: (u32, u32), configured: bool) -> Redraw {
    if current != wanted {
        Redraw::Resize(wanted.0, wanted.1)
    } else if configured {
        Redraw::Draw
    } else {
        Redraw::Wait
    }
}

/// The box's layout, decided once per open (Mae, 2026-10-05): the preview
/// panel is reserved for the app launcher from the first frame — blank while
/// the selection has nothing to preview or the catalog is not in yet — so
/// the box never changes width as the selection moves; a script menu never
/// has one. Driven by `model.is_script()` alone, never by the selection.
pub(crate) fn open_layout(output_columns: u16, output_rows: u16, model: &Launcher) -> BoxLayout {
    view::layout(output_columns, output_rows, model.config.rows, model.is_script(), !model.is_script())
}

/// The size a configure imposes, when it is not the one asked: a 0
/// dimension is "the client's choice", i.e. what was asked.
pub(crate) fn plan_configure(asked: (u32, u32), configured: (u32, u32)) -> Option<(u32, u32)> {
    let w = if configured.0 == 0 { asked.0 } else { configured.0 };
    let h = if configured.1 == 0 { asked.1 } else { configured.1 };
    ((w, h) != asked).then_some((w, h))
}

/// Bytes the box's shared-memory pool must carry: two buffers of its own
/// frame (`pick_buffer` alternates between them so a redraw never waits on
/// the compositor still holding the previous one), plus a small, fixed
/// share per output for that output's scrim — a 1×1 buffer, a handful of
/// real bytes, rounded up generously for wl_shm's own per-slot bookkeeping.
/// The box and every scrim share this one pool (`Scrim::show`'s `pool`
/// argument is `OpenLauncher::pool`, the same value): sizing it for the box
/// alone left a scrim with nowhere to allocate its buffer on a multi-output
/// session.
pub(crate) fn pool_size(frame_bytes: usize, outputs: usize) -> usize {
    2 * frame_bytes + 64 * outputs
}

/// Whether a surface event belongs to the launcher: pure past the two
/// identity checks (`is_box`, `is_scrim`), which are plain `WlSurface`
/// equality and never panic — computed by the caller BEFORE any guard runs,
/// so a panic inside launcher code can never make a bar surface's own
/// configure/scale/closed event look like "handled here, skip the bar"
/// (task 13 fix round, review item 2: `wayland.rs` used to decide ownership
/// from the guarded call's own result, `.unwrap_or(true)`).
pub(crate) fn owns_surface(open: bool, is_box: bool, is_scrim: bool) -> bool {
    open && (is_box || is_scrim)
}

/// The open layout refitted inside a size the compositor imposed (logical
/// px): its cells are the room, `view::layout`'s 1-cell margins given back.
pub(crate) fn layout_within(size: (u32, u32), model: &Launcher, advance_phys: f32, row_height: u32, scale: i32) -> BoxLayout {
    let (columns, rows) = output_cells(size.0, size.1, advance_phys, row_height, scale);
    open_layout(columns.saturating_add(2), rows, model)
}

/// What a scale change recomputes the box's layout within: the
/// compositor's own last-imposed size (`plan_configure`), if there is one —
/// it is a logical size, unaffected by scale, so it must survive a scale
/// change untouched rather than be recomputed from the whole output again —
/// otherwise the open layout refitted to the whole output, same math
/// `launcher_resize` always used before this size was tracked. Returns the
/// recomputed layout and the logical size it should be shown at.
pub(crate) fn resized_layout(
    imposed: Option<(u32, u32)>,
    output_size: (u32, u32),
    model: &Launcher,
    advance_phys: f32,
    row_height: u32,
    scale: i32,
    frame: &Frame,
) -> (BoxLayout, (u32, u32)) {
    match imposed {
        Some(size) => (layout_within(size, model, advance_phys, row_height, scale), size),
        None => {
            let (columns, rows) = output_cells(output_size.0, output_size.1, advance_phys, row_height, scale);
            let layout = open_layout(columns, rows, model);
            let size = layout_size(&layout, advance_phys, row_height, scale, frame);
            (layout, size)
        }
    }
}

/// The order `launcher_open` follows when its pool allocation fails: the
/// reply (if a client is waiting) goes out first; `model` — an argument,
/// otherwise alive until `launcher_open` itself returns — is dropped next,
/// since the failed attempt may still have allocated scratch (the ranker's
/// matrices, a catalog); `release` (`crate::memory::release_memory`,
/// malloc_trim) runs LAST, so it actually sees those pages freed instead of
/// running while `model` still holds them (review item M1, final review
/// slice 3: this used to call `release_memory` BEFORE `model` was dropped).
/// Pulled out so the order itself is tested, without a real pool, model or
/// `Pending`.
pub(crate) fn pool_failure_order(reply: impl FnOnce(), drop_model: impl FnOnce(), release: impl FnOnce()) {
    reply();
    drop_model();
    release();
}

/// Whether a newly announced output should get a scrim of its own: only
/// while the launcher is open (closed: nothing to catch a click outside
/// of) AND only with `wp_viewporter` (`make_scrim`'s own early return
/// already covers it, this is the decision pulled out in front of it) —
/// review item M5, final review slice 3. Pulled out so it is tested without
/// a real output or viewporter.
pub(crate) fn adds_scrim(open: bool, has_viewporter: bool) -> bool {
    open && has_viewporter
}

/// Some(path) unless the history is disabled (`history = 0`).
pub(crate) fn history_path_for(config: &LauncherConfig, state_dir: &Path) -> Option<PathBuf> {
    (config.history > 0).then(|| history::history_path(state_dir))
}

/// A waiting dmenu client's answer when the launcher closes without a
/// choice: Cancelled (the script sees Esc, exit 1), or Error when the
/// launcher itself failed.
pub(crate) fn reply_on_close(reply: Option<Pending>, reason: CloseReason) {
    let Some(p) = reply else { return };
    let r = match reason {
        CloseReason::Error => Reply::Error("the launcher failed (see the daemon's log)".to_string()),
        _ => Reply::Cancelled,
    };
    p.reply(&r);
}

/// The scan's $PATH, read at open: `ScanEnv::from_env` only reads the
/// environment (no filesystem walk — the directory scan proper runs on its
/// own thread, asynchronously), so the app launcher's preview can show a
/// command's resolved path from the very first frame, before the scan has
/// even delivered its catalog. A script menu runs nothing (`sh -c`, the
/// focus template and the scan's own commands never apply to it): no $PATH
/// is read for it.
pub(crate) fn open_path_dirs(script: bool, var: impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    if script { Vec::new() } else { ScanEnv::from_env(var).path_dirs }
}

/// Whether a failed keyboard bind is worth a log line: `UnsupportedCapability`
/// just means the seat has no keyboard yet — `launcher_bind_keyboard` is
/// retried once the capability appears (`SeatHandler`'s capability-added
/// event, wayland.rs) — expected and routine, not worth a line every time it
/// happens. Anything else (a dead seat, a protocol error) is.
pub(crate) fn should_log_keyboard_error(e: &KeyboardError) -> bool {
    !matches!(e, KeyboardError::Seat(SeatError::UnsupportedCapability(_)))
}

/// Which wlr-layer-shell layer a scrim uses: `Layer::Overlay`, the SAME
/// layer as the box — not `Layer::Top`, which Hyprland draws BENEATH a
/// fullscreen window (Mae, 2026-10-05: she plays fullscreen games on
/// Hyprland). A scrim on Top would then never see a click at all, silently
/// breaking "click outside" the moment a game is focused — review item M2,
/// final review slice 3. Pulled out as its own function so the choice is a
/// tested value, not just a literal buried in `make_scrim`.
fn scrim_layer() -> Layer {
    Layer::Overlay
}

/// A transparent 1×1 surface covering one output (outside the bar's
/// exclusive zone): a press on it is a click outside the box.
struct Scrim {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    viewport: WpViewport,
    buffer: Option<Buffer>,
}

impl Scrim {
    /// Attaches the 1×1 transparent buffer (once) and scales it to `size`.
    fn show(&mut self, pool: &mut SlotPool, (w, h): (u32, u32)) {
        if self.buffer.is_none() {
            match pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888) {
                Ok((buffer, canvas)) => {
                    canvas.fill(0);
                    self.buffer = Some(buffer);
                }
                Err(e) => {
                    log(&format!("launcher scrim buffer: {e}"));
                    return;
                }
            }
        }
        self.viewport.set_destination(w.max(1) as i32, h.max(1) as i32);
        let surface = self.layer.wl_surface();
        if let Some(b) = &self.buffer {
            // On a reconfigure the compositor still holds it: attach fails
            // and only the destination changes, which is all that's needed.
            let _ = b.attach_to(surface);
        }
        surface.damage_buffer(0, 0, 1, 1);
        self.layer.commit();
    }
}

impl Drop for Scrim {
    fn drop(&mut self) {
        // The viewport before its surface (the layer surface goes with the field).
        self.viewport.destroy();
    }
}

/// Everything one open launcher holds; dropped whole on close.
pub struct OpenLauncher {
    pub model: Launcher,
    pub reply: Option<Pending>,
    pub generation: u64,
    pub requested_at: Instant,
    pub spawner: Spawner,
    pub history_path: Option<PathBuf>,
    box_layer: LayerSurface,
    scrims: Vec<Scrim>,
    /// The box's own pool: dropped with it (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// Decided at open (`open_layout`); changed only by a scale change or an
    /// imposed configure size, never by the selection.
    layout: BoxLayout,
    /// Logical size of the box (asked, or imposed by a configure).
    size: (u32, u32),
    /// The compositor's own last-imposed size (`plan_configure`), if any:
    /// kept across scale changes (`resized_layout`), since it is a logical
    /// size the compositor dictates, unaffected by scale.
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
    /// The dmenu client's watch (host.rs), removed on drop.
    client_watch: Option<(LoopHandle<'static, App>, RegistrationToken)>,
}

impl OpenLauncher {
    /// Called by `host.rs`'s `launcher_watch_client` once the dmenu client's
    /// hang-up watch is registered on the loop; `client_watch`'s own `Drop`
    /// removes it when the launcher closes.
    pub(crate) fn set_client_watch(&mut self, handle: LoopHandle<'static, App>, token: RegistrationToken) {
        self.client_watch = Some((handle, token));
    }
}

impl Drop for OpenLauncher {
    fn drop(&mut self) {
        // The client watch; scrims, box and pool follow as fields (viewports
        // before their surfaces, see Scrim's Drop). The keyboard is the App's
        // (shared with the popups): launcher_close releases it.
        if let Some((handle, token)) = self.client_watch.take() {
            handle.remove(token);
        }
    }
}

impl App {
    /// Creates the box on the focused output (by name among the bar's outputs; unknown → compositor's
    /// choice) and the scrims; logs "launcher shown on <output> in <µs> µs" at the first commit.
    pub fn launcher_open(&mut self, qh: &QueueHandle<App>, mut model: Launcher, reply: Option<Pending>, requested_at: Instant) {
        if self.launcher.is_some() {
            self.launcher_close(CloseReason::Replaced);
        }
        // One of the two at a time: an open popup (menu, calendar, tooltip) goes.
        self.popup_close(PopupClose::Launcher);
        // A tooltip's hover tracking must not survive it (task 15 fix round, from the task 16
        // review): left alone, `Hover.over` would still name whatever icon it was armed on, and a
        // later motion that never truly left it would wrongly look like "still on the same icon",
        // never re-arming.
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
        // Sizing: the named output, else the first one (the compositor's
        // choice is usually the focused one; unknown here).
        let info = named.clone().or_else(|| self.outputs.outputs().next()).and_then(|o| self.outputs.info(&o));
        let scale = info.as_ref().map_or(1, |i| i.scale_factor).max(1);
        let output_size =
            info.as_ref().and_then(|i| i.logical_size).map_or(FALLBACK_OUTPUT_SIZE, |(w, h)| (w.max(1) as u32, h.max(1) as u32));
        let row_height = self.bar.config.height * scale as u32;
        let advance = self.fonts.advance(scale);
        let (columns, rows) = output_cells(output_size.0, output_size.1, advance, row_height, scale);
        // The single copy of the scan's $PATH lives on the model
        // (`Launcher::path_dirs`, read by both the preview and
        // `RunContext`). For the app launcher, `launcher_open_apps`
        // (host.rs) already set it, on `model`, from the one `ScanEnv` it
        // reads for the scan itself — read a second time here, it would
        // just be overwritten with the same value a few lines later (task
        // 13 fix round, review item 6). A script menu runs nothing through
        // $PATH: reset to empty, cheaply (no environment read).
        if model.is_script() {
            model.path_dirs = open_path_dirs(true, |k| std::env::var_os(k));
        }
        // Once per open: the box keeps this size whatever gets selected.
        let layout = open_layout(columns, rows, &model);
        model.set_list_rows(layout.list_rows as usize);
        let size = layout_size(&layout, advance, row_height, scale, &self.fonts.frame(scale));
        let bytes = (size.0 * scale as u32) as usize * (size.1 * scale as u32) as usize * 4;
        let outputs = self.outputs.outputs().count();
        let pool = match SlotPool::new(pool_size(bytes, outputs).max(4096), &self.shm) {
            Ok(p) => p,
            Err(e) => {
                log(&format!("launcher: no shared buffer ({e}), not opened"));
                let message = format!("the launcher could not open: {e}");
                pool_failure_order(
                    || {
                        if let Some(p) = reply {
                            p.reply(&Reply::Error(message));
                        }
                    },
                    || drop(model),
                    crate::memory::release_memory,
                );
                return;
            }
        };
        // Scrims created BEFORE the box (Mae, 2026-10-05 — review item M2,
        // final review slice 3): both now live on `scrim_layer()`, the SAME
        // layer as the box, instead of the scrims' former `Layer::Top` —
        // Hyprland draws the Top layer BENEATH a fullscreen window, so a
        // scrim there would never catch a click while Mae is in a
        // fullscreen game, and "click outside" would silently stop working.
        // Within one layer, wlr-layer-shell stacks surfaces by creation
        // order: created last, the box ends up on top of its own scrims, so
        // a click actually reaches it instead of being swallowed by a scrim
        // stacked above it.
        let scrims = self.launcher_scrims(qh);
        let surface = self.compositor.create_surface(qh);
        let box_layer = self.layers.create_layer_surface(qh, surface, Layer::Overlay, Some(NAMESPACE), named.as_ref());
        box_layer.set_size(size.0, size.1);
        box_layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        box_layer.commit();
        self.launcher_generation += 1;
        let history_path = history_path_for(&model.config, &paths::state_dir());
        self.launcher = Some(OpenLauncher {
            model,
            reply,
            generation: self.launcher_generation,
            requested_at,
            spawner: run::spawner(&paths::runtime_dir()),
            history_path,
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
            client_watch: None,
        });
        self.keyboard_acquire(qh);
    }

    fn launcher_scrims(&mut self, qh: &QueueHandle<App>) -> Vec<Scrim> {
        if self.viewporter.is_none() {
            log("launcher: no wp_viewporter, a click outside will not close it");
            return Vec::new();
        }
        let outputs: Vec<_> = self.outputs.outputs().collect();
        outputs.into_iter().filter_map(|output| self.make_scrim(qh, output)).collect()
    }

    /// One output's scrim; None without `wp_viewporter` (already logged once
    /// at open by `launcher_scrims` — a hot-plugged output stays silent
    /// about it too, nothing new to say).
    fn make_scrim(&mut self, qh: &QueueHandle<App>, output: wl_output::WlOutput) -> Option<Scrim> {
        let viewporter = self.viewporter.as_ref()?;
        let surface = self.compositor.create_surface(qh);
        let layer = self.layers.create_layer_surface(qh, surface, scrim_layer(), Some(SCRIM_NAMESPACE), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        // 0: placed beside the bar's exclusive zone, never over the bar.
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        let viewport = viewporter.get_viewport(layer.wl_surface(), qh, ());
        layer.commit();
        Some(Scrim { output, layer, viewport, buffer: None })
    }

    /// A new output while the launcher is open: it gets a scrim of its own,
    /// same as every output already had at open — a click outside the box
    /// must close it on a hot-plugged screen too, not only on the ones that
    /// existed when it was opened. Closed: nothing to do, no surface created
    /// for an output the launcher may never even be open to see.
    pub(crate) fn launcher_output_added(&mut self, qh: &QueueHandle<App>, output: wl_output::WlOutput) {
        if !adds_scrim(self.launcher_is_open(), self.viewporter.is_some()) {
            return;
        }
        let Some(scrim) = self.make_scrim(qh, output) else { return };
        if let Some(open) = self.launcher.as_mut() {
            open.scrims.push(scrim);
        }
    }

    /// Drops everything; a waiting dmenu client gets Reply::Cancelled (unless already answered).
    pub fn launcher_close(&mut self, reason: CloseReason) {
        let Some(mut open) = self.launcher.take() else { return };
        reply_on_close(open.reply.take(), reason);
        drop(open);
        self.keyboard_release();
        log(&format!("launcher closed ({})", reason.text()));
        // The catalog, the matcher's matrices, the box's pool: give the
        // freed pages back (50 open/close cycles stay within 1 MB).
        crate::memory::release_memory();
    }

    pub fn launcher_is_open(&self) -> bool {
        self.launcher.is_some()
    }

    /// Re-renders after a model change (key, click, catalog, windows). Never
    /// resizes: the box's size is decided at open (Mae, 2026-10-05 — the
    /// preview never makes it jump); only `launcher_scale` and an imposed
    /// configure size change it.
    pub fn launcher_redraw(&mut self) {
        self.launcher_draw();
    }

    /// After a scale change: the open layout recomputed at the new scale
    /// (same rule, `open_layout`), the box resized if its logical size moved
    /// (set_size + commit, redraw on configure).
    fn launcher_resize(&mut self) {
        let height = self.bar.config.height;
        let App { launcher, fonts, .. } = self;
        let Some(open) = launcher.as_mut() else { return };
        let e = open.scale.max(1);
        let advance = fonts.advance(e);
        let row_height = height * e as u32;
        let frame = fonts.frame(e);
        let (layout, wanted) = resized_layout(open.imposed_size, open.output_size, &open.model, advance, row_height, e, &frame);
        open.model.set_list_rows(layout.list_rows as usize);
        open.layout = layout;
        match plan_redraw(open.size, wanted, open.configured) {
            Redraw::Wait => {}
            Redraw::Resize(w, h) => {
                open.size = (w, h);
                open.box_layer.set_size(w, h);
                open.box_layer.commit();
                open.last = None;
            }
            Redraw::Draw => self.launcher_draw(),
        }
    }

    fn launcher_draw(&mut self) {
        let height = self.bar.config.height;
        let App { launcher, fonts, palette, .. } = self;
        let Some(open) = launcher.as_mut() else { return };
        if !open.configured {
            return;
        }
        let e = open.scale.max(1);
        let row_height = height * e as u32;
        let rows = view::render(&open.model, &open.layout);
        if open.last.as_ref() == Some(&rows) {
            return;
        }
        let (w, h) = (open.size.0 * e as u32, open.size.1 * e as u32);
        let OpenLauncher { buffers, pool, box_layer, .. } = open;
        let Some(used) = pick_buffer(buffers, pool, w, h) else { return };
        let Some(px) = buffers[used].0.canvas(pool) else {
            log(&format!("launcher buffer {w}×{h}: unexpectedly busy"));
            return;
        };
        fonts.paint_boxed(&rows, palette, px, w, h, row_height, e);
        let surface = box_layer.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if buffers[used].0.attach_to(surface).is_err() {
            return;
        }
        box_layer.commit();
        open.last = Some(rows);
        if !open.shown_logged {
            open.shown_logged = true;
            let place = open.output_name.as_deref().unwrap_or("the compositor's choice of output");
            log(&format!("launcher shown on {place} in {} µs", open.requested_at.elapsed().as_micros()));
        }
    }

    /// Applies an Outcome: Activate → run::perform (Err → model.show_message + redraw;
    /// Ok(Done::Reply) → reply Chosen + close; Ok(Done::Close) → close Activated).
    pub fn launcher_outcome(&mut self, o: Outcome) {
        match o {
            Outcome::Nothing => {}
            Outcome::Redraw => self.launcher_redraw(),
            Outcome::Close => self.launcher_close(CloseReason::Escape),
            Outcome::Activate(a) => {
                let App { launcher, exec, .. } = self;
                let Some(open) = launcher.as_mut() else { return };
                // Cloned out: `perform` needs `&mut open.model`, which the
                // single source of truth (`Launcher::path_dirs`, task 13)
                // would otherwise keep borrowed immutably for `ctx`.
                let path_dirs = open.model.path_dirs.clone();
                let ctx = RunContext {
                    exec: exec.as_ref(),
                    spawner: open.spawner,
                    history_path: open.history_path.as_deref(),
                    // The scan's $PATH: a program missing from it is reported in the box (task 10).
                    path_dirs: &path_dirs,
                };
                match run::perform(&mut open.model, a, &ctx) {
                    Err(m) => {
                        // A launch failure shows in the box, which stays open (spec § 3).
                        open.model.show_message(m);
                        self.launcher_redraw();
                    }
                    Ok(Done::Reply(line)) => {
                        if let Some(p) = open.reply.take() {
                            p.reply(&Reply::Chosen(line));
                        }
                        self.launcher_close(CloseReason::Activated);
                    }
                    Ok(Done::Close) => self.launcher_close(CloseReason::Activated),
                }
            }
        }
    }

    /// Binds the keyboard (with repeat on the event loop) while open; a seat
    /// without a keyboard yet is retried when the capability appears.
    pub(crate) fn launcher_key(&mut self, event: KeyEvent) {
        let Some(open) = self.launcher.as_mut() else { return };
        let Some(key) = keys::translate(event.keysym, event.utf8.as_deref(), self.mods) else { return };
        let outcome = open.model.key(key);
        self.launcher_outcome(outcome);
    }

    /// Pointer events on the launcher's surfaces; false: not ours (the bar's).
    pub(crate) fn launcher_pointer(&mut self, conn: &Connection, ev: &PointerEvent) -> bool {
        let height = self.bar.config.height;
        let Some(open) = self.launcher.as_ref() else { return false };
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
            PointerEventKind::Press { .. } if !on_box => self.launcher_close(CloseReason::ClickOutside),
            PointerEventKind::Press { button, .. } => {
                let advance = self.fonts.advance(scale);
                let frame = self.fonts.frame(scale);
                let outcome = match (self.launcher.as_mut(), actions::button(*button)) {
                    (Some(open), Some(b)) => {
                        let cell = hit_cell(ev.position, scale, advance, height * scale as u32, &frame, &open.layout);
                        match (cell.and_then(|(column, row)| view::hit(&open.model, &open.layout, column, row)), b) {
                            (Some(Click::Result(i)), Button::Left) => open.model.click(i, false),
                            (Some(Click::Result(i)), Button::Right) => open.model.click(i, true),
                            (Some(Click::Mode(m)), Button::Left) => open.model.click_mode(m),
                            _ => Outcome::Nothing,
                        }
                    }
                    _ => Outcome::Nothing,
                };
                self.launcher_outcome(outcome);
            }
            PointerEventKind::Axis { vertical, .. } if on_box => {
                if let Some(b) = self.wheel.add(vertical.absolute, vertical.discrete, vertical.value120) {
                    let rows = if b == Button::WheelUp { -1 } else { 1 };
                    let outcome = self.launcher.as_mut().map_or(Outcome::Nothing, |o| o.model.scroll(rows));
                    self.launcher_outcome(outcome);
                }
                if vertical.stop {
                    self.wheel.stop();
                }
            }
            _ => {}
        }
        true
    }

    /// Whether `surface` is the open launcher's box: checked outside any
    /// guard (`owns_surface`), so `scale_factor_changed` (wayland.rs) can
    /// decide which side handles the event before any launcher code,
    /// panicking or not, ever runs.
    pub(crate) fn launcher_owns_box(&self, surface: &wl_surface::WlSurface) -> bool {
        let is_box = self.launcher.as_ref().is_some_and(|o| o.box_layer.wl_surface() == surface);
        owns_surface(self.launcher.is_some(), is_box, false)
    }

    /// Whether `surface` is the open launcher's box OR one of its scrims
    /// (both receive `configure`/`closed`): same reasoning as
    /// `launcher_owns_box`.
    pub(crate) fn launcher_owns_surface(&self, surface: &wl_surface::WlSurface) -> bool {
        let Some(open) = self.launcher.as_ref() else { return false };
        let is_box = open.box_layer.wl_surface() == surface;
        let is_scrim = open.scrims.iter().any(|s| s.layer.wl_surface() == surface);
        owns_surface(true, is_box, is_scrim)
    }

    /// A configure for a launcher surface. The caller (`configure`,
    /// wayland.rs) now checks `launcher_owns_surface` first and only calls
    /// this once it is already known to be ours; the bool return (false:
    /// not ours) is kept as a cheap internal cross-check, no longer
    /// load-bearing for routing.
    pub(crate) fn launcher_configured(&mut self, surface: &wl_surface::WlSurface, size: (u32, u32)) -> bool {
        let height = self.bar.config.height;
        let App { launcher, fonts, .. } = self;
        let Some(open) = launcher.as_mut() else { return false };
        if open.box_layer.wl_surface() == surface {
            if let Some(imposed) = plan_configure(open.size, size) {
                // The compositor chose another size (e.g. the box was sized on
                // the first output and placed on a smaller one): obey it, the
                // layout refitted inside, never painted past it.
                let e = open.scale.max(1);
                let layout = layout_within(imposed, &open.model, fonts.advance(e), height * e as u32, e);
                open.model.set_list_rows(layout.list_rows as usize);
                open.layout = layout;
                open.size = imposed;
                open.imposed_size = Some(imposed);
            }
            open.configured = true;
            open.last = None;
            self.launcher_draw();
            return true;
        }
        let OpenLauncher { scrims, pool, .. } = open;
        let Some(s) = scrims.iter_mut().find(|s| s.layer.wl_surface() == surface) else { return false };
        s.show(pool, size);
        true
    }

    /// The compositor closed a launcher surface. Same note as
    /// `launcher_configured`: the caller (`closed`, wayland.rs) checks
    /// `launcher_owns_surface` first.
    pub(crate) fn launcher_layer_closed(&mut self, surface: &wl_surface::WlSurface) -> bool {
        let Some(open) = self.launcher.as_mut() else { return false };
        if open.box_layer.wl_surface() == surface {
            self.launcher_close(CloseReason::OutputGone);
            return true;
        }
        let before = open.scrims.len();
        open.scrims.retain(|s| s.layer.wl_surface() != surface);
        open.scrims.len() != before
    }

    /// An output went away: the box's → close; another one → its scrim only.
    pub(crate) fn launcher_output_gone(&mut self, output: &wl_output::WlOutput) {
        let Some(open) = self.launcher.as_mut() else { return };
        if open.output.as_ref() == Some(output) {
            self.launcher_close(CloseReason::OutputGone);
        } else {
            open.scrims.retain(|s| s.output != *output);
        }
    }

    /// Scale change of the box: repaint (and resize) at the new scale. Same
    /// note as `launcher_configured`: the caller (`scale_factor_changed`,
    /// wayland.rs) checks `launcher_owns_box` first.
    pub(crate) fn launcher_scale(&mut self, surface: &wl_surface::WlSurface, factor: i32) -> bool {
        let Some(open) = self.launcher.as_mut() else { return false };
        if open.box_layer.wl_surface() != surface {
            return false;
        }
        if open.scale != factor {
            open.scale = factor;
            open.last = None;
            self.launcher_resize();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::config::LauncherConfig;
    use alpheus_commun::launcher_ipc::{Reply, decode_reply};
    use std::ffi::OsString;
    use std::io::Read;

    #[test]
    fn output_cells_counts_whole_cells_at_any_scale() {
        assert_eq!(output_cells(1920, 1080, 8.25, 22, 1), (232, 49));
        assert_eq!(output_cells(1920, 1080, 16.5, 44, 2), (232, 49), "same logical size, scale 2");
        assert_eq!(output_cells(1920, 1080, 8.25, 0, 1).1, 0, "no row height: no rows, no division by zero");
    }

    #[test]
    fn redraw_resizes_on_a_new_size_and_waits_for_the_first_configure() {
        assert_eq!(plan_redraw((495, 330), (495, 330), true), Redraw::Draw);
        assert_eq!(plan_redraw((495, 330), (495, 330), false), Redraw::Wait, "never draw before the first configure");
        assert_eq!(plan_redraw((495, 330), (496, 330), true), Redraw::Resize(496, 330), "a scale change rounds the box anew");
        assert_eq!(plan_redraw((495, 330), (495, 154), false), Redraw::Resize(495, 154), "a size change is sent even before configure");
    }

    fn apps() -> Launcher {
        Launcher::for_apps(LauncherConfig::default(), Vec::new(), crate::launcher::history::History::empty(200), 0)
    }

    fn script() -> Launcher {
        Launcher::for_script(LauncherConfig::default(), None, vec!["a".into(), "b".into()])
    }

    /// Mae, 2026-10-05: the box's width is decided at open and never jumps.
    /// Nothing is selected yet here (no catalog): the panel is reserved anyway.
    #[test]
    fn the_layout_is_decided_at_open_whatever_the_selection() {
        let wide = open_layout(200, 60, &apps());
        assert_eq!(
            (wide.columns, wide.preview_columns),
            (view::BOX_COLUMNS + view::PREVIEW_COLUMNS + 1, view::PREVIEW_COLUMNS),
            "the app launcher reserves its preview from the first frame"
        );
        assert_eq!(open_layout(200, 60, &script()).preview_columns, 0, "a script menu has nothing to preview");
        assert_eq!(open_layout(98, 60, &apps()).preview_columns, 0, "an output too narrow: no panel, and still a fixed width");
    }

    #[test]
    fn a_configure_with_another_size_is_obeyed() {
        assert_eq!(plan_configure((495, 330), (495, 330)), None, "the size asked: nothing to change");
        assert_eq!(plan_configure((495, 330), (0, 0)), None, "0 × 0: the client's choice, i.e. what it asked");
        assert_eq!(plan_configure((800, 330), (640, 330)), Some((640, 330)), "a smaller output than the one the box was sized for");
        assert_eq!(plan_configure((495, 330), (640, 0)), Some((640, 330)), "a 0 dimension keeps what was asked");
    }

    /// A frame as the workstation's font measures it at scale 1 (8.25 px cells, 22 px rows).
    fn frame1() -> Frame {
        Frame { side: 4, top: 7, bottom: 6, stroke: 1, radius: 6 }
    }

    #[test]
    fn a_layout_refitted_to_a_configured_size_fills_it_and_never_exceeds_it() {
        // 495 × 330 logical px at 8.25 px × 22 px cells: exactly 60 × 15 cells, ring included.
        let b = layout_within((495, 330), &apps(), 8.25, 22, 1);
        assert_eq!((b.columns, b.rows), (60, 15), "the whole configured size is used");
        let (w, h) = layout_size(&b, 8.25, 22, 1, &frame1());
        assert_eq!((w, h), (479 + 8, 13 * 22 + 13), "58 × 13 cells and the pixel frame instead of the ring");
        assert!(w <= 495 && h <= 330, "the frame never takes more room than the ring did: {w} × {h}");
        let b = layout_within((400, 200), &apps(), 8.25, 22, 1);
        let (w, h) = layout_size(&b, 8.25, 22, 1, &frame1());
        assert!(w <= 400 && h <= 200, "never painted past the configured size: {w} × {h}");
        assert!(b.list_rows >= 1);
    }

    #[test]
    fn a_press_maps_to_the_layouts_cells_and_the_margin_hits_none() {
        let b = layout_within((495, 330), &apps(), 8.25, 22, 1);
        let f = frame1();
        // The first pixel of the first cell is the layout's cell (1, 1): the ring is the frame.
        assert_eq!(hit_cell((4.0, 7.0), 1, 8.25, 22, &f, &b), Some((1, 1)));
        // The third result row, a few columns in.
        assert_eq!(hit_cell((4.0 + 8.25 * 5.5, 7.0 + 22.0 * 3.5), 1, 8.25, 22, &f, &b), Some((6, 4)));
        // The margin: left, top, and the frame's other two sides.
        assert_eq!(hit_cell((3.0, 40.0), 1, 8.25, 22, &f, &b), None);
        assert_eq!(hit_cell((40.0, 6.0), 1, 8.25, 22, &f, &b), None);
        let (w, h) = layout_size(&b, 8.25, 22, 1, &f);
        assert_eq!(hit_cell((f64::from(w) - 1.0, 40.0), 1, 8.25, 22, &f, &b), None, "right margin");
        assert_eq!(hit_cell((40.0, f64::from(h) - 1.0), 1, 8.25, 22, &f, &b), None, "bottom margin");
        // And a press there is no click: `view::hit` never sees it.
        let mut l = apps();
        l.set_list_rows(usize::from(b.list_rows));
        assert_eq!(hit_cell((0.0, 0.0), 1, 8.25, 22, &f, &b).and_then(|(c, r)| view::hit(&l, &b, c, r)), None);
    }

    /// Review item M1, final review slice 3: `release_memory` must run only
    /// after `model` is already gone, not before — otherwise malloc_trim
    /// sees nothing new to give back.
    #[test]
    fn pool_failure_order_replies_then_drops_the_model_then_releases_memory() {
        let order: std::cell::RefCell<Vec<&str>> = std::cell::RefCell::new(Vec::new());
        pool_failure_order(|| order.borrow_mut().push("reply"), || order.borrow_mut().push("drop"), || order.borrow_mut().push("release"));
        assert_eq!(
            order.into_inner(),
            ["reply", "drop", "release"],
            "the model must be gone before release_memory runs, so it actually frees pages"
        );
    }

    /// Review item M2, final review slice 3: the scrims must share the
    /// box's layer (Overlay), not Top — Hyprland draws Top beneath a
    /// fullscreen window, which would let a click through to the game
    /// instead of closing the launcher.
    #[test]
    fn scrims_share_the_box_s_overlay_layer_not_top() {
        assert_eq!(scrim_layer(), Layer::Overlay);
    }

    /// Review item M5, final review slice 3: a hot-plugged output only gets
    /// a scrim while the launcher is actually open, and only with
    /// `wp_viewporter` available.
    #[test]
    fn adds_scrim_needs_both_an_open_launcher_and_wp_viewporter() {
        assert!(adds_scrim(true, true));
        assert!(!adds_scrim(false, true), "closed: nothing to catch a click outside of");
        assert!(!adds_scrim(true, false), "no wp_viewporter: make_scrim already returns None either way");
        assert!(!adds_scrim(false, false));
    }

    #[test]
    fn pool_size_carries_two_frames_and_a_scrim_share_per_output() {
        assert_eq!(pool_size(1000, 0), 2000, "no scrim at all: just the two box buffers");
        assert_eq!(pool_size(1000, 3), 2000 + 192, "one 64-byte share per output's scrim");
    }

    #[test]
    fn a_scale_change_keeps_a_compositor_imposed_size_untouched() {
        // Without an imposed size: the whole output, same as before this was tracked.
        let (free, free_size) = resized_layout(None, (1920, 1080), &apps(), 8.25, 22, 1, &frame1());
        assert_eq!(free_size, layout_size(&free, 8.25, 22, 1, &frame1()), "no constraint: sized from the output alone");
        // With one: the imposed logical size wins outright, whatever the output is —
        // a scale change must not quietly widen the box back out past what the
        // compositor already dictated.
        let (within, wanted) = resized_layout(Some((400, 200)), (1920, 1080), &apps(), 8.25, 22, 1, &frame1());
        assert_eq!(wanted, (400, 200), "the compositor's own size, unaffected by the output");
        assert_eq!(within, layout_within((400, 200), &apps(), 8.25, 22, 1), "the same refit a configure already uses");
    }

    #[test]
    fn keyboard_errors_are_logged_except_an_unsupported_capability() {
        assert!(
            !should_log_keyboard_error(&KeyboardError::Seat(SeatError::UnsupportedCapability(
                smithay_client_toolkit::seat::Capability::Keyboard
            ))),
            "no keyboard yet: launcher_bind_keyboard retries once the capability appears, nothing to log every time"
        );
        assert!(should_log_keyboard_error(&KeyboardError::InvalidKeymap));
        assert!(should_log_keyboard_error(&KeyboardError::Seat(SeatError::DeadObject)));
    }

    #[test]
    fn close_reasons_have_log_texts() {
        use CloseReason::*;
        let all = [Escape, Toggle, Close, FocusLost, ClickOutside, Activated, OutputGone, Replaced, Error];
        assert_eq!(
            all.map(CloseReason::text),
            ["escape", "toggle", "close", "focus lost", "click outside", "activated", "output gone", "replaced", "error"]
        );
    }

    #[test]
    fn closing_reasons_reply_cancelled_to_a_waiting_client() {
        use CloseReason::*;
        for reason in [Escape, Toggle, Close, FocusLost, ClickOutside, Activated, OutputGone, Replaced] {
            let (pending, mut client) = Pending::pair();
            reply_on_close(Some(pending), reason);
            let mut bytes = Vec::new();
            client.read_to_end(&mut bytes).unwrap();
            assert_eq!(decode_reply(&bytes).unwrap(), Reply::Cancelled, "{reason:?}: the script sees Esc (exit 1)");
        }
        let (pending, mut client) = Pending::pair();
        reply_on_close(Some(pending), Error);
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).unwrap();
        assert!(matches!(decode_reply(&bytes).unwrap(), Reply::Error(_)), "a launcher error is not a cancel");
        reply_on_close(None, Escape); // nobody waiting: nothing to do, no panic
    }

    #[test]
    fn the_history_path_is_none_when_the_history_is_off() {
        let state = Path::new("/state/alpheus");
        let on = LauncherConfig::default();
        assert_eq!(history_path_for(&on, state), Some(crate::launcher::history::history_path(state)));
        let off = LauncherConfig { history: 0, ..LauncherConfig::default() };
        assert_eq!(history_path_for(&off, state), None, "history = 0: never read, never written");
    }

    /// `Launcher::path_dirs`, GONE deviation from the brief (`preview_visible` and the toggle it
    /// drove are gone): the model's doc already says task 12 sets `path_dirs` from the same
    /// `ScanEnv` the scan uses, so the command preview shows the full path from the very first
    /// frame, before the scan itself has even delivered a catalog. The directory walk stays
    /// async (`scan::scan`, a short-lived thread, task 13): only `ScanEnv::from_env` — reading
    /// $PATH, no filesystem walk — runs synchronously at open.
    #[test]
    fn open_path_dirs_uses_the_scan_s_path_for_the_app_launcher_only() {
        let var = |k: &str| (k == "PATH").then(|| OsString::from("/a:/b"));
        assert_eq!(
            open_path_dirs(false, var),
            [PathBuf::from("/a"), PathBuf::from("/b")],
            "the app launcher: $PATH, split and kept in order"
        );
        assert_eq!(open_path_dirs(true, var), Vec::<PathBuf>::new(), "a script menu runs nothing: no $PATH needed");
    }

    /// Task 13 fix round, review item 2: ownership of a surface event is
    /// decided from `open`/`is_box`/`is_scrim` alone — computed outside any
    /// guard — never from whether launcher code then panics.
    #[test]
    fn owns_surface_requires_an_open_launcher_and_a_matching_identity() {
        assert!(!owns_surface(false, true, true), "closed: never ours, whatever the identity checks say");
        assert!(owns_surface(true, true, false), "the box");
        assert!(owns_surface(true, false, true), "a scrim");
        assert!(!owns_surface(true, false, false), "open, but neither surface matches: the bar's own");
    }
}
