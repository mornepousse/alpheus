//! De l'état à la grille d'un écran. Chaque module produit un segment ; les
//! segments sont posés de gauche à droite dans l'ordre de barre.toml,
//! séparés par « │ ». Le module fenêtre est élastique : il prend l'espace
//! libre (sans lui, l'espace libre précède le dernier module). Écran
//! étroit : les modules disparaissent par priorité (ressources, lecture,
//! puis titre) ; les bureaux et l'heure restent toujours.

use crate::compositeur::{Bureau, EtatCompositeur};
use crate::config::ModuleId;
use crate::etat::{Barre, Donnee};
use crate::grille::{Cible, Grille, Style, largeur_texte, tronquer};
use crate::{heure, icones};

/// Ordre de disparition quand la place manque.
const RETRAIT: [ModuleId; 3] = [ModuleId::Ressources, ModuleId::Lecture, ModuleId::Fenetre];
/// Largeur minimale du titre avant qu'il ne disparaisse.
const TITRE_MIN: u16 = 8;
/// Au-delà, les fenêtres d'un bureau sont résumées par « + » (Q4).
const POINTS_MAX: u16 = 4;
/// Un nom de bureau plus long est tronqué.
const NOM_MAX: u16 = 12;
const SEPARATEUR: &str = " │ ";
/// Marge d'une case à chaque bout.
const MARGE: u16 = 1;

struct Morceau {
    texte: String,
    style: Style,
    cible: Option<Cible>,
}

struct Segment {
    module: ModuleId,
    morceaux: Vec<Morceau>,
    elastique: bool,
}

impl Segment {
    fn largeur(&self) -> u16 {
        self.morceaux.iter().map(|m| largeur_texte(&m.texte)).fold(0, u16::saturating_add)
    }

    fn largeur_min(&self) -> u16 {
        if self.elastique { TITRE_MIN } else { self.largeur() }
    }
}

fn morceau(texte: impl Into<String>, style: Style) -> Morceau {
    Morceau { texte: texte.into(), style, cible: None }
}

fn etiquette(b: &Bureau) -> String {
    let n = b.fenetres;
    let urgent = if b.urgent { "!" } else { "" };
    let points = "·".repeat(n.min(POINTS_MAX) as usize);
    let plus = if n > POINTS_MAX { "+" } else { "" };
    format!(" {}{urgent}{points}{plus} ", tronquer(&b.nom, NOM_MAX))
}

fn style_bureau(b: &Bureau) -> Style {
    if b.urgent && !b.focalise {
        Style::ERREUR
    } else if b.focalise {
        Style::INVERSE
    } else if b.visible {
        Style::SURFACE
    } else if b.fenetres == 0 {
        Style::ESTOMPE
    } else {
        Style::NORMAL
    }
}

/// `None` si le compositeur ne connaît aucun bureau : pas de segment vide
/// (pas de « │ │ »), plutôt qu'un segment sans morceau.
fn bureaux(etat: &Donnee<EtatCompositeur>, sortie: &str) -> Option<Segment> {
    let morceaux = match etat {
        Donnee::Attente => vec![morceau(" … ", Style::ESTOMPE)],
        Donnee::Panne => vec![morceau(" – ", Style::ESTOMPE)],
        Donnee::Pret(e) => {
            let m: Vec<Morceau> = e
                .bureaux_de(sortie)
                .into_iter()
                .map(|b| Morceau {
                    texte: etiquette(b),
                    style: style_bureau(b),
                    cible: Some(Cible::Bureau { reference: b.reference.clone(), sortie: b.sortie.clone() }),
                })
                .collect();
            if m.is_empty() {
                return None;
            }
            m
        }
    };
    Some(Segment { module: ModuleId::Bureaux, morceaux, elastique: false })
}

