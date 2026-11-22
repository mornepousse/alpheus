//! Assembling the daemon: signals, lock, settings, fonts, Wayland, the event
//! loop (calloop), the sources the modules need (compositor, sound, D-Bus,
//! resources timer). Covered by tests/headless_session.rs (headless sway)
//! and the real-world trial; its pure decisions have their own tests.

use crate::clock;
use crate::clock::LocalTime;
use crate::config::{BarConfig, ModuleId, ResourcesConfig};
use crate::launcher::scan::Catalog;
use crate::launcher::socket::Server;
use crate::launcher::socket_glue::{launcher_socket_outcome, watch_launcher_socket};
use crate::launcher::sources::Entries;
use crate::launcher::surface::CloseReason;
use crate::lock;
use crate::modules::Needs;
use crate::nonblocking::NonBlocking;
use crate::painter::Fonts;
use crate::pipewire::Scope;
use crate::session_bus::bus::SessionBus;
use crate::session_bus::router::{Tray, TrayOutput};
use crate::sources;
use crate::sources::dbus::Domain;
use crate::sources::resources;
use crate::state::{Bar, Msg};
use crate::wayland::App;
use alpheus_commun::exec::{Executor, SystemExecutor};
use alpheus_commun::launcher_ipc;
use alpheus_commun::theme::settings::{ModeSetting, ThemeSettings, ThemeSource, effective_mode, resolve_dir};
// `Scheme`: calloop's `Mode` is already in scope.
use alpheus_commun::theme::{Mode as Scheme, Roles};
use alpheus_commun::{paths, shutdown};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);
static FINISHED: AtomicBool = AtomicBool::new(false);
/// Grace period left for the loop to exit on its own after a signal.
const GRACE: Duration = Duration::from_secs(1);

/// Mail received by the loop from the other threads. One is moved once per event: the bar's
/// messages (the sound state with the open panel's headset among them) outweigh the rest.
#[allow(clippy::large_enum_variant)]
pub enum Mail {
    Bar(Msg),
    /// A finished scan: adopted by the open launcher if `generation` is its
    /// own, OR if it has no catalog of its own yet — a reopen racing a
    /// still-running scan (`scan::catalog_is_current`); otherwise a stale
    /// scan from a launcher already closed or already fed, ignored (review
    /// This used to say "ignored unless
    /// generation is the open launcher's", true only for the common case).
    Catalog {
        generation: u64,
        catalog: Catalog,
    },
    /// A finished source run (sources spec § 6): the same open's second delivery, adopted under
    /// the catalog's rule on an app launcher only (`host::entries_are_current`).
    Entries {
        generation: u64,
        entries: Entries,
    },
    Stop,
}

/// bar.toml, read at startup, and its warnings to log. `[quick] wifi_more`'s, `vpn_more`'s and `mixer`'s defaults come from
/// launcher.toml's `terminal`, read once here (its warnings are the launcher's, logged at its own open).
pub fn settings(directory: &Path) -> (BarConfig, Vec<String>) {
    let (mut config, warning) = BarConfig::load(&directory.join("bar.toml"));
    let (launcher, _) = crate::launcher::config::LauncherConfig::load(&crate::launcher::config::config_path(directory));
    crate::config::resolve_wifi_more(&mut config.quick, &launcher.terminal);
    crate::config::resolve_vpn_more(&mut config.quick, &launcher.terminal);
    crate::config::resolve_mixer(&mut config.quick, &launcher.terminal);
    (config, warning.into_iter().collect())
}

/// The theme (themes spec § 2): theme.toml, the mode in effect (`portal`: the portal's word in mode
/// auto, None before it answers or without one) and the colour files, through the shared resolver.
/// Startup and every reload (`App::theme_changed`) go through `resolve_dir`. Warnings: one line each.
pub fn theme(directory: &Path, portal: Option<Scheme>) -> (ThemeSettings, Roles, Vec<String>) {
    let (settings, resolved) = resolve_dir(directory, || portal);
    let lines = resolved.lines();
    (settings, resolved.roles, lines)
}

/// "catppuccin, dark", "wallpaper, light (auto)": the theme in effect, for the log.
pub fn describe(settings: &ThemeSettings, mode: Scheme) -> String {
    let source = match &settings.source {
        ThemeSource::Preset(name) => *name,
        ThemeSource::Wallpaper => "wallpaper",
    };
    let mode = match mode {
        Scheme::Dark => "dark",
        Scheme::Light => "light",
    };
    let auto = if settings.mode == ModeSetting::Auto { " (auto)" } else { "" };
    format!("{source}, {mode}{auto}")
}

