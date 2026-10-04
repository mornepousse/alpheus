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
use std::time::Duration;

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
    let sig = non_vide("HYPRLAND_INSTANCE_SIGNATURE")?;
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

/// Fait tourner `session` (bloquante, rend Err quand la source tombe) tant
/// que `envoyer` accepte : publie chaque état, publie `None` (« – ») à
/// chaque panne, puis attend le délai de relance. `dormir` est injecté : les
/// tests comptent les délais en temps simulé.
pub fn superviser(
    mut session: impl FnMut(&mut dyn FnMut(EtatCompositeur) -> bool) -> io::Result<()>,
    envoyer: impl Fn(Option<EtatCompositeur>) -> bool,
    mut dormir: impl FnMut(Duration),
) {
    let mut relance = Relance::new();
    let mut premier = true;
    loop {
        let mut recu = false;
        let mut ferme = false;
        let r = session(&mut |e| {
            recu = true;
            let ok = envoyer(Some(e));
            ferme |= !ok;
            ok
        });
        if ferme {
            return; // la barre s'arrête
        }
        if recu {
            relance.reussite();
        }
        if let Err(e) = r {
            // Journal à la première panne et après chaque reprise, pas à
            // chaque tentative (jusqu'à 2 880 lignes par jour sinon).
            if premier || recu {
                eprintln!("alpheus-demon : source du compositeur en panne : {e}");
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
            Some(Compositeur::Hyprland { dossier }) => superviser(|p| hypr::session(&dossier, p), envoyer, std::thread::sleep),
            Some(Compositeur::Niri { socket }) => superviser(|p| niri::session(&socket, p), envoyer, std::thread::sleep),
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
                        publier(EtatCompositeur::default());
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
                v.len() < 10 // le 10e envoi : la barre s'arrête
            },
            |d| delais.borrow_mut().push(d.as_secs()),
        );
        assert_eq!(*delais.borrow(), [1, 2, 4, 8, 16, 30, 1, 2], "retour à 1 s après le succès du tour 7");
        let (f, t) = (false, true);
        assert_eq!(*envois.borrow(), [f, f, f, f, f, f, t, f, f, t], "« – » (None) à chaque panne");
    }
}
