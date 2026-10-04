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

fn journal(m: &str) {
    eprintln!("alpheus-demon : {m}");
}

/// Code de sortie du démon selon comment la boucle a fini : 0 après un
/// arrêt demandé (signal, `etat.fin` levé proprement), 1 si la boucle de
/// dispatch calloop est tombée (connexion Wayland perdue, compositeur
/// planté) — pour qu'un superviseur (systemd, nixos-config) sache qu'il
/// doit relancer la barre, au lieu de la croire sortie à la demande.
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
    let courrier = poignee.insert_source(rx, |ev, _, etat: &mut Etat| match ev {
        channel::Event::Msg(Courrier::Barre(m)) => {
            if distribuer(&mut etat.barre, m, || HeureLocale::maintenant().0) {
                etat.redessiner_tout();
            }
        }
        channel::Event::Msg(Courrier::Arret) | channel::Event::Closed => etat.fin = true,
    });
    // Horloge : un réveil par changement d'affichage (minute, ou seconde si
    // le format en montre), jamais plus.
    let horloge = poignee.insert_source(Timer::immediate(), move |_, _, etat: &mut Etat| {
        let (h, nanos) = HeureLocale::maintenant();
        if etat.barre.appliquer(Msg::Heure(h)) {
            etat.redessiner_tout();
        }
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
            journal(&format!("boucle : {e}"));
            panne_dispatch = true;
            break;
        }
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

    #[test]
    fn code_de_sortie_panne_dispatch_renvoie_1() {
        assert_eq!(code_de_sortie(false), 0, "arrêt demandé : code 0, un superviseur ne doit pas relancer");
        assert_eq!(code_de_sortie(true), 1, "boucle de dispatch tombée : code ≠ 0, un superviseur doit relancer");
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