/// Applies a message to the bar. Any compositor event, or a resources tick,
/// also refreshes the clock: coming out of sleep, the timer (a monotonic
/// clock, stopped during sleep) can lag by a minute; the first focus change
/// puts the clock right again — and so does the next resources reading,
/// which needs no user interaction with the compositor at all (B-m4,
/// The clock could otherwise stay stale for up to a minute after
/// resume if nothing compositor-related happens meanwhile). Cheapest
/// correct option: resources already ticks on its own already-scheduled
/// timer whenever the module is shown, so this reuses an existing wakeup
/// instead of adding one (e.g. threading a dedicated wake flag through the
/// D-Bus session/publish plumbing, `PrepareForSleep(false)`, into every
/// call site and test there).
pub fn dispatch_msg(bar: &mut Bar, m: Msg, now: impl Fn() -> LocalTime) -> bool {
    let refreshes_clock = matches!(m, Msg::Compositor(_) | Msg::Resources(_));
    let mut changed = bar.apply(m);
    if refreshes_clock {
        changed |= bar.apply(Msg::Clock(now()));
    }
    changed
}

/// What the loop needs to redraw at most once per pass through
/// `loop.dispatch()`, even if a burst of events (e.g. niri, which sends
/// several lines at once) triggers several callbacks there (channel, clock):
/// each one calls `on_message`, which updates the bar and raises the
/// `dirty` flag WITHOUT ever drawing; `after_dispatch`, called once after
/// `dispatch()` returns, draws if needed and lowers the flag. `App`
/// (wayland.rs) implements it for real; the tests implement it on a plain
/// counter, with no Wayland connection.
pub trait DeferredRedraw {
    fn bar_mut(&mut self) -> &mut Bar;
    fn dirty_mut(&mut self) -> &mut bool;
    fn redraw_now(&mut self);
    /// The OSD's pill drawn if its row changed during the pass (`osd_sound` only updates the
    /// model and the row): a burst of sound messages paints it once.
    fn flush_osd(&mut self);
}

/// Called by every callback (channel message, clock tick): never a redraw
/// here, only the flag.
pub fn on_message(target: &mut impl DeferredRedraw, m: Msg, now: impl Fn() -> LocalTime) {
    if dispatch_msg(target.bar_mut(), m, now) {
        *target.dirty_mut() = true;
    }
}

/// Called exactly once after `loop.dispatch()` returns: redraws if the flag
/// was raised by the burst just processed, and clears it in every case; then
/// the OSD's pill, if its row changed.
pub fn after_dispatch(target: &mut impl DeferredRedraw) {
    if std::mem::take(target.dirty_mut()) {
        target.redraw_now();
    }
    target.flush_osd();
}

/// Mirrors `DeferredRedraw` for the launcher's windows:
/// `Launcher::set_windows` rebuilds the ranker's per-window facts
/// and refilters, so it is not free — a burst of several `Msg::Compositor`
/// messages in the same `dispatch()` pass (niri can send several at once)
/// must feed an open launcher at most once, not once per message.
pub trait WindowsFeed {
    fn windows_dirty_mut(&mut self) -> &mut bool;
    fn feed_windows_now(&mut self);
}

/// Called by the mail callback on every `Msg::Compositor`: only raises the
/// flag, never feeds right away.
pub fn note_compositor_message(target: &mut impl WindowsFeed, m: &Msg) {
    if matches!(m, Msg::Compositor(_)) {
        *target.windows_dirty_mut() = true;
    }
}

/// Called once after `loop.dispatch()` returns (alongside `after_dispatch`):
/// feeds the launcher's windows if the flag was raised by the batch just
/// processed, and lowers it in every case.
pub fn after_dispatch_windows(target: &mut impl WindowsFeed) {
    if std::mem::take(target.windows_dirty_mut()) {
        target.feed_windows_now();
    }
}

use crate::log::write as log;

