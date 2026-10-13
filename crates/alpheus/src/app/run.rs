//! Real loop: terminal, worker thread, events, external tools. The loop
//! itself has no unit test (covered by tests/first_render.rs and the real
//! trial run); its exit decision and the signal guard do.

use super::App;
use super::action::Tab;
use super::keymap::Keymap;
use super::worker::{self, Request, Update};
use crate::backends::{BResult, BackendError, Backends, Displays, WmSetDisplays};
use crate::config::Config;
use crate::displays::{Plan, profiles};
use crate::exec::SystemExecutor;
use crate::external_tools::{self, CrosstermScreen, ProcessLauncher};
use crate::theme::Theme;
use crate::ui;
pub use alpheus_commun::paths::{config_dir, runtime_dir};
use alpheus_commun::shutdown::{SIGNAL_RECEIVED, STOP_REQUESTED, install_signals, watch};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Maximum time a system command gets before being killed.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// Raised once `run` is done, cleanup included.
static LOOP_DONE: AtomicBool = AtomicBool::new(false);
/// Time left for the loop to exit on its own after a signal.
const GRACE: Duration = Duration::from_millis(300);

/// Display trial to undo if alpheus closes before Mae has kept it.
#[derive(Debug, Default)]
struct ExitUndo {
    /// Plan that restores the state from before the trial.
    plan: Option<Plan>,
    /// The exit rollback has happened (or there was nothing to undo):
    /// nothing can re-arm it any more.
    done: bool,
}

/// Kept up to date by the loop, read by the exit cleanup and the guard thread.
type UndoSlot = Arc<Mutex<ExitUndo>>;

/// The loop records, every turn, what closing will have to undo. Once the
/// exit rollback has happened, ignored: a slow loop (not stuck) that comes
/// back around after the grace period does not re-arm a second rollback.
fn record_undo(slot: &Mutex<ExitUndo>, plan: Option<Plan>) {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if !g.done {
        g.plan = plan;
    }
}

/// Undoes the display trial in progress, exactly once: the lock is held for
/// the whole call, so the cleanup and the guard thread never run it twice
/// nor at the same time — and whichever one arrives second waits for the
/// other's rollback to finish before continuing (so the guard only calls
/// `exit` afterward). Best-effort: every step is tried even if another one
/// fails. Bounded duration: each `wm-set` step is killed after
/// `TOOL_TIMEOUT`. Returns the rollback's failure (nothing to undo: `Ok`).
fn undo_on_exit(slot: &Mutex<ExitUndo>, displays: &dyn Displays) -> BResult<()> {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    g.done = true;
    match g.plan.take() {
        Some(plan) => displays.apply_best_effort(&plan),
        None => Ok(()),
    }
}

/// A failed exit rollback leaves Mae with the rejected settings: reported on
/// stderr (the terminal is already restored). `writeln!`, not `eprintln!`:
/// after a SIGHUP, stderr can answer EIO, and `eprintln!` would panic — in
/// `Drop`, during an unwind, that would be an abort.
fn report_rollback(r: BResult<()>, err: &mut dyn Write) {
    let msg = match r {
        Ok(()) => return,
        Err(e @ BackendError::Incomplete { .. }) => format!("alpheus: displays: {e}"),
        Err(e) => format!("alpheus: could not roll back the displays: {e}"),
    };
    let _ = writeln!(err, "{msg}");
}

/// Close-down sequence, shared by the cleanup and the guard thread: first
/// kill the tools in progress (a trial's `wm-set` included) and close their
/// executor — nothing can reapply the trial after that —, then undo it with
/// a separate executor.
fn close(kill_running: impl FnOnce(), slot: &Mutex<ExitUndo>, displays: &dyn Displays) {
    kill_running();
    report_rollback(undo_on_exit(slot, displays), &mut io::stderr());
}

