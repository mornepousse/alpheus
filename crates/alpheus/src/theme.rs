//! Thème de la TUI : la palette commune (alpheus-commun) en couleurs
//! ratatui. Le reste de la TUI ne voit que `Theme`, comme avant le
//! découpage en workspace.

use alpheus_commun::theme::{Palette, Rgb};
use ratatui::style::Color;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub muted: Color,
    pub surface: Color,
    pub error: Color,
    pub ok: Color,
}

fn couleur(Rgb(r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

impl From<Palette> for Theme {
    fn from(p: Palette) -> Self {
        Theme {
            bg: couleur(p.bg),
            fg: couleur(p.fg),
            accent: couleur(p.accent),
            muted: couleur(p.muted),
            surface: couleur(p.surface),
            error: couleur(p.error),
            ok: couleur(p.ok),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Palette::default().into()
    }
}

impl Theme {
    /// colors.toml lu par le socle, converti ; mêmes messages qu'avant.
    pub fn load(path: &Path) -> (Theme, Option<String>) {
        let (p, w) = Palette::load(path);
        (p.into(), w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_commune_en_couleurs_ratatui() {
        let t = Theme::default();
        assert_eq!(t.bg, Color::Rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(t.accent, Color::Rgb(0xcb, 0xa6, 0xf7));
        assert_eq!(t.ok, Color::Rgb(0xa6, 0xe3, 0xa1));
        let p = Palette { accent: Rgb(1, 2, 3), ..Palette::default() };
        assert_eq!(Theme::from(p).accent, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn fichier_absent_secours_sans_message() {
        assert_eq!(Theme::load(Path::new("/nonexistent/colors.toml")), (Theme::default(), None));
    }
}
