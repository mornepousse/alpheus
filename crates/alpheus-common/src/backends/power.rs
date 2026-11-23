//! Session power. Lock: detached hyprlock; log out of the session: wm-set
//! (never the compositor directly); the rest: systemctl.
//!
//! Never:
//! - call the service behind the tool: the tool is the interface, and a missing one leaves the row
//!   or the tab empty rather than failing;
//! - run anything in a test: the fake executor answers (AGENTS.md).
//!
//! See: docs/features/control-center.md, docs/install.md (the tools), docs/compositors.md (what
//! `wm-set` and `shell-ctl` stand for).

use super::{BResult, run_ok, spawn_ok};
use crate::exec::Executor;
use std::sync::Arc;

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
}

impl SessionPower {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        SessionPower { exec }
    }
}

impl Power for SessionPower {
    fn perform(&self, a: PowerAction) -> BResult<()> {
        let e = &*self.exec;
        match a {
            PowerAction::Lock => spawn_ok(e, "hyprlock", &[]),
            PowerAction::Logout => run_ok(e, "wm-set", &["exit"]).map(|_| ()),
            PowerAction::Suspend => run_ok(e, "systemctl", &["suspend"]).map(|_| ()),
            PowerAction::Reboot => run_ok(e, "systemctl", &["reboot"]).map(|_| ()),
            PowerAction::Poweroff => run_ok(e, "systemctl", &["poweroff"]).map(|_| ()),
        }
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

    #[test]
    fn exact_commands() {
        let f = Arc::new(FakeExecutor::new());
        for l in ["hyprlock", "wm-set exit", "systemctl suspend", "systemctl reboot", "systemctl poweroff"] {
            f.ok(l, "");
        }
        let p = SessionPower::new(f.clone());
        for a in PowerAction::ALL {
            p.perform(a).unwrap();
        }
        assert_eq!(f.calls(), vec!["hyprlock", "wm-set exit", "systemctl suspend", "systemctl reboot", "systemctl poweroff"]);
    }
}
