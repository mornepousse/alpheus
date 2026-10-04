//! Assembling the daemon: signals, lock, settings, fonts, Wayland, the event
//! loop (calloop), the sources the modules need (compositor, sound, D-Bus,
//! resources timer). Covered by tests/headless_session.rs (headless sway)
//! and the real-world trial; its pure decisions have their own tests.

use crate::clock;
use crate::clock::LocalTime;
use crate::config::{BarConfig, ModuleId, ResourcesConfig};
use crate::lock;
use crate::nonblocking::NonBlocking;
use crate::painter::Fonts;
use crate::sources;
use crate::sources::dbus::Domain;
use crate::sources::resources;
use crate::state::{Bar, Msg};
use crate::wayland::App;
use alpheus_commun::exec::{Executor, SystemExecutor};
use alpheus_commun::theme::Palette;
use alpheus_commun::{paths, shutdown};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::Connection;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);
static FINISHED: AtomicBool = AtomicBool::new(false);
/// Grace period left for the loop to exit on its own after a signal.
const GRACE: Duration = Duration::from_secs(1);

/// Mail received by the loop from the other threads.
pub enum Mail {
    Bar(Msg),
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
    let mut event_loop: EventLoop<App> = match EventLoop::try_new() {
        Ok(b) => b,
        Err(e) => {
            log(&format!("event loop: {e}"));
            return 1;
        }
    };
    let handle = event_loop.handle();
    if WaylandSource::new(conn.clone(), file).insert(handle.clone()).is_err() {
        log("Wayland source: could not insert");
        return 1;
    }
    let (tx, rx) = channel::channel::<Mail>();
    // Each callback (channel, clock) only raises the `dirty` flag (see
    // `DeferredRedraw`): neither one draws. A burst of several messages
    // (e.g. niri, which sends several at once) handled in the same
    // `dispatch()` pass thus redraws only once, after the `while` loop,
    // instead of once per message.
    let mail = handle.insert_source(rx, |ev, _, state: &mut App| match ev {
        channel::Event::Msg(Mail::Bar(m)) => on_message(state, m, || LocalTime::now().0),
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
    }
    drop(state); // surfaces destroyed
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
}
