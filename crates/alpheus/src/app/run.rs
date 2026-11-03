//! Real loop: terminal, worker thread, events, external tools. The loop
//! itself has no unit test (covered by tests/first_render.rs and the real
//! trial run); its exit decision and what closing does with a display trial do.

use super::App;
use super::action::Tab;
use super::keymap::Keymap;
use super::worker::{self, Request, Update};
use crate::backends::{Backends, Displays};
use crate::config::Config;
use crate::displays::profiles;
use crate::exec::SystemExecutor;
use crate::external_tools::{self, CrosstermScreen, ProcessLauncher};
use crate::theme::Theme;
use crate::ui;
pub use alpheus_commun::paths::{config_dir, runtime_dir};
use alpheus_commun::shutdown::{SIGNAL_RECEIVED, STOP_REQUESTED, install_signals, watch};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Maximum time a system command gets before being killed.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// Raised once `run` is done, cleanup included.
static LOOP_DONE: AtomicBool = AtomicBool::new(false);
/// Time left for the loop to exit on its own after a signal.
const GRACE: Duration = Duration::from_millis(300);

/// Closing with a display trial waiting for Keep: the daemon is asked to revert it now. Best
/// effort, nothing reported: the daemon's own 15 s deadline is the safety net (a closed window, a
/// killed alpheus).
fn revert_on_close(app: &App, displays: &dyn Displays) {
    if app.trial_open() {
        let _ = displays.revert();
    }
}