/// The sources this bar.toml needs: a module that isn't shown costs nothing
/// (no thread, no child process, no timer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The pw-dump child: for the volume module, the OSD (its passive trigger) or the quick panel's rows.
    pub sound: bool,
    /// The D-Bus domains to read and follow (the quick panel's Wi-Fi row and power profile among
    /// them); empty: no D-Bus thread.
    pub dbus: Vec<Domain>,
    pub resources: bool,
    /// The tray's session-bus connection; false: no connection, no fd, no timer.
    pub tray: bool,
    /// The media module's MPRIS players: on the tray's connection, or on the bar's
    /// link when the tray is off; false: no connection, no rule, no call.
    pub media: bool,
    /// The notifications server and the bell: on the tray's connection, or
    /// on the bar's link when the tray is off; false: no RequestName, no surface, no call.
    pub notifications: bool,
    /// The user manager's failed-units count (the `failed` module): on the tray's connection, or on
    /// the bar's link when the tray is off; false: no connection, no rule, no call.
    pub failed: bool,
}

pub fn wanted(modules: &[ModuleId]) -> Wanted {
    wanted_for(Needs::of(modules))
}

/// What `needs` ask for together (`wanted`; tests give theirs directly).
pub(crate) fn wanted_for(needs: Needs) -> Wanted {
    // In this order, whichever modules ask: with the network module and the panel both, the
    // network domain is read once.
    let dbus = [
        (Needs::NETWORK, Domain::Network),
        (Needs::BLUETOOTH, Domain::Bluetooth),
        (Needs::BATTERY, Domain::Battery),
        (Needs::POWER_PROFILE, Domain::Power),
        (Needs::UNITS, Domain::Units),
    ]
    .into_iter()
    .filter(|(n, _)| needs.contains(*n))
    .map(|(_, d)| d)
    .collect();
    Wanted {
        sound: needs.contains(Needs::SOUND),
        dbus,
        resources: needs.contains(Needs::RESOURCES),
        tray: needs.contains(Needs::TRAY),
        media: needs.contains(Needs::MEDIA),
        notifications: needs.contains(Needs::NOTIFICATIONS),
        failed: needs.contains(Needs::UNITS),
    }
}

/// Whether the camera devices are watched: only with the `privacy` module (no inotify instance,
/// no timer and no scan otherwise).
pub fn camera_wanted(modules: &[ModuleId]) -> bool {
    Needs::of(modules).contains(Needs::CAMERA)
}

/// What the sound source publishes for: without the `osd` module, the bar's sink volume alone (a
/// microphone or a label change wakes nothing).
pub fn sound_scope(modules: &[ModuleId]) -> Scope {
    if Needs::of(modules).contains(Needs::OSD_SCOPE) { Scope::Osd } else { Scope::Bar }
}

pub fn resources_period(c: &ResourcesConfig) -> Duration {
    Duration::from_secs(c.interval)
}

impl App {
    /// One message from a source thread, in the order the readers need: the quick panel takes a
    /// sound message's sink list first, the OSD reads every sound message before the
    /// bar keeps it (its trigger rules compare it with the last one), the privacy
    /// indicators read sound and compositor messages first (privacy spec § 4); then the bar's own
    /// handling, and the panel refreshes if it took something.
    pub(crate) fn on_bar_message(&mut self, mut m: Msg) {
        let quick = self.quick_take(&mut m);
        if let Msg::Sound(s) = &m {
            self.osd_sound(s.as_ref());
        }
        // Hooks first: a sound or compositor source going down must be known when the privacy
        // indicators it clears reach `hooks_capture` (hooks spec § 2).
        self.hooks_msg(&m);
        self.privacy_feed(&m);
        self.battery_feed(&m);
        note_compositor_message(self, &m);
        // The system count was read: the user manager's again (its own link, `failed_app`).
        if matches!(m, Msg::UnitsRead) {
            self.failed_units_read();
        }
        let units = matches!(m, Msg::Units(_));
        on_message(self, m, || LocalTime::now().0);
        if units {
            self.failed_notify();
        }
        if quick {
            self.quick_refresh();
        }
    }
}

/// `SessionBus::session()` itself found nothing to connect to (spec § 3): the
/// one case that is really "no session bus".
fn no_session_bus(e: impl std::fmt::Display) -> String {
    format!("tray: no session bus ({e})")
}

/// The connection succeeded, but duplicating its fd for the event loop
/// failed: a different step than `no_session_bus` (2026-10-06 —
/// the two used to share the same wording, hiding which one actually
/// failed).
fn watch_fd_failed(e: impl std::fmt::Display) -> String {
    format!("tray: connected, but could not watch the session bus's fd ({e})")
}

