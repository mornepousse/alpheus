//! Clic et molette → commande. Aller à un bureau passe par la commande de
//! barre.toml (`[bureaux] commande`, défaut `wm-set workspace …`), jamais
//! par une écriture directe au compositeur. La molette calcule le bureau
//! voisin elle-même et envoie une référence absolue.

use crate::compositeur::Bureau;
use crate::etat::{Barre, Donnee};
use crate::grille::{Cible, Grille};
use crate::peintre::colonne_sous;
use alpheus_commun::exec::Executor;

use crate::journal::ecrire as journal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bouton {
    Gauche,
    Droit,
    Milieu,
    MoletteHaut,
    MoletteBas,
}

/// Bouton Wayland (codes evdev BTN_*) → bouton de la barre.
pub fn bouton(code: u32) -> Option<Bouton> {
    match code {
        0x110 => Some(Bouton::Gauche),
        0x111 => Some(Bouton::Droit),
        0x112 => Some(Bouton::Milieu),
        _ => None,
    }
}

/// Bureau suivant (ou précédent) du bureau visible de cette barre, sans boucler.
fn voisin<'a>(barre: &'a Barre, sortie: &str, suivant: bool) -> Option<&'a Bureau> {
    let Donnee::Pret(e) = &barre.compositeur else { return None };
    let l = e.bureaux_de(sortie);
    let i = l.iter().position(|b| b.visible)?;
    let j = if suivant { i + 1 } else { i.checked_sub(1)? };
    l.get(j).copied()
}

/// Remplace `{bureau}` et `{sortie}` en un seul passage sur `modele` (le
/// texte du barre.toml, jamais le résultat déjà substitué) : une référence
/// de bureau qui contiendrait littéralement `{sortie}` (ou `{bureau}`)
/// n'est donc jamais, elle, repassée à la moulinette — contrairement à deux
/// appels successifs de `str::replace` sur la chaîne déjà substituée.
fn substituer(modele: &str, bureau: &str, sortie: &str) -> String {
    let mut sortie_ecrite = String::with_capacity(modele.len());
    let mut reste = modele;
    while let Some(i) = reste.find('{') {
        sortie_ecrite.push_str(&reste[..i]);
        let a_partir_de_l_accolade = &reste[i..];
        if let Some(suite) = a_partir_de_l_accolade.strip_prefix("{bureau}") {
            sortie_ecrite.push_str(bureau);
            reste = suite;
        } else if let Some(suite) = a_partir_de_l_accolade.strip_prefix("{sortie}") {
            sortie_ecrite.push_str(sortie);
            reste = suite;
        } else {
            // Une accolade qui n'ouvre aucun des deux espaces réservés :
            // recopiée telle quelle, on avance d'un caractère.
            sortie_ecrite.push('{');
            reste = &a_partir_de_l_accolade[1..];
        }
    }
    sortie_ecrite.push_str(reste);
    sortie_ecrite
}

/// Commande pour un clic sur `cible` dans la barre de `sortie`.
pub fn commande(barre: &Barre, sortie: &str, cible: &Cible, b: Bouton) -> Option<Vec<String>> {
    match cible {
        Cible::Bureau { reference, sortie: s } => {
            let (bureau, sortie_bureau) = match b {
                Bouton::Gauche => (reference.clone(), s.clone()),
                Bouton::MoletteBas | Bouton::MoletteHaut => {
                    let v = voisin(barre, sortie, b == Bouton::MoletteBas)?;
                    (v.reference.clone(), v.sortie.clone())
                }
                Bouton::Droit | Bouton::Milieu => return None,
            };
            let modele = &barre.config.bureaux.commande;
            Some(modele.iter().map(|a| substituer(a, &bureau, &sortie_bureau)).collect())
        }
    }
}

/// Lance la commande sans l'attendre ; un échec va au journal.
pub fn executer(exec: &dyn Executor, argv: &[String]) {
    let Some((prog, args)) = argv.split_first() else { return };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = exec.spawn(prog, &args) {
        journal(&format!("{} : {e:?}", argv.join(" ")));
    }
}

/// Clic en `x_logique` (px logiques, comme le pointeur Wayland) sur la
/// barre de `sortie`, dont `grille` est la dernière dessinée.
pub fn clic(barre: &Barre, sortie: &str, grille: &Grille, x_logique: f64, echelle: i32, avance: f32, b: Bouton) -> Option<Vec<String>> {
    let col = colonne_sous(x_logique, echelle, avance);
    commande(barre, sortie, grille.cible_en(col)?, b)
}

/// Défilement continu (pavé tactile) qui vaut un cran de molette.
pub const PAS_CONTINU: f64 = 30.0;

/// Un cran de molette en `value120` (wl_pointer v8+, envoyé par Hyprland et
/// niri ; `discrete` n'est plus envoyé par les compositeurs récents).
const CRAN_120: i32 = 120;

