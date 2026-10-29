//! Assembling the daemon: signals, lock, settings, fonts, Wayland, the event
//! loop (calloop), the sources the modules need (compositor, sound, D-Bus,
//! resources timer). Covered by tests/headless_session.rs (headless sway)
//! and the real-world trial; its pure decisions have their own tests.

use crate::clock;
use crate::clock::LocalTime;
use crate::config::{BarConfig, ModuleId, ResourcesConfig};
use crate::launcher::host::guarded;
use crate::launcher::scan::Catalog;
use crate::launcher::socket::{Conn, MAX_CONNECTIONS, Progress, REQUEST_DEADLINE, Server};
use crate::launcher::sources::Entries;
use crate::launcher::surface::CloseReason;
use crate::lock;
use crate::nonblocking::NonBlocking;
use crate::painter::Fonts;
use crate::pipewire::Scope;
use crate::sources;
use crate::sources::dbus::Domain;
use crate::sources::resources;
use crate::state::{Bar, Msg};
use crate::tray::bus::SessionBus;
use crate::tray::router::{Tray, TrayOutput};
use crate::wayland::App;
use alpheus_commun::exec::{Executor, SystemExecutor};
use alpheus_commun::launcher_ipc::{self, Reply, Request};
use alpheus_commun::theme::settings::{ModeSetting, ThemeSettings, ThemeSource, effective_mode, resolve_dir};
// `Scheme`: calloop's `Mode` is already in scope.
use alpheus_commun::theme::{Mode as Scheme, Roles};
use alpheus_commun::{paths, shutdown};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use std::cell::{Cell, RefCell};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);
static FINISHED: AtomicBool = AtomicBool::new(false);
/// Grace period left for the loop to exit on its own after a signal.
const GRACE: Duration = Duration::from_secs(1);
/// How long the listener rests after an accept() error that stays pending.
const ACCEPT_PAUSE: Duration = Duration::from_secs(1);
/// A failed request that buffered at least this much gives its pages back.
const RELEASE_AFTER: usize = 1 << 20;