/// Connects the tray to the session bus (blocking, once: a local socket)
/// and puts its fd in the loop — no thread. No session
/// bus: one log line, the tray stays empty, never retried (spec § 3).
fn start_tray(state: &mut App, handle: &LoopHandle<'static, App>) {
    let bus = match SessionBus::session() {
        Ok(b) => b,
        Err(e) => {
            log(&no_session_bus(e));
            return;
        }
    };
    let mut tray = Tray::start(bus, PathBuf::from("/proc"));
    let fd = match tray.watch_fd() {
        Ok(fd) => fd,
        Err(e) => {
            log(&watch_fd_failed(e));
            return;
        }
    };
    // Level-triggered: whatever libdbus has not read yet wakes the loop again.
    // Once the bus has closed (`tray_output` dropped the tray), the source
    // removes itself: a dead socket must not keep the loop spinning.
    let source = Generic::new(fd, Interest::READ, Mode::Level);
    let inserted = handle.insert_source(source, |_, _, app: &mut App| {
        app.tray_readable();
        Ok(if app.tray.is_some() { PostAction::Continue } else { PostAction::Remove })
    });
    if inserted.is_err() {
        log("tray: event loop refused the session bus source");
        return;
    }
    log("tray: on the session bus");
    state.tray = Some(tray);
    // The startup calls are pending: the deadline timer is armed for them.
    state.tray_output(TrayOutput::default());
}

/// The Wayland connection closed gently (EPIPE on write, ECONNRESET on read
/// — the socket closing), rather than with a real protocol error. That is
/// what BOTH a clean end of session (log out, compositor restart — the
/// Hyprland signature changes, see spec § 3) AND a crashing compositor
/// produce: nothing, on the client side, tells the two apart. The daemon
/// never reconnects on its own in either case (it's the session, through
/// `exec-once` then `shell-ctl`, that relaunches it): treating them the same
/// way, as an ordinary end of session, keeps an external supervisor from
/// reading an ordinary restart or logout as a sign of failure.
///
/// `calloop` sometimes nests a source's error inside another (observed on
/// this machine: an `OtherError` wrapping the original `IoError`): the real
/// system error is thus not always first in the chain. We look for it by
/// following `source()` all the way, rather than only looking at the top.
fn end_of_session(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(e);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<io::Error>() {
            if matches!(io.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) {
                return true;
            }
        }
        current = error.source();
    }
    false
}

/// The daemon's exit code depending on how the loop ended: 0 after a
/// requested stop (signal, `state.stop` cleanly raised) or an end of
/// session (`end_of_session`); 1 for any other failure of the calloop
/// dispatch loop (protocol error, bug…) — so a supervisor (systemd,
/// nixos-config) knows it must relaunch the bar, instead of believing it
/// exited on request.
fn exit_code(dispatch_failure: bool) -> i32 {
    if dispatch_failure { 1 } else { 0 }
}

pub fn run() -> i32 {
    // First: a panic anywhere, any thread, logs its place, never its message (private text).
    crate::log::install_panic_hook();
    limit_malloc_arenas();
    // Block signals BEFORE any thread: all of them inherit the mask; only
    // the guard thread receives them (sigwait).
    let signals = match shutdown::block_signals() {
        Ok(s) => s,
        Err(e) => {
            log(&format!("signals: {e}"));
            return 1;
        }
    };
    let _lock = match lock::acquire(&paths::runtime_dir().join("alpheus-demon.lock")) {
        Ok(v) => v,
        Err(m) => {
            log(&m);
            return 1;
        }
    };
    // The launcher's socket, right after the lock: the lock guarantees no
    // other daemon owns it, so a leftover file is stale (Server::bind). The
    // real $XDG_RUNTIME_DIR, never a /tmp fallback (`paths::runtime_dir`).
    // Owned here (the loop only holds a Weak): dropped when `run` returns,
    // which unlinks the socket file whatever the loop's sources keep alive.
    let (launcher_server, warning) = launcher_socket_outcome(Server::bind(&launcher_ipc::socket_path(&paths::runtime_dir())));
    if let Some(w) = &warning {
        log(w);
    }
    let launcher_server: Option<Rc<Server>> = launcher_server.map(Rc::new);
    let loaded = load_settings();
    let plan = Plan::of(&loaded.config);
    // `run` (blocking) is refused: see nonblocking.rs.
    let exec: Arc<dyn Executor> = Arc::new(NonBlocking(SystemExecutor::new(Duration::from_secs(2))));
    let Some((mut core, rx)) = build_app(loaded, &exec) else { return 1 };
    if !insert_core_sources(&core.handle, rx, &plan) {
        return 1;
    }
    start_modules(&mut core.state, &core.handle, &plan);
    start_sources(&mut core.state, &core.tx, &plan, &exec);
    if let Some(server) = &launcher_server {
        match watch_launcher_socket(&core.handle, server, core.qh.clone()) {
            Ok(()) => log("launcher socket ready"),
            Err(e) => log(&format!("launcher socket: {e} — the bar runs without a launcher")),
        }
    }
    // After the socket watch: a `started` hook may call `alpheus-launch`.
    core.state.hooks_started();
    if !start_guard(signals, core.tx.clone()) {
        return 1;
    }
    let dispatch_failure = main_loop(&mut core.state, &mut core.event_loop, &core.qh);
    shut_down(core, launcher_server);
    exit_code(dispatch_failure)
}

