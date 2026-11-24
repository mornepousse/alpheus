//! Opening an external tool (wiremix, nmtui, bluetui, or a systemd action that asks for a password)
//! inside the control center's own window.
//!
//! The control center gives up the terminal (`Screen::suspend`), the tool runs in the foreground
//! (`Launcher::run`), then the control center takes its screen back (`Screen::resume`). `open` is
//! that sequence; `open_noticed` adds a line printed first and a wait for Enter after a failure, so
//! the error is read before the screen is redrawn. `ProcessLauncher` and `CrosstermScreen` are the
//! real ones; the sequence is tested with a fake screen and a fake launcher.
//!
//! Never:
//! - open a real terminal or run a session tool in a test: the order of the sequence is tested with
//!   the fakes; the real `ProcessLauncher` only runs harmless programs (`sh -c true`, `exit 4`, a
//!   missing name, a `kill -INT $PPID`), see `real_launcher` and the Ctrl+C test;
//! - leave the control center killed by the tool's Ctrl+C: `ProcessLauncher` swaps the SIGINT and
//!   SIGQUIT handlers for an empty one for the length of the run.
//!
//! See: docs/features/control-center.md.

use std::io;
use std::process::Command;
use std::sync::Mutex;

pub trait Screen {
    /// Leaves the alternate screen, raw mode and mouse capture.
    fn suspend(&mut self) -> io::Result<()>;
    /// Restores them.
    fn resume(&mut self) -> io::Result<()>;
    /// Writes one line on the plain terminal (between `suspend` and `resume`).
    fn say(&mut self, line: &str) -> io::Result<()>;
    /// Blocks until the user presses Enter (between `suspend` and `resume`).
    fn wait_enter(&mut self) -> io::Result<()>;
}

/// Runs a program in the foreground.
pub trait Launcher {
    /// Runs `argv` in the foreground and waits for it to finish.
    fn run(&mut self, argv: &[String]) -> Result<(), String>;
}

/// Opens `argv` in the window: suspends the screen, runs it, resumes the screen. An empty command, a
/// screen that cannot be given up, a screen that is not restored, or the tool failing, each is an
/// error message for the user. A screen that is not restored is reported even if the tool ran.
pub fn open(screen: &mut dyn Screen, launcher: &mut dyn Launcher, argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("empty command in config.toml".into());
    }
    screen.suspend().map_err(|e| format!("could not give up the terminal: {e}"))?;
    let ran = launcher.run(argv);
    match screen.resume() {
        Err(e) => Err(format!("alpheus's screen was not restored: {e}")),
        Ok(()) => ran,
    }
}

/// `open`, for a command that may need the user's eyes: `intro` is printed before it runs (what is
/// being run, and how to stop waiting), and a failure is printed and waits for Enter before the
/// alternate screen comes back, so the error is read rather than wiped.
pub fn open_noticed(screen: &mut dyn Screen, launcher: &mut dyn Launcher, argv: &[String], intro: &str) -> Result<(), String> {
    if argv.is_empty() {
        return Err("empty command".into());
    }
    screen.suspend().map_err(|e| format!("could not give up the terminal: {e}"))?;
    let _ = screen.say(intro);
    let ran = launcher.run(argv);
    if let Err(e) = &ran {
        let _ = screen.say(&format!("{e}. Press Enter to go back to alpheus."));
        let _ = screen.wait_enter();
    }
    match screen.resume() {
        Err(e) => Err(format!("alpheus's screen was not restored: {e}")),
        Ok(()) => ran,
    }
}

/// A signal handler that does nothing: see `SIGNALS` and `ProcessLauncher::run`.
extern "C" fn ignore_signal(_: libc::c_int) {}

