//! Icon for an application from its class (Hyprland) or app_id (niri): a
//! Nerd Font glyph, one cell wide like text (Q5). The real icons from the
//! theme will arrive with the tray (delivery 4). Codes checked in
//! JetBrainsMonoNerdFontMono-Regular.ttf.

/// (pattern looked up in the lowercased class, glyph), first match wins.
const TABLE: [(&str, char); 40] = [
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
    // Notification apps (notifications spec § 2); checked in the 3.4.0 post table.
    ("signal", '\u{f0369}'),       // md-message_text
    ("element", '\u{f0628}'),      // md-matrix
    ("slack", '\u{f04b1}'),        // md-slack
    ("telegram", '\u{f2c6}'),      // fa-telegram (the tray's)
    ("equibop", '\u{f066f}'),      // md-discord
    ("evolution", '\u{f01ee}'),    // md-email
    ("kdeconnect", '\u{f011c}'),   // md-cellphone
    ("transmission", '\u{f01da}'), // md-download
];

/// Unknown applications: md-application.
pub const GENERIC: char = '\u{f08c6}';

/// The table's glyph for `class` (substring of the lowercased class, first match wins); None when
/// nothing matches, so a notification can fall back to its category, then a letter.
pub fn find(class: &str) -> Option<char> {
    let c = class.to_lowercase();
    TABLE.iter().find(|(m, _)| c.contains(m)).map(|(_, g)| *g)
}

pub fn glyph(class: &str) -> char {
    find(class).unwrap_or(GENERIC)
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
    fn find_tells_a_miss_from_the_generic_glyph() {
        assert_eq!(find("firefox"), Some('\u{f0239}'));
        assert_eq!(find("unknown-app"), None);
        assert_eq!(glyph("unknown-app"), GENERIC);
        for (app, g) in [
            ("signal-desktop", '\u{f0369}'),
            ("Element", '\u{f0628}'),
            ("slack", '\u{f04b1}'),
            ("org.telegram.desktop", '\u{f2c6}'),
            ("equibop", '\u{f066f}'),
            ("org.gnome.Evolution", '\u{f01ee}'),
            ("org.kde.kdeconnect.daemon", '\u{f011c}'),
            ("transmission-gtk", '\u{f01da}'),
        ] {
            assert_eq!(find(app), Some(g), "{app}");
        }
    }

    #[test]
    fn each_glyph_fits_in_one_cell() {
        for (_, g) in TABLE {
            assert_eq!(text_width(&g.to_string()), 1, "{g:?}");
        }
        assert_eq!(text_width(&GENERIC.to_string()), 1);
    }
}
