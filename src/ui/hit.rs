//! Zones cliquables relevées au rendu, consultées par la souris.

use crate::app::action::Tab;
use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab(Tab),
    /// Ligne d'un élément focalisable (index dans App::items()).
    Item(usize),
    /// Barre d'un curseur (index dans App::items()).
    Slider { item: usize },
    /// Option du profil d'énergie (index dans Profiles::available).
    ProfileOption(usize),
    ConfirmYes,
    ConfirmNo,
    PaletteEntry(usize),
    /// Puce de la barre des profils d'écran (= nombre de profils :
    /// « + Enregistrer l'actuel »).
    DisplayProfile(usize),
    /// Interrupteur d'une sortie (index dans App::items()).
    OutputToggle(usize),
    /// Intérieur d'une surcouche : absorbe le clic.
    Overlay,
}

#[derive(Debug, Clone, Default)]
pub struct HitMap {
    regions: Vec<(Rect, Target)>,
}

impl HitMap {
    pub fn push(&mut self, r: Rect, t: Target) {
        if r.width > 0 && r.height > 0 {
            self.regions.push((r, t));
        }
    }

    pub fn at(&self, x: u16, y: u16) -> Option<(Rect, Target)> {
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
            .copied()
    }

    pub fn find(&self, t: Target) -> Option<Rect> {
        self.regions.iter().rev().find(|(_, u)| *u == t).map(|(r, _)| *r)
    }
}

pub fn slider_pick(bar: Rect, x: u16) -> i32 {
    if bar.width <= 1 {
        return 0;
    }
    let span = i32::from(bar.width - 1);
    let off = i32::from(x.saturating_sub(bar.x)).min(span);
    (off * 100 + span / 2) / span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_zone_la_plus_recente_gagne() {
        let mut h = HitMap::default();
        h.push(Rect::new(0, 2, 80, 1), Target::Item(0));
        h.push(Rect::new(23, 2, 24, 1), Target::Slider { item: 0 });
        h.push(Rect::new(0, 0, 0, 1), Target::Overlay); // vide : ignorée
        assert_eq!(h.at(30, 2).map(|x| x.1), Some(Target::Slider { item: 0 }));
        assert_eq!(h.at(5, 2).map(|x| x.1), Some(Target::Item(0)));
        assert_eq!(h.at(5, 3), None);
        assert_eq!(h.at(80, 2), None, "bord droit exclu");
    }

    #[test]
    fn valeur_d_un_clic_sur_une_barre() {
        let bar = Rect::new(10, 0, 21, 1);
        assert_eq!(slider_pick(bar, 10), 0);
        assert_eq!(slider_pick(bar, 30), 100);
        assert_eq!(slider_pick(bar, 20), 50);
        assert_eq!(slider_pick(bar, 2), 0, "glissé à gauche de la barre");
        assert_eq!(slider_pick(bar, 70), 100, "glissé à droite de la barre");
        assert_eq!(slider_pick(Rect::new(0, 0, 1, 1), 0), 0);
    }
}