/// Starts the guard thread. Window closed (SIGHUP): crossterm can spin
/// forever in `event::poll` on the hung-up terminal (read → 0/EIO, retried
/// in a loop): the loop never sees the flag again. The guard thread does
/// not depend on crossterm: once the grace period has passed, it kills the
/// tools in progress and ends the process.
fn spawn_guard(exec: Arc<SystemExecutor>) {
    std::thread::spawn(move || {
        watch(&STOP_REQUESTED, &LOOP_DONE, GRACE, Duration::from_millis(50), || {
            exec.kill_running(); // no tool left behind
            std::process::exit(128 + SIGNAL_RECEIVED.load(Ordering::SeqCst));
        });
    });
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

pub fn build_app(dir: &Path, tab: Tab, now: Instant) -> App {
    let (config, w_config) = Config::load(&dir.join("config.toml"));
    // The mode the daemon publishes (read from `$XDG_RUNTIME_DIR/alpheus/mode`), read only in mode auto; no D-Bus here.
    let runtime = alpheus_commun::theme::mode_file::runtime_from(|k| std::env::var_os(k));
    let (theme, w_theme) = Theme::load(dir, runtime.as_deref());
    let (keymap, w_keys) = match Keymap::from_config(&config.keys) {
        Ok(k) => (k, None),
        Err(e) => (Keymap::default(), Some(format!("keys: {e}"))),
    };
    let displays_toml = dir.join("displays.toml");
    let (display_profiles, w_displays) = profiles::load(&displays_toml);
    let mut app = App::with_tab(config, theme, keymap, tab, now);
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
/// the stack) restores the terminal and kills any tool still running.
/// `Drop` runs in every one of these cases, unlike code placed after an exit
/// point, which a `?` or a panic can skip.
struct Cleanup {
    exec: Arc<SystemExecutor>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
        self.exec.kill_running();
    }
}

pub fn run(options: Options) -> io::Result<()> {
    install_signals()?;
    let app = build_app(&config_dir(), options.tab, Instant::now());
    let exec = Arc::new(SystemExecutor::new(TOOL_TIMEOUT));
    let backends = Backends::system(exec.clone(), runtime_dir());
    let displays = backends.displays.clone();
    let (tx, rx) = worker::spawn(backends);
    spawn_guard(exec.clone());
    let r = {
        let mut terminal = ratatui::init(); // also installs terminal restoration on panic
        let _cleanup = Cleanup { exec };
        execute!(io::stdout(), EnableMouseCapture).and_then(|_| event_loop(&mut terminal, app, &tx, &rx, &*displays))
    }; // Cleanup runs here
    LOOP_DONE.store(true, Ordering::SeqCst);
    r
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    tx: &Sender<Request>,
    rx: &Receiver<Update>,
    displays: &dyn Displays,
) -> io::Result<()> {
    loop {
        if should_exit(&app, &STOP_REQUESTED) {
            revert_on_close(&app, displays);
            return Ok(()); // the Cleanup guard restores the terminal and kills the tools
        }
        for r in app.take_requests() {
            let _ = tx.send(r);
        }
        let mut hits = ui::HitMap::default();
        terminal.draw(|f| hits = ui::render(f, &app))?;
        if app.should_quit {
            revert_on_close(&app, displays);
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

    use crate::app::testing::FakeDisplays;
    use crate::app::worker::{Domain, Op, fetch};
    use crate::exec::fake::FakeExecutor;

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
        let mut a = build_app(&dir, Tab::Sound, Instant::now());
        assert_eq!(a.tab, Tab::Sound);
        assert_eq!(a.take_requests(), vec![Request::Refresh(Tab::Sound.domains().to_vec())]);
    }

    /// The daemon decides where the tab works (displays protocol spec): under Sway as under any
    /// compositor, opening on the tab reads the displays, and what the daemon answers fills it.
    #[test]
    fn the_tab_is_available_under_sway() {
        let dir = alpheus_commun::temp_path::TempPath::new("run-tab-under-sway");
        let mut a = build_app(&dir, Tab::Displays, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])]);
        let mut b = Backends::system(Arc::new(FakeExecutor::new()), dir.to_path_buf());
        let fake = Arc::new(FakeDisplays::default());
        b.displays = fake.clone();
        a.on_update(fetch(&b, Domain::Displays), Instant::now());
        assert_eq!(a.items().len(), 4, "the profile bar and three outputs: {:?}", a.items());
        a.toggle_output("HDMI-A-1", Instant::now());
        assert!(a.trial_open(), "a change is tried");
    }

    /// Closing with a trial waiting reverts it in the daemon at once; a kept trial, or none, sends
    /// nothing.
    #[test]
    fn closing_mid_trial_sends_revert() {
        let mut a = ready_app();
        a.go_tab(Tab::Displays, Instant::now());
        a.toggle_output("HDMI-A-1", Instant::now());
        let fake = FakeDisplays::default();
        revert_on_close(&a, &fake);
        assert_eq!(fake.calls(), vec!["revert"]);
        // Best effort: a daemon gone changes nothing to the close.
        *fake.answer.lock().unwrap() = Err("the alpheus daemon is not running".into());
        revert_on_close(&a, &fake);
        assert_eq!(fake.calls().len(), 2);

        let fake = FakeDisplays::default();
        revert_on_close(&ready_app(), &fake);
        let t0 = Instant::now();
        let tried = a.take_requests().into_iter().find_map(|r| match r {
            Request::Do(op @ Op::TryDisplays(_)) => Some(op),
            _ => None,
        });
        a.on_update(Update::Done(tried.expect("a try sent"), Ok(())), t0);
        a.keep_answer(true, t0);
        revert_on_close(&a, &fake);
        assert!(fake.calls().is_empty(), "no trial, or kept: {:?}", fake.calls());
    }

    #[test]
    fn display_profiles_loaded_at_startup() {
        let dir = alpheus_commun::temp_path::TempPath::dir("run-displays");
        let a = build_app(&dir, Tab::Home, Instant::now());
        assert!(a.displays.profiles.is_empty());
        assert_eq!(a.status.warning, None, "missing file: no message");
        assert!(!a.displays.profiles_invalid, "missing: saving will create it");
        std::fs::write(dir.join("displays.toml"), "[[profile]]\nname = \"Bureau\"\n").unwrap();
        let a = build_app(&dir, Tab::Home, Instant::now());
        assert_eq!(a.displays.profiles.len(), 1);
        assert_eq!(a.displays.profiles_path, Some(dir.join("displays.toml")));
        assert_eq!(a.status.warning, None);
        assert!(!a.displays.profiles_invalid);
        std::fs::write(dir.join("displays.toml"), "[[profile]\n").unwrap();
        let a = build_app(&dir, Tab::Home, Instant::now());
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
