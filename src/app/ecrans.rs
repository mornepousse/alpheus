//! Onglet Écrans : profils, sorties, placements en clair. Les réglages
//! eux-mêmes (essai, panneau, enregistrement) s'ajoutent aux tâches suivantes.

use super::App;
use crate::displays::geometry::{deduce, Align, Placement};
use crate::displays::profiles::{placed, Profile};
use crate::displays::{short_from_description, short_names, Output};

/// État de l'onglet hors données lues.
#[derive(Debug, Default)]
pub struct Ecrans {
    pub profiles: Vec<Profile>,
    /// Puce choisie dans la barre des profils ; `profiles.len()` désigne
    /// « + Enregistrer l'actuel ».
    pub profile_sel: usize,
    /// Session niri : l'onglet n'appelle rien.
    pub niri: bool,
}

fn label_of(outs: &[Output], labels: &[String], desc: &str) -> String {
    outs.iter().position(|o| o.description == desc).map_or_else(|| short_from_description(desc), |i| labels[i].clone())
}

/// Placement de chaque sortie, en clair, pour la liste : « origine », « à
/// droite de Alpha », « en dessous de Alpha, centré », « position 0, 1080 ».
pub fn placement_labels(outs: &[Output]) -> Vec<String> {
    let labels = short_names(outs);
    deduce(&placed(outs), None, None)
        .into_iter()
        .zip(outs)
        .map(|(p, o)| match (&o.active, p) {
            (None, _) => String::new(),
            (Some(_), Placement::Origin) => "origine".into(),
            (Some(_), Placement::Relative { relation, reference, align }) => {
                let mut s = format!("{} {}", relation.label(), label_of(outs, &labels, &reference));
                if align != Align::Start {
                    s.push_str(&format!(", {}", align.label(relation)));
                }
                s
            }
            (Some(a), Placement::Absolute { .. }) => format!("position {}, {}", a.x, a.y),
        })
        .collect()
}

impl App {
    /// Sorties lues (vide tant que rien n'est lu).
    pub fn outputs(&self) -> &[Output] {
        self.data.displays.ready().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn profile_bar_move(&mut self, dir: i32) {
        let n = self.ecrans.profiles.len() as i32;
        self.ecrans.profile_sel = (self.ecrans.profile_sel as i32 + dir).clamp(0, n) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action::Tab;
    use crate::app::testing::ready_app;
    use crate::app::worker::{Domain, Request};
    use crate::app::Item;
    use crate::displays::testing::*;
    use std::time::{Duration, Instant};

    #[test]
    fn elements_de_l_onglet() {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        assert_eq!(a.take_requests(), vec![Request::Refresh(vec![Domain::Displays])]);
        assert_eq!(a.items(), vec![
            Item::DisplayProfiles,
            Item::Output("eDP-1".into()),
            Item::Output("DVI-I-1".into()),
            Item::Output("HDMI-A-1".into()),
        ]);
    }

    #[test]
    fn sous_niri_l_onglet_n_appelle_rien() {
        let mut a = ready_app();
        a.ecrans.niri = true;
        a.go_tab(Tab::Ecrans, Instant::now());
        assert!(a.take_requests().is_empty());
        assert!(a.items().is_empty());
        a.on_tick(Instant::now() + Duration::from_secs(30));
        assert!(a.take_requests().is_empty(), "ni au tic");
    }

    #[test]
    fn barre_des_profils_bornee() {
        let mut a = ready_app();
        a.go_tab(Tab::Ecrans, Instant::now());
        a.adjust(1, Instant::now());
        a.adjust(1, Instant::now());
        a.adjust(1, Instant::now());
        assert_eq!(a.ecrans.profile_sel, 2, "deux profils, puis « + Enregistrer l'actuel »");
        a.adjust(-1, Instant::now());
        assert_eq!(a.ecrans.profile_sel, 1);
        assert!(a.take_requests().iter().all(|r| matches!(r, Request::Refresh(_))), "choisir n'applique rien");
    }

    #[test]
    fn libelles_de_placement() {
        assert_eq!(placement_labels(&poste()), vec!["position 0, 1080", "origine", "à droite de Alpha"]);
        let outs = vec![on("DP-1", ALPHA, 1920, 1080, 0, 0), on("DP-2", BRAVO, 800, 600, 560, 1080), off("DP-3", "Charlie 3")];
        assert_eq!(placement_labels(&outs), vec!["origine", "en dessous de Alpha, centré", ""]);
    }
}
