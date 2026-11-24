//! Session power. Lock, log out, reboot and power off run the commands of bar.toml's `[quick]`
//! (`SessionCommands`, the same ones as the bar's power menu); an empty command hides its action.
//! They start detached, as the bar starts them; Suspend is `systemctl suspend`, run and checked.
//!
//! Never:
//! - spell a session command out in code: they are the user's (`[quick]`), with neutral defaults;
//! - call the service behind the tool: the tool is the interface, and a missing one leaves the row
//!   or the tab empty rather than failing;
//! - run anything in a test: the fake executor answers (AGENTS.md).
//!
//! See: docs/features/control-center.md, docs/install.md (the tools), docs/decisions.md ("The
//! control center's session actions use bar.toml's [quick] commands").

use super::{BResult, BackendError, run_ok, spawn_ok};
use crate::exec::Executor;
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;

/// `[quick]`'s four session commands, run as given and never through a shell. An empty one hides
/// its action. The daemon's `QuickConfig` takes its defaults from here, so the bar and the control
/// center cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCommands {
    pub lock: Vec<String>,
    pub logout: Vec<String>,
    pub reboot: Vec<String>,
    pub poweroff: Vec<String>,
}

impl Default for SessionCommands {
    /// Neutral: the lock through logind, reboot and power off through systemd, no log out.
    fn default() -> Self {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        SessionCommands {
            lock: argv(&["loginctl", "lock-session"]),
            logout: Vec::new(),
            reboot: argv(&["systemctl", "reboot"]),
            poweroff: argv(&["systemctl", "poweroff"]),
        }
    }
}

/// The `[quick]` keys this module reads; every other key of bar.toml is the daemon's business.
#[derive(Deserialize, Default)]
struct RawFile {
    quick: Option<RawQuick>,
}

#[derive(Deserialize, Default)]
struct RawQuick {
    lock: Option<Vec<String>>,
    logout: Option<Vec<String>>,
    reboot: Option<Vec<String>>,
    poweroff: Option<Vec<String>>,
}

impl SessionCommands {
    /// From bar.toml's text: a key that is absent keeps its default, one that is given (even
    /// empty) is kept as is. Unreadable text: the defaults, like the daemon's own fallback.
    pub fn from_text(s: &str) -> Result<SessionCommands, String> {
        let raw: RawFile = toml::from_str(s).map_err(|e| e.message().to_string())?;
        let q = raw.quick.unwrap_or_default();
        let d = SessionCommands::default();
        Ok(SessionCommands {
            lock: q.lock.unwrap_or(d.lock),
            logout: q.logout.unwrap_or(d.logout),
            reboot: q.reboot.unwrap_or(d.reboot),
            poweroff: q.poweroff.unwrap_or(d.poweroff),
        })
    }

