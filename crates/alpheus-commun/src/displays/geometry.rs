//! Géométrie (pure, sans appel) : placements relatifs ↔ coordonnées
//! absolues. Taille logique = mode ÷ échelle, largeur et hauteur échangées
//! pour les rotations 1, 3, 5 et 7. L'origine est posée en (0, 0), puis
//! l'agencement entier est décalé pour qu'aucune coordonnée ne soit
//! négative (coin haut-gauche en (0, 0)).

use super::{Mode, Scale, short_from_description};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    RightOf,
    LeftOf,
    Above,
    Below,
}

impl Relation {
    pub const ALL: [Relation; 4] = [Relation::RightOf, Relation::LeftOf, Relation::Above, Relation::Below];

    pub fn label(self) -> &'static str {
        match self {
            Relation::RightOf => "à droite de",
            Relation::LeftOf => "à gauche de",
            Relation::Above => "au-dessus de",
            Relation::Below => "en dessous de",
        }
    }

    /// Côte à côte (l'alignement est alors vertical).
    fn horizontal(self) -> bool {
        matches!(self, Relation::RightOf | Relation::LeftOf)
    }
}

/// Alignement le long du bord commun : début (haut ou gauche), centre, fin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    pub const ALL: [Align; 3] = [Align::Start, Align::Center, Align::End];

    /// Libellé selon la relation : « en haut » à côté, « à gauche » dessous.
    pub fn label(self, rel: Relation) -> &'static str {
        match (self, rel.horizontal()) {
            (Align::Start, true) => "en haut",
            (Align::Start, false) => "à gauche",
            (Align::Center, _) => "centré",
            (Align::End, true) => "en bas",
            (Align::End, false) => "à droite",
        }
    }
}

/// Placement d'un écran. `Absolute` : décalage par rapport à l'origine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    Origin,
    Relative { relation: Relation, reference: String, align: Align },
    Absolute { x: i32, y: i32 },
}

/// Un écran pour le calcul : `id` = description (stable), `label` = nom
/// court (messages), `size` = taille logique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub size: (i32, i32),
    pub placement: Placement,
}

/// Refus de la géométrie, avec sa raison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeoError {
    NoScreen,
    TwoOrigins(String, String),
    MissingReference { screen: String, reference: String },
    DisabledReference { screen: String, reference: String },
    Cycle(String),
    Overlap(String, String),
}

impl fmt::Display for GeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeoError::NoScreen => write!(f, "aucun écran actif"),
            GeoError::TwoOrigins(a, b) => write!(f, "deux origines : {a} et {b}"),
            GeoError::MissingReference { screen, reference } => {
                write!(f, "{screen} est placé par rapport à {reference}, absent")
            }
            GeoError::DisabledReference { screen, reference } => {
                write!(f, "{screen} est placé par rapport à {reference}, désactivé")
            }
            GeoError::Cycle(s) => write!(f, "placement circulaire autour de {s}"),
            GeoError::Overlap(a, b) => write!(f, "{a} et {b} se chevauchent"),
        }
    }
}

/// Taille logique : mode ÷ échelle, arrondie ; axes échangés à 90° et 270°.
pub fn logical_size(mode: Mode, scale: Scale, transform: u8) -> (i32, i32) {
    let div = |px: u32| (f64::from(px) * 1000.0 / f64::from(scale.0.max(1))).round() as i32;
    let (w, h) = (div(mode.width), div(mode.height));
    if transform % 2 == 1 { (h, w) } else { (w, h) }
}

/// Début de l'écran (longueur `len`) le long d'un bord qui commence en `r0`
/// et mesure `rlen`.
fn along(r0: i32, rlen: i32, len: i32, align: Align) -> i32 {
    match align {
        Align::Start => r0,
        Align::Center => r0 + (f64::from(rlen - len) / 2.0).round() as i32,
        Align::End => r0 + rlen - len,
    }
}

/// Position d'un écran de taille `size` placé `rel` d'une référence posée
/// en `r` (coin haut-gauche) avec la taille `rsize`.
fn next_to(r: (i32, i32), rsize: (i32, i32), size: (i32, i32), rel: Relation, align: Align) -> (i32, i32) {
    let ((rx, ry), (rw, rh), (w, h)) = (r, rsize, size);
    match rel {
        Relation::RightOf => (rx + rw, along(ry, rh, h, align)),
        Relation::LeftOf => (rx - w, along(ry, rh, h, align)),
        Relation::Below => (along(rx, rw, w, align), ry + rh),
        Relation::Above => (along(rx, rw, w, align), ry - h),
    }
}

