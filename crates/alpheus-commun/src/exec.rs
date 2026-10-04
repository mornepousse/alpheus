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
        // Tués verrou tenu : `run` ne ramasse un fils qu'après l'avoir retiré
        // de la liste sous ce même verrou, donc chaque PID visé ici n'a pas
        // encore été ramassé — il ne peut pas avoir été réattribué. Le groupe
        // entier est tué : les petits-fils (`sh -c '… &'`) avec.
        for pid in guard.pids.drain() {
            kill_group(pid);
        }
    }
}

/// Cible de kill(2) pour le groupe `pgid` : `-pgid`, seulement si `pgid`
/// peut être le PID d'un de nos fils. Jamais `-1` (tous les processus de
/// l'utilisateur) ni `0` (notre propre groupe). Fonction pure : testée sans
/// jamais envoyer de signal.
fn group_target(pgid: u32) -> Option<libc::pid_t> {
    let p = libc::pid_t::try_from(pgid).ok()?;
    (p > 1).then_some(-p)
}

/// Tue tout le groupe de processus `pgid` (un fils lancé avec
/// `.process_group(0)` et ses descendants). N'est appelé que pour un fils
/// pas encore ramassé, dont le PID est donc toujours le sien.
fn kill_group(pgid: u32) {
    let Some(target) = group_target(pgid) else { return };
    assert!(target < -1, "cible de groupe dangereuse : {target}");
    // SAFETY : kill(2) vise le seul groupe `pgid`, créé par nous ; aucun pointeur.
    unsafe { libc::kill(target, libc::SIGKILL) };
}

/// Le fils `pid` est-il terminé ? Interroge sans le ramasser (WNOWAIT) :
/// tant qu'il n'est pas ramassé, son PID ne peut pas être réattribué.
fn exited(pid: u32) -> Result<bool, ExecError> {
    let id = libc::id_t::from(pid);
    // SAFETY : siginfo_t est une structure C simple, valide remplie de zéros.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY : `info` est une structure valide qui vit pendant l'appel.
    let r = unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    if r == -1 {
        return Err(ExecError::Io(std::io::Error::last_os_error().to_string()));
    }
    // SAFETY : avec WNOHANG, si_pid vaut 0 tant que rien n'a changé.
    Ok(unsafe { info.si_pid() } != 0)
}

fn io_err(e: std::io::Error) -> ExecError {
    if e.kind() == std::io::ErrorKind::NotFound { ExecError::NotFound } else { ExecError::Io(e.to_string()) }
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
            // Groupe de processus propre (PGID = PID du fils) : au délai
            // dépassé ou à la fermeture, on tue le groupe, petits-fils compris.
            .process_group(0)
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
                kill_group(pid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExecError::Io("alpheus se ferme".into()));
            }
            guard.pids.insert(pid);
        }
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let start = Instant::now();
        let ended = loop {
            match exited(pid) {
                Ok(true) => break Ok(()),
                Ok(false) if start.elapsed() >= self.timeout => {
                    // Fils pas encore ramassé : son PID (= PGID) est à nous.
                    kill_group(pid);
                    break Err(ExecError::Timeout);
                }
                Ok(false) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => break Err(e),
            }
        };
        // Retiré de la liste AVANT d'être ramassé (voir `kill_running`).
        self.running.lock().unwrap().pids.remove(&pid);
        if let Err(e) = ended {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let status = child.wait().map_err(|e| ExecError::Io(e.to_string()))?;
        Ok(Output { code: status.code(), stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
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

    /// Le petit-fils `pid` est-il encore vivant ? Un zombie (en attente
    /// d'être ramassé par son nouveau parent) compte comme mort.
    fn vivant(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => false,
            // « pid (comm) état … » : l'état suit la dernière parenthèse.
            Ok(st) => st.rsplit_once(')').and_then(|(_, r)| r.trim_start().chars().next()) != Some('Z'),
        }
    }

    fn pid_ecrit(f: &std::path::Path) -> i32 {
        let t = Instant::now();
        loop {
            if let Some(p) = std::fs::read_to_string(f).ok().and_then(|s| s.trim().parse().ok()) {
                return p;
            }
            assert!(t.elapsed() < Duration::from_secs(2), "PID du petit-fils jamais écrit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn attend_la_mort(pid: i32) {
        let t = Instant::now();
        while vivant(pid) {
            if t.elapsed() > Duration::from_secs(2) {
                // SAFETY : nettoyage du `sleep` lancé par ce test (pid > 1).
                unsafe { libc::kill(pid, libc::SIGKILL) };
                panic!("le petit-fils {pid} a survécu");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn delai_depasse_tue_aussi_les_petits_fils() {
        let f = std::env::temp_dir().join(format!("alpheus-petit-fils-{}-delai", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("sleep 30 & echo $! > {}; wait", f.display());
        let e = SystemExecutor::new(Duration::from_millis(100));
        let t = Instant::now();
        assert_eq!(e.run("sh", &["-c", &script]), Err(ExecError::Timeout));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        let pid = pid_ecrit(&f);
        let _ = std::fs::remove_file(&f);
        assert!(pid > 1);
        attend_la_mort(pid);
    }

    #[test]
    fn kill_running_tue_aussi_les_petits_fils() {
        let f = std::env::temp_dir().join(format!("alpheus-petit-fils-{}-fermeture", std::process::id()));
        let _ = std::fs::remove_file(&f);
        let script = format!("sleep 30 & echo $! > {}; wait", f.display());
        let e = Arc::new(SystemExecutor::new(Duration::from_secs(30)));
        let e2 = e.clone();
        let h = std::thread::spawn(move || e2.run("sh", &["-c", &script]));
        let pid = pid_ecrit(&f);
        let _ = std::fs::remove_file(&f);
        e.kill_running();
        assert!(pid > 1);
        // Avant le join : un petit-fils survivant garderait le tube de
        // sortie ouvert et bloquerait `run` jusqu'à sa fin.
        attend_la_mort(pid);
        let _ = h.join().unwrap();
    }

    #[test]
    fn jamais_de_pid_de_groupe_dangereux() {
        // -0 viserait notre propre groupe, -1 tous nos processus : refusés.
        // (Fonction pure : aucun signal n'est envoyé ici.)
        assert_eq!(group_target(0), None);
        assert_eq!(group_target(1), None);
        assert_eq!(group_target(u32::MAX), None, "ne tient pas dans un pid_t positif");
        assert_eq!(group_target(1 << 31), None);
        assert_eq!(group_target(4242), Some(-4242));
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
