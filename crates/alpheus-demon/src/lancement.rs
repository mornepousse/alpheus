//! Assemblage du démon : signaux, verrou, réglages, polices, Wayland, boucle
//! d'événements (calloop), sources. Couvert par tests/session_sans_tete.rs
//! (sway sans tête) et l'essai réel ; ses décisions pures ont leurs tests.

use crate::config::BarreConfig;
use crate::etat::{Barre, Msg};
use crate::heure;
use crate::heure::HeureLocale;
use crate::peintre::Polices;
use crate::sources;
use crate::verrou;
use crate::wayland::Etat;
use alpheus_commun::exec::SystemExecutor;
use alpheus_commun::theme::Palette;
use alpheus_commun::{arret, chemins};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{channel, EventLoop};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::Connection;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static FIN: AtomicBool = AtomicBool::new(false);
static FINIE: AtomicBool = AtomicBool::new(false);
/// Délai laissé à la boucle pour sortir seule après un signal.
const GRACE: Duration = Duration::from_secs(1);

/// Courrier reçu par la boucle depuis les autres fils.
pub enum Courrier {
    Barre(Msg),
    Arret,
}

/// Réglages lus au démarrage, avertissements à journaliser.
pub fn reglages(dossier: &Path) -> (BarreConfig, Palette, Vec<String>) {
    let (config, w1) = BarreConfig::load(&dossier.join("barre.toml"));
    let (palette, w2) = Palette::load(&dossier.join("colors.toml"));
    (config, palette, [w1, w2].into_iter().flatten().collect())
}

/// Applique un message à la barre. Tout événement du compositeur rafraîchit
/// aussi l'heure : au sortir de veille, la minuterie (horloge monotone,
/// arrêtée pendant la veille) peut retarder d'une minute ; le premier
/// changement de focus remet l'heure juste.
pub fn distribuer(barre: &mut Barre, m: Msg, maintenant: impl Fn() -> HeureLocale) -> bool {
    let du_compositeur = matches!(m, Msg::Compositeur(_));
    let mut change = barre.appliquer(m);
    if du_compositeur {
        change |= barre.appliquer(Msg::Heure(maintenant()));
    }
    change
}

/// Ce qu'il faut à la boucle pour redessiner au plus une fois par passage
/// dans `boucle.dispatch()`, même si une rafale d'événements (p. ex. niri,
/// qui envoie plusieurs lignes d'un coup) y déclenche plusieurs rappels
/// (canal, horloge) : chacun appelle `sur_message`, qui met à jour la barre
/// et lève le drapeau `sale` SANS jamais dessiner ; `apres_dispatch`,
/// appelée une seule fois après le retour de `dispatch()`, dessine si besoin
/// et abaisse le drapeau. `Etat` (wayland.rs) l'implémente pour de vrai ;
/// les tests l'implémentent sur un simple compteur, sans connexion Wayland.
pub trait RedessinDifere {
    fn barre_mut(&mut self) -> &mut Barre;
    fn sale_mut(&mut self) -> &mut bool;
    fn redessiner_maintenant(&mut self);
}

/// Appelé par chaque rappel (message du canal, tic d'horloge) : jamais de
/// redessin ici, seulement le drapeau.
pub fn sur_message(cible: &mut impl RedessinDifere, m: Msg, maintenant: impl Fn() -> HeureLocale) {
    if distribuer(cible.barre_mut(), m, maintenant) {
        *cible.sale_mut() = true;
    }
}

/// Appelé une seule fois après le retour de `boucle.dispatch()` : redessine
/// si le drapeau a été levé par la rafale qui vient d'être traitée, et le
/// remet à zéro dans tous les cas.
pub fn apres_dispatch(cible: &mut impl RedessinDifere) {
    if std::mem::take(cible.sale_mut()) {
        cible.redessiner_maintenant();
    }
}

use crate::journal::ecrire as journal;