/// Mail received by the loop from the other threads. One is moved once per event: the bar's
/// messages (the sound state with the open panel's headset among them) outweigh the rest.
#[allow(clippy::large_enum_variant)]
pub enum Mail {
    Bar(Msg),
    /// A finished scan: adopted by the open launcher if `generation` is its
    /// own, OR if it has no catalog of its own yet — a reopen racing a
    /// still-running scan (`scan::catalog_is_current`); otherwise a stale
    /// scan from a launcher already closed or already fed, ignored (review
    /// item M5, final review slice 1: this used to say "ignored unless
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

/// bar.toml, read at startup, and its warnings to log. `[quick] wifi_more`'s default comes from
/// launcher.toml's `terminal`, read once here (its warnings are the launcher's, logged at its own open).
pub fn settings(directory: &Path) -> (BarConfig, Vec<String>) {
    let (mut config, warning) = BarConfig::load(&directory.join("bar.toml"));
    let (launcher, _) = crate::launcher::config::LauncherConfig::load(&crate::launcher::config::config_path(directory));
    crate::config::resolve_wifi_more(&mut config.quick, &launcher.terminal);
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
/// review: the clock could otherwise stay stale for up to a minute after
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

/// Mirrors `DeferredRedraw` for the launcher's windows (task 13, carried
/// review): `Launcher::set_windows` rebuilds the ranker's per-window facts
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
    /// The tray's session-bus connection (tray plan 2); false: no connection, no fd, no timer.
    pub tray: bool,
    /// The media module's MPRIS players (media task 5): on the tray's connection, or on the bar's
    /// link when the tray is off; false: no connection, no rule, no call.
    pub media: bool,
    /// The notifications server and the bell (notifications task 5): on the tray's connection, or
    /// on the bar's link when the tray is off; false: no RequestName, no surface, no call.
    pub notifications: bool,
}

pub fn wanted(modules: &[ModuleId]) -> Wanted {
    let has = |m: ModuleId| modules.contains(&m);
    let dbus = [
        (ModuleId::Network, Domain::Network),
        // The quick panel's Wi-Fi row needs NetworkManager's state (quick Wi-Fi decision 3).
        (ModuleId::Quick, Domain::Network),
        (ModuleId::Bluetooth, Domain::Bluetooth),
        // The quick panel's Bluetooth row reads the bar's BtState (quick settings delivery 3, decision 1).
        (ModuleId::Quick, Domain::Bluetooth),
        (ModuleId::Battery, Domain::Battery),
        (ModuleId::Quick, Domain::Power),
    ]
    .into_iter()
    .filter(|(m, _)| has(*m))
    .map(|(_, d)| d)
    // With the network module and the panel both, the network domain is read once.
    .fold(Vec::new(), |mut v, d| {
        if !v.contains(&d) {
            v.push(d);
        }
        v
    });
    Wanted {
        sound: has(ModuleId::Volume) || has(ModuleId::Osd) || has(ModuleId::Quick) || has(ModuleId::Privacy),
        dbus,
        resources: has(ModuleId::Resources),
        tray: has(ModuleId::Tray),
        media: has(ModuleId::Media),
        notifications: has(ModuleId::Notifications),
    }
}

/// Whether the camera devices are watched: only with the `privacy` module (no inotify instance,
/// no timer and no scan otherwise).
pub fn camera_wanted(modules: &[ModuleId]) -> bool {
    modules.contains(&ModuleId::Privacy)
}

/// What the sound source publishes for: without the `osd` module, the bar's sink volume alone (a
/// microphone or a label change wakes nothing).
pub fn sound_scope(modules: &[ModuleId]) -> Scope {
    if modules.contains(&ModuleId::Osd) { Scope::Osd } else { Scope::Bar }
}

pub fn resources_period(c: &ResourcesConfig) -> Duration {
    Duration::from_secs(c.interval)
}

impl App {
    /// One message from a source thread, in the order the readers need: the quick panel takes a
    /// sound message's sink list first (quick task 6), the OSD reads every sound message before the
    /// bar keeps it (osd task 3: its trigger rules compare it with the last one), the privacy
    /// indicators read sound and compositor messages first (privacy spec § 4); then the bar's own
    /// handling, and the panel refreshes if it took something.
    pub(crate) fn on_bar_message(&mut self, mut m: Msg) {
        let quick = self.quick_take(&mut m);
        if let Msg::Sound(s) = &m {
            self.osd_sound(s.as_ref());
        }
        self.privacy_feed(&m);
        note_compositor_message(self, &m);
        on_message(self, m, || LocalTime::now().0);
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
/// failed: a different step than `no_session_bus` (fix round, 2026-10-06 —
/// the two used to share the same wording, hiding which one actually
/// failed).
fn watch_fd_failed(e: impl std::fmt::Display) -> String {
    format!("tray: connected, but could not watch the session bus's fd ({e})")
}

/// Connects the tray to the session bus (blocking, once: a local socket,
/// controller decision 2) and puts its fd in the loop — no thread. No session
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

/// What `run` does with the launcher socket's bind result: kept (Some) when
/// it succeeds; logged and dropped (None) when it fails — a socket is
/// optional (spec § 3): never a reason to refuse to start the bar.
fn launcher_socket_outcome<T, E: std::fmt::Display>(r: Result<T, E>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(format!("launcher socket: {e} — the bar runs without a launcher"))),
    }
}

/// A dmenu request may have buffered up to `MAX_DMENU_BYTES` (4 MiB) before
/// being decoded: once it reached `RELEASE_AFTER`, its pages are given back
/// to the allocator as soon as it is consumed (worst case, every one of
/// `MAX_CONNECTIONS` at once: 32 MiB) — a toggle/close never buffers more
/// than its tiny header, nothing to release there. A small dmenu line list
/// is not worth a `malloc_trim` either (it walks the whole heap): task 13
/// fix round, review item 3 — this used to run after EVERY dmenu request,
/// whatever its size.
fn releases_memory_once_consumed(request: &Request, buffered: usize) -> bool {
    matches!(request, Request::Dmenu { .. }) && buffered >= RELEASE_AFTER
}

/// Whether a newly accepted connection is refused for being the
/// `MAX_CONNECTIONS + 1`th at once (spec § 3): `watch_connection`'s only
/// stateful decision, pulled out so it is tested without a socket.
fn too_many_connections(inflight: usize) -> bool {
    inflight >= MAX_CONNECTIONS
}

/// One more connection tracked: the increment half of `watch_connection`'s
/// inflight bookkeeping, pulled out (alongside `untrack_connection`) so the
/// count itself — not just the cap check above — is tested without a
/// socket or calloop (review item M3, final review slice 1).
fn track_connection(inflight: &Cell<usize>) {
    inflight.set(inflight.get() + 1);
}

/// The matching decrement, whatever ends the connection (answered, failed,
/// expired, or a source that never even got registered).
fn untrack_connection(inflight: &Cell<usize>) {
    inflight.set(inflight.get().saturating_sub(1));
}

/// One pass of the level-triggered listener: takes every pending
/// connection. An error (EMFILE, ENFILE, ENOBUFS…) stays pending and would
/// wake the loop again at once: Disable instead (the caller re-enables it
/// after ACCEPT_PAUSE), logged once until accept() works again.
fn drain_listener<C>(
    mut accept: impl FnMut() -> io::Result<Option<C>>,
    mut take: impl FnMut(C),
    failing: &Cell<bool>,
    log: &mut dyn FnMut(&str),
) -> PostAction {
    loop {
        match accept() {
            Ok(Some(c)) => {
                if failing.replace(false) {
                    log("launcher socket: accepting again");
                }
                take(c);
            }
            Ok(None) => return PostAction::Continue,
            Err(e) => {
                if !failing.replace(true) {
                    log(&format!("launcher socket: accept: {e} — paused, retried every {} s", ACCEPT_PAUSE.as_secs()));
                }
                return PostAction::Disable;
            }
        }
    }
}

/// `Conn::read` behind the error boundary: a hostile request hitting a bug
/// in the decoder fails that connection, never the bar.
fn guarded_progress(read: impl FnOnce() -> Progress) -> Progress {
    guarded("socket read", read).unwrap_or_else(|| Progress::Failed("internal error".to_string()))
}

/// A reply behind the error boundary (the encoder is launcher code too).
fn reply_guarded(conn: Conn, r: &Reply) {
    let _ = guarded("socket reply", move || conn.reply(r));
}

/// The launcher's listening socket on the loop. Level-triggered: every
/// pending connection is accepted; one refused (another uid) leaves the
/// rest for the next pass. The closure holds the server only weakly: `run`
/// owns it, so its Drop (unlinking the socket file) runs when `run` returns
/// even if the loop's sources keep each other alive through handle clones.
fn watch_launcher_socket(handle: &LoopHandle<'static, App>, server: &Rc<Server>, qh: QueueHandle<App>) -> io::Result<()> {
    let fd = server.fd().try_clone_to_owned()?;
    let server: Weak<Server> = Rc::downgrade(server);
    let inflight = Rc::new(Cell::new(0usize));
    let failing = Cell::new(false);
    let me: Rc<Cell<Option<RegistrationToken>>> = Rc::new(Cell::new(None));
    let token = handle
        .insert_source(Generic::new(fd, Interest::READ, Mode::Level), {
            let me = me.clone();
            move |_, _, state: &mut App| {
                let Some(server) = server.upgrade() else { return Ok(PostAction::Remove) };
                let action =
                    drain_listener(|| server.accept(), |conn| watch_connection(state, conn, &qh, &inflight), &failing, &mut |m| log(m));
                if action == PostAction::Disable {
                    resume_listener_later(state, me.get());
                }
                Ok(action)
            }
        })
        .map_err(|e| io::Error::other(e.error.to_string()))?;
    me.set(Some(token));
    Ok(())
}

/// Re-enables the paused listener after ACCEPT_PAUSE: a one-shot timer that
/// exists only after an accept() error (never at rest).
fn resume_listener_later(state: &App, listener: Option<RegistrationToken>) {
    let (Some(handle), Some(token)) = (state.loop_handle.clone(), listener) else { return };
    let h = handle.clone();
    let timer = handle.insert_source(Timer::from_duration(ACCEPT_PAUSE), move |_, _, _: &mut App| {
        if let Err(e) = h.enable(&token) {
            log(&format!("launcher socket: not resumed: {e}"));
        }
        TimeoutAction::Drop
    });
    if let Err(e) = timer {
        log(&format!("launcher socket: resume timer: {} — the launcher stays off until the bar restarts", e.error));
    }
}

/// One accepted connection: read when readable; once whole, answered or
/// handed to the launcher. Its deadline (REQUEST_DEADLINE) is a timer that
/// exists only while the connection is being read.
fn watch_connection(state: &mut App, conn: Conn, qh: &QueueHandle<App>, inflight: &Rc<Cell<usize>>) {
    if too_many_connections(inflight.get()) {
        log("launcher socket: too many connections at once, one refused");
        reply_guarded(conn, &Reply::Error("too many connections".to_string()));
        return;
    }
    let Some(handle) = state.loop_handle.clone() else { return };
    let fd = match conn.fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(e) => {
            log(&format!("launcher socket: {e}"));
            return;
        }
    };
    let slot = Rc::new(RefCell::new(Some(conn)));
    let deadline: Rc<Cell<Option<RegistrationToken>>> = Rc::new(Cell::new(None));
    track_connection(inflight);
    let (s, d, n, h, q) = (slot.clone(), deadline.clone(), inflight.clone(), handle.clone(), qh.clone());
    let read = handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, state: &mut App| {
        // Taken by the deadline already: the socket was shut down, this is its hang-up.
        let Some(mut conn) = s.borrow_mut().take() else { return Ok(PostAction::Remove) };
        // Hostile bytes are decoded here, outside launcher_request's guard:
        // behind the error boundary too.
        match guarded_progress(|| conn.read()) {
            Progress::Pending => {
                *s.borrow_mut() = Some(conn);
                return Ok(PostAction::Continue);
            }
            // The notifications verbs (task 5) are answered here, before the launcher's guard: a
            // panic in there closes the launcher, and these are not the launcher's.
            Progress::Ready(request @ (Request::Dnd(_) | Request::ClearNotifications)) => {
                let reply = state.notifications_verb(&request);
                reply_guarded(conn, &reply);
            }
            // The OSD's verb (osd task 4): a sysfs read and a pill, answered here like the
            // notifications verbs, before the launcher's guard.
            Progress::Ready(request @ Request::Osd(_)) => {
                let reply = state.osd_verb(&request);
                reply_guarded(conn, &reply);
            }
            // The quick panel's verb (quick task 7): answered here like the OSD's, before the
            // launcher's guard.
            Progress::Ready(request @ Request::Quick(_)) => {
                let reply = state.quick_verb(&request);
                reply_guarded(conn, &reply);
            }
            Progress::Ready(request) => {
                // A dmenu request may have buffered up to 4 MiB: once it
                // reached RELEASE_AFTER, give the pages back as soon as it
                // is consumed, whatever the daemon then decides to do with
                // it (opened, or Busy). Read before `conn` is handed to
                // `launcher_request`, which consumes it.
                let buffered = conn.received();
                if releases_memory_once_consumed(&request, buffered) {
                    crate::memory::release_memory();
                }
                state.launcher_request(&q, request, conn)
            }
            Progress::Failed(m) => {
                log(&format!("launcher socket: {m}"));
                let buffered = conn.received();
                reply_guarded(conn, &Reply::Error(m));
                if buffered >= RELEASE_AFTER {
                    // A refused multi-MB request: its buffer is freed, give the pages back.
                    crate::memory::release_memory();
                }
            }
        }
        if let Some(t) = d.take() {
            h.remove(t);
        }
        untrack_connection(&n);
        Ok(PostAction::Remove)
    });
    if let Err(e) = read {
        log(&format!("launcher socket: {}", e.error));
        untrack_connection(inflight);
        return;
    }
    let n = inflight.clone();
    let timer = handle.insert_source(Timer::from_duration(REQUEST_DEADLINE), move |_, _, _: &mut App| {
        if let Some(conn) = slot.borrow_mut().take() {
            log("launcher socket: request not complete in time, dropped");
            let buffered = conn.received();
            reply_guarded(conn, &Reply::Error("request deadline passed".to_string()));
            untrack_connection(&n);
            if buffered >= RELEASE_AFTER {
                crate::memory::release_memory();
            }
        }
        TimeoutAction::Drop
    });
    match timer {
        Ok(t) => deadline.set(Some(t)),
        Err(e) => log(&format!("launcher socket: deadline timer: {}", e.error)),
    }
}