/// `None` sans fenêtre active : pas de segment vide (pas de
/// « │ <vide> │ »), plutôt qu'un morceau de texte vide.
fn fenetre(etat: &Donnee<EtatCompositeur>) -> Option<Segment> {
    let texte = match etat {
        Donnee::Attente => "…".to_string(),
        Donnee::Panne => "–".to_string(),
        Donnee::Pret(EtatCompositeur { fenetre: Some(f), .. }) => format!("{} {}", icones::glyphe(&f.classe), f.titre),
        Donnee::Pret(_) => return None,
    };
    Some(Segment { module: ModuleId::Fenetre, morceaux: vec![morceau(texte, Style::NORMAL)], elastique: true })
}

fn horloge(barre: &Barre) -> Segment {
    let morceaux = match &barre.heure {
        None => vec![morceau("…", Style::ESTOMPE)],
        Some(h) => {
            let c = &barre.config.heure;
            let mut v = Vec::new();
            if !c.date.is_empty() {
                v.push(morceau(format!("{} ", heure::formater(&c.date, h)), Style::ESTOMPE));
            }
            v.push(morceau(heure::formater(&c.format, h), Style::ACCENT_GRAS));
            v
        }
    };
    Segment { module: ModuleId::Heure, morceaux, elastique: false }
}

/// Segment d'un module, ou None s'il n'a rien à montrer.
fn segment(barre: &Barre, m: ModuleId, sortie: &str) -> Option<Segment> {
    match m {
        ModuleId::Bureaux => bureaux(&barre.compositeur, sortie),
        ModuleId::Fenetre => fenetre(&barre.compositeur),
        ModuleId::Heure => Some(horloge(barre)),
        // Livraisons 3 à 5 : pas encore affichés.
        ModuleId::Lecture
        | ModuleId::Volume
        | ModuleId::Reseau
        | ModuleId::Bluetooth
        | ModuleId::Batterie
        | ModuleId::Ressources
        | ModuleId::Plateau
        | ModuleId::Notifications => None,
    }
}

fn separateurs(n: usize) -> u16 {
    largeur_texte(SEPARATEUR).saturating_mul(n.saturating_sub(1) as u16)
}

/// Largeur minimale : marges, séparateurs, segments (titre à TITRE_MIN).
fn besoin(segs: &[Segment]) -> u16 {
    segs.iter().map(Segment::largeur_min).fold(2 * MARGE + separateurs(segs.len()), u16::saturating_add)
}

impl Barre {
    pub fn grille(&self, sortie: &str, colonnes: u16) -> Grille {
        let mut segs: Vec<Segment> = self.config.modules.iter().filter_map(|m| segment(self, *m, sortie)).collect();
        for retire in RETRAIT {
            if besoin(&segs) <= colonnes {
                break;
            }
            segs.retain(|s| s.module != retire);
        }
        poser(&segs, colonnes)
    }
}

