//! Ce que la barre sait du compositeur, quel qu'il soit : bureaux, fenêtres
//! par bureau, urgence, fenêtre active. `hypr` et `niri` traduisent leur
//! compositeur vers cet état normalisé ; le reste du démon ne connaît que lui.

pub mod hypr;
pub mod niri;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EtatCompositeur {
    pub bureaux: Vec<Bureau>,
    /// Fenêtre qui a le focus, s'il y en a une.
    pub fenetre: Option<Fenetre>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bureau {
    /// Affiché dans la barre.
    pub nom: String,
    /// Passé à `wm-set workspace` pour y aller.
    pub reference: String,
    /// Nom de la sortie (eDP-1, HDMI-A-1…).
    pub sortie: String,
    pub fenetres: u16,
    pub urgent: bool,
    /// Bureau affiché sur sa sortie.
    pub visible: bool,
    /// Visible sur la sortie qui a le focus.
    pub focalise: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fenetre {
    /// Classe (Hyprland) ou app_id (niri).
    pub classe: String,
    pub titre: String,
}

impl EtatCompositeur {
    /// Bureaux de la sortie `sortie`, dans l'ordre. Sortie dont le
    /// compositeur ne connaît aucun bureau (nom vide, écran tout juste
    /// branché) : tous les bureaux, plutôt qu'une barre vide.
    pub fn bureaux_de(&self, sortie: &str) -> Vec<&Bureau> {
        let a: Vec<&Bureau> = self.bureaux.iter().filter(|b| b.sortie == sortie).collect();
        if a.is_empty() { self.bureaux.iter().collect() } else { a }
    }
}

/// État de référence des tests : trois écrans, un bureau urgent à six
/// fenêtres, un bureau nommé.
#[cfg(test)]
pub(crate) fn exemple() -> EtatCompositeur {
    let b = |nom: &str, reference: &str, sortie: &str, fenetres: u16, urgent: bool, visible: bool, focalise: bool| Bureau {
        nom: nom.into(),
        reference: reference.into(),
        sortie: sortie.into(),
        fenetres,
        urgent,
        visible,
        focalise,
    };
    EtatCompositeur {
        bureaux: vec![
            b("1", "1", "eDP-1", 0, false, true, false),
            b("2", "2", "HDMI-A-1", 1, false, true, true),
            b("4", "4", "HDMI-A-1", 6, true, false, false),
            b("5", "5", "HDMI-A-1", 0, false, false, false),
            b("3", "3", "DVI-I-1", 2, false, true, false),
            b("web", "name:web", "DVI-I-1", 1, false, false, false),
        ],
        fenetre: Some(Fenetre { classe: "firefox".into(), titre: "Page web — Mozilla Firefox".into() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bureaux_par_sortie_et_sortie_inconnue() {
        let e = exemple();
        let noms = |s: &str| e.bureaux_de(s).iter().map(|b| b.nom.as_str()).collect::<Vec<_>>();
        assert_eq!(noms("HDMI-A-1"), ["2", "4", "5"]);
        assert_eq!(noms("DVI-I-1"), ["3", "web"]);
        assert_eq!(noms(""), ["1", "2", "4", "5", "3", "web"]);
        assert_eq!(noms("HEADLESS-1"), ["1", "2", "4", "5", "3", "web"]);
    }
}
