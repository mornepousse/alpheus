//! The launcher's Wayland side, kept thin. One centred `Overlay` layer
//! surface with exclusive keyboard focus (the box), one transparent 1×1
//! "scrim" per output scaled by wp_viewporter to catch a click outside, a
//! wl_keyboard bound only while open, buffers from a pool of its own.
//! Everything lives in `OpenLauncher`, dropped whole on close: nothing of
//! the launcher stays resident (spec § 3).

use crate::actions::{self, Button};
use crate::grid::Grid;
use crate::launcher::config::LauncherConfig;
use crate::launcher::history;
use crate::launcher::input::{self, Mods};
use crate::launcher::model::{Launcher, Outcome};
use crate::launcher::run::{self, Done, RunContext, Spawner};
use crate::launcher::scan::ScanEnv;
use crate::launcher::socket::Pending;
use crate::launcher::view::{self, BoxLayout, Click};
use crate::log::write as log;
use crate::painter;
use crate::state::Feed;
use crate::wayland::{App, pick_buffer};
use alpheus_commun::launcher_ipc::Reply;
use alpheus_commun::paths;
use smithay_client_toolkit::reexports::calloop::{LoopHandle, RegistrationToken};
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers};
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

/// Logical px size of the box for a layout: the width rounded UP (never cut
/// the last column), whole rows of `row_height` physical px.
pub fn box_size(b: &BoxLayout, advance_phys: f32, row_height: u32, scale: i32) -> (u32, u32) {
    let e = scale.max(1) as u32;
    let width_phys = (b.columns as f32 * advance_phys).ceil() as u32;
    (width_phys.div_ceil(e), (b.rows as u32 * row_height).div_ceil(e))
}

/// Output grid in cells (for view::layout) from its logical size.
pub fn output_cells(width: u32, height: u32, advance_phys: f32, row_height: u32, scale: i32) -> (u16, u16) {
    let e = scale.max(1) as u32;
    let columns = painter::columns(width * e, advance_phys);
    let rows = (height * e).checked_div(row_height).map_or(0, |r| r.min(u16::MAX as u32) as u16);
    (columns, rows)
}

