//! TUI theme: the common palette (alpheus-commun) turned into ratatui
//! colors. The rest of the TUI only ever sees `Theme`, same as before the
//! workspace split.

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

fn color(Rgb(r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

impl From<Palette> for Theme {
    fn from(p: Palette) -> Self {
        Theme {
            bg: color(p.bg),
            fg: color(p.fg),
            accent: color(p.accent),
            muted: color(p.muted),
            surface: color(p.surface),
            error: color(p.error),
            ok: color(p.ok),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Palette::default().into()
    }
}

impl Theme {
    /// colors.toml read by the core, converted; same messages as before.
    pub fn load(path: &Path) -> (Theme, Option<String>) {
        let (p, w) = Palette::load(path);
        (p.into(), w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_palette_as_ratatui_colors() {
        let t = Theme::default();
        assert_eq!(t.bg, Color::Rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(t.accent, Color::Rgb(0xcb, 0xa6, 0xf7));
        assert_eq!(t.ok, Color::Rgb(0xa6, 0xe3, 0xa1));
        let p = Palette { accent: Rgb(1, 2, 3), ..Palette::default() };
        assert_eq!(Theme::from(p).accent, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn missing_file_falls_back_silently() {
        assert_eq!(Theme::load(Path::new("/nonexistent/colors.toml")), (Theme::default(), None));
    }
}