/// Starts the guard thread. Window closed (SIGHUP): crossterm can spin
/// forever in `event::poll` on the hung-up terminal (read → 0/EIO, retried
/// in a loop): the loop never sees the flag again. The guard thread does
/// not depend on crossterm: once the grace period has passed, it kills the
/// tools in progress, undoes a display trial (or waits for the cleanup to
/// finish doing so) and ends the process.
fn spawn_guard(exec: Arc<SystemExecutor>, undo: UndoSlot, displays: Arc<dyn Displays>) {
    std::thread::spawn(move || {
        watch(&STOP_REQUESTED, &LOOP_DONE, GRACE, Duration::from_millis(50), || {
            close(|| exec.kill_running(), &undo, &*displays); // no tool left behind
            std::process::exit(128 + SIGNAL_RECEIVED.load(Ordering::SeqCst));
        });
    });
}

/// The session's compositor, in the daemon's order (nixos-config's `compositor()`, niri last):
/// SWAYSOCK, HYPRLAND_INSTANCE_SIGNATURE, MANGO_INSTANCE_SIGNATURE, NIRI_SOCKET. An empty
/// variable counts as unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Session {
    Sway,
    Hyprland,
    Mango,
    Niri,
    Unknown,
}

pub(crate) fn session(var: impl Fn(&str) -> Option<OsString>) -> Session {
    let set = |k: &str| var(k).is_some_and(|v| !v.is_empty());
    if set("SWAYSOCK") {
        Session::Sway
    } else if set("HYPRLAND_INSTANCE_SIGNATURE") {
        Session::Hyprland
    } else if set("MANGO_INSTANCE_SIGNATURE") {
        Session::Mango
    } else if set("NIRI_SOCKET") {
        Session::Niri
    } else {
        Session::Unknown
    }
}

impl Session {
    /// The name the Displays tab shows where it calls nothing (`wm-set outputs` refuses there):
    /// Sway and niri. Hyprland, Mango (the tab works there once nixos-config's `wm-set` writes its
    /// monitors.conf) and an unknown compositor go through `wm-set`, as before.
    pub(crate) fn displays_unavailable(self) -> Option<&'static str> {
        match self {
            Session::Sway => Some("Sway"),
            Session::Niri => Some("niri"),
            Session::Hyprland | Session::Mango | Session::Unknown => None,
        }
    }
}

/// Should the loop stop? ("q" or a stop signal received.)
fn should_exit(app: &App, done: &AtomicBool) -> bool {
    app.should_quit || done.load(Ordering::SeqCst)
}

/// Command-line options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Tab shown on startup (`--tab <name>`).
    pub tab: Tab,
}

/// `alpheus [--tab <name>]`, also `--tab=<name>`. The error is a message
/// for the user: main shows it and exits with code 2.
pub fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut tab = Tab::Home;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let value = if a == "--tab" {
            i += 1;
            args.get(i).map(String::as_str).ok_or("--tab expects a tab name")?
        } else if let Some(v) = a.strip_prefix("--tab=") {
            v
        } else {
            return Err(format!("unknown option: \"{a}\" (usage: alpheus [--tab <name>])"));
        };
        tab = Tab::from_slug(value).ok_or_else(|| {
            let names: Vec<&str> = Tab::ALL.iter().map(|t| t.slug()).collect();
            format!("unknown tab: \"{value}\" (expected: {})", names.join(", "))
        })?;
        i += 1;
    }
    Ok(Options { tab })
}

/// `unavailable`: the Displays tab's compositor where it calls nothing (Sway, niri), set before
/// the first reads leave.
pub fn build_app(dir: &Path, tab: Tab, unavailable: Option<&'static str>, now: Instant) -> App {
    let (config, w_config) = Config::load(&dir.join("config.toml"));
    // The mode the daemon publishes (themes task 6), read only in mode auto; no D-Bus here.
    let runtime = alpheus_commun::theme::mode_file::runtime_from(|k| std::env::var_os(k));
    let (theme, w_theme) = Theme::load(dir, runtime.as_deref());
    let (keymap, w_keys) = match Keymap::from_config(&config.keys) {
        Ok(k) => (k, None),
        Err(e) => (Keymap::default(), Some(format!("keys: {e}"))),
    };
    let displays_toml = dir.join("displays.toml");
    let (display_profiles, w_displays) = profiles::load(&displays_toml);
    let mut app = App::with_tab(config, theme, keymap, tab, now);
    app.set_displays_unavailable(unavailable);
    app.displays.profiles = display_profiles;
    app.displays.profiles_path = Some(displays_toml);
    // Unreadable or invalid file: saving would overwrite it without having
    // read it, refused until Mae has fixed it.
    app.displays.profiles_invalid = w_displays.is_some();
    app.appearance.dir = Some(dir.to_path_buf());
    app.appearance.runtime = runtime;
    if app.tab == Tab::Appearance {
        app.appearance_open();
    }
    for w in [w_config, w_keys, w_displays].into_iter().flatten().chain(w_theme) {
        app.warn(w);
    }
    app
}