#[derive(Debug, Default)]
pub struct Molette {
    cumul: f64,
    cumul120: i32,
}

fn sens(positif: bool) -> Bouton {
    if positif { Bouton::MoletteBas } else { Bouton::MoletteHaut }
}

impl Molette {
    /// Un cran de molette (value120 cumulé à ±120, ou discrete des anciens
    /// compositeurs), ou 30 px de défilement continu, = un bureau.
    /// Wayland : positif = vers le bas.
    pub fn ajouter(&mut self, absolu: f64, discret: i32, value120: i32) -> Option<Bouton> {
        if value120 != 0 {
            self.cumul = 0.0;
            self.cumul120 += value120;
            if self.cumul120.abs() >= CRAN_120 {
                let b = sens(self.cumul120 > 0);
                self.cumul120 = 0;
                return Some(b);
            }
            return None;
        }
        if discret != 0 {
            self.cumul = 0.0;
            return Some(sens(discret > 0));
        }
        self.cumul += absolu;
        if self.cumul >= PAS_CONTINU {
            self.cumul = 0.0;
            Some(Bouton::MoletteBas)
        } else if self.cumul <= -PAS_CONTINU {
            self.cumul = 0.0;
            Some(Bouton::MoletteHaut)
        } else {
            None
        }
    }

