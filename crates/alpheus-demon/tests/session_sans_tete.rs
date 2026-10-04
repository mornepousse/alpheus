//! Le démon sur un vrai compositeur Wayland, sans tête : une barre par
//! sortie, branchement à chaud, arrêt propre, un seul démon. Demande sway
//! (`nix develop`) : `cargo test -p alpheus-demon --test session_sans_tete -- --ignored --test-threads=1`.

mod commun;

use commun::{FauxHyprland, SIGNATURE, Sway, lancer_demon};
use std::time::Duration;

const DELAI: Duration = Duration::from_secs(10);

#[test]
#[ignore = "sway sans tête (nix develop)"]
fn une_barre_par_sortie_et_branchement_a_chaud() {
    let sway = Sway::lancer("sorties", 3);
    let _hypr = FauxHyprland::demarrer(&sway.runtime, SIGNATURE);
    let mut d = lancer_demon(&sway);
    for s in ["HEADLESS-1", "HEADLESS-2", "HEADLESS-3"] {
        assert!(d.attendre(&format!("barre posée sur {s}"), DELAI), "{s} : {:?}", d.journal());
    }
    sway.commande("output HEADLESS-2 disable"); // écran débranché (dock arraché)
    assert!(d.attendre("barre retirée de HEADLESS-2", DELAI), "{:?}", d.journal());
    assert_eq!(d.fin(Duration::from_millis(500)), None, "le démon survit au débranchement");
    sway.commande("output HEADLESS-2 enable");
    let posees = || d.journal().iter().filter(|l| l.contains("barre posée sur HEADLESS-2")).count();
    let t = std::time::Instant::now();
    while posees() < 2 && t.elapsed() < DELAI {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(posees(), 2, "la barre revient avec l'écran : {:?}", d.journal());
    sway.commande("create_output");
    assert!(d.attendre("barre posée sur HEADLESS-4", DELAI), "écran ajouté : {:?}", d.journal());
}

#[test]
#[ignore = "sway sans tête (nix develop)"]
fn arret_propre_sur_sigterm_et_verrou_relache() {
    let sway = Sway::lancer("arret", 1);
    let _hypr = FauxHyprland::demarrer(&sway.runtime, SIGNATURE);
    let mut d = lancer_demon(&sway);
    assert!(d.attendre("barre posée sur HEADLESS-1", DELAI), "{:?}", d.journal());
    d.signal(libc::SIGTERM);
    assert_eq!(d.fin(Duration::from_millis(1500)), Some(0), "sortie propre, code 0 : {:?}", d.journal());
    assert!(d.attendre("arrêt", Duration::from_millis(500)));
    let d2 = lancer_demon(&sway);
    assert!(d2.attendre("barre posée sur HEADLESS-1", DELAI), "le verrou a été relâché : {:?}", d2.journal());
}

#[test]
#[ignore = "sway sans tête (nix develop)"]
fn compositeur_tombe_est_une_fin_de_session_code_0() {
    // Décidé et vérifié sur ce poste (relevé 2026-10-04) : tuer sway d'un
    // SIGKILL ferme la connexion Wayland du démon avec EPIPE (« Broken
    // pipe (os error 32) »), exactement ce que produirait aussi une
    // déconnexion propre (fin de session, redémarrage du compositeur —
    // voir la spec § 3). Rien, côté client, ne distingue les deux. Le
    // démon ne se reconnecte jamais lui-même dans aucun cas (c'est la
    // session qui le relance, par exec-once puis shell-ctl) : les traiter
    // pareil (code 0, fin_de_session) évite qu'un superviseur externe y
    // voie, à tort, un signe de panne à chaque fin de session ordinaire.
    let mut sway = Sway::lancer("compositeur-tombe", 1);
    let _hypr = FauxHyprland::demarrer(&sway.runtime, SIGNATURE);
    let mut d = lancer_demon(&sway);
    assert!(d.attendre("barre posée sur HEADLESS-1", DELAI), "{:?}", d.journal());
    sway.tuer();
    let code = d.fin(DELAI);
    assert_eq!(code, Some(0), "connexion coupée (EPIPE/ECONNRESET) : fin de session, code 0 ({:?})", d.journal());
    assert!(d.attendre("fin de session", Duration::from_millis(500)), "{:?}", d.journal());
}

#[test]
#[ignore = "sway sans tête (nix develop)"]
fn un_seul_demon_a_la_fois() {
    let sway = Sway::lancer("unique", 1);
    let _hypr = FauxHyprland::demarrer(&sway.runtime, SIGNATURE);
    let d1 = lancer_demon(&sway);
    assert!(d1.attendre("barre posée sur HEADLESS-1", DELAI));
    let mut d2 = lancer_demon(&sway);
    assert_eq!(d2.fin(DELAI), Some(1));
    assert!(d2.attendre("tourne déjà", Duration::from_secs(1)), "{:?}", d2.journal());
}
