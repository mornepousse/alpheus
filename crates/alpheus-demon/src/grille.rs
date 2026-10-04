//! Grille de cellules : ce que la barre affiche, case par case (caractère,
//! teintes, gras), et les zones cliquables. C'est l'objet testé ; le dessin
//! en pixels (`peintre`) ne fait que la recopier.

use std::str::Chars;
use unicode_normalization::{IsNormalized, Recompositions, UnicodeNormalization, is_nfc_quick};
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

/// Contrôles C0/C1, et séparateurs Unicode de ligne/paragraphe (U+2028,
/// U+2029) : une largeur de 1 d'après `unicode-width`, mais rien à afficher
/// tels quels dans une cellule. Traités pareil partout (`largeur_car` et
/// `Grille::ecrire`) pour ne jamais se contredire sur leur largeur.
fn est_controle_affiche(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// Largeur affichée : un caractère de contrôle (ou assimilé, voir
/// `est_controle_affiche`) devient une espace (1), un combinant ne prend
/// pas de case (0), CJK et emoji en prennent 2.
fn largeur_car(c: char) -> u16 {
    if est_controle_affiche(c) { 1 } else { c.width().unwrap_or(0) as u16 }
}

/// Itère les caractères de `s` recomposés en NFC (accents décomposés, par
/// exemple venus de macOS, recomposés en précomposés) : `largeur_texte`,
/// `tronquer` et `Grille::ecrire` voient tous les trois ce même texte, pour
/// que la largeur annoncée reste ce qui est réellement écrit. Un combinant
/// sans forme précomposée (ex. `q\u{301}`) n'est pas recomposable : il
/// reste seul dans le flux, et donc toujours sauté plus loin (largeur 0),
/// comportement inchangé.
enum CarsNfc<'a> {
    /// `s` est déjà en NFC (cas de presque tous les titres) : aucune
    /// allocation, on relit directement ses caractères.
    Direct(Chars<'a>),
    /// `s` est décomposé : on passe par l'itérateur de recomposition
    /// (toujours sans `String` intermédiaire).
    Recompose(Recompositions<Chars<'a>>),
}

impl<'a> Iterator for CarsNfc<'a> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        match self {
            CarsNfc::Direct(it) => it.next(),
            CarsNfc::Recompose(it) => it.next(),
        }
    }
}

fn cars_nfc(s: &str) -> CarsNfc<'_> {
    if is_nfc_quick(s.chars()) == IsNormalized::Yes { CarsNfc::Direct(s.chars()) } else { CarsNfc::Recompose(s.nfc()) }
}

pub fn largeur_texte(s: &str) -> u16 {
    cars_nfc(s).map(largeur_car).fold(0u16, u16::saturating_add)
}

