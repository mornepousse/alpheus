//! Couleurs : ~/.config/alpheus/colors.toml (modèle matugen), sinon palette
//! intégrée (Catppuccin Mocha, comme les autres secours du poste).
//! Indépendant de tout affichage : la TUI convertit en couleurs ratatui
//! (alpheus::theme), le démon en pixels.

use serde::Deserialize;
use std::path::Path;

/// Couleur 24 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb,
    pub fg: Rgb,
    pub accent: Rgb,
    pub muted: Rgb,
    pub surface: Rgb,
    pub error: Rgb,
    pub ok: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            bg: Rgb(0x1e, 0x1e, 0x2e),
            fg: Rgb(0xcd, 0xd6, 0xf4),
            accent: Rgb(0xcb, 0xa6, 0xf7),
            muted: Rgb(0x6c, 0x70, 0x86),
            surface: Rgb(0x31, 0x32, 0x44),
            error: Rgb(0xf3, 0x8b, 0xa8),
            ok: Rgb(0xa6, 0xe3, 0xa1),
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
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Rgb(c(0)?, c(2)?, c(4)?))
}

impl Raw {
    fn to_palette(&self) -> Option<Palette> {
        Some(Palette {
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

impl Palette {
    pub fn load(path: &Path) -> (Palette, Option<String>) {
        let s = match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Palette::default(), None),
            Err(e) => return (Palette::default(), Some(format!("colors.toml illisible : {e}"))),
            Ok(s) => s,
        };
        match toml::from_str::<Raw>(&s) {
            Err(e) => (Palette::default(), Some(format!("colors.toml invalide : {}", e.message()))),
            Ok(raw) => match raw.to_palette() {
                Some(p) => (p, None),
                None => (Palette::default(), Some("colors.toml : couleur invalide (attendu #rrggbb)".into())),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(name: &str, content: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-palette-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("colors.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    const VALIDE: &str = "bg = \"#101010\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n";

    #[test]
    fn hex() {
        assert_eq!(parse_hex("#1e1e2e"), Some(Rgb(30, 30, 46)));
        assert_eq!(parse_hex("1e1e2e"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#aééa"), None); // 6 octets non ASCII : pas de découpe au milieu de « é »
    }

    #[test]
    fn absent_secours_sans_message() {
        assert_eq!(Palette::load(Path::new("/nonexistent/colors.toml")), (Palette::default(), None));
    }

    #[test]
    fn fichier_valide() {
        let (p, w) = Palette::load(&file("valide", VALIDE));
        assert_eq!(w, None);
        assert_eq!(p.accent, Rgb(0xff, 0x88, 0x00));
    }

    #[test]
    fn couleur_invalide_secours_et_message() {
        let (p, w) = Palette::load(&file("mauvais", &VALIDE.replace("#ff8800", "orange")));
        assert_eq!(p, Palette::default());
        assert!(w.unwrap().contains("colors.toml"));
    }

    #[test]
    fn cle_manquante_secours_et_message() {
        let (p, w) = Palette::load(&file("manque", "bg = \"#101010\"\n"));
        assert_eq!(p, Palette::default());
        assert!(w.is_some());
    }
}