/// Caps glibc's malloc arenas before any thread starts (startup budget,
/// 15 MB): otherwise the sound and D-Bus threads would each
/// get their own arena, whose freed fragments `release_memory`
/// (`malloc_trim`) can't always hand back to the system.
fn limit_malloc_arenas() {
    // SAFETY: mallopt(3) only changes the allocator's configuration; no
    // other thread exists yet (sources and the guard start further below).
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
}

/// bar.toml and the theme, read once at start-up.
struct Loaded {
    config: BarConfig,
    theme_settings: ThemeSettings,
    roles: Roles,
    theme_warnings: Vec<String>,
}

/// Reads bar.toml and the theme (warnings logged, the theme line too).
fn load_settings() -> Loaded {
    let config_dir = paths::config_dir();
    let (config, warnings) = settings(&config_dir);
    for a in &warnings {
        log(a);
    }
    // No portal yet (the portal is followed once the loop runs): mode auto starts dark.
    let (theme_settings, roles, theme_warnings) = theme(&config_dir, None);
    for a in &theme_warnings {
        log(a);
    }
    log(&format!("theme: {}", describe(&theme_settings, effective_mode(theme_settings.mode, None))));
    Loaded { config, theme_settings, roles, theme_warnings }
}

/// What the configuration asks for, derived once before the config moves into the bar.
struct Plan {
    seconds: bool,
    want: Wanted,
    scope: Scope,
    privacy: bool,
    camera_roots: (PathBuf, PathBuf),
    resources_config: ResourcesConfig,
}

impl Plan {
    fn of(config: &BarConfig) -> Plan {
        Plan {
            seconds: clock::has_seconds(&config.clock.format) || clock::has_seconds(&config.clock.date),
            want: wanted(&config.modules),
            scope: sound_scope(&config.modules),
            privacy: camera_wanted(&config.modules),
            camera_roots: (config.privacy.dev_dir.clone(), config.privacy.proc_dir.clone()),
            resources_config: config.resources.clone(),
        }
    }
}

/// The daemon's live parts after `build_app`. The field order is the drop order (the loop's handle,
/// the loop, the queue handle, the connection, the sender, the app last); `shut_down` drops `state`
/// explicitly first, before the final flush.
struct Core {
    handle: LoopHandle<'static, App>,
    event_loop: EventLoop<'static, App>,
    qh: QueueHandle<App>,
    conn: Connection,
    tx: channel::Sender<Mail>,
    state: App,
}

