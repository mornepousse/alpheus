//! Alimentation de la session. Verrouiller : hyprlock détaché ; quitter la
//! session : wm-set (jamais le compositeur en direct) ; le reste : systemctl.

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
            PowerAction::Lock => "Verrouiller",
            PowerAction::Logout => "Se déconnecter",
            PowerAction::Suspend => "Veille",
            PowerAction::Reboot => "Redémarrer",
            PowerAction::Poweroff => "Éteindre",
        }
    }

    /// Question de la boîte de confirmation.
    pub fn question(self) -> &'static str {
        match self {
            PowerAction::Lock => "Verrouiller la session ?",
            PowerAction::Logout => "Fermer la session ?",
            PowerAction::Suspend => "Mettre en veille ?",
            PowerAction::Reboot => "Redémarrer l'ordinateur ?",
            PowerAction::Poweroff => "Éteindre l'ordinateur ?",
        }
    }

    /// Confirmation obligatoire : ce qui ferme la session ou la machine.
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
    fn libelles() {
        let l: Vec<_> = PowerAction::ALL.iter().map(|a| a.label()).collect();
        assert_eq!(l, vec!["Verrouiller", "Se déconnecter", "Veille", "Redémarrer", "Éteindre"]);
        assert_eq!(PowerAction::Poweroff.question(), "Éteindre l'ordinateur ?");
    }

    #[test]
    fn confirmation_pour_deconnexion_redemarrage_extinction() {
        let c: Vec<_> = PowerAction::ALL.iter().filter(|a| a.needs_confirm()).copied().collect();
        assert_eq!(c, vec![PowerAction::Logout, PowerAction::Reboot, PowerAction::Poweroff]);
    }

    #[test]
    fn commandes_exactes() {
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
