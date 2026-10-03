//! Boucle réelle : terminal, fil de travail, événements, outils externes.
//! La boucle elle-même n'a pas de test unitaire (couverte par
//! tests/premier_rendu.rs et l'essai réel) ; sa décision de sortie et le
//! gestionnaire de signaux en ont.

use super::keymap::Keymap;
use super::worker::{self, Request, Update};
use super::App;
use crate::backends::{BResult, Backends, Displays, WmSetDisplays};
use crate::config::Config;
use crate::displays::{profiles, Plan};
use crate::exec::SystemExecutor;
use crate::externes::{self, CrosstermScreen, ProcessLauncher};
use crate::theme::Theme;
use crate::ui;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Délai maximal d'une commande système avant d'être tuée.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// Levé par SIGTERM ou SIGHUP (fermeture de la fenêtre, fin de session) :
/// la boucle sort normalement et la garde `Nettoyage` s'exécute.
static FIN_DEMANDEE: AtomicBool = AtomicBool::new(false);
/// Numéro du signal reçu (code de sortie 128 + n si la sortie est forcée).
static SIGNAL_RECU: AtomicI32 = AtomicI32::new(0);
/// Levé quand `run` a fini, garde Nettoyage comprise.
static BOUCLE_FINIE: AtomicBool = AtomicBool::new(false);
/// Délai laissé à la boucle pour sortir seule après un signal.
const GRACE: Duration = Duration::from_millis(300);

/// Gestionnaire de signal : deux stores atomiques, async-signal-safe.
extern "C" fn sur_signal(sig: libc::c_int) {
    SIGNAL_RECU.store(sig, Ordering::SeqCst);
    FIN_DEMANDEE.store(true, Ordering::SeqCst);
}

/// Essai d'écrans à défaire si alpheus se ferme avant que Mae l'ait gardé.
#[derive(Debug, Default)]
struct ExitUndo {
    /// Plan qui rétablit l'état d'avant l'essai.
    plan: Option<Plan>,
    /// Le retour de sortie a eu lieu (ou rien n'était à défaire) : plus
    /// rien ne peut le réarmer.
    done: bool,
}

/// Tenu à jour par la boucle, lu par le nettoyage de sortie et le fil de garde.
type UndoSlot = Arc<Mutex<ExitUndo>>;

/// La boucle note, à chaque tour, ce que la fermeture devra défaire. Une
/// fois le retour de sortie fait, ignoré : une boucle lente (pas coincée)
/// qui repasse après la garde ne réarme pas un second retour.
fn record_undo(slot: &Mutex<ExitUndo>, plan: Option<Plan>) {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if !g.done {
        g.plan = plan;
    }
}

/// Défait l'essai d'écrans en cours, une seule fois : le verrou reste tenu
/// pendant l'application, donc le nettoyage et le fil de garde ne la
/// lancent jamais deux fois ni en même temps — et celui qui arrive second
/// attend la fin du retour avant de continuer (la garde n'appelle donc
/// `exit` qu'après). Durée bornée : chaque étape `wm-set` est tuée au bout
/// de `TOOL_TIMEOUT`. Renvoie l'échec du retour (rien à défaire : `Ok`).
fn undo_on_exit(slot: &Mutex<ExitUndo>, displays: &dyn Displays) -> BResult<()> {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    g.done = true;
    match g.plan.take() {
        Some(plan) => displays.apply(&plan),
        None => Ok(()),
    }
}

/// Un retour de sortie raté laisse Mae avec les réglages rejetés : on le dit
/// sur la sortie d'erreur (le terminal est déjà restauré). `writeln!` et non
/// `eprintln!` : après un SIGHUP, stderr peut répondre EIO, et `eprintln!`
/// paniquerait — dans `Drop`, pendant un déroulement, ce serait un abort.
fn signaler_retour(r: BResult<()>, err: &mut dyn Write) {
    if let Err(e) = r {
        let _ = writeln!(err, "alpheus : retour des écrans impossible : {e}");
    }
}

/// Ordre de fermeture, commun au nettoyage et au fil de garde : d'abord
/// tuer les outils en cours (un `wm-set` de l'essai compris) et fermer leur
/// exécuteur — rien ne peut plus réappliquer l'essai —, puis le défaire
/// avec un exécuteur à part.
fn fermer(kill_running: impl FnOnce(), slot: &Mutex<ExitUndo>, displays: &dyn Displays) {
    kill_running();
    signaler_retour(undo_on_exit(slot, displays), &mut io::stderr());
}

