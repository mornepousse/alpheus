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
use crate::launcher::surface::CloseReason;
use crate::lock;
use crate::nonblocking::NonBlocking;
use crate::painter::Fonts;
use crate::sources;
use crate::sources::dbus::Domain;
use crate::sources::resources;
use crate::state::{Bar, Msg};
use crate::wayland::App;
use alpheus_commun::exec::{Executor, SystemExecutor};
use alpheus_commun::launcher_ipc::{self, Reply, Request};
use alpheus_commun::theme::Palette;
use alpheus_commun::{paths, shutdown};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use std::cell::{Cell, RefCell};
use std::io;
use std::path::Path;
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

/// Mail received by the loop from the other threads.
pub enum Mail {
    Bar(Msg),
    /// A finished scan; ignored unless `generation` is the open launcher's.
    Catalog {
        generation: u64,
        catalog: Catalog,
    },
    Stop,
}

/// Settings read at startup, warnings to log.
pub fn settings(directory: &Path) -> (BarConfig, Palette, Vec<String>) {
    let (config, w1) = BarConfig::load(&directory.join("bar.toml"));
    let (palette, w2) = Palette::load(&directory.join("colors.toml"));
    (config, palette, [w1, w2].into_iter().flatten().collect())
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
}

/// Called by every callback (channel message, clock tick): never a redraw
/// here, only the flag.
pub fn on_message(target: &mut impl DeferredRedraw, m: Msg, now: impl Fn() -> LocalTime) {
    if dispatch_msg(target.bar_mut(), m, now) {
        *target.dirty_mut() = true;
    }
}

/// Called exactly once after `loop.dispatch()` returns: redraws if the flag
/// was raised by the burst just processed, and clears it in every case.
pub fn after_dispatch(target: &mut impl DeferredRedraw) {
    if std::mem::take(target.dirty_mut()) {
        target.redraw_now();
    }
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
    pub sound: bool,
    /// The D-Bus domains to read and follow; empty: no D-Bus thread.
    pub dbus: Vec<Domain>,
    pub resources: bool,
}

pub fn wanted(modules: &[ModuleId]) -> Wanted {
    let has = |m: ModuleId| modules.contains(&m);
    let dbus = [(ModuleId::Network, Domain::Network), (ModuleId::Bluetooth, Domain::Bluetooth), (ModuleId::Battery, Domain::Battery)]
        .into_iter()
        .filter(|(m, _)| has(*m))
        .map(|(_, d)| d)
        .collect();
    Wanted { sound: has(ModuleId::Volume), dbus, resources: has(ModuleId::Resources) }
}