/// Fonts, the Wayland connection, the app and its event loop, with the mail channel's two ends
/// (the receiver is returned to be inserted by `insert_core_sources`). None: the reason is logged
/// and `run` returns 1.
fn build_app(loaded: Loaded, exec: &Arc<dyn Executor>) -> Option<(Core, channel::Channel<Mail>)> {
    let Loaded { config, theme_settings, roles, theme_warnings } = loaded;
    // cosmic-text parses fonts with rayon: one thread is enough for a bar,
    // instead of one thread per core kept around for the whole session.
    // SAFETY: no other thread exists yet (sources and the guard start after).
    unsafe { std::env::set_var("RAYON_NUM_THREADS", "1") };
    let fonts = match Fonts::load(&config.font, config.height) {
        Ok((p, warning)) => {
            if let Some(a) = warning {
                log(&a);
            }
            p
        }
        Err(m) => {
            log(&m);
            return None;
        }
    };
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            log(&format!("no Wayland session: {e}"));
            return None;
        }
    };
    let (globals, file) = match registry_queue_init::<App>(&conn) {
        Ok(x) => x,
        Err(e) => {
            log(&format!("Wayland registry: {e}"));
            return None;
        }
    };
    let qh = file.handle();
    // The bar's tints and shape follow `module_colors` and `style` from its first paint (`Bar::new`
    // starts at none and flat).
    let mut bar = Bar::new(config);
    bar.module_colors = theme_settings.module_colors;
    bar.style = theme_settings.style;
    let mut state = match App::new(&globals, &qh, bar, theme_settings, roles, fonts, exec.clone()) {
        Ok(e) => e,
        Err(m) => {
            log(&m);
            return None;
        }
    };
    state.theme.warnings = theme_warnings;
    let event_loop: EventLoop<'static, App> = match EventLoop::try_new() {
        Ok(b) => b,
        Err(e) => {
            log(&format!("event loop: {e}"));
            return None;
        }
    };
    let handle = event_loop.handle();
    state.loop_handle = Some(handle.clone());
    state.displays_sync();
    if WaylandSource::new(conn.clone(), file).insert(handle.clone()).is_err() {
        log("Wayland source: could not insert");
        return None;
    }
    let (tx, rx) = channel::channel::<Mail>();
    state.mail = Some(tx.clone());
    Some((Core { handle, event_loop, qh, conn, tx, state }, rx))
}

/// The loop's own sources: the mail channel, the clock, and the resources timer when shown.
/// False: one was refused (logged).
fn insert_core_sources(handle: &LoopHandle<'static, App>, rx: channel::Channel<Mail>, plan: &Plan) -> bool {
    // Each callback (channel, clock) only raises the `dirty` flag (see
    // `DeferredRedraw`): neither one draws. A burst of several messages
    // (e.g. niri, which sends several at once) handled in the same
    // `dispatch()` pass thus redraws only once, after the `while` loop,
    // instead of once per message. A `Msg::Compositor` among them also
    // raises `WindowsFeed`'s flag, fed to an open launcher at most once the
    // same way (`after_dispatch_windows`, below the loop).
    let mail = handle.insert_source(rx, |ev, _, state: &mut App| match ev {
        channel::Event::Msg(Mail::Bar(m)) => state.on_bar_message(m),
        channel::Event::Msg(Mail::Catalog { generation, catalog }) => state.launcher_catalog(generation, catalog),
        channel::Event::Msg(Mail::Entries { generation, entries }) => state.launcher_entries(generation, entries),
        channel::Event::Msg(Mail::Stop) | channel::Event::Closed => state.stop = true,
    });
    // Clock: one wakeup per display change (minute, or second if the format
    // shows one), never more.
    let seconds = plan.seconds;
    let timer = handle.insert_source(Timer::immediate(), move |_, _, state: &mut App| {
        let (h, nanos) = LocalTime::now();
        on_message(state, Msg::Clock(h), || h); // never consulted: Msg::Clock is not "from the compositor"
        state.dnd_check_clock(); // a timed do-not-disturb's end, by the wall clock (suspend)
        state.timer_check_clock(); // the focus timer's end, likewise
        TimeoutAction::ToDuration(clock::next_tick(h.second, nanos, seconds))
    });
    if mail.is_err() || timer.is_err() {
        log("event loop: source refused");
        return false;
    }
    // Resources: a reading of /proc every `interval` seconds, on the loop
    // (a few µs), only if the module is shown.
    if plan.want.resources {
        let mut reader = resources::Reader::new(
            resources::Roots {
                root: "/".into(),
                home: std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from),
                statfs: crate::sys::statvfs,
            },
            &plan.resources_config,
        );
        let period = resources_period(&plan.resources_config);
        let r = handle.insert_source(Timer::immediate(), move |_, _, state: &mut App| {
            state.resources_tick(reader.sample());
            TimeoutAction::ToDuration(period)
        });
        if r.is_err() {
            log("event loop: resources timer refused");
            return false;
        }
    }
    true
}

