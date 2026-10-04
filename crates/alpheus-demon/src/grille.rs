//! Grille de cellules : ce que la barre affiche, case par case (caractère,
//! teintes, gras), et les zones cliquables. C'est l'objet testé ; le dessin
//! en pixels (`peintre`) ne fait que la recopier.

use unicode_width::UnicodeWidthChar;

/// Teinte symbolique, résolue en couleur par la palette au dessin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Teinte {
    Fond,
    Texte,
    Accent,
    Estompe,
    Surface,
    Erreur,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Teinte,
    pub bg: Teinte,
    pub gras: bool,
}

impl Style {
    pub const NORMAL: Style = Style { fg: Teinte::Texte, bg: Teinte::Fond, gras: false };
    pub const ESTOMPE: Style = Style { fg: Teinte::Estompe, bg: Teinte::Fond, gras: false };
    /// Élément actif : accent inversé.
    pub const INVERSE: Style = Style { fg: Teinte::Fond, bg: Teinte::Accent, gras: false };
    pub const SURFACE: Style = Style { fg: Teinte::Texte, bg: Teinte::Surface, gras: false };
    pub const ERREUR: Style = Style { fg: Teinte::Erreur, bg: Teinte::Fond, gras: false };
    pub const ACCENT_GRAS: Style = Style { fg: Teinte::Accent, bg: Teinte::Fond, gras: true };

    /// Code d'une lettre pour les instantanés.
    pub fn code(self) -> char {
        match self {
            Style::NORMAL => '.',
            Style::ESTOMPE => 'm',
            Style::INVERSE => 'A',
            Style::SURFACE => 's',
            Style::ERREUR => 'e',
            Style::ACCENT_GRAS => 'G',
            _ => '?',
        }
    }
}

/// Deuxième case d'un caractère large : rien à dessiner.
pub const SUITE: char = '\0';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cellule {
    pub ch: char,
    pub style: Style,
}

/// Ce que vise un clic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cible {
    Bureau { reference: String, sortie: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    pub debut: u16,
    /// Exclue.
    pub fin: u16,
    pub cible: Cible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grille {
    cellules: Vec<Cellule>,
    zones: Vec<Zone>,
}

/// Largeur affichée : un caractère de contrôle devient une espace (1),
/// un combinant ne prend pas de case (0), CJK et emoji en prennent 2.
fn largeur_car(c: char) -> u16 {
    if c.is_control() { 1 } else { c.width().unwrap_or(0) as u16 }
}

pub fn largeur_texte(s: &str) -> u16 {
    s.chars().map(largeur_car).fold(0u16, u16::saturating_add)
}

/// `s` coupé à `max` cases, terminé par « … » s'il a été coupé.
pub fn tronquer(s: &str, max: u16) -> String {
    if largeur_texte(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut l = 0u16;
    for c in s.chars() {
        let w = largeur_car(c);
        if l + w > max - 1 {
            break;
        }
        out.push(c);
        l += w;
    }
    out.push('…');
    out
}

impl Grille {
    pub fn vide(colonnes: u16) -> Grille {
        Grille { cellules: vec![Cellule { ch: ' ', style: Style::NORMAL }; colonnes as usize], zones: Vec::new() }
    }

    pub fn largeur(&self) -> u16 {
        self.cellules.len() as u16
    }

    pub fn cellules(&self) -> &[Cellule] {
        &self.cellules
    }

    pub fn zones(&self) -> &[Zone] {
        &self.zones
    }

    /// Écrit `texte` en `col`, sans dépasser `fin` (exclue) ni la grille ;
    /// rend la colonne suivante. Un caractère large qui ne tient plus n'est
    /// pas coupé en deux.
    pub fn ecrire(&mut self, col: u16, fin: u16, texte: &str, style: Style) -> u16 {
        let fin = fin.min(self.largeur());
        let mut x = col;
        for c in texte.chars() {
            let c = if c.is_control() { ' ' } else { c };
            let w = largeur_car(c);
            if w == 0 {
                continue;
            }
            if x.saturating_add(w) > fin {
                break;
            }
            self.cellules[x as usize] = Cellule { ch: c, style };
            if w == 2 {
                self.cellules[x as usize + 1] = Cellule { ch: SUITE, style };
            }
            x += w;
        }
        x
    }

    pub fn zone(&mut self, debut: u16, fin: u16, cible: Cible) {
        if debut < fin {
            self.zones.push(Zone { debut, fin, cible });
        }
    }

    pub fn cible_en(&self, col: u16) -> Option<&Cible> {
        self.zones.iter().find(|z| (z.debut..z.fin).contains(&col)).map(|z| &z.cible)
    }

    /// Texte de la grille (instantanés), suites des caractères larges omises.
    pub fn texte(&self) -> String {
        self.cellules.iter().filter(|c| c.ch != SUITE).map(|c| c.ch).collect()
    }

    /// Un code de style par case (voir `Style::code`).
    pub fn styles(&self) -> String {
        self.cellules.iter().map(|c| c.style.code()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecrire_et_styles() {
        let mut g = Grille::vide(8);
        let x = g.ecrire(1, 8, "ab", Style::INVERSE);
        assert_eq!(x, 3);
        g.ecrire(x, 8, "cd", Style::ESTOMPE);
        assert_eq!(g.texte(), " abcd   ");
        assert_eq!(g.styles(), ".AAmm...");
    }

    #[test]
    fn jamais_hors_de_la_grille() {
        let mut g = Grille::vide(5);
        assert_eq!(g.ecrire(3, 99, "abcdef", Style::NORMAL), 5);
        assert_eq!(g.texte(), "   ab");
        assert_eq!(g.ecrire(9, 99, "x", Style::NORMAL), 9, "départ hors grille : rien écrit");
        let mut vide = Grille::vide(0);
        vide.ecrire(0, 10, "abc", Style::NORMAL);
        assert_eq!(vide.texte(), "");
    }

    #[test]
    fn caracteres_larges_et_controles() {
        let mut g = Grille::vide(6);
        assert_eq!(g.ecrire(0, 6, "a日本語", Style::NORMAL), 5, "le troisième caractère large ne tient pas");
        assert_eq!(g.texte(), "a日本 ");
        assert_eq!(g.cellules()[2].ch, SUITE);
        let mut g = Grille::vide(8);
        g.ecrire(0, 8, "a\tb\nc\u{1b}[e\u{301}", Style::NORMAL);
        assert_eq!(g.texte(), "a b c [e", "contrôles en espaces, combinant ignoré");
        assert_eq!(largeur_texte("🦀日a"), 5);
    }

    #[test]
    fn troncature_par_points_de_suspension() {
        assert_eq!(tronquer("Bonjour", 5), "Bonj…");
        assert_eq!(tronquer("Bonjour", 7), "Bonjour");
        assert_eq!(tronquer("日本語", 4), "日…");
        assert_eq!(tronquer("abc", 0), "");
        assert_eq!(tronquer("abc", 1), "…");
        assert!(largeur_texte(&tronquer("🦀🦀🦀🦀", 5)) <= 5);
    }

    #[test]
    fn zones_cliquables() {
        let mut g = Grille::vide(10);
        let c = Cible::Bureau { reference: "4".into(), sortie: "HDMI-A-1".into() };
        g.zone(2, 5, c.clone());
        g.zone(6, 6, c.clone()); // vide : ignorée
        assert_eq!(g.cible_en(1), None);
        assert_eq!(g.cible_en(2), Some(&c));
        assert_eq!(g.cible_en(4), Some(&c));
        assert_eq!(g.cible_en(5), None);
        assert_eq!(g.zones().len(), 1);
    }
}
