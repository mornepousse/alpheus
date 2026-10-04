//! Icon for an application from its class (Hyprland) or app_id (niri): a
//! Nerd Font glyph, one cell wide like text (Q5). The real icons from the
//! theme will arrive with the tray (delivery 4). Codes checked in
//! JetBrainsMonoNerdFontMono-Regular.ttf.

/// (pattern looked up in the lowercased class, glyph), first match wins.
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

/// Unknown applications: md-application.
pub const GENERIC: char = '\u{f08c6}';

pub fn glyph(class: &str) -> char {
    let c = class.to_lowercase();
    TABLE.iter().find(|(m, _)| c.contains(m)).map(|(_, g)| *g).unwrap_or(GENERIC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::text_width;

    #[test]
    fn glyphs_by_class() {
        assert_eq!(glyph("firefox"), '\u{f0239}');
        assert_eq!(glyph("com.mitchellh.ghostty"), '\u{f018d}');
        assert_eq!(glyph("Alacritty"), '\u{f018d}');
        assert_eq!(glyph("org.gnome.Nautilus"), '\u{f024b}');
        assert_eq!(glyph("VSCodium"), '\u{f0a1e}');
        assert_eq!(glyph(""), GENERIC);
        assert_eq!(glyph("unknown-app"), GENERIC);
    }

    #[test]
    fn each_glyph_fits_in_one_cell() {
        for (_, g) in TABLE {
            assert_eq!(text_width(&g.to_string()), 1, "{g:?}");
        }
        assert_eq!(text_width(&GENERIC.to_string()), 1);
    }
}