fn poser(segs: &[Segment], colonnes: u16) -> Grille {
    let mut g = Grille::vide(colonnes);
    let fin = colonnes.saturating_sub(MARGE);
    let fixe = segs.iter().filter(|s| !s.elastique).map(Segment::largeur).fold(2 * MARGE + separateurs(segs.len()), u16::saturating_add);
    let libre = colonnes.saturating_sub(fixe);
    // Sans segment élastique, l'espace libre précède le dernier segment.
    let espace_avant = if segs.iter().any(|s| s.elastique) || segs.len() < 2 { None } else { Some(segs.len() - 1) };
    let mut x = MARGE;
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            if espace_avant == Some(i) {
                x = x.saturating_add(libre);
            }
            x = g.ecrire(x, fin, SEPARATEUR, Style::ESTOMPE);
        }
        let limite = if s.elastique { x.saturating_add(libre).min(fin) } else { fin };
        for m in &s.morceaux {
            let texte = if s.elastique { tronquer(&m.texte, limite.saturating_sub(x)) } else { m.texte.clone() };
            let debut = x;
            x = g.ecrire(x, limite, &texte, m.style);
            if let Some(c) = &m.cible {
                g.zone(debut, x, c.clone());
            }
        }
        if s.elastique {
            x = limite;
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositeur::{Fenetre, exemple};
    use crate::config::BarreConfig;
    use crate::etat::Msg;
    use crate::heure::HeureLocale;

    const DIM: HeureLocale = HeureLocale { annee: 2026, mois: 10, jour: 4, jour_semaine: 0, heure: 14, minute: 5, seconde: 9 };

    fn barre() -> Barre {
        let mut b = Barre::new(BarreConfig::default());
        b.appliquer(Msg::Compositeur(Some(exemple())));
        b.appliquer(Msg::Heure(DIM));
        b
    }

    fn instantane(g: &Grille) -> String {
        format!("{}\n{}", g.texte(), g.styles())
    }

    #[test]
    fn barre_large_de_l_ecran_hdmi() {
        let g = barre().grille("HDMI-A-1", 100);
        let (t, s) = (g.texte(), g.styles());
        assert_eq!(g.largeur(), 100);
        assert!(t.starts_with("  2·  4!····+  5  │ \u{f0239} Page web — Mozilla Firefox "), "{t}");
        assert!(t.ends_with(" │ dim. 4 oct. 14:05 "), "{t}");
        assert!(s.starts_with(".AAAAeeeeeeeeemmmmmm"), "{s}");
        assert!(s.ends_with("mmmmmmmmmmmmmmmGGGGG."), "{s}");
        insta::assert_snapshot!("hdmi_100", instantane(&g));
    }

    #[test]
    fn titre_tronque_puis_retire_quand_la_place_manque() {
        let g = barre().grille("HDMI-A-1", 60);
        assert!(g.texte().contains("Mozil…"), "{}", g.texte());
        insta::assert_snapshot!("hdmi_60", instantane(&g));
        let g = barre().grille("HDMI-A-1", 40);
        let t = g.texte();
        assert!(!t.contains("Page") && t.contains("14:05") && t.contains(" 4!····+ "), "{t}");
        assert_eq!(g.largeur(), 40);
        insta::assert_snapshot!("hdmi_40", instantane(&g));
    }

    #[test]
    fn tailles_extremes_sans_panique() {
        for w in [0u16, 1, 5, 12, 39] {
            assert_eq!(barre().grille("HDMI-A-1", w).largeur(), w);
        }
        // À 39 colonnes le titre a disparu (voir
        // titre_tronque_puis_retire_quand_la_place_manque), mais bureaux et
        // heure, jamais retirés, restent tous les deux affichés.
        let t = barre().grille("HDMI-A-1", 39).texte();
        assert!(t.contains(" 4!····+ ") && t.contains("14:05"), "{t}");
    }

    #[test]
    fn bureaux_de_chaque_ecran() {
        let b = barre();
        let g = b.grille("eDP-1", 60);
        assert!(g.texte().starts_with("  1  │ "), "{}", g.texte());
        assert!(g.styles().starts_with(".sss"), "visible sans le focus : surface");
        let g = b.grille("DVI-I-1", 60);
        assert!(g.texte().starts_with("  3··  web·  │ "), "{}", g.texte());
        let t = b.grille("HEADLESS-1", 120).texte();
        assert!(t.contains(" 1 ") && t.contains(" web· "), "écran inconnu : tous les bureaux ({t})");
    }

    #[test]
    fn zones_des_bureaux() {
        let g = barre().grille("HDMI-A-1", 100);
        let deux = Cible::Bureau { reference: "2".into(), sortie: "HDMI-A-1".into() };
        let quatre = Cible::Bureau { reference: "4".into(), sortie: "HDMI-A-1".into() };
        let cinq = Cible::Bureau { reference: "5".into(), sortie: "HDMI-A-1".into() };
        // Bords de chaque zone (« 2· » en 1-4, « 4!····+ » en 5-13, « 5 » en 14-16) :
        // une borne décalée d'une case laisserait passer ou couperait le clic.
        assert_eq!(g.cible_en(1), Some(&deux), "premier bord du bureau 2");
        assert_eq!(g.cible_en(4), Some(&deux), "dernier bord du bureau 2");
        assert_eq!(g.cible_en(5), Some(&quatre), "premier bord du bureau 4");
        assert_eq!(g.cible_en(6), Some(&quatre));
        assert_eq!(g.cible_en(13), Some(&quatre), "dernier bord du bureau 4");
        assert_eq!(g.cible_en(16), Some(&cinq), "dernier bord du bureau 5");
        assert_eq!(g.cible_en(17), None, "sur le séparateur, aucune zone");
        assert_eq!(g.cible_en(0), None, "marge");
        assert_eq!(g.cible_en(30), None, "titre");
        let g = barre().grille("DVI-I-1", 60);
        let web = Cible::Bureau { reference: "name:web".into(), sortie: "DVI-I-1".into() };
        assert_eq!(g.cible_en(7), Some(&web));
        // Écran inconnu : chaque bureau garde SA sortie dans la commande.
        let g = barre().grille("HEADLESS-1", 120);
        assert_eq!(g.cible_en(2), Some(&Cible::Bureau { reference: "1".into(), sortie: "eDP-1".into() }));
    }

    #[test]
    fn ordre_de_barre_toml() {
        let mut b = barre();
        b.config.modules = vec![ModuleId::Heure, ModuleId::Bureaux];
        let t = b.grille("HDMI-A-1", 60).texte();
        assert!(t.starts_with(" dim. 4 oct. 14:05"), "{t}");
        assert!(t.ends_with(" │  2·  4!····+  5  "), "espace libre avant le dernier module : {t}");
        b.config.heure.date = String::new();
        assert!(b.grille("HDMI-A-1", 60).texte().starts_with(" 14:05"));
    }

    #[test]
    fn attente_et_panne() {
        let b = Barre::new(BarreConfig::default());
        assert!(b.grille("eDP-1", 40).texte().starts_with("  …  │ …"), "rien reçu : « … »");
        let mut b = barre();
        b.appliquer(Msg::Compositeur(None));
        let t = b.grille("eDP-1", 40).texte();
        assert!(t.starts_with("  –  │ –"), "source en panne : « – » ({t})");
        assert!(t.contains("14:05"), "le reste de la barre continue");
    }

    #[test]
    fn titre_hostile() {
        let mut e = exemple();
        e.fenetre = Some(Fenetre { classe: "Alacritty".into(), titre: format!("a\tb\n日本語 — 🦀 \u{1b}[31m{}", "x".repeat(300)) });
        let mut b = barre();
        b.appliquer(Msg::Compositeur(Some(e)));
        for w in [30u16, 47, 48, 49, 200] {
            let g = b.grille("HDMI-A-1", w);
            assert_eq!(g.largeur(), w);
            let t = g.texte();
            assert!(!t.contains(['\t', '\n', '\u{1b}']), "{t:?}");
        }
    }

    #[test]
    fn modules_pas_encore_livres_invisibles() {
        let mut b = barre();
        b.config.modules = vec![ModuleId::Bureaux, ModuleId::Volume, ModuleId::Plateau, ModuleId::Heure];
        let t = b.grille("HDMI-A-1", 60).texte();
        assert_eq!(t.matches('│').count(), 1, "aucun séparateur pour un module absent : {t}");
    }

    #[test]
    fn aucun_segment_vide_sans_fenetre_ni_bureau_connu() {
        // Sans fenêtre active : le module fenêtre ne doit laisser aucun
        // segment vide (donc pas de « │ <vide> │ »).
        let mut e = exemple();
        e.fenetre = None;
        let mut b = barre();
        b.appliquer(Msg::Compositeur(Some(e)));
        let t = b.grille("HDMI-A-1", 60).texte();
        assert_eq!(t.matches('│').count(), 1, "bureaux et heure seulement : {t}");

        // Sans aucun bureau connu du compositeur : même chose pour le
        // module bureaux.
        let e2 = EtatCompositeur { bureaux: Vec::new(), fenetre: Some(Fenetre { classe: "firefox".into(), titre: "Page".into() }) };
        let mut b2 = barre();
        b2.appliquer(Msg::Compositeur(Some(e2)));
        let t2 = b2.grille("HDMI-A-1", 60).texte();
        assert_eq!(t2.matches('│').count(), 1, "fenêtre et heure seulement : {t2}");

        // En attente ou en panne, « … » / « – » restent affichés pour les
        // deux modules (couvert par `attente_et_panne`), jamais supprimés.
    }
}