/// La connexion Wayland s'est coupée en douceur (le socket s'est fermé :
/// EPIPE à l'écriture, ECONNRESET à la lecture), plutôt qu'avec une
/// véritable erreur de protocole. C'est ce que produisent À LA FOIS une fin
/// de session propre (déconnexion, redémarrage du compositeur — la
/// signature Hyprland change, voir la spec § 3) ET un compositeur qui
/// plante : rien, du côté du client, ne permet de distinguer les deux. Le
/// démon ne se reconnecte jamais lui-même dans aucun des deux cas (c'est la
/// session, par `exec-once` puis `shell-ctl`, qui le relance) : les traiter
/// pareil, comme une fin de session normale, évite qu'un superviseur externe
/// y voie un signe de panne à chaque redémarrage ou déconnexion ordinaire.
///
/// `calloop` emboîte parfois l'erreur d'une source dans une autre (observé
/// sur ce poste : un `OtherError` qui enveloppe l'`IoError` d'origine) : la
/// vraie erreur système n'est donc pas toujours la première de la chaîne.
/// On la cherche en suivant `source()` de bout en bout, plutôt que de ne
/// regarder que le sommet.
fn fin_de_session(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut courant = Some(e);
    while let Some(erreur) = courant {
        if let Some(io) = erreur.downcast_ref::<io::Error>() {
            if matches!(io.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) {
                return true;
            }
        }
        courant = erreur.source();
    }
    false
}

/// Code de sortie du démon selon comment la boucle a fini : 0 après un
/// arrêt demandé (signal, `etat.fin` levé proprement) ou une fin de
/// session (`fin_de_session`) ; 1 pour toute autre panne de la boucle de
/// dispatch calloop (erreur de protocole, bogue…) — pour qu'un superviseur
/// (systemd, nixos-config) sache qu'il doit relancer la barre, au lieu de
/// la croire sortie à la demande.
fn code_de_sortie(panne_dispatch: bool) -> i32 {
    if panne_dispatch { 1 } else { 0 }
}

