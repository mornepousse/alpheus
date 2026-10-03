//! Exécuteur de commandes injectable : le SEUL point par lequel alpheus lance
//! un outil système. Les tests injectent `fake::FakeExecutor`, jamais le vrai.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod fake;

/// Résultat d'une commande menée à son terme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Code de sortie ; `None` si la commande a été tuée par un signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Succès (code 0) avec cette sortie standard.
    pub fn ok(stdout: &str) -> Self {
        Output { code: Some(0), stdout: stdout.to_string(), stderr: String::new() }
    }

    /// Échec avec ce code et ce message d'erreur.
    pub fn fail(code: i32, stderr: &str) -> Self {
        Output { code: Some(code), stdout: String::new(), stderr: stderr.to_string() }
    }

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Échec survenu avant d'obtenir une sortie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// Programme absent du PATH.
    NotFound,
    /// Délai dépassé : la commande a été tuée.
    Timeout,
    /// Autre erreur d'entrée-sortie.
    Io(String),
}

pub trait Executor: Send + Sync {
    /// Lance `prog args…`, attend sa fin (délai borné), capture les sorties.
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError>;
    /// Lance `prog args…` sans l'attendre (ex. hyprlock), sorties jetées.
    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError>;
}

/// PID des commandes `run` en cours, et verrou de fermeture : passé à
/// `true` par `kill_running`, il interdit tout nouveau lancement — avant
/// comme après l'appel, jamais de fenêtre où un outil pourrait démarrer
/// sans qu'on le sache et donc sans qu'on puisse le tuer.
#[derive(Default)]
struct Running {
    pids: HashSet<u32>,
    closed: bool,
}

/// Exécuteur réel : `std::process`, délai maximal par commande.
pub struct SystemExecutor {
    timeout: Duration,
    running: Arc<Mutex<Running>>,
}

impl SystemExecutor {
    pub fn new(timeout: Duration) -> Self {
        SystemExecutor { timeout, running: Arc::new(Mutex::new(Running::default())) }
    }

    /// Tue les commandes encore en cours et ferme la porte : alpheus ne
    /// laisse rien derrière lui, et rien ne peut plus démarrer après.
    pub fn kill_running(&self) {
        let mut guard = self.running.lock().unwrap();
        guard.closed = true;
        let pids: Vec<u32> = guard.pids.drain().collect();
        drop(guard);
        for pid in pids {
            // SAFETY : kill(2) sur un PID lancé par nous ; aucun pointeur en jeu.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

fn io_err(e: std::io::Error) -> ExecError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ExecError::NotFound
    } else {
        ExecError::Io(e.to_string())
    }
}

/// Lit un tube jusqu'au bout dans un fil séparé : une sortie plus grosse que
/// le tampon du tube ne peut pas bloquer l'attente du processus.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

impl Executor for SystemExecutor {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        // Alpheus se ferme (`kill_running` déjà appelé) : aucun lancement.
        if self.running.lock().unwrap().closed {
            return Err(ExecError::Io("alpheus se ferme".into()));
        }
        let mut child = Command::new(prog)
            .args(args)
            // Sorties non traduites : les analyseurs attendent l'anglais.
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(io_err)?;
        let pid = child.id();
        {
            let mut guard = self.running.lock().unwrap();
            if guard.closed {
                // Fermeture survenue entre la vérification et le spawn : le
                // fils vient de naître sans qu'on ait pu l'annoncer à
                // `kill_running`, qui ne le tuera donc jamais — on le tue
                // nous-mêmes sur-le-champ.
                drop(guard);
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExecError::Io("alpheus se ferme".into()));
            }
            guard.pids.insert(pid);
        }
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Ok(st),
                Ok(None) if start.elapsed() >= self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(ExecError::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => break Err(ExecError::Io(e.to_string())),
            }
        };
        self.running.lock().unwrap().pids.remove(&pid);
        let status = status?;
        Ok(Output {
            code: status.code(),
            stdout: out.join().unwrap_or_default(),
            stderr: err.join().unwrap_or_default(),
        })
    }

    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        let mut child = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Groupe de processus à part : la fermeture de la fenêtre d'alpheus
            // (SIGHUP au groupe du terminal) ne tue pas l'écran de verrouillage.
            .process_group(0)
            .spawn()
            .map_err(io_err)?;
        // Ramasse le fils à sa fin (pas de zombie) sans bloquer l'appelant.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Seuls `sh` et `sleep` sont lancés ici : aucun outil de session.
    fn exec() -> SystemExecutor {
        SystemExecutor::new(Duration::from_secs(5))
    }

    #[test]
    fn capture_la_sortie_standard() {
        let out = exec().run("sh", &["-c", "printf bonjour"]).unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, "bonjour");
    }

    #[test]
    fn rapporte_code_et_erreur() {
        let out = exec().run("sh", &["-c", "echo oups >&2; exit 3"]).unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stderr, "oups\n");
        assert!(!out.success());
    }

    #[test]
    fn programme_absent() {
        assert_eq!(exec().run("alpheus-programme-inexistant", &[]), Err(ExecError::NotFound));
    }

    #[test]
    fn delai_depasse_tue_la_commande() {
        let e = SystemExecutor::new(Duration::from_millis(100));
        let t = Instant::now();
        assert_eq!(e.run("sleep", &["5"]), Err(ExecError::Timeout));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }

    #[test]
    fn sortie_non_localisee() {
        // nmcli traduit certaines sorties : on force une locale neutre.
        let out = exec().run("sh", &["-c", "printf %s \"$LC_ALL\""]).unwrap();
        assert_eq!(out.stdout, "C.UTF-8");
    }

    #[test]
    fn kill_running_tue_les_commandes_en_cours() {
        let e = Arc::new(SystemExecutor::new(Duration::from_secs(30)));
        let e2 = e.clone();
        let h = std::thread::spawn(move || e2.run("sleep", &["30"]));
        let t = Instant::now();
        while e.running.lock().unwrap().pids.is_empty() {
            assert!(t.elapsed() < Duration::from_secs(2), "la commande n'a pas démarré");
            std::thread::sleep(Duration::from_millis(5));
        }
        e.kill_running();
        let out = h.join().unwrap().unwrap();
        assert_eq!(out.code, None, "tuée par un signal");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn aucun_lancement_apres_la_fermeture() {
        let e = exec();
        e.kill_running();
        assert_eq!(e.run("sh", &["-c", "true"]), Err(ExecError::Io("alpheus se ferme".into())));
    }

    #[test]
    fn spawn_ne_bloque_pas() {
        let t = Instant::now();
        exec().spawn("sleep", &["1"]).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn spawn_programme_absent() {
        assert_eq!(exec().spawn("alpheus-programme-inexistant", &[]), Err(ExecError::NotFound));
    }
}