/// The SIGINT/SIGQUIT handlers are GLOBAL process state (not data owned by
/// `ProcessLauncher`): two parallel calls to `run` (only in tests; alpheus
/// never launches more than one at a time) would fight over the same
/// handler. Without this lock, one call could restore the original handler
/// while the other's child is still sending its SIGINT to the group,
/// killing the whole binary instead of just the intended tool. The lock is
/// held for the whole call (install → wait for the child → restore), which
/// also serializes the two tests that used to run it in parallel.
static SIGNALS: Mutex<()> = Mutex::new(());

/// The real launcher: runs the program with `std::process::Command` and waits for it.
pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn run(&mut self, argv: &[String]) -> Result<(), String> {
        let Some((prog, args)) = argv.split_first() else {
            return Err("empty command".into());
        };
        // A poisoned mutex (a previous call panicked while holding the
        // lock) must not block later launches: the protected data is `()`,
        // a poisoning can't corrupt anything in it.
        let _guard = SIGNALS.lock().unwrap_or_else(|e| e.into_inner());
        // Outside raw mode, Ctrl+C in the tool sends SIGINT to the whole
        // foreground group, alpheus included. An EMPTY handler protects
        // alpheus; the tool itself gets the default behavior back at its
        // own exec (a handler is not inherited, unlike SIG_IGN).
        let handler = ignore_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: signal(2) with an empty handler (async-signal-safe).
        let (old_int, old_quit) = unsafe { (libc::signal(libc::SIGINT, handler), libc::signal(libc::SIGQUIT, handler)) };
        let status = Command::new(prog).args(args).status();
        // SAFETY: restores the previous handlers.
        unsafe {
            libc::signal(libc::SIGINT, old_int);
            libc::signal(libc::SIGQUIT, old_quit);
        }
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("{prog} exited with an error ({s})")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!("{prog} not found")),
            Err(e) => Err(format!("{prog}: {e}")),
        }
    }
}

/// The real screen, driven through crossterm.
pub struct CrosstermScreen;

impl Screen for CrosstermScreen {
    fn suspend(&mut self) -> io::Result<()> {
        use ratatui::crossterm::{cursor, event, execute, terminal};
        execute!(io::stdout(), event::DisableMouseCapture, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()
    }

    fn resume(&mut self) -> io::Result<()> {
        use ratatui::crossterm::{cursor, event, execute, terminal};
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, event::EnableMouseCapture, cursor::Hide)
    }

    fn say(&mut self, line: &str) -> io::Result<()> {
        use std::io::Write;
        let mut out = io::stdout();
        writeln!(out, "{line}")?;
        out.flush()
    }

    fn wait_enter(&mut self) -> io::Result<()> {
        io::stdin().read_line(&mut String::new()).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    //! Tests of the hand-over (`external_tools.rs`): the order of suspend, run and resume with a fake
    //! screen and launcher, and the real launcher on harmless programs.

    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<String>>>;

    struct FakeScreen {
        log: Log,
        fail_suspend: bool,
    }
    impl Screen for FakeScreen {
        fn suspend(&mut self) -> io::Result<()> {
            self.log.borrow_mut().push("leave screen".into());
            if self.fail_suspend { Err(io::Error::other("tty")) } else { Ok(()) }
        }
        fn resume(&mut self) -> io::Result<()> {
            self.log.borrow_mut().push("restore screen".into());
            Ok(())
        }
        fn say(&mut self, line: &str) -> io::Result<()> {
            self.log.borrow_mut().push(format!("say {line}"));
            Ok(())
        }
        fn wait_enter(&mut self) -> io::Result<()> {
            self.log.borrow_mut().push("wait enter".into());
            Ok(())
        }
    }

    struct FakeLauncher {
        log: Log,
        result: Result<(), String>,
    }
    impl Launcher for FakeLauncher {
        fn run(&mut self, argv: &[String]) -> Result<(), String> {
            self.log.borrow_mut().push(format!("run {}", argv.join(" ")));
            self.result.clone()
        }
    }

    fn fakes(fail_suspend: bool, result: Result<(), String>) -> (Log, FakeScreen, FakeLauncher) {
        let log: Log = Rc::default();
        (log.clone(), FakeScreen { log: log.clone(), fail_suspend }, FakeLauncher { log, result })
    }

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn leave_run_restore_sequence() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        assert_eq!(open(&mut s, &mut l, &argv("wiremix")), Ok(()));
        assert_eq!(*log.borrow(), vec!["leave screen", "run wiremix", "restore screen"]);
    }