/// Lance le fil de garde. Fenêtre fermée (SIGHUP), crossterm peut tourner
/// sans fin dans `event::poll` sur le terminal raccroché (lecture → 0/EIO,
/// relancée en boucle) : la boucle ne revoit jamais le drapeau. Le fil de
/// garde ne dépend pas de crossterm : passé le délai de grâce, il tue les
/// outils en cours, défait un essai d'écrans (ou attend que le nettoyage
/// ait fini de le faire) et termine le processus.
fn lancer_garde(exec: Arc<SystemExecutor>, undo: UndoSlot, displays: Arc<dyn Displays>) {
    std::thread::spawn(move || {
        surveiller(&FIN_DEMANDEE, &BOUCLE_FINIE, GRACE, Duration::from_millis(50), || {
            fermer(|| exec.kill_running(), &undo, &*displays); // aucun outil laissé derrière
            std::process::exit(128 + SIGNAL_RECU.load(Ordering::SeqCst));
        });
    });
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

/// Fil de garde : attend le drapeau de fin, laisse `grace` à la boucle pour
/// sortir d'elle-même (et lever `finie`), sinon appelle `forcer`. Renvoie
/// `true` si la sortie a été forcée. Ne dépend ni du terminal ni de
/// crossterm : testable avec des drapeaux locaux et des délais courts.
fn surveiller(fin: &AtomicBool, finie: &AtomicBool, grace: Duration, pas: Duration, forcer: impl FnOnce()) -> bool {
    while !fin.load(Ordering::SeqCst) {
        std::thread::sleep(pas);
    }
    let t = Instant::now();
    while t.elapsed() < grace {
        if finie.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(pas);
    }
    if finie.load(Ordering::SeqCst) {
        return false;
    }
    forcer();
    true
}

/// Session niri ? (D'après `NIRI_SOCKET`.)
fn niri_actif(socket: Option<OsString>) -> bool {
    socket.is_some()
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
    let ecrans_toml = dir.join("ecrans.toml");
    let (display_profiles, w_ecrans) = profiles::load(&ecrans_toml);
    let mut app = App::new(config, theme, keymap, now);
    app.ecrans.profiles = display_profiles;
    app.ecrans.profiles_path = Some(ecrans_toml);
    // Fichier illisible ou invalide : l'enregistrement l'écraserait sans
    // l'avoir lu, il est refusé tant que Mae ne l'a pas corrigé.
    app.ecrans.profiles_invalid = w_ecrans.is_some();
    for w in [w_config, w_theme, w_keys, w_ecrans].into_iter().flatten() {
        app.warn(w);
    }
    app
}

/// Garde de nettoyage : tant qu'elle vit, sa destruction (retour normal de
/// `run`, `?` précoce comme celui d'`EnableMouseCapture`, ou panique qui
/// déroule la pile) restaure le terminal, tue les outils encore en cours et
/// défait un essai d'écrans non gardé. `Drop` s'exécute dans tous ces cas,
/// contrairement à du code placé après un point de sortie qu'un `?` ou une
/// panique peut sauter.
struct Nettoyage {
    exec: Arc<SystemExecutor>,
    undo: UndoSlot,
    displays: Arc<dyn Displays>,
}

impl Drop for Nettoyage {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        ratatui::restore();
        fermer(|| self.exec.kill_running(), &self.undo, &*self.displays);
    }
}

