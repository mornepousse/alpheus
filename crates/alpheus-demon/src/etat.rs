//! L'état pur de la barre : ce que les sources ont dit, rien d'autre. Les
//! messages le modifient ; `disposition` en tire la grille de chaque écran.

use crate::compositeur::EtatCompositeur;
use crate::config::BarreConfig;
use crate::heure::HeureLocale;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Donnee<T> {
    /// Rien reçu encore : « … ».
    Attente,
    /// Source tombée : « – ».
    Panne,
    Pret(T),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// None : la source du compositeur est en panne.
    Compositeur(Option<EtatCompositeur>),
    Heure(HeureLocale),
}

#[derive(Debug, Clone)]
pub struct Barre {
    pub config: BarreConfig,
    pub compositeur: Donnee<EtatCompositeur>,
    pub heure: Option<HeureLocale>,
}

impl Barre {
    pub fn new(config: BarreConfig) -> Barre {
        Barre { config, compositeur: Donnee::Attente, heure: None }
    }

    /// Applique un message ; vrai si l'affichage peut avoir changé.
    pub fn appliquer(&mut self, m: Msg) -> bool {
        match m {
            Msg::Compositeur(e) => {
                let n = match e {
                    Some(e) => Donnee::Pret(e),
                    None => Donnee::Panne,
                };
                if n == self.compositeur {
                    false
                } else {
                    self.compositeur = n;
                    true
                }
            }
            Msg::Heure(h) => {
                if self.heure == Some(h) {
                    false
                } else {
                    self.heure = Some(h);
                    true
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositeur::exemple;

    const H: HeureLocale = HeureLocale { annee: 2026, mois: 10, jour: 4, jour_semaine: 0, heure: 14, minute: 5, seconde: 9 };

    #[test]
    fn messages_et_changements() {
        let mut b = Barre::new(BarreConfig::default());
        assert_eq!(b.compositeur, Donnee::Attente);
        assert!(b.appliquer(Msg::Compositeur(Some(exemple()))));
        assert!(!b.appliquer(Msg::Compositeur(Some(exemple()))), "même état : rien à redessiner");
        assert!(b.appliquer(Msg::Compositeur(None)));
        assert_eq!(b.compositeur, Donnee::Panne);
        assert!(!b.appliquer(Msg::Compositeur(None)));
        assert!(b.appliquer(Msg::Heure(H)));
        assert!(!b.appliquer(Msg::Heure(H)));
        assert_eq!(b.heure, Some(H));
    }
}