/// Cleanup guard: as long as it's alive, its destruction (normal return of
/// `run`, an early `?` such as `EnableMouseCapture`'s, or a panic unwinding
/// the stack) restores the terminal, kills any tool still running and
/// undoes an unkept display trial. `Drop` runs in every one of these cases,
/// unlike code placed after an exit point, which a `?` or a panic can skip.
struct Cleanup {
    exec: Arc<SystemExecutor>,
    undo: UndoSlot,
    displays: Arc<dyn Displays>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
        close(|| self.exec.kill_running(), &self.undo, &*self.displays);
    }
}

pub fn run(options: Options) -> io::Result<()> {
    install_signals()?;
    // Under Sway and niri, the Displays tab calls nothing (`wm-set outputs` refuses there).
    let unavailable = session(|k| std::env::var_os(k)).displays_unavailable();
    let app = build_app(&config_dir(), options.tab, unavailable, Instant::now());
    let exec = Arc::new(SystemExecutor::new(TOOL_TIMEOUT));
    let (tx, rx) = worker::spawn(Backends::system(exec.clone(), runtime_dir()));
    // Separate executor to undo a display trial on close: the worker
    // thread's own executor is closed (kill_running) right before.
    let exit_displays: Arc<dyn Displays> = Arc::new(WmSetDisplays::new(Arc::new(SystemExecutor::new(TOOL_TIMEOUT))));
    let undo: UndoSlot = Arc::default();
    spawn_guard(exec.clone(), undo.clone(), exit_displays.clone());
    let r = {
        let mut terminal = ratatui::init(); // also installs terminal restoration on panic
        let _cleanup = Cleanup { exec, undo: undo.clone(), displays: exit_displays };
        execute!(io::stdout(), EnableMouseCapture).and_then(|_| event_loop(&mut terminal, app, &tx, &rx, &undo))
    }; // Cleanup runs here
    LOOP_DONE.store(true, Ordering::SeqCst);
    r
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    tx: &Sender<Request>,
    rx: &Receiver<Update>,
    undo: &Mutex<ExitUndo>,
) -> io::Result<()> {
    loop {
        // Display trial in progress: what closing will have to undo.
        // Recorded before the turn's requests are sent: a trial is never
        // sent to the worker thread without its rollback already being
        // recorded here.
        record_undo(undo, app.exit_undo());
        if should_exit(&app, &STOP_REQUESTED) {
            return Ok(()); // the Cleanup guard restores the terminal and kills the tools
        }
        for r in app.take_requests() {
            let _ = tx.send(r);
        }
        let mut hits = ui::HitMap::default();
        terminal.draw(|f| hits = ui::render(f, &app))?;
        if app.should_quit {
            return Ok(());
        }
        if let Some(ext) = app.take_external() {
            let r = external_tools::open(&mut CrosstermScreen, &mut ProcessLauncher, &app.external_argv(ext));
            terminal.clear()?;
            app.external_done(ext, r, Instant::now());
            continue;
        }
        if event::poll(Duration::from_millis(100))? {
            let now = Instant::now();
            match event::read()? {
                Event::Key(k) => app.on_key(k, now),
                Event::Mouse(m) => app.on_mouse(m, &hits, now),
                _ => {} // resize: redrawn on the next turn
            }
        }
        while let Ok(u) = rx.try_recv() {
            app.on_update(u, Instant::now());
        }
        app.on_tick(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::ready_app;

    use crate::displays::Step;
    use crate::exec::fake::FakeExecutor;

    /// No polling for `forced_exit_only_after_the_rollback` (the only test
    /// here that still calls `watch` directly).
    const STEP_DELAY: Duration = Duration::from_millis(5);

    fn rollback_plan() -> Plan {
        Plan { steps: vec![Step { output: "HDMI-A-1".into(), active: None }] }
    }

    fn slot_with(plan: Option<Plan>) -> Mutex<ExitUndo> {
        let s = Mutex::new(ExitUndo::default());
        record_undo(&s, plan);
        s
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn command_line_options() {
        assert_eq!(parse_options(&[]), Ok(Options { tab: Tab::Home }));
        assert_eq!(parse_options(&args(&["--tab", "sound"])), Ok(Options { tab: Tab::Sound }));
        assert_eq!(parse_options(&args(&["--tab=Network"])), Ok(Options { tab: Tab::Network }));
        assert_eq!(parse_options(&args(&["--tab", "displays"])), Ok(Options { tab: Tab::Displays }));
        let e = parse_options(&args(&["--tab", "weather"])).unwrap_err();
        assert!(e.contains("weather") && e.contains("home, sound"), "{e}");
        assert!(parse_options(&args(&["--tab"])).unwrap_err().contains("expects"));
        assert!(parse_options(&args(&["-v"])).unwrap_err().contains("unknown option"));
    }

    #[test]
    fn opening_on_a_tab_only_reads_its_domains() {
        let dir = std::env::temp_dir().join("alpheus-tab-no-config");
        let mut a = build_app(&dir, Tab::Sound, None, Instant::now());
        assert_eq!(a.tab, Tab::Sound);
        assert_eq!(a.take_requests(), vec![Request::Refresh(worker::domains(Tab::Sound).to_vec())]);
    }

    /// Spec § 4: under Sway or niri the Displays tab calls nothing, opening on it included
    /// (`alpheus --tab displays`): no read of the displays queued, so no `wm-set outputs`.
    #[test]
    fn opening_on_the_displays_tab_under_sway_or_niri_reads_nothing() {
        let dir = std::env::temp_dir().join("alpheus-tab-no-config");
        for name in ["Sway", "niri"] {
            let mut a = build_app(&dir, Tab::Displays, Some(name), Instant::now());
            assert_eq!(a.displays.unavailable, Some(name));
            assert_eq!(a.take_requests(), vec![], "{name}");
        }
        let mut a = build_app(&dir, Tab::Displays, None, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Refresh(worker::domains(Tab::Displays).to_vec())], "elsewhere: read");
    }

    #[test]
    fn display_rollback_on_close_only_once() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_with(Some(rollback_plan()));
        let _ = undo_on_exit(&slot, &d); // exit cleanup
        let _ = undo_on_exit(&slot, &d); // then the guard thread: nothing left to redo
        assert_eq!(f.calls(), vec!["wm-set monitor-disable HDMI-A-1"]);
        assert_eq!(slot.lock().unwrap().plan, None);
        let _ = undo_on_exit(&slot_with(None), &d);
        assert_eq!(f.calls().len(), 1, "no trial: no call");
    }

    #[test]
    fn the_loop_does_not_rearm_an_already_done_rollback() {
        // Slow loop but not stuck (external tool closed by the same
        // SIGHUP): the guard has already undone the trial, the loop comes
        // back around and rewrites the trial still present in the App; the
        // cleanup that follows must not undo it a second time.
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_with(Some(rollback_plan()));
        let _ = undo_on_exit(&slot, &d); // guard thread
        record_undo(&slot, Some(rollback_plan())); // the loop comes back around
        let _ = undo_on_exit(&slot, &d); // cleanup
        assert_eq!(f.calls().len(), 1, "a single rollback: {:?}", f.calls());
    }

    #[test]
    fn trial_kept_or_nothing_to_undo_on_close() {
        let f = Arc::new(FakeExecutor::new());
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_with(Some(rollback_plan()));
        record_undo(&slot, None); // Mae kept the settings
        let _ = undo_on_exit(&slot, &d);
        assert!(f.calls().is_empty(), "{:?}", f.calls());
    }

    #[test]
    fn close_kills_the_tools_before_the_rollback() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_with(Some(rollback_plan()));
        let mut calls_at_kill = None;
        close(|| calls_at_kill = Some(f.calls().len()), &slot, &d);
        assert_eq!(calls_at_kill, Some(0), "kill_running first");
        assert_eq!(f.calls().len(), 1, "then the rollback");
    }

    #[test]
    fn rollback_impossible_is_reported() {
        // wm-set fails (here: an unprogrammed response, "not found").
        let f = Arc::new(FakeExecutor::new());
        let d = WmSetDisplays::new(f.clone());
        let r = undo_on_exit(&slot_with(Some(rollback_plan())), &d);
        assert!(r.is_err(), "the rollback's failure propagates: {r:?}");
        let mut err = Vec::new();
        report_rollback(r, &mut err);
        let msg = String::from_utf8(err).unwrap();
        assert_eq!(msg, "alpheus: displays: incomplete rollback: HDMI-A-1 (wm-set not found)\n");
        let mut err = Vec::new();
        report_rollback(Err(BackendError::Timeout { tool: "wm-set".into() }), &mut err);
        assert_eq!(String::from_utf8(err).unwrap(), "alpheus: could not roll back the displays: wm-set is not responding\n");
        let mut err = Vec::new();
        report_rollback(undo_on_exit(&slot_with(None), &d), &mut err);
        assert!(err.is_empty(), "nothing to undo: nothing to say");
    }

    #[test]
    fn close_rollback_is_best_effort() {
        // Decision D2: a failed rollback step does not stop the others.
        let plan = Plan { steps: vec![Step { output: "DVI-I-1".into(), active: None }, Step { output: "HDMI-A-1".into(), active: None }] };
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", ""); // DVI-I-1 not programmed: failure
        let d = WmSetDisplays::new(f.clone());
        let r = undo_on_exit(&slot_with(Some(plan)), &d);
        assert_eq!(f.calls(), vec!["wm-set monitor-disable DVI-I-1", "wm-set monitor-disable HDMI-A-1"]);
        let mut err = Vec::new();
        report_rollback(r, &mut err);
        assert_eq!(String::from_utf8(err).unwrap(), "alpheus: displays: incomplete rollback: DVI-I-1 (wm-set not found)\n");
    }

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| pairs.iter().find(|(c, _)| *c == k).map(|(_, v)| OsString::from(v))
    }

    /// The daemon's order; an empty variable counts as unset. Sway and niri: the Displays tab
    /// calls nothing; Hyprland, Mango and an unknown compositor use wm-set.
    #[test]
    fn the_session_is_detected_in_the_daemons_order() {
        let all: &'static [(&str, &str)] = &[
            ("SWAYSOCK", "/run/user/1000/sway-ipc.1000.42.sock"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
            ("MANGO_INSTANCE_SIGNATURE", "/run/user/1000/mango-42.sock"),
            ("NIRI_SOCKET", "/run/user/1000/niri.sock"),
        ];
        assert_eq!(session(vars(all)), Session::Sway);
        assert_eq!(session(vars(&all[1..])), Session::Hyprland);
        assert_eq!(session(vars(&all[2..])), Session::Mango);
        assert_eq!(session(vars(&all[3..])), Session::Niri);
        assert_eq!(session(vars(&[])), Session::Unknown);
        assert_eq!(session(vars(&[("SWAYSOCK", ""), ("NIRI_SOCKET", "/run/user/1000/niri.sock")])), Session::Niri, "empty: unset");
        assert_eq!(
            session(vars(&[("NIRI_SOCKET", "/run/user/1000/niri.sock"), ("HYPRLAND_INSTANCE_SIGNATURE", "abc_123")])),
            Session::Hyprland,
            "a niri socket left over in a Hyprland session's environment"
        );
        let unavailable =
            [Session::Sway, Session::Hyprland, Session::Mango, Session::Niri, Session::Unknown].map(Session::displays_unavailable);
        assert_eq!(unavailable, [Some("Sway"), None, None, Some("niri"), None]);
    }

    #[test]
    fn the_guard_waits_for_a_rollback_in_progress() {
        // The cleanup applies a slow rollback; the guard, triggered during
        // it, waits for it to finish instead of running it twice (or
        // killing alpheus halfway through).
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(200)));
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d: Arc<dyn Displays> = Arc::new(WmSetDisplays::new(f.clone()));
        let slot: UndoSlot = Arc::new(slot_with(Some(rollback_plan())));
        let (s2, d2) = (slot.clone(), d.clone());
        let h = std::thread::spawn(move || {
            let _ = undo_on_exit(&s2, &*d2);
        });
        let t0 = Instant::now();
        while f.calls().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(2), "the rollback did not start");
            std::thread::yield_now();
        }
        let t = Instant::now();
        let _ = undo_on_exit(&slot, &*d);
        assert!(t.elapsed() >= Duration::from_millis(100), "the guard waited ({:?})", t.elapsed());
        h.join().unwrap();
        assert_eq!(f.calls().len(), 1, "a single rollback");
    }

    #[test]
    fn forced_exit_only_after_the_rollback() {
        // Signal received, cleanup mid-rollback and the rollback is slower
        // than the grace period: the guard's forced exit only goes through
        // once the rollback is done — `exit` never cuts it short.
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(250)));
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d: Arc<dyn Displays> = Arc::new(WmSetDisplays::new(f.clone()));
        let slot: UndoSlot = Arc::new(slot_with(Some(rollback_plan())));
        let (s2, d2) = (slot.clone(), d.clone());
        let h = std::thread::spawn(move || close(|| {}, &s2, &*d2)); // Cleanup::drop
        while f.calls().is_empty() {
            std::thread::yield_now();
        }
        let rollback_start = Instant::now();
        let (done, finished) = (AtomicBool::new(true), AtomicBool::new(false));
        let mut exit_after = None;
        let forced = watch(&done, &finished, Duration::from_millis(20), STEP_DELAY, || {
            close(|| {}, &slot, &*d);
            exit_after = Some(rollback_start.elapsed()); // here: process::exit
        });
        assert!(forced, "stuck loop: forced exit");
        let exit_after = exit_after.unwrap();
        assert!(exit_after >= Duration::from_millis(200), "exit before the rollback finished ({exit_after:?})");
        h.join().unwrap();
        assert_eq!(f.calls().len(), 1, "a single rollback");
    }

    #[test]
    fn display_profiles_loaded_at_startup() {
        let dir = std::env::temp_dir().join(format!("alpheus-{}-run-displays", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = build_app(&dir, Tab::Home, None, Instant::now());
        assert!(a.displays.profiles.is_empty());
        assert_eq!(a.status.warning, None, "missing file: no message");
        assert!(!a.displays.profiles_invalid, "missing: saving will create it");
        std::fs::write(dir.join("displays.toml"), "[[profile]]\nname = \"Bureau\"\n").unwrap();
        let a = build_app(&dir, Tab::Home, None, Instant::now());
        assert_eq!(a.displays.profiles.len(), 1);
        assert_eq!(a.displays.profiles_path, Some(dir.join("displays.toml")));
        assert_eq!(a.status.warning, None);
        assert!(!a.displays.profiles_invalid);
        std::fs::write(dir.join("displays.toml"), "[[profile]\n").unwrap();
        let a = build_app(&dir, Tab::Home, None, Instant::now());
        assert!(a.displays.profiles.is_empty());
        assert!(a.status.warning.as_deref().unwrap_or("").starts_with("invalid displays.toml"), "{:?}", a.status.warning);
        assert!(a.displays.profiles_invalid, "invalid file: saving refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_decision() {
        let mut a = ready_app();
        let done = AtomicBool::new(false);
        assert!(!should_exit(&a, &done));
        done.store(true, Ordering::SeqCst);
        assert!(should_exit(&a, &done), "signal received: the loop exits");
        done.store(false, Ordering::SeqCst);
        a.should_quit = true;
        assert!(should_exit(&a, &done));
    }
}