pub fn run() -> io::Result<()> {
    installer_signaux()?;
    let mut app = build_app(&config_dir(), Instant::now());
    // Sous niri, l'onglet Écrans n'appelle rien (pas encore pris en charge).
    app.ecrans.niri = niri_actif(std::env::var_os("NIRI_SOCKET"));
    let exec = Arc::new(SystemExecutor::new(TOOL_TIMEOUT));
    let (tx, rx) = worker::spawn(Backends::system(exec.clone(), runtime_dir()));
    // Exécuteur à part pour défaire un essai d'écrans à la fermeture : celui
    // du fil de travail est fermé (kill_running) juste avant.
    let exit_displays: Arc<dyn Displays> = Arc::new(WmSetDisplays::new(Arc::new(SystemExecutor::new(TOOL_TIMEOUT))));
    let undo: UndoSlot = Arc::default();
    lancer_garde(exec.clone(), undo.clone(), exit_displays.clone());
    let r = {
        let mut terminal = ratatui::init(); // installe aussi la restauration du terminal en cas de panique
        let _nettoyage = Nettoyage { exec, undo: undo.clone(), displays: exit_displays };
        execute!(io::stdout(), EnableMouseCapture)
            .and_then(|_| event_loop(&mut terminal, app, &tx, &rx, &undo))
    }; // Nettoyage s'exécute ici
    BOUCLE_FINIE.store(true, Ordering::SeqCst);
    r
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    tx: &Sender<Request>,
    rx: &Receiver<Update>,
    undo: &Mutex<ExitUndo>,
) -> io::Result<()> {
    loop {
        // Essai d'écrans en cours : ce que la fermeture devra défaire. Noté
        // avant l'envoi des requêtes du tour : un essai n'est jamais envoyé
        // au fil de travail sans que son retour soit déjà noté ici.
        record_undo(undo, app.exit_undo());
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

    use crate::displays::Step;
    use crate::exec::fake::FakeExecutor;

    fn plan_retour() -> Plan {
        Plan { steps: vec![Step { output: "HDMI-A-1".into(), active: None }] }
    }

    fn slot_avec(plan: Option<Plan>) -> Mutex<ExitUndo> {
        let s = Mutex::new(ExitUndo::default());
        record_undo(&s, plan);
        s
    }

    #[test]
    fn retour_d_ecrans_a_la_fermeture_une_seule_fois() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_avec(Some(plan_retour()));
        let _ = undo_on_exit(&slot, &d); // nettoyage de sortie
        let _ = undo_on_exit(&slot, &d); // puis fil de garde : rien à refaire
        assert_eq!(f.calls(), vec!["wm-set monitor-disable HDMI-A-1"]);
        assert_eq!(slot.lock().unwrap().plan, None);
        let _ = undo_on_exit(&slot_avec(None), &d);
        assert_eq!(f.calls().len(), 1, "aucun essai : aucun appel");
    }

    #[test]
    fn la_boucle_ne_rearme_pas_un_retour_deja_fait() {
        // Boucle lente mais pas coincée (outil externe fermé par le même
        // SIGHUP) : la garde a déjà défait l'essai, la boucle repasse et
        // réécrit l'essai encore présent dans l'App ; le nettoyage qui suit
        // ne doit pas le défaire une seconde fois.
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_avec(Some(plan_retour()));
        let _ = undo_on_exit(&slot, &d); // fil de garde
        record_undo(&slot, Some(plan_retour())); // la boucle repasse
        let _ = undo_on_exit(&slot, &d); // nettoyage
        assert_eq!(f.calls().len(), 1, "un seul retour : {:?}", f.calls());
    }

    #[test]
    fn essai_garde_ou_defait_rien_a_la_fermeture() {
        let f = Arc::new(FakeExecutor::new());
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_avec(Some(plan_retour()));
        record_undo(&slot, None); // Mae a gardé les réglages
        let _ = undo_on_exit(&slot, &d);
        assert!(f.calls().is_empty(), "{:?}", f.calls());
    }

    #[test]
    fn fermer_tue_les_outils_avant_le_retour() {
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d = WmSetDisplays::new(f.clone());
        let slot = slot_avec(Some(plan_retour()));
        let mut appels_au_kill = None;
        fermer(|| appels_au_kill = Some(f.calls().len()), &slot, &d);
        assert_eq!(appels_au_kill, Some(0), "kill_running d'abord");
        assert_eq!(f.calls().len(), 1, "puis le retour");
    }

    #[test]
    fn retour_impossible_signale() {
        // wm-set échoue (ici : réponse non programmée, « introuvable »).
        let f = Arc::new(FakeExecutor::new());
        let d = WmSetDisplays::new(f.clone());
        let r = undo_on_exit(&slot_avec(Some(plan_retour())), &d);
        assert!(r.is_err(), "l'échec du retour remonte : {r:?}");
        let mut err = Vec::new();
        signaler_retour(r, &mut err);
        let msg = String::from_utf8(err).unwrap();
        assert!(msg.starts_with("alpheus : retour des écrans impossible : "), "{msg:?}");
        assert!(msg.ends_with('\n'));
        let mut err = Vec::new();
        signaler_retour(undo_on_exit(&slot_avec(None), &d), &mut err);
        assert!(err.is_empty(), "rien à défaire : rien à dire");
    }

    #[test]
    fn niri_detecte_par_son_socket() {
        assert!(niri_actif(Some("/run/user/1000/niri.sock".into())));
        assert!(!niri_actif(None));
    }

    #[test]
    fn la_garde_attend_le_retour_en_cours() {
        // Le nettoyage applique un retour lent ; la garde, déclenchée pendant,
        // attend sa fin au lieu de le doubler (ou de tuer alpheus au milieu).
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(200)));
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d: Arc<dyn Displays> = Arc::new(WmSetDisplays::new(f.clone()));
        let slot: UndoSlot = Arc::new(slot_avec(Some(plan_retour())));
        let (s2, d2) = (slot.clone(), d.clone());
        let h = std::thread::spawn(move || {
            let _ = undo_on_exit(&s2, &*d2);
        });
        let t0 = Instant::now();
        while f.calls().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(2), "le retour n'a pas démarré");
            std::thread::yield_now();
        }
        let t = Instant::now();
        let _ = undo_on_exit(&slot, &*d);
        assert!(t.elapsed() >= Duration::from_millis(100), "la garde a attendu ({:?})", t.elapsed());
        h.join().unwrap();
        assert_eq!(f.calls().len(), 1, "un seul retour");
    }

    #[test]
    fn sortie_forcee_seulement_apres_le_retour() {
        // Signal reçu, nettoyage en plein retour lent (plus long que la
        // grâce) : la sortie forcée de la garde (son `forcer`) ne passe
        // qu'une fois le retour fini — `exit` ne le coupe jamais.
        let f = Arc::new(FakeExecutor::with_delay(Duration::from_millis(250)));
        f.ok("wm-set monitor-disable HDMI-A-1", "");
        let d: Arc<dyn Displays> = Arc::new(WmSetDisplays::new(f.clone()));
        let slot: UndoSlot = Arc::new(slot_avec(Some(plan_retour())));
        let (s2, d2) = (slot.clone(), d.clone());
        let h = std::thread::spawn(move || fermer(|| {}, &s2, &*d2)); // Nettoyage::drop
        while f.calls().is_empty() {
            std::thread::yield_now();
        }
        let debut_retour = Instant::now();
        let (fin, finie) = (AtomicBool::new(true), AtomicBool::new(false));
        let mut sortie_apres = None;
        let force = surveiller(&fin, &finie, Duration::from_millis(20), PAS, || {
            fermer(|| {}, &slot, &*d);
            sortie_apres = Some(debut_retour.elapsed()); // ici : process::exit
        });
        assert!(force, "boucle coincée : sortie forcée");
        let sortie_apres = sortie_apres.unwrap();
        assert!(sortie_apres >= Duration::from_millis(200), "exit avant la fin du retour ({sortie_apres:?})");
        h.join().unwrap();
        assert_eq!(f.calls().len(), 1, "un seul retour");
    }

    #[test]
    fn profils_d_ecran_charges_au_demarrage() {
        let dir = std::env::temp_dir().join(format!("alpheus-{}-run-ecrans", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = build_app(&dir, Instant::now());
        assert!(a.ecrans.profiles.is_empty());
        assert_eq!(a.status.warning, None, "fichier absent : aucun message");
        assert!(!a.ecrans.profiles_invalid, "absent : l'enregistrement le créera");
        std::fs::write(dir.join("ecrans.toml"), "[[profil]]\nnom = \"Bureau\"\n").unwrap();
        let a = build_app(&dir, Instant::now());
        assert_eq!(a.ecrans.profiles.len(), 1);
        assert_eq!(a.ecrans.profiles_path, Some(dir.join("ecrans.toml")));
        assert_eq!(a.status.warning, None);
        assert!(!a.ecrans.profiles_invalid);
        std::fs::write(dir.join("ecrans.toml"), "[[profil]\n").unwrap();
        let a = build_app(&dir, Instant::now());
        assert!(a.ecrans.profiles.is_empty());
        assert!(a.status.warning.as_deref().unwrap_or("").starts_with("ecrans.toml invalide"), "{:?}", a.status.warning);
        assert!(a.ecrans.profiles_invalid, "fichier invalide : enregistrement refusé");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    const GRACE: Duration = Duration::from_millis(150);
    const PAS: Duration = Duration::from_millis(5);

    #[test]
    fn garde_force_la_sortie_si_la_boucle_est_coincee() {
        let (fin, finie) = (AtomicBool::new(true), AtomicBool::new(false));
        let mut force = false;
        let t = Instant::now();
        assert!(surveiller(&fin, &finie, GRACE, PAS, || force = true));
        assert!(force, "boucle coincée : sortie forcée");
        assert!(t.elapsed() >= GRACE, "après le délai de grâce seulement ({:?})", t.elapsed());
    }

    #[test]
    fn garde_laisse_sortir_la_boucle_a_temps() {
        let fin = Arc::new(AtomicBool::new(false));
        let finie = Arc::new(AtomicBool::new(false));
        let (f2, d2) = (fin.clone(), finie.clone());
        let h = std::thread::spawn(move || {
            let mut force = false;
            let r = surveiller(&f2, &d2, GRACE, PAS, || force = true);
            (r, force)
        });
        // Plus long que la grâce : sans signal, la garde ne compte rien.
        std::thread::sleep(GRACE + Duration::from_millis(100));
        assert!(!h.is_finished(), "sans signal, la garde attend");
        fin.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        finie.store(true, Ordering::SeqCst); // la boucle sort dans le délai
        assert_eq!(h.join().unwrap(), (false, false), "aucune sortie forcée");
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
