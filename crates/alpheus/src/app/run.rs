//! Boucle réelle : terminal, fil de travail, événements, outils externes.
//! La boucle elle-même n'a pas de test unitaire (couverte par
//! tests/premier_rendu.rs et l'essai réel) ; sa décision de sortie et le
//! gestionnaire de signaux en ont.

use super::action::Tab;
use super::keymap::Keymap;
use super::worker::{self, Request, Update};
use super::App;
use crate::backends::{BResult, BackendError, Backends, Displays, WmSetDisplays};
use crate::config::Config;
use crate::displays::{profiles, Plan};
use crate::exec::SystemExecutor;
use crate::externes::{self, CrosstermScreen, ProcessLauncher};
use crate::theme::Theme;
use crate::ui;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use alpheus_commun::arret::{installer_signaux, surveiller, FIN_DEMANDEE, SIGNAL_RECU};
pub use alpheus_commun::chemins::{config_dir, runtime_dir};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Délai maximal d'une commande système avant d'être tuée.
const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

/// Levé quand `run` a fini, garde Nettoyage comprise.
static BOUCLE_FINIE: AtomicBool = AtomicBool::new(false);
/// Délai laissé à la boucle pour sortir seule après un signal.
const GRACE: Duration = Duration::from_millis(300);

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
/// `exit` qu'après). Au mieux : chaque étape est tentée même si une autre
/// échoue. Durée bornée : chaque étape `wm-set` est tuée au bout de
/// `TOOL_TIMEOUT`. Renvoie l'échec du retour (rien à défaire : `Ok`).
fn undo_on_exit(slot: &Mutex<ExitUndo>, displays: &dyn Displays) -> BResult<()> {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    g.done = true;
    match g.plan.take() {
        Some(plan) => displays.apply_best_effort(&plan),
        None => Ok(()),
    }
}

/// Un retour de sortie raté laisse Mae avec les réglages rejetés : on le dit
/// sur la sortie d'erreur (le terminal est déjà restauré). `writeln!` et non
/// `eprintln!` : après un SIGHUP, stderr peut répondre EIO, et `eprintln!`
/// paniquerait — dans `Drop`, pendant un déroulement, ce serait un abort.
fn signaler_retour(r: BResult<()>, err: &mut dyn Write) {
    let msg = match r {
        Ok(()) => return,
        Err(e @ BackendError::Incomplete { .. }) => format!("alpheus : écrans : {e}"),
        Err(e) => format!("alpheus : retour des écrans impossible : {e}"),
    };
    let _ = writeln!(err, "{msg}");
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

/// Session niri ? (D'après `NIRI_SOCKET` ; une session Hyprland, signalée
/// par `HYPRLAND_INSTANCE_SIGNATURE`, l'emporte sur un socket niri resté
/// dans l'environnement.)
fn niri_actif(socket: Option<OsString>, hyprland: Option<OsString>) -> bool {
    socket.is_some() && hyprland.is_none()
}

/// La boucle doit-elle s'arrêter ? (« q » ou signal de fin reçu.)
fn doit_sortir(app: &App, fin: &AtomicBool) -> bool {
    app.should_quit || fin.load(Ordering::SeqCst)
}

/// Options de la ligne de commande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Onglet affiché à l'ouverture (`--onglet <nom>`).
    pub onglet: Tab,
}

/// `alpheus [--onglet <nom>]`, aussi `--onglet=<nom>`. L'erreur est un
/// message pour l'utilisateur : main l'affiche et sort en code 2.
pub fn lire_options(args: &[String]) -> Result<Options, String> {
    let mut onglet = Tab::Accueil;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let valeur = if a == "--onglet" {
            i += 1;
            args.get(i).map(String::as_str).ok_or("--onglet attend un nom d'onglet")?
        } else if let Some(v) = a.strip_prefix("--onglet=") {
            v
        } else {
            return Err(format!("option inconnue : « {a} » (usage : alpheus [--onglet <nom>])"));
        };
        onglet = Tab::from_slug(valeur).ok_or_else(|| {
            let noms: Vec<&str> = Tab::ALL.iter().map(|t| t.slug()).collect();
            format!("onglet inconnu : « {valeur} » (attendus : {})", noms.join(", "))
        })?;
        i += 1;
    }
    Ok(Options { onglet })
}