    /// bar.toml at `path`. Absent: the defaults and no message; unreadable or invalid: the
    /// defaults and a message (the bar then runs on its defaults too).
    pub fn load(path: &Path) -> (SessionCommands, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (SessionCommands::default(), None),
            Err(e) => (SessionCommands::default(), Some(format!("bar.toml unreadable: {e}"))),
            Ok(s) => match SessionCommands::from_text(&s) {
                Ok(c) => (c, None),
                Err(m) => (SessionCommands::default(), Some(format!("bar.toml invalid: {m}"))),
            },
        }
    }

    /// The configured command of `a`; Suspend has none (it is fixed).
    fn command(&self, a: PowerAction) -> Option<&[String]> {
        match a {
            PowerAction::Lock => Some(&self.lock),
            PowerAction::Logout => Some(&self.logout),
            PowerAction::Reboot => Some(&self.reboot),
            PowerAction::Poweroff => Some(&self.poweroff),
            PowerAction::Suspend => None,
        }
    }

    /// Whether the Power tab offers `a`: Suspend always, the others when their command has a
    /// program (an empty one, or one whose program is blank, hides the action).
    pub fn shows(&self, a: PowerAction) -> bool {
        self.command(a).is_none_or(|c| c.first().is_some_and(|p| !p.trim().is_empty()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerAction {
    Lock,
    Logout,
    Suspend,
    Reboot,
    Poweroff,
}

impl PowerAction {
    pub const ALL: [PowerAction; 5] =
        [PowerAction::Lock, PowerAction::Logout, PowerAction::Suspend, PowerAction::Reboot, PowerAction::Poweroff];

    pub fn label(self) -> &'static str {
        match self {
            PowerAction::Lock => "Lock",
            PowerAction::Logout => "Log out",
            PowerAction::Suspend => "Suspend",
            PowerAction::Reboot => "Reboot",
            PowerAction::Poweroff => "Power off",
        }
    }

    /// Confirmation dialog's question.
    pub fn question(self) -> &'static str {
        match self {
            PowerAction::Lock => "Lock the session?",
            PowerAction::Logout => "Log out of the session?",
            PowerAction::Suspend => "Suspend?",
            PowerAction::Reboot => "Restart the computer?",
            PowerAction::Poweroff => "Shut down the computer?",
        }
    }

    /// Confirmation required: whatever closes the session or the machine.
    pub fn needs_confirm(self) -> bool {
        matches!(self, PowerAction::Logout | PowerAction::Reboot | PowerAction::Poweroff)
    }
}

pub trait Power: Send + Sync {
    fn perform(&self, a: PowerAction) -> BResult<()>;
}

pub struct SessionPower {
    exec: Arc<dyn Executor>,
    commands: SessionCommands,
}

impl SessionPower {
    pub fn new(exec: Arc<dyn Executor>, commands: SessionCommands) -> Self {
        SessionPower { exec, commands }
    }
}

impl Power for SessionPower {
    fn perform(&self, a: PowerAction) -> BResult<()> {
        let e = &*self.exec;
        let Some(argv) = self.commands.command(a) else {
            return run_ok(e, "systemctl", &["suspend"]).map(|_| ());
        };
        if !self.commands.shows(a) {
            // The tab does not offer it; a stale palette or key still ends in a sentence, not a run.
            return Err(BackendError::NotConfigured(format!("{} is not configured in bar.toml's [quick]", a.label())));
        }
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        // Started detached, like the bar starts them: the user's command is not timed (the TUI
        // would kill it after its 2 s limit) and its exit status is not waited for; only a program
        // that cannot start is reported.
        spawn_ok(e, &argv[0], &args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::fake::FakeExecutor;

    #[test]
    fn labels() {
        let l: Vec<_> = PowerAction::ALL.iter().map(|a| a.label()).collect();
        assert_eq!(l, vec!["Lock", "Log out", "Suspend", "Reboot", "Power off"]);
        assert_eq!(PowerAction::Poweroff.question(), "Shut down the computer?");
    }

    #[test]
    fn confirmation_for_logout_reboot_and_poweroff() {
        let c: Vec<_> = PowerAction::ALL.iter().filter(|a| a.needs_confirm()).copied().collect();
        assert_eq!(c, vec![PowerAction::Logout, PowerAction::Reboot, PowerAction::Poweroff]);
    }

    fn sp(f: &Arc<FakeExecutor>, c: SessionCommands) -> SessionPower {
        SessionPower::new(f.clone(), c)
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_defaults_are_neutral_and_log_out_is_absent() {
        let f = Arc::new(FakeExecutor::new());
        for l in ["loginctl lock-session", "systemctl suspend", "systemctl reboot", "systemctl poweroff"] {
            f.ok(l, "");
        }
        let p = sp(&f, SessionCommands::default());
        let shown: Vec<_> = PowerAction::ALL.iter().filter(|a| p.commands.shows(**a)).copied().collect();
        assert_eq!(shown, vec![PowerAction::Lock, PowerAction::Suspend, PowerAction::Reboot, PowerAction::Poweroff]);
        for a in shown {
            p.perform(a).unwrap();
        }
        assert_eq!(f.calls(), vec!["loginctl lock-session", "systemctl suspend", "systemctl reboot", "systemctl poweroff"]);
    }

    #[test]
    fn configured_commands_run_as_given() {
        let f = Arc::new(FakeExecutor::new());
        for l in ["screen-lock -f", "session-logout --now", "my-reboot --now", "my-off"] {
            f.ok(l, "");
        }
        let c = SessionCommands {
            lock: argv(&["screen-lock", "-f"]),
            logout: argv(&["session-logout", "--now"]),
            reboot: argv(&["my-reboot", "--now"]),
            poweroff: argv(&["my-off"]),
        };
        let p = sp(&f, c);
        for a in [PowerAction::Lock, PowerAction::Logout, PowerAction::Reboot, PowerAction::Poweroff] {
            p.perform(a).unwrap();
        }
        assert_eq!(f.calls(), vec!["screen-lock -f", "session-logout --now", "my-reboot --now", "my-off"]);
    }

    /// A user's session command starts like the bar starts it, detached: a log out that outlives
    /// the TUI's 2 s limit must not be killed, and what it exits with is not waited for.
    #[test]
    fn configured_session_commands_are_started_detached_not_run_with_a_limit() {
        let f = Arc::new(FakeExecutor::new());
        // `run` would turn this exit status into a failure; a detached start never sees it.
        f.respond("session-logout --now", Ok(crate::exec::Output::fail(1, "late")));
        f.ok("my-reboot", "");
        f.ok("my-off", "");
        let c = SessionCommands {
            logout: argv(&["session-logout", "--now"]),
            reboot: argv(&["my-reboot"]),
            poweroff: argv(&["my-off"]),
            ..SessionCommands::default()
        };
        let p = sp(&f, c);
        for a in [PowerAction::Logout, PowerAction::Reboot, PowerAction::Poweroff] {
            assert_eq!(p.perform(a), Ok(()), "{a:?}");
        }
        assert_eq!(p.perform(PowerAction::Suspend), Err(BackendError::Missing { tool: "systemctl".into() }), "suspend stays a plain run");
    }

    #[test]
    fn an_action_that_is_not_configured_says_so() {
        let f = Arc::new(FakeExecutor::new());
        let p = sp(&f, SessionCommands::default());
        assert_eq!(
            p.perform(PowerAction::Logout),
            Err(BackendError::NotConfigured("Log out is not configured in bar.toml's [quick]".into()))
        );
    }

    #[test]
    fn an_empty_or_blank_command_hides_its_action_and_runs_nothing() {
        let f = Arc::new(FakeExecutor::new());
        let c = SessionCommands { lock: vec![], logout: argv(&["  "]), reboot: vec![], poweroff: vec![] };
        let p = sp(&f, c);
        for a in [PowerAction::Lock, PowerAction::Logout, PowerAction::Reboot, PowerAction::Poweroff] {
            assert!(!p.commands.shows(a), "{a:?}");
            assert!(p.perform(a).is_err(), "{a:?}");
        }
        assert!(p.commands.shows(PowerAction::Suspend));
        assert!(f.calls().is_empty(), "{:?}", f.calls());
    }

    #[test]
    fn bar_toml_quick_commands_are_read_and_the_rest_is_ignored() {
        let text = "modules = [\"clock\"]\n[quick]\nstep = 10\nlock = [\"screen-lock\"]\nlogout = [\"session-logout\", \"--now\"]\nreboot = []\n[quick.keep_awake]\ntoggle = [\"x\"]\n";
        let c = SessionCommands::from_text(text).unwrap();
        let d = SessionCommands::default();
        assert_eq!(c.lock, argv(&["screen-lock"]));
        assert_eq!(c.logout, argv(&["session-logout", "--now"]));
        assert!(c.reboot.is_empty(), "given and empty: kept, hidden");
        assert_eq!(c.poweroff, d.poweroff, "absent: the default");
        assert_eq!(SessionCommands::from_text("height = 22\n").unwrap(), d);
    }

    #[test]
    fn a_missing_or_bad_bar_toml_gives_the_defaults() {
        let d = SessionCommands::default();
        let dir = crate::temp_path::TempPath::dir("session-commands");
        assert_eq!(SessionCommands::load(&dir.to_path_buf().join("bar.toml")), (d.clone(), None));
        let bad = dir.to_path_buf().join("bad.toml");
        std::fs::write(&bad, "[quick\n").unwrap();
        let (c, w) = SessionCommands::load(&bad);
        assert_eq!(c, d);
        assert!(w.unwrap().starts_with("bar.toml invalid"));
        let wrong = dir.to_path_buf().join("wrong.toml");
        std::fs::write(&wrong, "[quick]\nlock = \"screen-lock\"\n").unwrap();
        assert_eq!(SessionCommands::load(&wrong).0, d, "a key of the wrong type: the bar refuses the file too");
        std::fs::write(&wrong, "[quick]\nlock = [\"screen-lock\"]\n").unwrap();
        assert_eq!(SessionCommands::load(&wrong).0.lock, argv(&["screen-lock"]));
    }
}