/// The modules that live on the loop: the theme watch, the camera watch, the tray, media,
/// notifications, the mode file and the portal; the theme is resolved once more last. The order
/// matters (see the comments, and the guardrail on the startup theme).
fn start_modules(state: &mut App, handle: &LoopHandle<'static, App>, plan: &Plan) {
    let want = &plan.want;
    // Hot reload of the theme: one inotify fd, no thread, no timer.
    crate::watch::start(state, handle);
    // The camera devices (privacy delivery 2): an inotify fd and one scan, only with `privacy`.
    if plan.privacy {
        crate::camera_app::start(state, handle, &plan.camera_roots.0, &plan.camera_roots.1);
    }
    if want.tray {
        start_tray(state, handle);
    }
    // The media module: on the tray's connection, or on the bar's link, which the portal joins next.
    state.start_media(want.media);
    // The user manager's failed-units count, on the same connection.
    state.start_failed(want.failed);
    // The notifications server: do-not-disturb as it was left, then the name requested on the
    // tray's connection or on the bar's link (shared with the media module and the portal).
    if want.notifications {
        state.dnd_load(crate::notify_app::unix_now());
    }
    // The focus timer as it was left (nothing without `timer` in `modules`): its chip, even idle,
    // and its deadline; with the loop handle made, whatever `notifications` is.
    state.timer_load(crate::timer_app::unix_ms());
    state.start_notifications(want.notifications);
    // The mode file for the TUI: $XDG_RUNTIME_DIR/alpheus/mode, nothing when unset.
    state.theme.mode_runtime = alpheus_commun::theme::mode_file::runtime_from(|k| std::env::var_os(k));
    // mode = "auto": the portal on the tray's connection, or on the bar's link (shared with the media
    // module) when the tray is off. Never a second connection, never a
    // retry.
    state.follow_portal_if_auto();
    // Written now, once the mode is known: dark until the portal answers (then rewritten).
    state.publish_mode();
    // One more resolve, last: a file matugen wrote between the first read and the watch being
    // armed (or half-written at the first read) is picked up now, and from here on the watch sees
    // everything. After `start_tray` and `follow_portal_if_auto`, never before: earlier, the
    // portal start would see no bus and give up for the whole session. Idempotent: no repaint when
    // the roles are equal, warnings deduplicated.
    state.theme_changed();
}

/// The threads that feed the loop: compositor, sound and D-Bus, each only when wanted.
fn start_sources(state: &mut App, tx: &channel::Sender<Mail>, plan: &Plan, exec: &Arc<dyn Executor>) {
    let want = &plan.want;
    let tx_compositor = tx.clone();
    sources::start(sources::detect(|k| std::env::var_os(k)), move |e| tx_compositor.send(Mail::Bar(Msg::Compositor(e))).is_ok());
    if want.sound {
        let tx = tx.clone();
        // Shared with the quick panel: the loop keeps a clone (`App::quick.sound_tap`).
        let tap = Arc::new(crate::pipewire::SoundTap::new(plan.privacy));
        state.quick.sound_tap = Some(tap.clone());
        sources::sound::start(exec.clone(), plan.scope, tap, move |s| tx.send(Mail::Bar(Msg::Sound(s))).is_ok());
    }
    if !want.dbus.is_empty() {
        let tx = tx.clone();
        // The quick panel's Wi-Fi (quick Wi-Fi spec § 5): the loop asks the D-Bus thread through this
        // tap, two fds at rest, no thread, no timer. Only with the panel and the network domain.
        if state.quick_enabled() && want.dbus.contains(&Domain::Network) {
            match crate::sources::dbus::wifi::WifiTap::new() {
                Ok(t) => state.quick.wifi_tap = Some(Arc::new(t)),
                Err(e) => log(&format!("quick: no Wi-Fi tap ({e})")),
            }
        }
        // The Bluetooth row and detail, the same way (quick settings delivery 3).
        if state.quick_enabled() && want.dbus.contains(&Domain::Bluetooth) {
            match crate::sources::dbus::bt::BtTap::new() {
                Ok(t) => state.quick.bt_tap = Some(Arc::new(t)),
                Err(e) => log(&format!("quick: no Bluetooth tap ({e})")),
            }
        }
        // The VPN row and detail (quick VPN spec § 5): read on the network domain, with the Wi-Fi's.
        if state.quick_enabled() && want.dbus.contains(&Domain::Network) {
            match crate::sources::dbus::vpn::VpnTap::new() {
                Ok(t) => state.quick.vpn_tap = Some(Arc::new(t)),
                Err(e) => log(&format!("quick: no VPN tap ({e})")),
            }
        }
        // The failed-units count's word (`failed-refresh`), only with its domain.
        if want.dbus.contains(&Domain::Units) {
            match sources::dbus::UnitsTap::new() {
                Ok(t) => state.units_tap = Some(Arc::new(t)),
                Err(e) => log(&format!("failed: no units tap ({e})")),
            }
        }
        let tx_read = tx.clone();
        sources::dbus::start(
            want.dbus.clone(),
            state.quick.wifi_tap.clone(),
            state.quick.bt_tap.clone(),
            state.quick.vpn_tap.clone(),
            state.units_tap.clone(),
            sources::dbus::units::UnitsOptions { subscribe: state.bar.config.failed.subscribe, ..Default::default() },
            move |s| sources::dbus::messages(s).into_iter().all(|m| tx.send(Mail::Bar(m)).is_ok()),
            move || tx_read.send(Mail::Bar(Msg::UnitsRead)).is_ok(),
        );
    }
}

