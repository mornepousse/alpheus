//! Colors: ~/.config/alpheus/colors.toml (matugen template), otherwise a
//! built-in palette (Catppuccin Mocha, like the machine's other fallbacks).
//! Independent of any display: the TUI converts to ratatui colors
//! (alpheus::theme), the daemon to pixels.

use serde::Deserialize;
use std::path::Path;

/// 24-bit color.
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

/// `#rrggbb` → color; any other format → None (never a panic).
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
            Err(e) => return (Palette::default(), Some(format!("colors.toml unreadable: {e}"))),
            Ok(s) => s,
        };
        match toml::from_str::<Raw>(&s) {
            Err(e) => (Palette::default(), Some(format!("invalid colors.toml: {}", e.message()))),
            Ok(raw) => match raw.to_palette() {
                Some(p) => (p, None),
                None => (Palette::default(), Some("colors.toml: invalid color (expected #rrggbb)".into())),
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

    const VALID: &str = "bg = \"#101010\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n";

    #[test]
    fn hex() {
        assert_eq!(parse_hex("#1e1e2e"), Some(Rgb(30, 30, 46)));
        assert_eq!(parse_hex("1e1e2e"), None);
        assert_eq!(parse_hex("#zzzzzz"), None);
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#aééa"), None); // 6 non-ASCII bytes: no cut in the middle of « é »
    }

    #[test]
    fn absent_falls_back_with_no_message() {
        assert_eq!(Palette::load(Path::new("/nonexistent/colors.toml")), (Palette::default(), None));
    }

    #[test]
    fn valid_file() {
        let (p, w) = Palette::load(&file("valid", VALID));
        assert_eq!(w, None);
        assert_eq!(p.accent, Rgb(0xff, 0x88, 0x00));
    }

    #[test]
    fn invalid_color_falls_back_with_a_message() {
        let (p, w) = Palette::load(&file("bad", &VALID.replace("#ff8800", "orange")));
        assert_eq!(p, Palette::default());
        assert!(w.unwrap().contains("colors.toml"));
    }

    #[test]
    fn missing_key_falls_back_with_a_message() {
        let (p, w) = Palette::load(&file("missing", "bg = \"#101010\"\n"));
        assert_eq!(p, Palette::default());
        assert!(w.is_some());
    }
}