pub fn build_app(dir: &Path, onglet: Tab, now: Instant) -> App {
    let (config, w_config) = Config::load(&dir.join("config.toml"));
    let (theme, w_theme) = Theme::load(&dir.join("colors.toml"));
    let (keymap, w_keys) = match Keymap::from_config(&config.keys) {
        Ok(k) => (k, None),
        Err(e) => (Keymap::default(), Some(format!("touches : {e}"))),
    };
    let ecrans_toml = dir.join("ecrans.toml");
    let (display_profiles, w_ecrans) = profiles::load(&ecrans_toml);
    let mut app = App::sur_onglet(config, theme, keymap, onglet, now);
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

pub fn run(options: Options) -> io::Result<()> {
    installer_signaux()?;
    let mut app = build_app(&config_dir(), options.onglet, Instant::now());
    // Sous niri, l'onglet Écrans n'appelle rien (pas encore pris en charge).
    app.ecrans.niri = niri_actif(std::env::var_os("NIRI_SOCKET"), std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE"));
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

    /// Pas de scrutation de `sortie_forcee_seulement_apres_le_retour` (seul
    /// test d'ici à encore appeler `surveiller` directement).
    const PAS: Duration = Duration::from_millis(5);

    fn plan_retour() -> Plan {
        Plan { steps: vec![Step { output: "HDMI-A-1".into(), active: None }] }
    }

    fn slot_avec(plan: Option<Plan>) -> Mutex<ExitUndo> {
        let s = Mutex::new(ExitUndo::default());
        record_undo(&s, plan);
        s
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_de_la_ligne_de_commande() {
        assert_eq!(lire_options(&[]), Ok(Options { onglet: Tab::Accueil }));
        assert_eq!(lire_options(&args(&["--onglet", "son"])), Ok(Options { onglet: Tab::Son }));
        assert_eq!(lire_options(&args(&["--onglet=Réseau"])), Ok(Options { onglet: Tab::Reseau }));
        assert_eq!(lire_options(&args(&["--onglet", "ecrans"])), Ok(Options { onglet: Tab::Ecrans }));
        let e = lire_options(&args(&["--onglet", "météo"])).unwrap_err();
        assert!(e.contains("météo") && e.contains("accueil, son"), "{e}");
        assert!(lire_options(&args(&["--onglet"])).unwrap_err().contains("attend"));
        assert!(lire_options(&args(&["-v"])).unwrap_err().contains("option inconnue"));
    }

    #[test]
    fn ouverture_sur_un_onglet_ne_lit_que_ses_domaines() {
        let dir = std::env::temp_dir().join("alpheus-onglet-sans-config");
        let mut a = build_app(&dir, Tab::Son, Instant::now());
        assert_eq!(a.tab, Tab::Son);
        assert_eq!(a.take_requests(), vec![Request::Refresh(worker::domains(Tab::Son).to_vec())]);
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
        assert_eq!(msg, "alpheus : écrans : retour incomplet : HDMI-A-1 (wm-set introuvable)\n");
        let mut err = Vec::new();
        signaler_retour(Err(BackendError::Timeout { tool: "wm-set".into() }), &mut err);
        assert_eq!(String::from_utf8(err).unwrap(), "alpheus : retour des écrans impossible : wm-set ne répond pas\n");
        let mut err = Vec::new();
        signaler_retour(undo_on_exit(&slot_avec(None), &d), &mut err);
        assert!(err.is_empty(), "rien à défaire : rien à dire");
    }

    #[test]
    fn retour_de_fermeture_au_mieux() {
        // Décision D2 : une étape du retour en échec n'empêche pas les autres.
        let plan = Plan { steps: vec![
            Step { output: "DVI-I-1".into(), active: None },
            Step { output: "HDMI-A-1".into(), active: None },
        ] };
        let f = Arc::new(FakeExecutor::new());
        f.ok("wm-set monitor-disable HDMI-A-1", ""); // DVI-I-1 non programmé : échec
        let d = WmSetDisplays::new(f.clone());
        let r = undo_on_exit(&slot_avec(Some(plan)), &d);
        assert_eq!(f.calls(), vec!["wm-set monitor-disable DVI-I-1", "wm-set monitor-disable HDMI-A-1"]);
        let mut err = Vec::new();
        signaler_retour(r, &mut err);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "alpheus : écrans : retour incomplet : DVI-I-1 (wm-set introuvable)\n"
        );
    }

    #[test]
    fn niri_detecte_par_son_socket() {
        assert!(niri_actif(Some("/run/user/1000/niri.sock".into()), None));
        assert!(!niri_actif(None, None));
        // Socket niri resté dans l'environnement d'une session Hyprland.
        assert!(!niri_actif(Some("/run/user/1000/niri.sock".into()), Some("abc_123".into())));
        assert!(!niri_actif(None, Some("abc_123".into())));
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
        let a = build_app(&dir, Tab::Accueil, Instant::now());
        assert!(a.ecrans.profiles.is_empty());
        assert_eq!(a.status.warning, None, "fichier absent : aucun message");
        assert!(!a.ecrans.profiles_invalid, "absent : l'enregistrement le créera");
        std::fs::write(dir.join("ecrans.toml"), "[[profil]]\nnom = \"Bureau\"\n").unwrap();
        let a = build_app(&dir, Tab::Accueil, Instant::now());
        assert_eq!(a.ecrans.profiles.len(), 1);
        assert_eq!(a.ecrans.profiles_path, Some(dir.join("ecrans.toml")));
        assert_eq!(a.status.warning, None);
        assert!(!a.ecrans.profiles_invalid);
        std::fs::write(dir.join("ecrans.toml"), "[[profil]\n").unwrap();
        let a = build_app(&dir, Tab::Accueil, Instant::now());
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

}