/// Guard thread: waits for a signal without running, asks the loop to
/// stop, forces an exit if it doesn't finish within the grace period.
/// False: it could not start (logged).
fn start_guard(signals: libc::sigset_t, tx: channel::Sender<Mail>) -> bool {
    let guard = std::thread::Builder::new().name("guard".into()).spawn(move || {
        let sig = shutdown::wait_signal(&signals);
        STOP.store(true, Ordering::SeqCst);
        let _ = tx.send(Mail::Stop);
        shutdown::watch(&STOP, &FINISHED, GRACE, Duration::from_millis(20), || std::process::exit(128 + sig));
    });
    if guard.is_err() {
        log("guard thread failed");
        return false;
    }
    true
}

/// Dispatches until a stop is asked or the loop fails; true when it failed.
fn main_loop(state: &mut App, event_loop: &mut EventLoop<'static, App>, qh: &QueueHandle<App>) -> bool {
    let mut dispatch_failure = false;
    while !state.stop {
        if let Err(e) = event_loop.dispatch(None, state) {
            if end_of_session(&e) {
                log(&format!("end of session: {e}"));
            } else {
                log(&format!("loop: {e}"));
                dispatch_failure = true;
            }
            break;
        }
        after_dispatch(state);
        after_dispatch_windows(state);
        // A launcher draw that found both buffers busy, done now if one was released.
        state.launcher_guarded("deferred draw", |app| app.launcher_after_dispatch());
        state.notes_guarded("deferred draw", |app| app.notes_after_dispatch());
        // Outside the dispatch: a released keyboard's last reference (`retired`).
        state.keyboard.retired.drain();
        // After the whole batch (so any `output_destroyed` paired with a
        // `closed` of this same batch has already run): places the bar
        // back on outputs `closed` marked for restoration and that are
        // still known now (see `App::restore_pending`'s doc).
        state.restore_pending(qh);
    }
    dispatch_failure
}

/// Closes the launcher and the quick notepad (which saves), drops the app (its surfaces), the socket, flushes the connection and says
/// the shutdown is over.
fn shut_down(mut core: Core, launcher_server: Option<Rc<Server>>) {
    // A script still waiting gets "cancelled" (exit 1), not a cut connection.
    // Guarded like every launcher entry point: a launcher bug must not turn a
    // clean stop into a crash.
    core.state.launcher_guarded("shutdown", |app| app.launcher_close(CloseReason::Close));
    // The quick notepad saves what was typed (even after a failed autosave) and goes, guarded too.
    core.state.notes_guarded("shutdown", |app| app.notes_close(CloseReason::Close));
    // A trial left open would outlive the daemon: its state from before goes out with the flush.
    let state = &mut core.state;
    crate::launcher::host::guarded("displays shutdown", || state.displays_shutdown());
    drop(core.state); // surfaces destroyed
    // The socket file goes now (Server's Drop), not with the loop: the
    // loop's sources may keep each other alive through LoopHandle clones.
    drop(launcher_server);
    let _ = core.conn.flush();
    FINISHED.store(true, Ordering::SeqCst);
    log("shutdown");
}

#[cfg(test)]
mod tests;
