//! Couleurs : ~/.config/alpheus/colors.toml (modèle matugen), sinon palette
//! intégrée (Catppuccin Mocha, comme les autres secours du poste).

use ratatui::style::Color;
use serde::Deserialize;
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

impl Default for Theme {
    fn default() -> Self {
        Theme {
            bg: Color::Rgb(0x1e, 0x1e, 0x2e),
            fg: Color::Rgb(0xcd, 0xd6, 0xf4),
            accent: Color::Rgb(0xcb, 0xa6, 0xf7),
            muted: Color::Rgb(0x6c, 0x70, 0x86),
            surface: Color::Rgb(0x31, 0x32, 0x44),
            error: Color::Rgb(0xf3, 0x8b, 0xa8),
            ok: Color::Rgb(0xa6, 0xe3, 0xa1),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    bg: String,
    fg: String,
    accent: String,
    muted: String,
    surface: String,
    error: String,
    ok: String,
}

/// `#rrggbb` → couleur ; tout autre format → None (jamais de panique).
pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color::Rgb(c(0)?, c(2)?, c(4)?))
}

impl Raw {
    fn to_theme(&self) -> Option<Theme> {
        Some(Theme {
            bg: parse_hex(&self.bg)?,
            fg: parse_hex(&self.fg)?,
            accent: parse_hex(&self.accent)?,
            muted: parse_hex(&self.muted)?,
            surface: parse_hex(&self.surface)?,
            error: parse_hex(&self.error)?,
            ok: parse_hex(&self.ok)?,
        })
    }
}

impl Theme {
    pub fn load(path: &Path) -> (Theme, Option<String>) {
        let s = match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Theme::default(), None),
            Err(e) => return (Theme::default(), Some(format!("colors.toml illisible : {e}"))),
            Ok(s) => s,
        };
        match toml::from_str::<Raw>(&s) {
            Err(e) => (Theme::default(), Some(format!("colors.toml invalide : {}", e.to_string().lines().next().unwrap_or("erreur inconnue")))),
            Ok(raw) => match raw.to_theme() {
                Some(t) => (t, None),
                None => (Theme::default(), Some("colors.toml : couleur invalide (attendu #rrggbb)".into())),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(name: &str, content: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-theme-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("colors.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    const VALIDE: &str = "bg = \"#101010\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n";

    #[test]
    fn hex() {
        assert_eq!(parse_hex("#1e1e2e"), Some(Color::Rgb(30, 30, 46)));
        assert_eq!(parse_hex("1e1e2e"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#aééa"), None); // 6 octets non ASCII : pas de découpe au milieu de « é »
    }

    #[test]
    fn absent_secours_sans_message() {
        assert_eq!(Theme::load(Path::new("/nonexistent/colors.toml")), (Theme::default(), None));
    }

    #[test]
    fn fichier_valide() {
        let (t, w) = Theme::load(&file("valide", VALIDE));
        assert_eq!(w, None);
        assert_eq!(t.accent, Color::Rgb(0xff, 0x88, 0x00));
    }

    #[test]
    fn couleur_invalide_secours_et_message() {
        let (t, w) = Theme::load(&file("mauvais", &VALIDE.replace("#ff8800", "orange")));
        assert_eq!(t, Theme::default());
        assert!(w.unwrap().contains("colors.toml"));
    }

    #[test]
    fn cle_manquante_secours_et_message() {
        let (t, w) = Theme::load(&file("manque", "bg = \"#101010\"\n"));
        assert_eq!(t, Theme::default());
        assert!(w.is_some());
    }
}