fn label_of(screens: &[Screen], id: &str) -> String {
    screens.iter().find(|s| s.id == id).map_or_else(|| short_from_description(id), |s| s.label.clone())
}

struct Solver<'a> {
    screens: &'a [Screen],
    pos: Vec<Option<(i32, i32)>>,
    /// 0 : à placer, 1 : en cours (détection de cycle), 2 : placé.
    state: Vec<u8>,
}

impl Solver<'_> {
    fn place(&mut self, i: usize) -> Result<(i32, i32), GeoError> {
        let s = &self.screens[i];
        match self.state[i] {
            2 => return Ok(self.pos[i].unwrap_or((0, 0))),
            1 => return Err(GeoError::Cycle(s.label.clone())),
            _ => {}
        }
        self.state[i] = 1;
        let p = match &s.placement {
            Placement::Origin => (0, 0),
            Placement::Absolute { x, y } => (*x, *y),
            Placement::Relative { relation, reference, align } => {
                let missing = || GeoError::MissingReference { screen: s.label.clone(), reference: label_of(self.screens, reference) };
                let r = self.screens.iter().position(|o| o.id == *reference).ok_or_else(missing)?;
                if !self.screens[r].enabled {
                    return Err(GeoError::DisabledReference { screen: s.label.clone(), reference: self.screens[r].label.clone() });
                }
                let rp = self.place(r)?;
                next_to(rp, self.screens[r].size, s.size, *relation, *align)
            }
        };
        self.state[i] = 2;
        self.pos[i] = Some(p);
        Ok(p)
    }
}

/// Coordonnées absolues des écrans activés, dans l'ordre de `screens`
/// (`None` pour un écran désactivé). Refus : aucun écran actif, deux
/// origines, référence absente ou désactivée, cycle, chevauchement.
pub fn resolve(screens: &[Screen]) -> Result<Vec<Option<(i32, i32)>>, GeoError> {
    let enabled: Vec<usize> = (0..screens.len()).filter(|&i| screens[i].enabled).collect();
    if enabled.is_empty() {
        return Err(GeoError::NoScreen);
    }
    let origins: Vec<usize> = enabled.iter().copied().filter(|&i| screens[i].placement == Placement::Origin).collect();
    if let [a, b, ..] = origins[..] {
        return Err(GeoError::TwoOrigins(screens[a].label.clone(), screens[b].label.clone()));
    }
    let mut solver = Solver { screens, pos: vec![None; screens.len()], state: vec![0; screens.len()] };
    for &i in &enabled {
        solver.place(i)?;
    }
    let mut pos = solver.pos;
    // Aucune coordonnée négative : le coin haut-gauche de l'ensemble en (0, 0).
    let min_x = enabled.iter().filter_map(|&i| pos[i]).map(|p| p.0).min().unwrap_or(0);
    let min_y = enabled.iter().filter_map(|&i| pos[i]).map(|p| p.1).min().unwrap_or(0);
    for p in pos.iter_mut().flatten() {
        *p = (p.0 - min_x, p.1 - min_y);
    }
    for (n, &i) in enabled.iter().enumerate() {
        for &j in &enabled[n + 1..] {
            let (Some((ax, ay)), Some((bx, by))) = (pos[i], pos[j]) else { continue };
            let ((aw, ah), (bw, bh)) = (screens[i].size, screens[j].size);
            if ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah {
                return Err(GeoError::Overlap(screens[i].label.clone(), screens[j].label.clone()));
            }
        }
    }
    Ok(pos)
}

/// Écran déjà posé, pour la déduction : coordonnées et taille logiques.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub id: String,
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub size: (i32, i32),
}

/// Relation et alignement qui reproduisent exactement la position de `s`
/// à partir de `r`, s'il en existe.
fn fits(s: &Placed, r: &Placed) -> Option<(Relation, Align)> {
    for rel in [Relation::RightOf, Relation::Below, Relation::LeftOf, Relation::Above] {
        for al in Align::ALL {
            if next_to((r.x, r.y), r.size, s.size, rel, al) == (s.x, s.y) {
                return Some((rel, al));
            }
        }
    }
    None
}