pub fn lancer() -> i32 {
    // Masque des signaux AVANT tout fil : tous en héritent ; seul le fil de
    // garde les reçoit (sigwait).
    let signaux = match arret::bloquer_signaux() {
        Ok(s) => s,
        Err(e) => {
            journal(&format!("signaux : {e}"));
            return 1;
        }
    };
    let _verrou = match verrou::prendre(&chemins::runtime_dir().join("alpheus-demon.lock")) {
        Ok(v) => v,
        Err(m) => {
            journal(&m);
            return 1;
        }
    };
    let (config, palette, avertissements) = reglages(&chemins::config_dir());
    for a in &avertissements {
        journal(a);
    }
    // cosmic-text analyse les polices avec rayon : un seul fil suffit à une
    // barre, au lieu d'un fil par cœur gardé tout le temps de la session.
    // SAFETY : aucun autre fil n'existe encore (sources et garde partent après).
    unsafe { std::env::set_var("RAYON_NUM_THREADS", "1") };
    let polices = match Polices::charger(&config.police, config.hauteur) {
        Ok((p, avertissement)) => {
            if let Some(a) = avertissement {
                journal(&a);
            }
            p
        }
        Err(m) => {
            journal(&m);
            return 1;
        }
    };
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            journal(&format!("pas de session Wayland : {e}"));
            return 1;
        }
    };
    let (globals, file) = match registry_queue_init::<Etat>(&conn) {
        Ok(x) => x,
        Err(e) => {
            journal(&format!("registre Wayland : {e}"));
            return 1;
        }
    };
    let qh = file.handle();
    let secondes = heure::a_des_secondes(&config.heure.format) || heure::a_des_secondes(&config.heure.date);
    let exec = Arc::new(SystemExecutor::new(Duration::from_secs(2)));
    let mut etat = match Etat::new(&globals, &qh, Barre::new(config), palette, polices, exec) {
        Ok(e) => e,
        Err(m) => {
            journal(&m);
            return 1;
        }
    };
    let mut boucle: EventLoop<Etat> = match EventLoop::try_new() {
        Ok(b) => b,
        Err(e) => {
            journal(&format!("boucle d'événements : {e}"));
            return 1;
        }
    };
    let poignee = boucle.handle();
    if WaylandSource::new(conn.clone(), file).insert(poignee.clone()).is_err() {
        journal("source Wayland : insertion impossible");
        return 1;
    }
    let (tx, rx) = channel::channel::<Courrier>();
    // Chaque rappel (canal, horloge) lève seulement le drapeau `sale` :
    // aucun des deux ne redessine (voir `RedessinDifere`). Une rafale de
    // plusieurs messages (p. ex. niri, qui en envoie plusieurs d'un coup)
    // traités dans le même passage de `dispatch()` ne redessine donc
    // qu'une fois, après la boucle `while`, au lieu d'une fois par message.
    let courrier = poignee.insert_source(rx, |ev, _, etat: &mut Etat| match ev {
        channel::Event::Msg(Courrier::Barre(m)) => sur_message(etat, m, || HeureLocale::maintenant().0),
        channel::Event::Msg(Courrier::Arret) | channel::Event::Closed => etat.fin = true,
    });
    // Horloge : un réveil par changement d'affichage (minute, ou seconde si
    // le format en montre), jamais plus.
    let horloge = poignee.insert_source(Timer::immediate(), move |_, _, etat: &mut Etat| {
        let (h, nanos) = HeureLocale::maintenant();
        sur_message(etat, Msg::Heure(h), || h); // jamais consultée : Msg::Heure n'est pas « du compositeur »
        TimeoutAction::ToDuration(heure::prochain_tic(h.seconde, nanos, secondes))
    });
    if courrier.is_err() || horloge.is_err() {
        journal("boucle d'événements : source refusée");
        return 1;
    }
    let tx_compositeur = tx.clone();
    sources::lancer(sources::detecter(|k| std::env::var_os(k)), move |e| {
        tx_compositeur.send(Courrier::Barre(Msg::Compositeur(e))).is_ok()
    });
    // Fil de garde : attend un signal sans tourner, demande l'arrêt à la
    // boucle, force la sortie si elle ne finit pas dans le délai de grâce.
    let garde = std::thread::Builder::new().name("garde".into()).spawn(move || {
        let sig = arret::attendre_signal(&signaux);
        FIN.store(true, Ordering::SeqCst);
        let _ = tx.send(Courrier::Arret);
        arret::surveiller(&FIN, &FINIE, GRACE, Duration::from_millis(20), || std::process::exit(128 + sig));
    });
    if garde.is_err() {
        journal("fil de garde impossible");
        return 1;
    }
    let mut panne_dispatch = false;
    while !etat.fin {
        if let Err(e) = boucle.dispatch(None, &mut etat) {
            if fin_de_session(&e) {
                journal(&format!("fin de session : {e}"));
            } else {
                journal(&format!("boucle : {e}"));
                panne_dispatch = true;
            }
            break;
        }
        apres_dispatch(&mut etat);
    }
    drop(etat); // surfaces détruites
    let _ = conn.flush();
    FINIE.store(true, Ordering::SeqCst);
    journal("arrêt");
    code_de_sortie(panne_dispatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositeur::exemple;

    const AVANT: HeureLocale = HeureLocale { annee: 2026, mois: 10, jour: 4, jour_semaine: 0, heure: 14, minute: 5, seconde: 9 };
    const APRES: HeureLocale = HeureLocale { minute: 47, ..AVANT };

    #[test]
    fn un_evenement_du_compositeur_remet_l_heure_juste() {
        let mut b = Barre::new(BarreConfig::default());
        assert!(distribuer(&mut b, Msg::Heure(AVANT), || APRES));
        assert_eq!(b.heure, Some(AVANT), "un tic d'horloge ne relit pas l'heure");
        assert!(distribuer(&mut b, Msg::Compositeur(Some(exemple())), || APRES));
        assert_eq!(b.heure, Some(APRES), "sortie de veille : l'heure suit le premier événement");
        assert!(!distribuer(&mut b, Msg::Compositeur(Some(exemple())), || APRES), "rien de neuf : pas de redessin");
    }

    /// Remplace `Etat` (wayland.rs, une vraie connexion Wayland) : compte
    /// les redessins au lieu de dessiner pour de vrai.
    struct Compteur {
        barre: Barre,
        sale: bool,
        redessins: u32,
    }

    impl RedessinDifere for Compteur {
        fn barre_mut(&mut self) -> &mut Barre {
            &mut self.barre
        }
        fn sale_mut(&mut self) -> &mut bool {
            &mut self.sale
        }
        fn redessiner_maintenant(&mut self) {
            self.redessins += 1;
        }
    }

    #[test]
    fn une_rafale_de_messages_ne_redessine_qu_une_fois() {
        let mut c = Compteur { barre: Barre::new(BarreConfig::default()), sale: false, redessins: 0 };
        // Une rafale de 9 messages qui changent CHACUN l'affichage (bascule
        // Some/None, comme les jusqu'à 9 grilles d'une seule rafale niri sur
        // 3 écrans) : chacun appelle `sur_message`, aucun ne redessine
        // directement — sans quoi ce test ne mordrait pas (un message
        // répété, lui, ne change rien à partir du deuxième : voir
        // `etat::tests::messages_et_changements`).
        for i in 0..9 {
            let e = if i % 2 == 0 { Some(exemple()) } else { None };
            sur_message(&mut c, Msg::Compositeur(e), || AVANT);
        }
        apres_dispatch(&mut c);
        assert_eq!(c.redessins, 1, "rafale de 9 messages qui changent tous l'affichage : un seul redessin, pas 9");
        assert!(!c.sale, "le drapeau retombe après le redessin");
        // Rien de neuf dans la rafale suivante : pas de redessin du tout.
        sur_message(&mut c, Msg::Compositeur(Some(exemple())), || AVANT);
        apres_dispatch(&mut c);
        assert_eq!(c.redessins, 1, "rien de changé : aucun redessin de plus");
    }

    #[test]
    fn code_de_sortie_panne_dispatch_renvoie_1() {
        assert_eq!(code_de_sortie(false), 0, "arrêt demandé : code 0, un superviseur ne doit pas relancer");
        assert_eq!(code_de_sortie(true), 1, "boucle de dispatch tombée : code ≠ 0, un superviseur doit relancer");
    }

    #[test]
    fn fin_de_session_reconnait_epipe_et_econnreset_seulement() {
        assert!(fin_de_session(&io::Error::from(io::ErrorKind::BrokenPipe)), "EPIPE : compositeur parti, socket fermé");
        assert!(fin_de_session(&io::Error::from(io::ErrorKind::ConnectionReset)), "ECONNRESET : même chose à la lecture");
        assert!(!fin_de_session(&io::Error::from(io::ErrorKind::PermissionDenied)), "une vraie erreur reste une panne");
        assert!(!fin_de_session(&io::Error::other("erreur de protocole")), "idem");
    }

    #[test]
    fn fin_de_session_traverse_l_emboitement_de_calloop() {
        use smithay_client_toolkit::reexports::calloop::Error as ErreurBoucle;
        // Relevé sur ce poste (sway sans tête, compositeur tué) : calloop
        // renvoie parfois un `OtherError` qui enveloppe l'`IoError` d'une
        // source, au lieu de le rendre directement — la vraie erreur système
        // est alors au deuxième niveau de `source()`, pas au premier.
        let direct = ErreurBoucle::IoError(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(fin_de_session(&direct), "cas direct : IoError au premier niveau");
        let emboite = ErreurBoucle::OtherError(Box::new(ErreurBoucle::IoError(io::Error::from(io::ErrorKind::BrokenPipe))));
        assert!(fin_de_session(&emboite), "cas observé : OtherError(IoError(EPIPE))");
        let sans_rapport = ErreurBoucle::OtherError(Box::new(io::Error::other("erreur de protocole")));
        assert!(!fin_de_session(&sans_rapport), "emboîté mais pas EPIPE/ECONNRESET : reste une panne");
    }

    #[test]
    fn reglages_absents_ou_invalides() {
        let d = std::env::temp_dir().join(format!("alpheus-reglages-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (c, p, w) = reglages(&d);
        assert_eq!((c, p, w.len()), (BarreConfig::default(), Palette::default(), 0));
        std::fs::write(d.join("barre.toml"), "modules = [\"meteo\"]").unwrap();
        std::fs::write(d.join("colors.toml"), "bg = 3").unwrap();
        let (c, _, w) = reglages(&d);
        assert_eq!(c, BarreConfig::default());
        assert!(w.iter().any(|m| m.contains("barre.toml")) && w.iter().any(|m| m.contains("colors.toml")), "{w:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
