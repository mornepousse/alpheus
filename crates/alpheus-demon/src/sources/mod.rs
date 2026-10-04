//! Sources d'événements du compositeur : connexion en LECTURE seule,
//! publication de l'état normalisé, relance à délai croissant quand la
//! source tombe (1 s, 2 s, 4 s… 30 s au plus ; retour à 1 s après un
//! succès). Pendant une panne, la barre affiche « – ».

pub mod hypr;
pub mod niri;

use crate::compositeur::EtatCompositeur;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compositeur {
    Hyprland { dossier: PathBuf },
    Niri { socket: PathBuf },
}

/// Même règle que `compositor()` de nixos-config (home/scripts/lib.sh) :
/// NIRI_SOCKET d'abord, puis HYPRLAND_INSTANCE_SIGNATURE.
pub fn detecter(var: impl Fn(&str) -> Option<OsString>) -> Option<Compositeur> {
    let non_vide = |k: &str| var(k).filter(|v| !v.is_empty());
    if let Some(s) = non_vide("NIRI_SOCKET") {
        return Some(Compositeur::Niri { socket: s.into() });
    }
    // La signature est un nom de dossier : « / » ou « .. » en sortiraient.
    let sig = non_vide("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| s != ".." && !s.as_encoded_bytes().contains(&b'/'))?;
    let runtime = non_vide("XDG_RUNTIME_DIR")?;
    Some(Compositeur::Hyprland { dossier: PathBuf::from(runtime).join("hypr").join(sig) })
}

pub const DELAI_MAX: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Relance {
    prochain: Duration,
}

impl Default for Relance {
    fn default() -> Self {
        Relance::new()
    }
}

impl Relance {
    pub fn new() -> Self {
        Relance { prochain: Duration::from_secs(1) }
    }

    /// Délai avant la prochaine tentative ; le suivant double (30 s au plus).
    pub fn suivant(&mut self) -> Duration {
        let d = self.prochain;
        self.prochain = (d * 2).min(DELAI_MAX);
        d
    }

    /// La source a fonctionné : la prochaine panne repart de 1 s.
    pub fn reussite(&mut self) {
        self.prochain = Duration::from_secs(1);
    }
}

/// Durée au-delà de laquelle une session qui a publié compte comme une
/// réussite, même sans événement après l'état initial.
pub const DUREE_STABLE: Duration = Duration::from_secs(10);

/// Fait tourner `session` (bloquante, rend Err quand la source tombe) tant
/// que `envoyer` accepte : publie chaque état, publie `None` (« – ») à
/// chaque panne, puis attend le délai de relance. `dormir`, `maintenant` et
/// `journal` sont injectés : les tests comptent délais et lignes de journal
/// en temps simulé.
pub fn superviser(
    mut session: impl FnMut(&mut dyn FnMut(EtatCompositeur) -> bool) -> io::Result<()>,
    envoyer: impl Fn(Option<EtatCompositeur>) -> bool,
    mut dormir: impl FnMut(Duration),
    mut maintenant: impl FnMut() -> Instant,
    mut journal: impl FnMut(&str),
) {
    let mut relance = Relance::new();
    let mut premier = true;
    loop {
        let mut publications = 0u32;
        let mut ferme = false;
        let debut = maintenant();
        let r = session(&mut |e| {
            publications += 1;
            let ok = envoyer(Some(e));
            ferme |= !ok;
            ok
        });
        if ferme {
            return; // la barre s'arrête
        }
        // Réussite : au moins un événement du flux après l'état initial (une
        // seconde publication), ou une session tenue DUREE_STABLE. Une source
        // qui sert l'état initial puis tombe aussitôt recule donc au lieu de
        // boucler à 1 s (réveils, barre qui clignote, journal inondé).
        let duree = maintenant().saturating_duration_since(debut);
        let reussie = publications >= 2 || (publications >= 1 && duree >= DUREE_STABLE);
        if reussie {
            relance.reussite();
        }
        if let Err(e) = r {
            // Journal à la première panne et après chaque reprise, pas à
            // chaque tentative (jusqu'à 2 880 lignes par jour sinon).
            if premier || reussie {
                journal(&format!("alpheus-demon : source du compositeur en panne : {e}"));
            }
        }
        premier = false;
        if !envoyer(None) {
            return;
        }
        dormir(relance.suivant());
    }
}

/// Lance la source dans son fil. Aucun compositeur reconnu : « – » une
/// fois, sans relance (rien ne changera dans cette session).
pub fn lancer(c: Option<Compositeur>, envoyer: impl Fn(Option<EtatCompositeur>) -> bool + Send + 'static) {
    std::thread::Builder::new()
        .name("compositeur".into())
        .spawn(move || match c {
            None => {
                eprintln!("alpheus-demon : ni niri ni Hyprland détecté (NIRI_SOCKET, HYPRLAND_INSTANCE_SIGNATURE)");
                envoyer(None);
            }
            Some(Compositeur::Hyprland { dossier }) => superviser(|p| hypr::session(&dossier, p), envoyer, std::thread::sleep, Instant::now, |m| eprintln!("{m}")),
            Some(Compositeur::Niri { socket }) => superviser(|p| niri::session(&socket, p), envoyer, std::thread::sleep, Instant::now, |m| eprintln!("{m}")),
        })
        .expect("fil du compositeur");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn env(paires: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| paires.iter().find(|(c, _)| *c == k).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn detection_niri_d_abord() {
        assert_eq!(
            detecter(env(&[("NIRI_SOCKET", "/run/niri.sock"), ("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("XDG_RUNTIME_DIR", "/run/user/1000")])),
            Some(Compositeur::Niri { socket: "/run/niri.sock".into() })
        );
        assert_eq!(
            detecter(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc"), ("XDG_RUNTIME_DIR", "/run/user/1000")])),
            Some(Compositeur::Hyprland { dossier: "/run/user/1000/hypr/abc".into() })
        );
        assert_eq!(detecter(env(&[("NIRI_SOCKET", ""), ("HYPRLAND_INSTANCE_SIGNATURE", "")])), None);
        assert_eq!(detecter(env(&[("HYPRLAND_INSTANCE_SIGNATURE", "abc")])), None, "sans XDG_RUNTIME_DIR");
    }

    #[test]
    fn delai_double_jusqu_a_30_s() {
        let mut r = Relance::new();
        let d: Vec<u64> = (0..8).map(|_| r.suivant().as_secs()).collect();
        assert_eq!(d, [1, 2, 4, 8, 16, 30, 30, 30]);
        r.reussite();
        assert_eq!(r.suivant(), Duration::from_secs(1));
    }

    /// Horloge simulée : avance de `pas` à chaque lecture.
    fn horloge(pas: Duration) -> impl FnMut() -> Instant {
        let mut t = Instant::now();
        move || {
            t += pas;
            t
        }
    }

    #[test]
    fn panne_tiret_puis_relance_a_delai_croissant() {
        let delais = RefCell::new(Vec::new());
        let envois = RefCell::new(Vec::new());
        let mut tour = 0;
        superviser(
            |publier| {
                tour += 1;
                match tour {
                    1..=6 | 8 => Err(io::Error::other("refusé")),
                    7 => {
                        publier(EtatCompositeur::default()); // état initial
                        publier(EtatCompositeur::default()); // un événement du flux
                        Err(io::Error::other("tombé"))
                    }
                    _ => {
                        publier(EtatCompositeur::default());
                        Ok(())
                    }
                }
            },
            |e| {
                let mut v = envois.borrow_mut();
                v.push(e.is_some());
                v.len() < 11 // le 11e envoi : la barre s'arrête
            },
            |d| delais.borrow_mut().push(d.as_secs()),
            horloge(Duration::ZERO),
            |_| {},
        );
        assert_eq!(*delais.borrow(), [1, 2, 4, 8, 16, 30, 1, 2], "retour à 1 s après le succès du tour 7");
        let (f, t) = (false, true);
        assert_eq!(*envois.borrow(), [f, f, f, f, f, f, t, t, f, f, t], "« – » (None) à chaque panne");
    }

    /// Une source qui sert l'état initial puis tombe aussitôt : le délai
    /// croît quand même, une seule ligne au journal.
    fn bagotage(mut session: impl FnMut(&mut dyn FnMut(EtatCompositeur) -> bool) -> io::Result<()>, pas: Duration) -> (Vec<u64>, Vec<String>, usize) {
        let delais = RefCell::new(Vec::new());
        let journal = RefCell::new(Vec::new());
        let publies = RefCell::new(0);
        let cycles = RefCell::new(0);
        superviser(
            &mut session,
            |e| {
                if e.is_some() {
                    *publies.borrow_mut() += 1;
                    return true;
                }
                *cycles.borrow_mut() += 1;
                *cycles.borrow() < 6
            },
            |d| delais.borrow_mut().push(d.as_secs()),
            horloge(pas),
            |m| journal.borrow_mut().push(m.to_string()),
        );
        (delais.into_inner(), journal.into_inner(), publies.into_inner())
    }

    #[test]
    fn etat_initial_seul_ne_compte_pas_comme_reussite() {
        let (delais, journal, _) = bagotage(
            |p| {
                p(EtatCompositeur::default());
                Err(io::Error::other("tombé"))
            },
            Duration::from_secs(1),
        );
        assert_eq!(delais, [1, 2, 4, 8, 16]);
        assert_eq!(journal.len(), 1, "{journal:?}");
    }

    #[test]
    fn session_de_10_s_compte_comme_reussite() {
        let (delais, journal, _) = bagotage(
            |p| {
                p(EtatCompositeur::default());
                Err(io::Error::other("tombé"))
            },
            DUREE_STABLE,
        );
        assert_eq!(delais, [1, 1, 1, 1, 1]);
        assert_eq!(journal.len(), 6, "une ligne par panne après chaque reprise");
    }

    #[test]
    fn hyprland_bagoteur_recule() {
        let f = hypr::faux::FauxHyprland::demarrer_avec("bagoteur", hypr::faux::CLIENTS.as_bytes(), true);
        let d = f.dossier.clone();
        let (delais, journal, publies) = bagotage(|p| hypr::session(&d, p), Duration::ZERO);
        assert_eq!(publies, 6, "l'état initial servi à chaque tour");
        assert_eq!(delais, [1, 2, 4, 8, 16]);
        assert_eq!(journal.len(), 1, "{journal:?}");
    }

    #[test]
    fn niri_bagoteur_recule() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dossier = std::env::temp_dir().join(format!("alpheus-niri-bagoteur-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dossier);
        std::fs::create_dir_all(&dossier).unwrap();
        let chemin = dossier.join("niri.sock");
        let l = UnixListener::bind(&chemin).unwrap();
        // Chaque connexion : l'état initial complet, puis fermeture.
        let initial: String = include_str!("../../tests/fixtures/niri-evenements.jsonl").lines().take(6).map(|l| format!("{l}\n")).collect();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { return };
                let mut demande = String::new();
                let _ = BufReader::new(&c).read_line(&mut demande);
                let _ = (&c).write_all(initial.as_bytes());
            }
        });
        let (delais, journal, publies) = bagotage(|p| niri::session(&chemin, p), Duration::ZERO);
        assert_eq!(publies, 6, "un seul état publié par tour, une fois l'état initial complet");
        assert_eq!(delais, [1, 2, 4, 8, 16]);
        assert_eq!(journal.len(), 1, "{journal:?}");
        let _ = std::fs::remove_dir_all(&dossier);
    }

    #[test]
    fn signature_hyprland_qui_sort_du_dossier_refusee() {
        for sig in ["../../etc", "a/b", ".."] {
            let v: &'static str = Box::leak(sig.to_string().into_boxed_str());
            let paires: &'static [(&str, &str)] = Box::leak(Box::new([("HYPRLAND_INSTANCE_SIGNATURE", v), ("XDG_RUNTIME_DIR", "/run/user/1000")]));
            assert_eq!(detecter(env(paires)), None, "{sig}");
        }
    }
}