    #[test]
    fn the_screen_comes_back_even_if_the_tool_fails() {
        let (log, mut s, mut l) = fakes(false, Err("wiremix not found".into()));
        assert_eq!(open(&mut s, &mut l, &argv("wiremix")), Err("wiremix not found".into()));
        assert_eq!(log.borrow().last().unwrap(), "restore screen");
    }

    #[test]
    fn nothing_runs_if_the_terminal_does_not_free_up() {
        let (log, mut s, mut l) = fakes(true, Ok(()));
        assert!(open(&mut s, &mut l, &argv("nmtui")).unwrap_err().contains("could not give up the terminal"));
        assert_eq!(*log.borrow(), vec!["leave screen"]);
    }

    #[test]
    fn an_empty_command_is_refused() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        assert!(open(&mut s, &mut l, &[]).is_err());
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn a_noticed_run_says_what_it_runs_before_running_it() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        let r = open_noticed(
            &mut s,
            &mut l,
            &argv("systemctl restart -- mnt-data.mount"),
            "Running systemctl restart -- mnt-data.mount (Ctrl+C stops waiting)",
        );
        assert_eq!(r, Ok(()));
        assert_eq!(
            *log.borrow(),
            vec![
                "leave screen",
                "say Running systemctl restart -- mnt-data.mount (Ctrl+C stops waiting)",
                "run systemctl restart -- mnt-data.mount",
                "restore screen"
            ]
        );
    }

    #[test]
    fn a_failed_noticed_run_waits_for_enter_before_the_screen_comes_back() {
        let (log, mut s, mut l) = fakes(false, Err("systemctl exited with an error (exit status: 1)".into()));
        let r = open_noticed(&mut s, &mut l, &argv("systemctl restart -- a.service"), "Running");
        assert_eq!(r, Err("systemctl exited with an error (exit status: 1)".into()));
        let log = log.borrow();
        let n = log.len();
        assert_eq!(log[n - 1], "restore screen");
        assert_eq!(log[n - 2], "wait enter");
        assert!(log[n - 3].starts_with("say ") && log[n - 3].contains("exited with an error") && log[n - 3].contains("Enter"), "{log:?}");
    }

    #[test]
    fn a_successful_noticed_run_does_not_wait() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        let _ = open_noticed(&mut s, &mut l, &argv("systemctl restart -- a.service"), "Running");
        assert!(!log.borrow().iter().any(|l| l == "wait enter"));
    }

    // Real launcher: only `sh` (no session tool).
    #[test]
    fn the_real_launcher_is_the_one_configured() {
        let mut l = ProcessLauncher;
        assert_eq!(l.run(&argv("sh -c true")), Ok(()));
        assert!(l.run(&["sh".into(), "-c".into(), "exit 4".into()]).unwrap_err().contains("4"));
        assert_eq!(l.run(&argv("alpheus-nonexistent-program")), Err("alpheus-nonexistent-program not found".into()));
    }

    #[test]
    fn alpheus_survives_the_tool_s_ctrl_c() {
        // Ctrl+C in the tool sends SIGINT to the whole foreground group,
        // alpheus included. Simulated here: the tool kills its parent with
        // SIGINT. Without protection, the whole test binary would die.
        let mut l = ProcessLauncher;
        let r = l.run(&["sh".into(), "-c".into(), "kill -INT $PPID; sleep 0.2".into()]);
        assert_eq!(r, Ok(()));
    }
}