/// Pointer position (logical px) → (column, row).
pub fn cell_under(x: f64, y: f64, scale: i32, advance_phys: f32, row_height: u32) -> (u16, u16) {
    let column = painter::column_under(x, scale, advance_phys);
    let row =
        if row_height == 0 || y <= 0.0 { 0 } else { ((y * scale.max(1) as f64) / row_height as f64).floor().min(u16::MAX as f64) as u16 };
    (column, row)
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

fn release(k: wl_keyboard::WlKeyboard) {
    if k.version() >= 3 {
        k.release();
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
    keyboard: Option<wl_keyboard::WlKeyboard>,
    mods: Mods,
    /// The box's own pool: dropped with it (a SlotPool never shrinks).
    pool: SlotPool,
    buffers: Vec<(Buffer, u32, u32)>,
    /// Decided at open (`open_layout`); changed only by a scale change or an
    /// imposed configure size, never by the selection.
    layout: BoxLayout,
    /// Logical size of the box (asked, or imposed by a configure).
    size: (u32, u32),
    /// Some: the focused output, found by name; None: the compositor's choice.
    output: Option<wl_output::WlOutput>,
    output_name: Option<String>,
    /// Logical size of the output the layout is computed for.
    output_size: (u32, u32),
    scale: i32,
    configured: bool,
    shown_logged: bool,
    /// Rows last committed: an unchanged frame is not repainted.
    last: Option<Vec<Grid>>,
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
        // The keyboard goes first: its data (keymap, repeat timer) is dropped
        // with the object. Then the client watch. Scrims, box and pool follow
        // as fields (viewports before their surfaces, see Scrim's Drop).
        if let Some(k) = self.keyboard.take() {
            release(k);
        }
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
        let size = box_size(&layout, advance, row_height, scale);
        let bytes = (size.0 * scale as u32) as usize * (size.1 * scale as u32) as usize * 4;
        let pool = match SlotPool::new(bytes.max(4096), &self.shm) {
            Ok(p) => p,
            Err(e) => {
                log(&format!("launcher: no shared buffer ({e}), not opened"));
                if let Some(p) = reply {
                    p.reply(&Reply::Error(format!("the launcher could not open: {e}")));
                }
                return;
            }
        };
        let surface = self.compositor.create_surface(qh);
        let box_layer = self.layers.create_layer_surface(qh, surface, Layer::Overlay, Some(NAMESPACE), named.as_ref());
        box_layer.set_size(size.0, size.1);
        box_layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        box_layer.commit();
        let scrims = self.launcher_scrims(qh);
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
            keyboard: None,
            mods: Mods::default(),
            pool,
            buffers: Vec::new(),
            layout,
            size,
            output: named,
            output_name,
            output_size,
            scale,
            configured: false,
            shown_logged: false,
            last: None,
            client_watch: None,
        });
        if let Some(seat) = self.seat.clone() {
            self.launcher_bind_keyboard(qh, &seat);
        }
    }

    fn launcher_scrims(&mut self, qh: &QueueHandle<App>) -> Vec<Scrim> {
        let Some(viewporter) = self.viewporter.as_ref() else {
            log("launcher: no wp_viewporter, a click outside will not close it");
            return Vec::new();
        };
        let mut scrims = Vec::new();
        for output in self.outputs.outputs() {
            let surface = self.compositor.create_surface(qh);
            let layer = self.layers.create_layer_surface(qh, surface, Layer::Top, Some(SCRIM_NAMESPACE), Some(&output));
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            // 0: placed beside the bar's exclusive zone, never over the bar.
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_size(0, 0);
            let viewport = viewporter.get_viewport(layer.wl_surface(), qh, ());
            layer.commit();
            scrims.push(Scrim { output, layer, viewport, buffer: None });
        }
        scrims
    }

    /// Drops everything; a waiting dmenu client gets Reply::Cancelled (unless already answered).
    pub fn launcher_close(&mut self, reason: CloseReason) {
        let Some(mut open) = self.launcher.take() else { return };
        reply_on_close(open.reply.take(), reason);
        drop(open);
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
        let (columns, rows) = output_cells(open.output_size.0, open.output_size.1, advance, row_height, e);
        let layout = open_layout(columns, rows, &open.model);
        open.model.set_list_rows(layout.list_rows as usize);
        open.layout = layout;
        match plan_redraw(open.size, box_size(&layout, advance, row_height, e), open.configured) {
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
        fonts.paint_rows(&rows, palette, px, w, row_height, e);
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
    pub(crate) fn launcher_bind_keyboard(&mut self, qh: &QueueHandle<App>, seat: &wl_seat::WlSeat) {
        let Some(handle) = self.loop_handle.clone() else { return };
        let Some(open) = self.launcher.as_mut() else { return };
        if open.keyboard.is_some() {
            return;
        }
        let repeat = Box::new(|app: &mut App, _: &wl_keyboard::WlKeyboard, event: KeyEvent| {
            app.launcher_guarded("key", move |a| a.launcher_key(event));
        });
        if let Ok(k) = self.seats.get_keyboard_with_repeat(qh, seat, None, handle, repeat) {
            open.keyboard = Some(k);
        }
    }

    pub(crate) fn launcher_release_keyboard(&mut self) {
        if let Some(k) = self.launcher.as_mut().and_then(|o| o.keyboard.take()) {
            release(k);
        }
    }

    pub(crate) fn launcher_key(&mut self, event: KeyEvent) {
        let Some(open) = self.launcher.as_mut() else { return };
        let Some(key) = input::translate(event.keysym, event.utf8.as_deref(), open.mods) else { return };
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
                let (column, row) = cell_under(ev.position.0, ev.position.1, scale, advance, height * scale as u32);
                let outcome = match (self.launcher.as_mut(), actions::button(*button)) {
                    (Some(open), Some(b)) => match (view::hit(&open.model, &open.layout, column, row), b) {
                        (Some(Click::Result(i)), Button::Left) => open.model.click(i, false),
                        (Some(Click::Result(i)), Button::Right) => open.model.click(i, true),
                        (Some(Click::Mode(m)), Button::Left) => open.model.click_mode(m),
                        _ => Outcome::Nothing,
                    },
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
        if self.launcher.as_ref().is_some_and(|o| o.box_layer.wl_surface() == surface) {
            self.launcher_guarded("focus", |app| app.launcher_close(CloseReason::FocusLost));
        }
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.launcher_guarded("key", move |app| app.launcher_key(event));
    }

    /// wl_keyboard v10 repeats (compositor-side); calloop repeats go through
    /// the callback given at bind time, to the same place.
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.launcher_guarded("key", move |app| app.launcher_key(event));
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
        if let Some(open) = self.launcher.as_mut() {
            open.mods = Mods { ctrl: m.ctrl, shift: m.shift, alt: m.alt, logo: m.logo };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::config::LauncherConfig;
    use alpheus_commun::launcher_ipc::{Reply, decode_reply};
    use std::ffi::OsString;
    use std::io::Read;

    fn layout(columns: u16, rows: u16) -> BoxLayout {
        BoxLayout { columns, rows, list_top: 3, list_rows: rows.saturating_sub(5), preview_columns: 0 }
    }

    #[test]
    fn box_size_rounds_the_width_up_and_keeps_whole_rows() {
        // Advances chosen exactly representable in binary: the assertions
        // test the rounding rule, not float noise.
        assert_eq!(box_size(&layout(60, 15), 8.25, 22, 1), (495, 330));
        assert_eq!(box_size(&layout(61, 15), 8.125, 22, 1), (496, 330), "495.625 px: the last column is never cut");
        assert_eq!(box_size(&layout(60, 15), 16.5, 44, 2), (495, 330), "logical px at scale 2");
        assert_eq!(box_size(&layout(60, 15), 8.25, 22, 0), (495, 330), "scale 0 treated as 1");
    }

    #[test]
    fn output_cells_counts_whole_cells_at_any_scale() {
        assert_eq!(output_cells(1920, 1080, 8.25, 22, 1), (232, 49));
        assert_eq!(output_cells(1920, 1080, 16.5, 44, 2), (232, 49), "same logical size, scale 2");
        assert_eq!(output_cells(1920, 1080, 8.25, 0, 1).1, 0, "no row height: no rows, no division by zero");
    }

    #[test]
    fn cell_under_maps_logical_pointer_positions() {
        assert_eq!(cell_under(100.0, 45.0, 1, 8.25, 22), (12, 2));
        assert_eq!(cell_under(100.0, 45.0, 2, 16.5, 44), (12, 2), "scale 2: physical cells, logical pointer");
        assert_eq!(cell_under(-3.0, -1.0, 1, 8.25, 22), (0, 0), "a pointer at the very edge");
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

    #[test]
    fn a_layout_refitted_to_a_configured_size_fills_it_and_never_exceeds_it() {
        // 495 × 330 logical px at 8.25 px × 22 px cells: exactly 60 × 15 cells.
        let b = layout_within((495, 330), &apps(), 8.25, 22, 1);
        assert_eq!((b.columns, b.rows), (60, 15), "the whole configured size is used");
        assert_eq!(box_size(&b, 8.25, 22, 1), (495, 330));
        let b = layout_within((400, 200), &apps(), 8.25, 22, 1);
        let (w, h) = box_size(&b, 8.25, 22, 1);
        assert!(w <= 400 && h <= 200, "never painted past the configured size: {w} × {h}");
        assert!(b.list_rows >= 1);
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
