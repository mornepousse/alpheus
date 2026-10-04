//! Icône d'une application d'après sa classe (Hyprland) ou son app_id
//! (niri) : un glyphe Nerd Font, d'une case comme le texte (Q5). Les vraies
//! icônes du thème arriveront avec le plateau (livraison 4). Codes vérifiés
//! dans JetBrainsMonoNerdFontMono-Regular.ttf.

/// (motif cherché dans la classe en minuscules, glyphe), premier trouvé.
const TABLE: [(&str, char); 32] = [
    ("firefox", '\u{f0239}'),   // md-firefox
    ("chrom", '\u{f02af}'),     // md-google_chrome
    ("alacritty", '\u{f018d}'), // md-console
    ("kitty", '\u{f018d}'),
    ("foot", '\u{f018d}'),
    ("ghostty", '\u{f018d}'),
    ("wezterm", '\u{f018d}'),
    ("terminal", '\u{f018d}'),
    ("alpheus", '\u{f018d}'),
    ("thunar", '\u{f024b}'), // md-folder
    ("nautilus", '\u{f024b}'),
    ("nemo", '\u{f024b}'),
    ("codium", '\u{f0a1e}'), // md-microsoft_visual_studio_code
    ("code", '\u{f0a1e}'),
    ("discord", '\u{f066f}'), // md-discord
    ("vesktop", '\u{f066f}'),
    ("steam", '\u{f04d3}'),   // md-steam
    ("spotify", '\u{f04c7}'), // md-spotify
    ("cider", '\u{f075a}'),   // md-music
    ("mpv", '\u{f0381}'),     // md-movie
    ("vlc", '\u{f057c}'),     // md-vlc
    ("imv", '\u{f02e9}'),     // md-image
    ("satty", '\u{f02e9}'),
    ("thunderbird", '\u{f01ee}'), // md-email
    ("gimp", '\u{f03d8}'),        // md-palette
    ("inkscape", '\u{f03d8}'),
    ("affinity", '\u{f03d8}'),
    ("kicad", '\u{f061a}'),       // md-chip
    ("freecad", '\u{f01a7}'),     // md-cube_outline
    ("libreoffice", '\u{f0219}'), // md-file_document
    ("obsidian", '\u{f082e}'),    // md-notebook
    ("github", '\u{f02a4}'),      // md-github
];

/// Applications inconnues : md-application.
pub const GENERIQUE: char = '\u{f08c6}';

pub fn glyphe(classe: &str) -> char {
    let c = classe.to_lowercase();
    TABLE.iter().find(|(m, _)| c.contains(m)).map(|(_, g)| *g).unwrap_or(GENERIQUE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grille::largeur_texte;

    #[test]
    fn glyphes_par_classe() {
        assert_eq!(glyphe("firefox"), '\u{f0239}');
        assert_eq!(glyphe("com.mitchellh.ghostty"), '\u{f018d}');
        assert_eq!(glyphe("Alacritty"), '\u{f018d}');
        assert_eq!(glyphe("org.gnome.Nautilus"), '\u{f024b}');
        assert_eq!(glyphe("VSCodium"), '\u{f0a1e}');
        assert_eq!(glyphe(""), GENERIQUE);
        assert_eq!(glyphe("appli-inconnue"), GENERIQUE);
    }

    #[test]
    fn chaque_glyphe_tient_dans_une_case() {
        for (_, g) in TABLE {
            assert_eq!(largeur_texte(&g.to_string()), 1, "{g:?}");
        }
        assert_eq!(largeur_texte(&GENERIQUE.to_string()), 1);
    }
}