pub fn resources_period(c: &ResourcesConfig) -> Duration {
    Duration::from_secs(c.interval)
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
    inflight.set(inflight.get() + 1);
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
        n.set(n.get() - 1);
        Ok(PostAction::Remove)
    });
    if let Err(e) = read {
        log(&format!("launcher socket: {}", e.error));
        inflight.set(inflight.get() - 1);
        return;
    }
    let n = inflight.clone();
    let timer = handle.insert_source(Timer::from_duration(REQUEST_DEADLINE), move |_, _, _: &mut App| {
        if let Some(conn) = slot.borrow_mut().take() {
            log("launcher socket: request not complete in time, dropped");
            let buffered = conn.received();
            reply_guarded(conn, &Reply::Error("request deadline passed".to_string()));
            n.set(n.get() - 1);
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
    // 15 MB, step 6): otherwise the sound, D-Bus and zbus threads would each
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
    let (config, palette, warnings) = settings(&paths::config_dir());
    for a in &warnings {
        log(a);
    }
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
    let resources_config = config.resources.clone();
    // `run` (blocking) is refused: see nonblocking.rs.
    let exec: Arc<dyn Executor> = Arc::new(NonBlocking(SystemExecutor::new(Duration::from_secs(2))));
    let mut state = match App::new(&globals, &qh, Bar::new(config), palette, fonts, exec.clone()) {
        Ok(e) => e,
        Err(m) => {
            log(&m);
            return 1;
        }
    };
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
        channel::Event::Msg(Mail::Bar(m)) => {
            note_compositor_message(state, &m);
            on_message(state, m, || LocalTime::now().0);
        }
        channel::Event::Msg(Mail::Catalog { generation, catalog }) => state.launcher_catalog(generation, catalog),
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
    let tx_compositor = tx.clone();
    sources::start(sources::detect(|k| std::env::var_os(k)), move |e| tx_compositor.send(Mail::Bar(Msg::Compositor(e))).is_ok());
    if want.sound {
        let tx = tx.clone();
        sources::sound::start(exec.clone(), move |s| tx.send(Mail::Bar(Msg::Sound(s))).is_ok());
    }
    if !want.dbus.is_empty() {
        let tx = tx.clone();
        sources::dbus::start(want.dbus.clone(), move |s| sources::dbus::messages(s).into_iter().all(|m| tx.send(Mail::Bar(m)).is_ok()));
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
mod tests {
    use super::*;
    use crate::compositor::example;
    use crate::config::{ModuleId, ResourcesConfig};
    use crate::sources::dbus::{Domain, SystemState, messages};
    use crate::system::*;

    const BEFORE: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };
    const AFTER: LocalTime = LocalTime { minute: 47, ..BEFORE };

    #[test]
    fn only_the_sources_the_modules_need() {
        let all = vec![Domain::Network, Domain::Bluetooth, Domain::Battery];
        assert_eq!(wanted(&ModuleId::ALL), Wanted { sound: true, dbus: all, resources: true });
        assert_eq!(
            wanted(&[ModuleId::Workspaces, ModuleId::Window, ModuleId::Clock]),
            Wanted { sound: false, dbus: vec![], resources: false }
        );
        assert_eq!(wanted(&[ModuleId::Battery]), Wanted { sound: false, dbus: vec![Domain::Battery], resources: false });
        assert_eq!(
            wanted(&[ModuleId::Bluetooth, ModuleId::Clock]),
            Wanted { sound: false, dbus: vec![Domain::Bluetooth], resources: false }
        );
        assert_eq!(
            wanted(&[ModuleId::Battery, ModuleId::Network]),
            Wanted { sound: false, dbus: vec![Domain::Network, Domain::Battery], resources: false },
            "reading order, whatever the bar's order"
        );
        assert_eq!(wanted(&[ModuleId::Volume, ModuleId::Resources]), Wanted { sound: true, dbus: vec![], resources: true });
    }

    #[test]
    fn the_resources_period_comes_from_bar_toml() {
        assert_eq!(resources_period(&ResourcesConfig::default()), Duration::from_secs(2));
        assert_eq!(resources_period(&ResourcesConfig { interval: 5, ..ResourcesConfig::default() }), Duration::from_secs(5));
    }

    #[test]
    fn system_messages_redraw_once_and_only_resources_resets_the_clock() {
        let mut c = Counter { bar: Bar::new(BarConfig::default()), dirty: false, redraws: 0 };
        let s = SystemState { network: Some(NetState::Offline), bluetooth: Some(BtState::Off), battery: Some(PowerState::default()) };
        for m in messages(Some(s)) {
            on_message(&mut c, m, || AFTER);
        }
        on_message(&mut c, Msg::Sound(Some(SoundState { sink: None })), || AFTER);
        assert_eq!(c.bar.clock, None, "network, bluetooth, battery, sound: not a clock-refreshing event");
        on_message(&mut c, Msg::Resources(Sample { cpu: Some(5), memory: Some(61), temperature: None }), || AFTER);
        after_dispatch(&mut c);
        assert_eq!(c.redraws, 1, "a burst of system messages: one redraw");
        assert_eq!(c.bar.clock, Some(AFTER), "a resources tick also puts the clock right (B-m4)");
    }

    #[test]
    fn a_compositor_event_or_a_resources_tick_resets_the_clock() {
        let mut b = Bar::new(BarConfig::default());
        assert!(dispatch_msg(&mut b, Msg::Clock(BEFORE), || AFTER));
        assert_eq!(b.clock, Some(BEFORE), "a clock tick does not reread the time");
        assert!(dispatch_msg(&mut b, Msg::Compositor(Some(example())), || AFTER));
        assert_eq!(b.clock, Some(AFTER), "coming out of sleep: the clock follows the first event");
        assert!(!dispatch_msg(&mut b, Msg::Compositor(Some(example())), || AFTER), "nothing new: no redraw");
        // Resources has no down state, so a sleeping laptop with nothing
        // compositor-related happening right after resume (no focus
        // change yet) still gets the clock corrected by the next reading.
        assert!(dispatch_msg(&mut b, Msg::Clock(BEFORE), || AFTER), "clock put back stale for this part of the test");
        assert_eq!(b.clock, Some(BEFORE));
        let sample = Sample { cpu: Some(10), memory: Some(50), temperature: Some(40) };
        assert!(dispatch_msg(&mut b, Msg::Resources(sample), || AFTER));
        assert_eq!(b.clock, Some(AFTER), "a resources tick follows the clock too, with no compositor event needed");
    }

    /// Replaces `App` (wayland.rs, a real Wayland connection): counts
    /// redraws instead of actually drawing.
    struct Counter {
        bar: Bar,
        dirty: bool,
        redraws: u32,
    }

    impl DeferredRedraw for Counter {
        fn bar_mut(&mut self) -> &mut Bar {
            &mut self.bar
        }
        fn dirty_mut(&mut self) -> &mut bool {
            &mut self.dirty
        }
        fn redraw_now(&mut self) {
            self.redraws += 1;
        }
    }

    #[test]
    fn a_burst_of_messages_redraws_only_once() {
        let mut c = Counter { bar: Bar::new(BarConfig::default()), dirty: false, redraws: 0 };
        // A burst of 9 messages that EACH change the display (toggling
        // Some/None, like the up to 9 grids from a single niri burst over 3
        // screens): each one calls `on_message`, none draws directly —
        // otherwise this test would not bite (a repeated message, itself,
        // changes nothing from the second one on: see
        // `state::tests::messages_and_changes`).
        for i in 0..9 {
            let e = if i % 2 == 0 { Some(example()) } else { None };
            on_message(&mut c, Msg::Compositor(e), || BEFORE);
        }
        after_dispatch(&mut c);
        assert_eq!(c.redraws, 1, "burst of 9 messages that all change the display: one redraw, not 9");
        assert!(!c.dirty, "the flag drops back down after the redraw");
        // Nothing new in the next burst: no redraw at all.
        on_message(&mut c, Msg::Compositor(Some(example())), || BEFORE);
        after_dispatch(&mut c);
        assert_eq!(c.redraws, 1, "nothing changed: no further redraw");
    }

    #[test]
    fn exit_code_dispatch_failure_returns_1() {
        assert_eq!(exit_code(false), 0, "requested stop: code 0, a supervisor must not relaunch");
        assert_eq!(exit_code(true), 1, "the event loop's dispatch failed: code != 0, a supervisor must relaunch");
    }

    #[test]
    fn end_of_session_recognizes_epipe_and_econnreset_only() {
        assert!(end_of_session(&io::Error::from(io::ErrorKind::BrokenPipe)), "EPIPE: compositor gone, socket closed");
        assert!(end_of_session(&io::Error::from(io::ErrorKind::ConnectionReset)), "ECONNRESET: same thing on read");
        assert!(!end_of_session(&io::Error::from(io::ErrorKind::PermissionDenied)), "a real error stays a failure");
        assert!(!end_of_session(&io::Error::other("protocol error")), "same here");
    }

    #[test]
    fn end_of_session_traverses_calloop_nesting() {
        use smithay_client_toolkit::reexports::calloop::Error as LoopError;
        // Observed on this machine (headless sway, compositor killed):
        // calloop sometimes returns an `OtherError` wrapping a source's
        // `IoError`, instead of returning it directly — the real system
        // error is then at the second level of `source()`, not the first.
        let direct = LoopError::IoError(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(end_of_session(&direct), "direct case: IoError at the first level");
        let nested = LoopError::OtherError(Box::new(LoopError::IoError(io::Error::from(io::ErrorKind::BrokenPipe))));
        assert!(end_of_session(&nested), "observed case: OtherError(IoError(EPIPE))");
        let unrelated = LoopError::OtherError(Box::new(io::Error::other("protocol error")));
        assert!(!end_of_session(&unrelated), "nested but not EPIPE/ECONNRESET: still a failure");
    }

    #[test]
    fn settings_absent_or_invalid() {
        let d = std::env::temp_dir().join(format!("alpheus-settings-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (c, p, w) = settings(&d);
        assert_eq!((c, p, w.len()), (BarConfig::default(), Palette::default(), 0));
        std::fs::write(d.join("bar.toml"), "modules = [\"weather\"]").unwrap();
        std::fs::write(d.join("colors.toml"), "bg = 3").unwrap();
        let (c, _, w) = settings(&d);
        assert_eq!(c, BarConfig::default());
        assert!(w.iter().any(|m| m.contains("bar.toml")) && w.iter().any(|m| m.contains("colors.toml")), "{w:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_socket_bind_failure_is_logged_and_the_bar_runs_without_a_launcher() {
        assert_eq!(launcher_socket_outcome(Ok::<_, &str>(5)), (Some(5), None));
        assert_eq!(
            launcher_socket_outcome(Err::<u8, _>("EACCES")),
            (None, Some("launcher socket: EACCES — the bar runs without a launcher".to_string()))
        );
    }

    #[test]
    fn only_a_large_dmenu_request_releases_memory_once_consumed() {
        assert!(releases_memory_once_consumed(&Request::Dmenu { prompt: None, lines: vec![] }, RELEASE_AFTER), "reached the threshold");
        assert!(
            !releases_memory_once_consumed(&Request::Dmenu { prompt: None, lines: vec![] }, RELEASE_AFTER - 1),
            "a small dmenu list: not worth a malloc_trim (it walks the whole heap)"
        );
        assert!(!releases_memory_once_consumed(&Request::Toggle, RELEASE_AFTER), "a tiny header: nothing to release");
        assert!(!releases_memory_once_consumed(&Request::Close, RELEASE_AFTER));
    }

    #[test]
    fn max_connections_is_enforced() {
        assert!(!too_many_connections(MAX_CONNECTIONS - 1), "room for one more");
        assert!(too_many_connections(MAX_CONNECTIONS), "already at the limit: the next one is refused");
        assert!(too_many_connections(MAX_CONNECTIONS + 1));
    }

    /// accept() keeps failing (EMFILE: the error stays pending on a
    /// level-triggered source): the listener pauses instead of spinning,
    /// and says so once, not once per pass.
    #[test]
    fn a_failing_accept_pauses_the_listener_and_logs_once() {
        let failing = Cell::new(false);
        let mut lines = Vec::new();
        for _ in 0..3 {
            let mut calls = 0;
            let action = drain_listener(
                || {
                    calls += 1;
                    Err::<Option<u8>, _>(io::Error::from_raw_os_error(libc::EMFILE))
                },
                |_| panic!("nothing was accepted"),
                &failing,
                &mut |m| lines.push(m.to_string()),
            );
            assert_eq!(action, PostAction::Disable, "a pending error must not spin the loop");
            assert_eq!(calls, 1, "no retry within the same pass");
        }
        assert_eq!(lines.len(), 1, "logged once while it keeps failing: {lines:?}");
        assert!(lines[0].contains("paused"), "{lines:?}");
        // It recovers: everything pending is taken, the failure is forgotten.
        let mut queue = vec![Ok(None), Ok(Some(2u8)), Ok(Some(1))];
        let mut taken = Vec::new();
        let action = drain_listener(|| queue.pop().unwrap(), |c| taken.push(c), &failing, &mut |m| lines.push(m.to_string()));
        assert_eq!((action, taken), (PostAction::Continue, vec![1, 2]));
        assert!(!failing.get());
        assert_eq!(lines.len(), 2, "one line when it accepts again: {lines:?}");
    }

    /// A hostile request that hits a bug in the decoder: the connection
    /// fails, the bar does not.
    #[test]
    fn a_panic_while_reading_a_request_is_a_failed_request() {
        let progress = guarded_progress(|| panic!("decoder bug"));
        assert!(matches!(progress, Progress::Failed(ref m) if m == "internal error"), "{progress:?}");
        assert!(matches!(guarded_progress(|| Progress::Pending), Progress::Pending));
    }

    /// Mirrors `a_burst_of_messages_redraws_only_once`, for the launcher's
    /// windows: a burst of several `Msg::Compositor` messages in the same
    /// `dispatch()` pass must feed the launcher once, never once per
    /// message; a pass with none at all must not feed it again on a stale
    /// flag.
    struct WindowsCounter {
        dirty: bool,
        feeds: u32,
    }

    impl WindowsFeed for WindowsCounter {
        fn windows_dirty_mut(&mut self) -> &mut bool {
            &mut self.dirty
        }
        fn feed_windows_now(&mut self) {
            self.feeds += 1;
        }
    }

    #[test]
    fn a_burst_of_compositor_messages_feeds_the_launcher_once() {
        let mut c = WindowsCounter { dirty: false, feeds: 0 };
        for i in 0..9 {
            let m = if i % 2 == 0 { Msg::Compositor(Some(example())) } else { Msg::Clock(BEFORE) };
            note_compositor_message(&mut c, &m);
        }
        after_dispatch_windows(&mut c);
        assert_eq!(c.feeds, 1, "a burst of several compositor messages: one re-rank, not N");
        assert!(!c.dirty, "the flag drops back down after the feed");
        // No compositor message in this pass: the stale flag from the
        // previous pass must not still be set, and nothing is fed again.
        note_compositor_message(&mut c, &Msg::Clock(BEFORE));
        after_dispatch_windows(&mut c);
        assert_eq!(c.feeds, 1, "nothing compositor-related this pass: no re-rank");
    }
}
