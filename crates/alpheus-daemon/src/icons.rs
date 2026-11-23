//! The glyph that stands for an application, from the name the compositor gives it.
//!
//! A built-in table maps a class (Hyprland), an app id (niri, Sway, Mango) or a process name to a
//! Nerd Font glyph, one cell wide like the rest of the bar; an application the table does not know
//! falls back to a letter. The window module, the launcher's rows, the tray's icons and the
//! notifications' glyphs all come through here, so one application looks the same everywhere. The
//! codes are checked against JetBrainsMonoNerdFontMono-Regular.ttf.
//!
//! Never:
//! - ask an application for a picture: the tray never requests an image icon, so no application
//!   sends the bar one (docs/features/tray.md);
//! - grow per application: the table is read-only data, and an unknown name costs a letter.
//!
//! See: docs/features/tray.md ("Icons"), docs/features/bar.md (the window module),
//! docs/install.md (the font the glyphs need).

/// (pattern looked up in the lowercased class, glyph), first match wins. A pattern starting with
/// `=` is a whole label: it matches a part of the class between `.`, `-`, `_` or a space, never a
/// piece of one ("=element" is element-desktop, never io.elementary.mail).
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
    ("=signal", '\u{f0369}'),      // md-message_text
    ("=element", '\u{f0628}'),     // md-matrix
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
    let matches = |m: &str| match m.strip_prefix('=') {
        Some(label) => c.split(['.', '-', '_', ' ']).any(|part| part == label),
        None => c.contains(m),
    };
    TABLE.iter().find(|(m, _)| matches(m)).map(|(_, g)| *g)
}

pub fn glyph(class: &str) -> char {
    find(class).unwrap_or(GENERIC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::text_width;

    #[test]
    fn a_class_or_app_id_finds_its_glyph() {
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
        for (app, g) in [("org.signal.Signal", '\u{f0369}'), ("element-desktop", '\u{f0628}'), ("signal-desktop-beta", '\u{f0369}')] {
            assert_eq!(find(app), Some(g), "{app}");
        }
    }

    /// "signal" and "element" are whole names, never a part of one: elementary OS's apps and
    /// any class merely containing the word keep their own glyph or none.
    #[test]
    fn signal_and_element_match_a_whole_label_only() {
        for app in ["io.elementary.mail", "io.elementary.calendar", "elemental", "org.example.SignalWatcher", "nosignal"] {
            assert_eq!(find(app), None, "{app}");
        }
        assert_eq!(glyph("io.elementary.code"), '\u{f0a1e}', "its own table entry still");
    }

    #[test]
    fn each_glyph_fits_in_one_cell() {
        for (_, g) in TABLE {
            assert_eq!(text_width(&g.to_string()), 1, "{g:?}");
        }
        assert_eq!(text_width(&GENERIC.to_string()), 1);
    }
}
