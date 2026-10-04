//! External tools (wiremix, nmtui, bluetui) opened INSIDE alpheus's window:
//! alpheus gives up the terminal, the tool runs in the foreground, then
//! alpheus takes its screen back. The sequence is tested with a fake screen
//! and launcher; no real terminal in tests.

use std::io;
use std::process::Command;
use std::sync::Mutex;

pub trait Screen {
    /// Leaves the alternate screen, raw mode and mouse capture.
    fn suspend(&mut self) -> io::Result<()>;
    /// Restores them.
    fn resume(&mut self) -> io::Result<()>;
}

pub trait Launcher {
    /// Runs `argv` in the foreground and waits for it to finish.
    fn run(&mut self, argv: &[String]) -> Result<(), String>;
}

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

/// Real launcher.
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

/// Real screen (crossterm).
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
}

#[cfg(test)]
mod tests {
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
    fn empty_command() {
        let (log, mut s, mut l) = fakes(false, Ok(()));
        assert!(open(&mut s, &mut l, &[]).is_err());
        assert!(log.borrow().is_empty());
    }

    // Real launcher: only `sh` (no session tool).
    #[test]
    fn real_launcher() {
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
