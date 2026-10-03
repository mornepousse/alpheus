//! Boucle réelle : terminal, fil de travail, événements, outils externes.
//! La boucle elle-même n'a pas de test unitaire (couverte par
//! tests/premier_rendu.rs et l'essai réel) ; sa décision de sortie et le
//! gestionnaire de signaux en ont.

use super::keymap::Keymap;
use super::worker::{self, Request, Update};
use super::App;
use crate::backends::Backends;
use crate::config::Config;
use crate::exec::SystemExecutor;
use crate::externes::{self, CrosstermScreen, ProcessLauncher};
use crate::theme::Theme;
use crate::ui;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Délai maximal d'une commande système avant d'être tuée.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// Levé par SIGTERM ou SIGHUP (fermeture de la fenêtre, fin de session) :
/// la boucle sort normalement et la garde `Nettoyage` s'exécute.
static FIN_DEMANDEE: AtomicBool = AtomicBool::new(false);

/// Gestionnaire de signal : un seul store atomique, async-signal-safe.
extern "C" fn sur_signal(_sig: libc::c_int) {
    FIN_DEMANDEE.store(true, Ordering::SeqCst);
}

/// Installe le gestionnaire pour SIGTERM et SIGHUP. SA_RESTART : un appel
/// système interrompu (lecture du terminal, attente d'un outil) reprend au
/// lieu d'échouer ; la boucle voit le drapeau au tour suivant (≤ 100 ms).
fn installer_signaux() -> io::Result<()> {
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY : sigaction est une structure C simple, valide remplie de zéros.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = sur_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART;
        // SAFETY : `sa` est valide pendant l'appel ; le gestionnaire ne fait
        // qu'un store atomique (async-signal-safe).
        let r = unsafe {
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut())
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// La boucle doit-elle s'arrêter ? (« q » ou signal de fin reçu.)
fn doit_sortir(app: &App, fin: &AtomicBool) -> bool {
    app.should_quit || fin.load(Ordering::SeqCst)
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("alpheus")
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        // SAFETY : getuid(2) ne peut pas échouer.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

pub fn build_app(dir: &Path, now: Instant) -> App {
    let (config, w_config) = Config::load(&dir.join("config.toml"));
    let (theme, w_theme) = Theme::load(&dir.join("colors.toml"));
    let (keymap, w_keys) = match Keymap::from_config(&config.keys) {
        Ok(k) => (k, None),
        Err(e) => (Keymap::default(), Some(format!("touches : {e}"))),
    };
    let mut app = App::new(config, theme, keymap, now);
    for w in [w_config, w_theme, w_keys].into_iter().flatten() {
        app.warn(w);
    }
    app
}

/// Garde de nettoyage : tant qu'elle vit, sa destruction (retour normal de
/// `run`, `?` précoce comme celui d'`EnableMouseCapture`, ou panique qui
/// déroule la pile) restaure le terminal et tue les outils encore en cours.
/// `Drop` s'exécute dans tous ces cas, contrairement à du code placé après
/// un point de sortie qu'un `?` ou une panique peut sauter.
struct Nettoyage {
    exec: Arc<SystemExecutor>,
}

impl Drop for Nettoyage {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
        self.exec.kill_running(); // aucun outil laissé derrière
    }
}

pub fn run() -> io::Result<()> {
    installer_signaux()?;
    let app = build_app(&config_dir(), Instant::now());
    let exec = Arc::new(SystemExecutor::new(TOOL_TIMEOUT));
    let (tx, rx) = worker::spawn(Backends::system(exec.clone(), runtime_dir()));
    let mut terminal = ratatui::init(); // installe aussi la restauration du terminal en cas de panique
    let _nettoyage = Nettoyage { exec };
    execute!(io::stdout(), EnableMouseCapture)?;
    event_loop(&mut terminal, app, &tx, &rx)
}

fn event_loop(terminal: &mut DefaultTerminal, mut app: App, tx: &Sender<Request>, rx: &Receiver<Update>) -> io::Result<()> {
    loop {
        if doit_sortir(&app, &FIN_DEMANDEE) {
            return Ok(()); // la garde Nettoyage restaure le terminal et tue les outils
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
            let r = externes::open(&mut CrosstermScreen, &mut ProcessLauncher, &app.external_argv(ext));
            terminal.clear()?;
            app.external_done(ext, r, Instant::now());
            continue;
        }
        if event::poll(Duration::from_millis(100))? {
            let now = Instant::now();
            match event::read()? {
                Event::Key(k) => app.on_key(k, now),
                Event::Mouse(m) => app.on_mouse(m, &hits, now),
                _ => {} // redimensionnement : redessiné au tour suivant
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

    #[test]
    fn decision_de_sortie() {
        let mut a = ready_app();
        let fin = AtomicBool::new(false);
        assert!(!doit_sortir(&a, &fin));
        fin.store(true, Ordering::SeqCst);
        assert!(doit_sortir(&a, &fin), "signal reçu : la boucle sort");
        fin.store(false, Ordering::SeqCst);
        a.should_quit = true;
        assert!(doit_sortir(&a, &fin));
    }

    /// Disposition actuelle du signal `sig`.
    fn gestionnaire(sig: libc::c_int) -> libc::sighandler_t {
        // SAFETY : lecture seule de la disposition (nouvelle action nulle).
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(sig, std::ptr::null(), &mut old), 0);
            old.sa_sigaction
        }
    }

    #[test]
    fn sigterm_et_sighup_levent_le_drapeau() {
        installer_signaux().unwrap();
        let attendu = sur_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for sig in [libc::SIGTERM, libc::SIGHUP] {
            // Vérifié AVANT raise : sans gestionnaire, le signal tuerait les tests.
            assert_eq!(gestionnaire(sig), attendu, "signal {sig} sans gestionnaire");
            FIN_DEMANDEE.store(false, Ordering::SeqCst);
            // SAFETY : raise(3) vers nous-mêmes, gestionnaire installé ci-dessus.
            assert_eq!(unsafe { libc::raise(sig) }, 0);
            assert!(FIN_DEMANDEE.load(Ordering::SeqCst), "signal {sig} : drapeau non levé");
        }
        FIN_DEMANDEE.store(false, Ordering::SeqCst);
        // SAFETY : rétablit la disposition par défaut pour le reste des tests.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
        }
    }
}