pub fn run() -> i32 {
    // Caps glibc's malloc arenas before any thread starts (startup budget,
    // 15 MB, step 6): otherwise the sound and D-Bus threads would each
    // get their own arena, whose freed fragments `release_memory`
    // (`malloc_trim`) can't always hand back to the system.
    // SAFETY: mallopt(3) only changes the allocator's configuration; no
    // other thread exists yet (sources and the guard start further below).
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
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
    let config_dir = paths::config_dir();
    let (config, warnings) = settings(&config_dir);
    for a in &warnings {
        log(a);
    }
    // No portal yet (themes task 6 follows it once the loop runs): mode auto starts dark.
    let (theme_settings, roles, theme_warnings) = theme(&config_dir, None);
    for a in &theme_warnings {
        log(a);
    }
    log(&format!("theme: {}", describe(&theme_settings, effective_mode(theme_settings.mode, None))));
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
            return 1;
        }
    };
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            log(&format!("no Wayland session: {e}"));
            return 1;
        }
    };
    let (globals, file) = match registry_queue_init::<App>(&conn) {
        Ok(x) => x,
        Err(e) => {
            log(&format!("Wayland registry: {e}"));
            return 1;
        }
    };
    let qh = file.handle();
    let seconds = clock::has_seconds(&config.clock.format) || clock::has_seconds(&config.clock.date);
    let want = wanted(&config.modules);
    let scope = sound_scope(&config.modules);
    let privacy = camera_wanted(&config.modules);
    let camera_roots = (config.privacy.dev_dir.clone(), config.privacy.proc_dir.clone());
    let resources_config = config.resources.clone();
    // `run` (blocking) is refused: see nonblocking.rs.
    let exec: Arc<dyn Executor> = Arc::new(NonBlocking(SystemExecutor::new(Duration::from_secs(2))));
    // The bar's tints and shape follow `module_colors` and `style` from its first paint (`Bar::new`
    // starts at none and flat).
    let mut bar = Bar::new(config);
    bar.module_colors = theme_settings.module_colors;
    bar.style = theme_settings.style;
    let mut state = match App::new(&globals, &qh, bar, theme_settings, roles, fonts, exec.clone()) {
        Ok(e) => e,
        Err(m) => {
            log(&m);
            return 1;
        }
    };
    state.theme_warnings = theme_warnings;
    let mut event_loop: EventLoop<'static, App> = match EventLoop::try_new() {
        Ok(b) => b,
        Err(e) => {
            log(&format!("event loop: {e}"));
            return 1;
        }
    };
    let handle = event_loop.handle();
    state.loop_handle = Some(handle.clone());
    if WaylandSource::new(conn.clone(), file).insert(handle.clone()).is_err() {
        log("Wayland source: could not insert");
        return 1;
    }
    let (tx, rx) = channel::channel::<Mail>();
    state.mail = Some(tx.clone());
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
    let timer = handle.insert_source(Timer::immediate(), move |_, _, state: &mut App| {
        let (h, nanos) = LocalTime::now();
        on_message(state, Msg::Clock(h), || h); // never consulted: Msg::Clock is not "from the compositor"
        TimeoutAction::ToDuration(clock::next_tick(h.second, nanos, seconds))
    });
    if mail.is_err() || timer.is_err() {
        log("event loop: source refused");
        return 1;
    }
    // Resources: a reading of /proc every `interval` seconds, on the loop
    // (a few µs), only if the module is shown.
    if want.resources {
        let mut reader = resources::Reader::new(Path::new("/"), &resources_config.sensor);
        let period = resources_period(&resources_config);
        let r = handle.insert_source(Timer::immediate(), move |_, _, state: &mut App| {
            on_message(state, Msg::Resources(reader.sample()), || LocalTime::now().0);
            TimeoutAction::ToDuration(period)
        });
        if r.is_err() {
            log("event loop: resources timer refused");
            return 1;
        }
    }

    // Hot reload of the theme: one inotify fd, no thread, no timer (themes task 5).
    crate::watch::start(&mut state, &handle);
    // The camera devices (privacy delivery 2): an inotify fd and one scan, only with `privacy`.
    if privacy {
        crate::camera_app::start(&mut state, &handle, &camera_roots.0, &camera_roots.1);
    }
    if want.tray {
        start_tray(&mut state, &handle);
    }
    // The media module: on the tray's connection, or on the bar's link, which the portal joins next.
    state.start_media(want.media);
    // The notifications server: do-not-disturb as it was left, then the name requested on the
    // tray's connection or on the bar's link (shared with the media module and the portal).
    if want.notifications {
        state.dnd = crate::state_file::dnd_on(&paths::state_dir());
    }
    state.start_notifications(want.notifications);
    // The mode file for the TUI (themes task 6): $XDG_RUNTIME_DIR/alpheus/mode, nothing when unset.
    state.mode_runtime = alpheus_commun::theme::mode_file::runtime_from(|k| std::env::var_os(k));
    // mode = "auto": the portal on the tray's connection, or on the bar's link (shared with the media
    // module) when the tray is off (themes task 6, media task 5). Never a second connection, never a
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
    let tx_compositor = tx.clone();
    sources::start(sources::detect(|k| std::env::var_os(k)), move |e| tx_compositor.send(Mail::Bar(Msg::Compositor(e))).is_ok());
    if want.sound {
        let tx = tx.clone();
        // Shared with the quick panel: the loop keeps a clone (`App::sound_tap`).
        let tap = Arc::new(crate::pipewire::SoundTap::new(privacy));
        state.sound_tap = Some(tap.clone());
        sources::sound::start(exec.clone(), scope, tap, move |s| tx.send(Mail::Bar(Msg::Sound(s))).is_ok());
    }
    if !want.dbus.is_empty() {
        let tx = tx.clone();
        // The quick panel's Wi-Fi (quick Wi-Fi spec § 5): the loop asks the D-Bus thread through this
        // tap, two fds at rest, no thread, no timer. Only with the panel and the network domain.
        if state.quick_enabled() && want.dbus.contains(&Domain::Network) {
            match crate::sources::dbus::wifi::WifiTap::new() {
                Ok(t) => state.wifi_tap = Some(Arc::new(t)),
                Err(e) => log(&format!("quick: no Wi-Fi tap ({e})")),
            }
        }
        // The Bluetooth row and detail, the same way (quick settings delivery 3).
        if state.quick_enabled() && want.dbus.contains(&Domain::Bluetooth) {
            match crate::sources::dbus::bt::BtTap::new() {
                Ok(t) => state.bt_tap = Some(Arc::new(t)),
                Err(e) => log(&format!("quick: no Bluetooth tap ({e})")),
            }
        }
        sources::dbus::start(want.dbus.clone(), state.wifi_tap.clone(), state.bt_tap.clone(), move |s| {
            sources::dbus::messages(s).into_iter().all(|m| tx.send(Mail::Bar(m)).is_ok())
        });
    }
    if let Some(server) = &launcher_server {
        match watch_launcher_socket(&handle, server, qh.clone()) {
            Ok(()) => log("launcher socket ready"),
            Err(e) => log(&format!("launcher socket: {e} — the bar runs without a launcher")),
        }
    }
    // Guard thread: waits for a signal without running, asks the loop to
    // stop, forces an exit if it doesn't finish within the grace period.
    let guard = std::thread::Builder::new().name("guard".into()).spawn(move || {
        let sig = shutdown::wait_signal(&signals);
        STOP.store(true, Ordering::SeqCst);
        let _ = tx.send(Mail::Stop);
        shutdown::watch(&STOP, &FINISHED, GRACE, Duration::from_millis(20), || std::process::exit(128 + sig));
    });
    if guard.is_err() {
        log("guard thread failed");
        return 1;
    }
    let mut dispatch_failure = false;
    while !state.stop {
        if let Err(e) = event_loop.dispatch(None, &mut state) {
            if end_of_session(&e) {
                log(&format!("end of session: {e}"));
            } else {
                log(&format!("loop: {e}"));
                dispatch_failure = true;
            }
            break;
        }
        after_dispatch(&mut state);
        after_dispatch_windows(&mut state);
        // Outside the dispatch: a released keyboard's last reference (`retired`).
        state.retired_keyboards.drain();
        // After the whole batch (so any `output_destroyed` paired with a
        // `closed` of this same batch has already run): places the bar
        // back on outputs `closed` marked for restoration and that are
        // still known now (see `App::restore_pending`'s doc).
        state.restore_pending(&qh);
    }
    // A script still waiting gets "cancelled" (exit 1), not a cut connection.
    // Guarded like every launcher entry point: a launcher bug must not turn a
    // clean stop into a crash.
    state.launcher_guarded("shutdown", |app| app.launcher_close(CloseReason::Close));
    drop(state); // surfaces destroyed
    // The socket file goes now (Server's Drop), not with the loop: the
    // loop's sources may keep each other alive through LoopHandle clones.
    drop(launcher_server);
    let _ = conn.flush();
    FINISHED.store(true, Ordering::SeqCst);
    log("shutdown");
    exit_code(dispatch_failure)
}

#[cfg(test)]
mod tests;