    /// Vide le reste continu et le cumul de cran : rien n'est reporté sur
    /// le prochain geste. À appeler quand Wayland dit que CE geste de
    /// molette est fini — `wl_pointer::axis_source` en `AxisScroll::stop`
    /// (`axis_stop`, pavé tactile qui se lève) et `wl_pointer::leave`
    /// (pointeur sorti de la surface) — sans quoi un reste proche du seuil
    /// se combinerait à tort avec le geste suivant. Câblage fait en tâche 12
    /// (Wayland) ; cette tâche ne fournit que la méthode et son test.
    pub fn arreter(&mut self) {
        self.cumul = 0.0;
        self.cumul120 = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositeur::exemple;
    use crate::config::BarreConfig;
    use crate::etat::Msg;
    use alpheus_commun::exec::fake::FakeExecutor;

    #[test]
    fn chaine_du_clic_coordonnee_case_zone_commande() {
        let b = barre();
        let g = b.grille("HDMI-A-1", 100); // « 4 » en cases 5 à 13
        // Avance de 8 px à l'échelle 1, 16 px à l'échelle 2 : même case.
        assert_eq!(clic(&b, "HDMI-A-1", &g, 52.0, 1, 8.0, Bouton::Gauche), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
        assert_eq!(clic(&b, "HDMI-A-1", &g, 52.0, 2, 16.0, Bouton::Gauche), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
        assert_eq!(clic(&b, "HDMI-A-1", &g, 4.0, 1, 8.0, Bouton::Gauche), None, "marge");
        assert_eq!(clic(&b, "HDMI-A-1", &g, 400.0, 1, 8.0, Bouton::Gauche), None, "titre");
        assert_eq!(clic(&b, "HDMI-A-1", &g, 52.0, 1, 8.0, Bouton::MoletteBas), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
    }

    fn barre() -> Barre {
        let mut b = Barre::new(BarreConfig::default());
        b.appliquer(Msg::Compositeur(Some(exemple())));
        b
    }

    fn bureau(reference: &str, sortie: &str) -> Cible {
        Cible::Bureau { reference: reference.into(), sortie: sortie.into() }
    }

    fn argv(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn clic_gauche_va_au_bureau() {
        let b = barre();
        assert_eq!(commande(&b, "HDMI-A-1", &bureau("4", "HDMI-A-1"), Bouton::Gauche), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
        assert_eq!(commande(&b, "DVI-I-1", &bureau("name:web", "DVI-I-1"), Bouton::Gauche), argv(&["wm-set", "workspace", "name:web", "DVI-I-1"]));
        assert_eq!(commande(&b, "HDMI-A-1", &bureau("4", "HDMI-A-1"), Bouton::Droit), None);
        assert_eq!(commande(&b, "HDMI-A-1", &bureau("4", "HDMI-A-1"), Bouton::Milieu), None);
    }

    #[test]
    fn nom_de_bureau_avec_caracteres_speciaux_reste_un_seul_argument() {
        // {bureau} et {sortie} sont substitués dans chaque élément d'argv,
        // jamais via `sh -c` ni par concaténation en une chaîne shell : un
        // nom de bureau avec espaces, « ; » ou « $(…) » reste un seul
        // argument littéral (ni coupé, ni interprété).
        let b = barre();
        let c = bureau("mon bureau; $(rm -rf ~)", "HDMI-A-1");
        assert_eq!(
            commande(&b, "HDMI-A-1", &c, Bouton::Gauche),
            argv(&["wm-set", "workspace", "mon bureau; $(rm -rf ~)", "HDMI-A-1"])
        );
    }

    #[test]
    fn reference_contenant_litteralement_un_espace_reserve_non_resubstituee() {
        // Une référence de bureau qui contient littéralement « {sortie} »
        // (improbable, mais possible) ne doit pas être réécrite par la
        // substitution de {sortie} : {bureau} et {sortie} sont remplacés en
        // une seule passe sur le MODÈLE, jamais en deux passes successives
        // sur le résultat (la deuxième repasserait sur ce que la première a
        // inséré).
        let b = barre();
        let c = bureau("name:{sortie}", "HDMI-A-1");
        assert_eq!(commande(&b, "HDMI-A-1", &c, Bouton::Gauche), argv(&["wm-set", "workspace", "name:{sortie}", "HDMI-A-1"]));
    }

    #[test]
    fn commande_reglable_dans_barre_toml() {
        let mut b = barre();
        b.config.bureaux.commande = vec!["mon-outil".into(), "--aller".into(), "{bureau}".into()];
        assert_eq!(commande(&b, "HDMI-A-1", &bureau("4", "HDMI-A-1"), Bouton::Gauche), argv(&["mon-outil", "--aller", "4"]));
    }

    #[test]
    fn molette_vers_le_bureau_voisin_sans_boucler() {
        let b = barre();
        // HDMI-A-1 : 2 (visible), 4, 5.
        let c = bureau("2", "HDMI-A-1");
        assert_eq!(commande(&b, "HDMI-A-1", &c, Bouton::MoletteBas), argv(&["wm-set", "workspace", "4", "HDMI-A-1"]));
        assert_eq!(commande(&b, "HDMI-A-1", &c, Bouton::MoletteHaut), None, "premier bureau : on ne boucle pas");
        // DVI-I-1 : 3 (visible), web.
        assert_eq!(commande(&b, "DVI-I-1", &bureau("3", "DVI-I-1"), Bouton::MoletteBas), argv(&["wm-set", "workspace", "name:web", "DVI-I-1"]));
        let attente = Barre::new(BarreConfig::default());
        assert_eq!(commande(&attente, "HDMI-A-1", &c, Bouton::MoletteBas), None);
    }

    #[test]
    fn execution_par_l_executeur_injecte() {
        let fake = FakeExecutor::new();
        executer(&fake, &["wm-set".into(), "workspace".into(), "4".into(), "HDMI-A-1".into()]);
        executer(&fake, &[]);
        assert_eq!(fake.calls(), ["wm-set workspace 4 HDMI-A-1"]);
    }

    #[test]
    fn boutons_et_molette() {
        assert_eq!(bouton(0x110), Some(Bouton::Gauche));
        assert_eq!(bouton(0x113), None);
        let mut m = Molette::default();
        // Molette (compositeurs récents) : value120, discrete à 0, absolute ignoré.
        assert_eq!(m.ajouter(15.0, 0, 120), Some(Bouton::MoletteBas));
        assert_eq!(m.ajouter(-15.0, 0, -120), Some(Bouton::MoletteHaut));
        assert_eq!(m.ajouter(7.5, 0, 60), None, "molette fine : demi-cran");
        assert_eq!(m.ajouter(7.5, 0, 60), Some(Bouton::MoletteBas), "deux demi-crans = un bureau");
        // Anciens compositeurs : discrete seul.
        assert_eq!(m.ajouter(15.0, 1, 0), Some(Bouton::MoletteBas));
        // Pavé tactile : ni cran ni value120, 30 px cumulés.
        assert_eq!(m.ajouter(12.0, 0, 0), None);
        assert_eq!(m.ajouter(12.0, 0, 0), None);
        assert_eq!(m.ajouter(12.0, 0, 0), Some(Bouton::MoletteBas));
        assert_eq!(m.ajouter(-31.0, 0, 0), Some(Bouton::MoletteHaut));
    }

    #[test]
    fn arreter_remet_a_zero_le_reste_continu_et_le_cumul_de_cran() {
        let mut m = Molette::default();
        assert_eq!(m.ajouter(25.0, 0, 0), None, "reste sous le seuil des 30 px");
        m.arreter();
        assert_eq!(m.ajouter(5.0, 0, 0), None, "remis à zéro : 25 + 5 l'aurait déclenché sans l'arrêt");
        // Même chose pour le cumul en value120 (cran fin, deux demi-crans).
        assert_eq!(m.ajouter(0.0, 0, 60), None, "demi-cran");
        m.arreter();
        assert_eq!(m.ajouter(0.0, 0, 60), None, "remis à zéro : 60 + 60 l'aurait déclenché sans l'arrêt");
    }
}
