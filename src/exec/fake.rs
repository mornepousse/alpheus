//! Faux exécuteur pour les tests : réponses programmées, appels journalisés.
//! Une commande non programmée répond « introuvable » : un test ne peut
//! JAMAIS atteindre un vrai outil.

use super::{ExecError, Executor, Output};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
pub struct FakeExecutor {
    responses: Mutex<HashMap<String, Result<Output, ExecError>>>,
    calls: Mutex<Vec<String>>,
    delay: Duration,
}

impl FakeExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Chaque appel `run` dure `delay` (simule un outil lent).
    pub fn with_delay(delay: Duration) -> Self {
        FakeExecutor { delay, ..Self::default() }
    }

    /// Programme la réponse à la ligne de commande exacte « prog arg1 arg2 ».
    pub fn respond(&self, cmdline: &str, r: Result<Output, ExecError>) {
        self.responses.lock().unwrap().insert(cmdline.to_string(), r);
    }

    /// Raccourci : succès avec cette sortie standard.
    pub fn ok(&self, cmdline: &str, stdout: &str) {
        self.respond(cmdline, Ok(Output::ok(stdout)));
    }

    /// Lignes de commande reçues (run et spawn), dans l'ordre.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn line(prog: &str, args: &[&str]) -> String {
    std::iter::once(prog).chain(args.iter().copied()).collect::<Vec<_>>().join(" ")
}

impl Executor for FakeExecutor {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.responses.lock().unwrap().get(&l).cloned().unwrap_or(Err(ExecError::NotFound))
    }

    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        let l = line(prog, args);
        self.calls.lock().unwrap().push(l.clone());
        match self.responses.lock().unwrap().get(&l).cloned() {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(ExecError::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn commande_non_programmee_introuvable_et_journalisee() {
        let f = FakeExecutor::new();
        assert_eq!(f.run("wpctl", &["status"]), Err(ExecError::NotFound));
        assert_eq!(f.calls(), vec!["wpctl status".to_string()]);
    }

    #[test]
    fn reponse_programmee() {
        let f = FakeExecutor::new();
        f.ok("nmcli -t radio wifi", "enabled\n");
        f.respond("bluetoothctl show", Err(ExecError::Timeout));
        assert_eq!(f.run("nmcli", &["-t", "radio", "wifi"]).unwrap().stdout, "enabled\n");
        assert_eq!(f.run("bluetoothctl", &["show"]), Err(ExecError::Timeout));
    }

    #[test]
    fn spawn_journalise() {
        let f = FakeExecutor::new();
        f.ok("hyprlock", "");
        assert_eq!(f.spawn("hyprlock", &[]), Ok(()));
        assert_eq!(f.spawn("inconnu", &[]), Err(ExecError::NotFound));
        assert_eq!(f.calls(), vec!["hyprlock".to_string(), "inconnu".to_string()]);
    }

    #[test]
    fn delai_simule() {
        let f = FakeExecutor::with_delay(Duration::from_millis(50));
        let t = Instant::now();
        let _ = f.run("wpctl", &["status"]);
        assert!(t.elapsed() >= Duration::from_millis(50));
    }
}