/// Placements qui reproduisent ces coordonnées. L'origine est `origin` si
/// donné, sinon l'écran en (0, 0), sinon le plus haut (puis le plus à
/// gauche). Chaque écran est placé contre un voisin déjà placé quand une
/// relation et un alignement reproduisent exactement sa position, sinon en
/// coordonnées absolues (décalage par rapport à l'origine). `avoid` : écran
/// que personne ne prend pour référence ni pour origine (celui qu'on règle).
/// Un écran désactivé reçoit `Origin`, ignoré par `resolve`.
pub fn deduce(screens: &[Placed], origin: Option<usize>, avoid: Option<usize>) -> Vec<Placement> {
    let mut out = vec![Placement::Origin; screens.len()];
    let enabled: Vec<usize> = (0..screens.len()).filter(|&i| screens[i].enabled).collect();
    let candidates: Vec<usize> = match enabled.iter().copied().filter(|&i| Some(i) != avoid).collect::<Vec<_>>() {
        v if v.is_empty() => enabled.clone(),
        v => v,
    };
    let Some(o) = origin.filter(|i| enabled.contains(i)).or_else(|| {
        candidates
            .iter()
            .copied()
            .find(|&i| (screens[i].x, screens[i].y) == (0, 0))
            .or_else(|| candidates.iter().copied().min_by_key(|&i| (screens[i].y, screens[i].x)))
    }) else {
        return out;
    };
    let mut placed = vec![o];
    loop {
        let mut progress = false;
        for &i in &enabled {
            if placed.contains(&i) {
                continue;
            }
            let found = placed.iter().filter(|&&r| Some(r) != avoid).find_map(|&r| fits(&screens[i], &screens[r]).map(|f| (r, f)));
            if let Some((r, (relation, align))) = found {
                out[i] = Placement::Relative { relation, reference: screens[r].id.clone(), align };
                placed.push(i);
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    for &i in &enabled {
        if !placed.contains(&i) {
            out[i] = Placement::Absolute { x: screens[i].x - screens[o].x, y: screens[i].y - screens[o].y };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::testing::mode;

    fn scr(id: &str, w: i32, h: i32, p: Placement) -> Screen {
        Screen { id: id.into(), label: id.into(), enabled: true, size: (w, h), placement: p }
    }
    fn rel(relation: Relation, reference: &str, align: Align) -> Placement {
        Placement::Relative { relation, reference: reference.into(), align }
    }
    fn at(screens: &[Screen]) -> Vec<Option<(i32, i32)>> {
        resolve(screens).unwrap()
    }

    #[test]
    fn taille_logique() {
        assert_eq!(logical_size(mode(1920, 1080, 60), Scale::ONE, 0), (1920, 1080));
        assert_eq!(logical_size(mode(2560, 1600, 60), Scale(2000), 0), (1280, 800));
        assert_eq!(logical_size(mode(2560, 1600, 60), Scale(1200), 0), (2133, 1333), "arrondi");
        for t in [1, 3, 5, 7] {
            assert_eq!(logical_size(mode(1920, 1080, 60), Scale::ONE, t), (1080, 1920), "rotation {t}");
        }
        for t in [2, 4, 6] {
            assert_eq!(logical_size(mode(1920, 1080, 60), Scale::ONE, t), (1920, 1080), "rotation {t}");
        }
    }

    #[test]
    fn chaque_relation_et_chaque_alignement() {
        // Référence 1000×600 à l'origine, écran placé 400×200.
        let cas = [
            (Relation::RightOf, Align::Start, (1000, 0)),
            (Relation::RightOf, Align::Center, (1000, 200)),
            (Relation::RightOf, Align::End, (1000, 400)),
            (Relation::LeftOf, Align::Start, (-400, 0)),
            (Relation::LeftOf, Align::Center, (-400, 200)),
            (Relation::LeftOf, Align::End, (-400, 400)),
            (Relation::Below, Align::Start, (0, 600)),
            (Relation::Below, Align::Center, (300, 600)),
            (Relation::Below, Align::End, (600, 600)),
            (Relation::Above, Align::Start, (0, -200)),
            (Relation::Above, Align::Center, (300, -200)),
            (Relation::Above, Align::End, (600, -200)),
        ];
        for (r, a, (x, y)) in cas {
            let s = [scr("R", 1000, 600, Placement::Origin), scr("S", 400, 200, rel(r, "R", a))];
            // Décalage final : aucune coordonnée négative.
            let (dx, dy) = (-x.min(0), -y.min(0));
            assert_eq!(at(&s), vec![Some((dx, dy)), Some((x + dx, y + dy))], "{r:?} {a:?}");
        }
    }

    #[test]
    fn centre_arrondi() {
        // (1000 − 401) / 2 = 299,5 → 300.
        let s = [scr("R", 1000, 600, Placement::Origin), scr("S", 401, 200, rel(Relation::Below, "R", Align::Center))];
        assert_eq!(at(&s)[1], Some((300, 600)));
    }

    #[test]
    fn echelle_et_rotation_changent_la_place_des_voisins() {
        let lenovo = logical_size(mode(1920, 1080, 60), Scale(1250), 0); // 1536×864
        let s = [scr("A", lenovo.0, lenovo.1, Placement::Origin), scr("B", 1920, 1080, rel(Relation::RightOf, "A", Align::Start))];
        assert_eq!(at(&s)[1], Some((1536, 0)));
        let tourne = logical_size(mode(1920, 1080, 60), Scale::ONE, 1); // 1080×1920
        let s = [scr("A", tourne.0, tourne.1, Placement::Origin), scr("B", 1920, 1080, rel(Relation::RightOf, "A", Align::End))];
        assert_eq!(at(&s)[1], Some((1080, 840)));
    }

    #[test]
    fn ecran_desactive_ignore() {
        let mut s = vec![
            scr("A", 1920, 1080, Placement::Origin),
            scr("B", 1920, 1080, Placement::Origin), // deuxième origine, mais désactivé
            scr("C", 1920, 1080, rel(Relation::RightOf, "A", Align::Start)),
        ];
        s[1].enabled = false;
        assert_eq!(at(&s), vec![Some((0, 0)), None, Some((1920, 0))]);
    }

    #[test]
    fn absolu_et_decalage_sans_negatif() {
        let s = [scr("A", 1920, 1080, Placement::Origin), scr("B", 2560, 1600, Placement::Absolute { x: -213, y: 1080 })];
        assert_eq!(at(&s), vec![Some((213, 0)), Some((0, 1080))]);
    }

    #[test]
    fn refus_avec_raison() {
        let e = |s: &[Screen]| resolve(s).unwrap_err().to_string();
        let cycle =
            [scr("A", 100, 100, rel(Relation::RightOf, "B", Align::Start)), scr("B", 100, 100, rel(Relation::RightOf, "A", Align::Start))];
        assert!(e(&cycle).starts_with("placement circulaire autour de"), "{}", e(&cycle));
        let soi = [scr("A", 100, 100, rel(Relation::RightOf, "A", Align::Start))];
        assert_eq!(e(&soi), "placement circulaire autour de A");
        let absente =
            [scr("A", 100, 100, Placement::Origin), scr("B", 100, 100, rel(Relation::Below, "Charlie Modèle 3 SERIE0003", Align::Start))];
        assert_eq!(e(&absente), "B est placé par rapport à Charlie, absent");
        let mut desactivee = vec![
            scr("A", 100, 100, Placement::Origin),
            scr("B", 100, 100, rel(Relation::Below, "C", Align::Start)),
            scr("C", 100, 100, Placement::Origin),
        ];
        desactivee[2].enabled = false;
        assert_eq!(e(&desactivee), "B est placé par rapport à C, désactivé");
        let deux = [scr("A", 100, 100, Placement::Origin), scr("B", 100, 100, Placement::Origin)];
        assert_eq!(e(&deux), "deux origines : A et B");
        let chevauche = [scr("A", 1920, 1080, Placement::Origin), scr("B", 1920, 1080, Placement::Absolute { x: 1000, y: 500 })];
        assert_eq!(e(&chevauche), "A et B se chevauchent");
        let mut aucun = vec![scr("A", 100, 100, Placement::Origin)];
        aucun[0].enabled = false;
        assert_eq!(e(&aucun), "aucun écran actif");
        // Bords qui se touchent : pas un chevauchement.
        let contact = [scr("A", 1920, 1080, Placement::Origin), scr("B", 1920, 1080, Placement::Absolute { x: 1920, y: 1080 })];
        assert!(resolve(&contact).is_ok());
    }

    fn placed(id: &str, x: i32, y: i32, w: i32, h: i32) -> Placed {
        Placed { id: id.into(), enabled: true, x, y, size: (w, h) }
    }

    /// Aller-retour : coordonnées → placements déduits → coordonnées.
    fn aller_retour(p: &[Placed]) -> Vec<Option<(i32, i32)>> {
        let placements = deduce(p, None, None);
        let screens: Vec<Screen> = p
            .iter()
            .zip(placements)
            .map(|(s, pl)| Screen { id: s.id.clone(), label: s.id.clone(), enabled: s.enabled, size: s.size, placement: pl })
            .collect();
        resolve(&screens).unwrap()
    }

    #[test]
    fn deduction_cote_a_cote_et_dessous() {
        let p = [placed("A", 0, 0, 1920, 1080), placed("B", 1920, 0, 1920, 1080), placed("C", 0, 1080, 2560, 1600)];
        assert_eq!(
            deduce(&p, None, None),
            vec![
                Placement::Origin,
                Placement::Relative { relation: Relation::RightOf, reference: "A".into(), align: Align::Start },
                Placement::Relative { relation: Relation::Below, reference: "A".into(), align: Align::Start },
            ]
        );
        assert_eq!(aller_retour(&p), vec![Some((0, 0)), Some((1920, 0)), Some((0, 1080))]);
    }

    #[test]
    fn deduction_de_l_agencement_du_poste() {
        // Relevé réel : Alpha (213, 0), Bravo (2133, 0), dalle interne (0, 1080).
        // Aucun écran en (0, 0) : l'origine est le plus haut, Alpha ; la dalle
        // touche Alpha mais aucun alignement ne redonne x = 0 : absolue.
        let p = [placed("Interne", 0, 1080, 2560, 1600), placed("Alpha", 213, 0, 1920, 1080), placed("Bravo", 2133, 0, 1920, 1080)];
        assert_eq!(
            deduce(&p, None, None),
            vec![
                Placement::Absolute { x: -213, y: 1080 },
                Placement::Origin,
                Placement::Relative { relation: Relation::RightOf, reference: "Alpha".into(), align: Align::Start },
            ]
        );
        assert_eq!(aller_retour(&p), vec![Some((0, 1080)), Some((213, 0)), Some((2133, 0))], "aller-retour identique");
    }

    #[test]
    fn deduction_ecran_isole_et_alignements() {
        let p = [
            placed("A", 0, 0, 1920, 1080),
            placed("B", 1920, 480, 800, 600),  // à droite, aligné en bas
            placed("C", 600, 1080, 720, 400),  // dessous, centré
            placed("D", 5000, 3000, 800, 600), // voisin de personne
        ];
        let d = deduce(&p, None, None);
        assert_eq!(d[1], Placement::Relative { relation: Relation::RightOf, reference: "A".into(), align: Align::End });
        assert_eq!(d[2], Placement::Relative { relation: Relation::Below, reference: "A".into(), align: Align::Center });
        assert_eq!(d[3], Placement::Absolute { x: 5000, y: 3000 });
        assert_eq!(aller_retour(&p), vec![Some((0, 0)), Some((1920, 480)), Some((600, 1080)), Some((5000, 3000))]);
    }

    #[test]
    fn deduction_en_chaine() {
        // C n'est voisin que de B, placé après lui dans l'ordre : point fixe.
        let p = [placed("C", 3840, 0, 1920, 1080), placed("A", 0, 0, 1920, 1080), placed("B", 1920, 0, 1920, 1080)];
        let d = deduce(&p, None, None);
        assert_eq!(d[0], Placement::Relative { relation: Relation::RightOf, reference: "B".into(), align: Align::Start });
        assert_eq!(aller_retour(&p), vec![Some((3840, 0)), Some((0, 0)), Some((1920, 0))]);
    }

    #[test]
    fn deduction_origine_imposee_et_ecran_evite() {
        let p = [placed("A", 0, 0, 1920, 1080), placed("B", 1920, 0, 1920, 1080)];
        let d = deduce(&p, Some(1), None);
        assert_eq!(
            d,
            vec![Placement::Relative { relation: Relation::LeftOf, reference: "B".into(), align: Align::Start }, Placement::Origin,]
        );
        // A évité : ni origine, ni référence ; A lui-même reste placé contre B.
        let d = deduce(&p, None, Some(0));
        assert_eq!(d[1], Placement::Origin);
        assert_eq!(d[0], Placement::Relative { relation: Relation::LeftOf, reference: "B".into(), align: Align::Start });
        // B évité : C, collé à B, ne s'appuie pas sur lui (position absolue).
        let p3 = [placed("A", 0, 0, 1920, 1080), placed("B", 1920, 0, 1920, 1080), placed("C", 3840, 0, 1920, 1080)];
        assert_eq!(deduce(&p3, None, Some(1))[2], Placement::Absolute { x: 3840, y: 0 });
    }

    #[test]
    fn deduction_ignore_les_desactives() {
        let mut p = vec![placed("A", 0, 0, 1920, 1080), placed("B", 0, 0, 1920, 1080)];
        p[0].enabled = false;
        assert_eq!(deduce(&p, None, None), vec![Placement::Origin, Placement::Origin]);
        assert_eq!(deduce(&[], None, None), Vec::<Placement>::new());
    }

    #[test]
    fn libelles() {
        assert_eq!(Relation::Below.label(), "en dessous de");
        assert_eq!(Align::End.label(Relation::RightOf), "en bas");
        assert_eq!(Align::End.label(Relation::Below), "à droite");
        assert_eq!(Align::Start.label(Relation::LeftOf), "en haut");
    }
}