/// `s` coupé à `max` cases, terminé par « … » s'il a été coupé.
pub fn tronquer(s: &str, max: u16) -> String {
    if largeur_texte(s) <= max {
        // Déjà une allocation de toute façon (on rend une `String`) : on
        // recompose ici aussi, pour rester cohérent avec `ecrire`.
        return cars_nfc(s).collect();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut l = 0u16;
    for c in cars_nfc(s) {
        let w = largeur_car(c);
        if l.saturating_add(w) > max - 1 {
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
        for c in cars_nfc(texte) {
            let c = if est_controle_affiche(c) { ' ' } else { c };
            let w = largeur_car(c);
            if w == 0 {
                continue;
            }
            if x.saturating_add(w) > fin {
                break;
            }
            self.effacer_bord_large(x, w);
            self.cellules[x as usize] = Cellule { ch: c, style };
            if w == 2 {
                self.cellules[x as usize + 1] = Cellule { ch: SUITE, style };
            }
            x += w;
        }
        x
    }

    /// Avant d'écrire en `x` une case de largeur `w`, efface la moitié d'un
    /// caractère large déjà présent qui deviendrait orpheline : si `x` est
    /// la suite (`SUITE`) d'un large, son début en `x - 1` ; si la case
    /// juste après l'écriture (`x + w`) est une suite, son début vient
    /// d'être recouvert : cette suite.
    fn effacer_bord_large(&mut self, x: u16, w: u16) {
        let i = x as usize;
        let vide = Cellule { ch: ' ', style: Style::NORMAL };
        if i > 0 && self.cellules[i].ch == SUITE {
            self.cellules[i - 1] = vide;
        }
        let apres = i + w as usize;
        if apres < self.cellules.len() && self.cellules[apres].ch == SUITE {
            self.cellules[apres] = vide;
        }
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
        // Depuis la tâche 10b, le "e" décomposé + accent combinant est
        // recomposé en NFC ("é") avant écriture, comme le reste du texte.
        assert_eq!(g.texte(), "a b c [é", "contrôles en espaces, accent décomposé recomposé");
        assert_eq!(largeur_texte("🦀日a"), 5);
    }

    #[test]
    fn borne_fin_interieure_respectee() {
        // Mord sur `let fin = fin.min(self.largeur());` : remplacer par
        // `let fin = self.largeur();` (ignorer le paramètre) laisse ces
        // deux assertions rouges (voir rapport de correction).
        assert_eq!(Grille::vide(8).ecrire(0, 3, "abcdef", Style::NORMAL), 3);
        let mut g = Grille::vide(8);
        g.ecrire(0, 3, "abcdef", Style::NORMAL);
        assert_eq!(g.texte(), "abc     ");

        // Un caractère large qui ne tient pas dans une borne intérieure
        // (pas seulement dans la grille entière).
        assert_eq!(Grille::vide(4).ecrire(0, 2, "a日", Style::NORMAL), 1);
    }

    #[test]
    fn large_jamais_orphelin() {
        // Écraser la première moitié d'un large par un caractère étroit
        // doit blanchir sa suite, sinon elle reste affichée seule.
        let mut g = Grille::vide(4);
        g.ecrire(1, 4, "日", Style::NORMAL);
        g.ecrire(1, 4, "a", Style::NORMAL);
        assert_eq!(g.cellules()[2].ch, ' ', "la suite du 日 effacé ne doit pas rester");
        assert_eq!(g.texte(), " a  ");

        // Écrire directement sur la case `SUITE` doit blanchir le large à
        // gauche, sinon il déborde visuellement sur la nouvelle case.
        let mut g = Grille::vide(4);
        g.ecrire(1, 4, "日", Style::NORMAL);
        g.ecrire(2, 4, "b", Style::NORMAL);
        assert_eq!(g.cellules()[1].ch, ' ', "le 日 dont la suite est écrasée ne doit pas rester");
        assert_eq!(g.texte(), "  b ");

        // Un large écrit sur le début d'un autre large recouvre ce début
        // par sa propre suite : la suite de l'ancien, juste après, doit
        // être blanchie.
        let mut g = Grille::vide(4);
        g.ecrire(1, 4, "日", Style::NORMAL);
        g.ecrire(0, 4, "本", Style::NORMAL);
        assert_eq!(g.cellules()[2].ch, ' ', "la suite du 日 recouvert ne doit pas rester");
        assert_eq!(g.texte(), "本  ");
    }

    #[test]
    fn separateurs_unicode_comme_controles() {
        // U+2028 (séparateur de ligne) et U+2029 (de paragraphe) ont une
        // largeur de 1 d'après unicode-width sans être `is_control` :
        // traités comme les contrôles, en espace, pas affichés tels quels.
        assert_eq!(largeur_texte("a\u{2028}b\u{2029}"), 4);
        let mut g = Grille::vide(4);
        g.ecrire(0, 4, "a\u{2028}b\u{2029}", Style::NORMAL);
        assert_eq!(g.texte(), "a b ");
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
    fn accents_decomposes_recomposes() {
        // "e\u{301}lan" : un "e" décomposé (e + accent aigu combinant) doit
        // s'écrire recomposé en NFC, comme un titre venu de macOS.
        let mut g = Grille::vide(6);
        let x = g.ecrire(0, 6, "e\u{301}lan", Style::NORMAL);
        assert_eq!(g.texte(), "élan  ");
        assert_eq!(x, 4);
        assert_eq!(largeur_texte("e\u{301}lan"), 4);
        assert_eq!(tronquer("Ame\u{301}lie", 4), "Amé…");
        // Mord sur le retour rapide de `tronquer` (texte qui tient déjà) :
        // le rendre sans passer par `cars_nfc` laisserait "e\u{301}lan" tel
        // quel (décomposé), même si sa largeur ne change pas.
        assert_eq!(tronquer("e\u{301}lan", 10), "élan");
    }

    #[test]
    fn combinant_sans_precompose_toujours_saute() {
        // "q" n'a pas de forme précomposée avec l'accent aigu : le
        // combinant reste sauté, comportement inchangé.
        let mut g = Grille::vide(4);
        g.ecrire(0, 4, "q\u{301}x", Style::NORMAL);
        assert_eq!(g.texte(), "qx  ");
        assert_eq!(largeur_texte("q\u{301}x"), 2);
    }

    #[test]
    fn deja_nfc_inchange() {
        // Un titre déjà en NFC (accents et CJK) s'écrit comme avant.
        let mut g = Grille::vide(8);
        g.ecrire(0, 8, "é日本", Style::NORMAL);
        assert_eq!(g.texte(), "é日本   ");
        assert_eq!(largeur_texte("é日本"), 5);
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
