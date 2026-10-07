//! Colours (spec § 1): 16 named roles shared by the daemon and the TUI,
//! read from a colour file holding either the 16 roles or the legacy 7 keys
//! (derived per mode). Independent of any display: the TUI converts to
//! ratatui colours (alpheus::theme), the daemon to pixels.

pub mod contrast;
pub mod presets;

use std::path::Path;

/// 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Which half of a theme is in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

/// The 16 colour roles (spec § 1). The frame colour is not a role: it is
/// derived from `fg_dim` by `border_over`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Roles {
    pub bar_bg: Rgb,
    pub pill_bg: Rgb,
    pub popup_bg: Rgb,
    pub raised: Rgb,
    pub selection: Rgb,
    pub fg: Rgb,
    pub fg_dim: Rgb,
    pub on_accent: Rgb,
    pub accent: Rgb,
    pub accent_container: Rgb,
    pub accent2: Rgb,
    pub accent2_container: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub error: Rgb,
    pub urgent: Rgb,
}

/// Role names in field order: the keys of a 16-role colour file and of
/// `[colors]` in theme.toml.
pub const ROLE_NAMES: [&str; 16] = [
    "bar_bg",
    "pill_bg",
    "popup_bg",
    "raised",
    "selection",
    "fg",
    "fg_dim",
    "on_accent",
    "accent",
    "accent_container",
    "accent2",
    "accent2_container",
    "ok",
    "warn",
    "error",
    "urgent",
];

/// The keys of a colour file from before the 16 roles.
const LEGACY_KEYS: [&str; 7] = ["bg", "fg", "accent", "muted", "surface", "error", "ok"];

/// Per-channel distance between two surface levels derived from a legacy
/// file (Catppuccin's own Surface0 → Surface1 spacing).
const STEP: u8 = 20;
/// The fixed amber of `warn` derived from a legacy file.
const AMBER_DARK: Rgb = Rgb(0xff, 0xb0, 0x00);
const AMBER_LIGHT: Rgb = Rgb(0x8a, 0x53, 0x00);
/// `fg_dim`'s weight in the border colour: 40 % of 255.
const BORDER_ALPHA: u16 = 102;

impl Roles {
    /// fg_dim at 40 % over `bg`, rounded: frames and borders (spec § 1).
    pub fn border_over(&self, bg: Rgb) -> Rgb {
        let mix = |f: u8, b: u8| ((f as u16 * BORDER_ALPHA + b as u16 * (255 - BORDER_ALPHA) + 127) / 255) as u8;
        Rgb(mix(self.fg_dim.0, bg.0), mix(self.fg_dim.1, bg.1), mix(self.fg_dim.2, bg.2))
    }

    /// The field named `name` (one of `ROLE_NAMES`), None otherwise.
    pub(crate) fn slot(&mut self, name: &str) -> Option<&mut Rgb> {
        Some(match name {
            "bar_bg" => &mut self.bar_bg,
            "pill_bg" => &mut self.pill_bg,
            "popup_bg" => &mut self.popup_bg,
            "raised" => &mut self.raised,
            "selection" => &mut self.selection,
            "fg" => &mut self.fg,
            "fg_dim" => &mut self.fg_dim,
            "on_accent" => &mut self.on_accent,
            "accent" => &mut self.accent,
            "accent_container" => &mut self.accent_container,
            "accent2" => &mut self.accent2,
            "accent2_container" => &mut self.accent2_container,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "error" => &mut self.error,
            "urgent" => &mut self.urgent,
            _ => return None,
        })
    }
}

/// Catppuccin Mocha, the default theme's dark half.
impl Default for Roles {
    fn default() -> Self {
        presets::CATPPUCCIN_DARK
    }
}

/// `#rrggbb` → colour; any other format → None (never a panic).
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Rgb(c(0)?, c(2)?, c(4)?))
}

/// A colour file from before the 16 roles.
struct Legacy {
    bg: Rgb,
    fg: Rgb,
    accent: Rgb,
    muted: Rgb,
    surface: Rgb,
    error: Rgb,
    ok: Rgb,
}

/// `times` surface levels away from the background: lighter in dark mode,
/// darker in light mode, `STEP` per level and channel, saturating.
fn step(c: Rgb, mode: Mode, times: u8) -> Rgb {
    let d = STEP.saturating_mul(times);
    let f = |x: u8| match mode {
        Mode::Dark => x.saturating_add(d),
        Mode::Light => x.saturating_sub(d),
    };
    Rgb(f(c.0), f(c.1), f(c.2))
}

/// The 16 roles from a legacy file (spec § 1, backward compatibility).
fn derive_legacy(l: &Legacy, mode: Mode) -> Roles {
    Roles {
        bar_bg: l.bg,
        pill_bg: l.surface,
        popup_bg: l.surface,
        raised: step(l.surface, mode, 1),
        selection: step(l.surface, mode, 2),
        fg: l.fg,
        fg_dim: l.muted,
        on_accent: l.bg,
        accent: l.accent,
        accent_container: l.accent,
        accent2: l.ok,
        accent2_container: l.ok,
        ok: l.ok,
        warn: match mode {
            Mode::Dark => AMBER_DARK,
            Mode::Light => AMBER_LIGHT,
        },
        error: l.error,
        urgent: l.error,
    }
}

/// A colour file's text: the 16 role keys, or the legacy 7 (`bg fg accent
/// muted surface error ok`), derived for `mode` (spec § 1). Unknown keys are
/// ignored. Err: one human line.
pub fn parse_colors(text: &str, mode: Mode) -> Result<Roles, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| e.message().trim().to_string())?;
    let color = |key: &str| -> Result<Option<Rgb>, String> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::String(s)) => {
                parse_hex(s).map(Some).ok_or_else(|| format!("{key}: invalid colour {s:?} (expected \"#rrggbb\")"))
            }
            Some(_) => Err(format!("{key}: expected a \"#rrggbb\" string")),
        }
    };
    let sixteen = ROLE_NAMES.iter().any(|k| !LEGACY_KEYS.contains(k) && table.contains_key(*k));
    if sixteen {
        let mut roles = Roles::default();
        for name in ROLE_NAMES {
            let c = color(name)?.ok_or_else(|| format!("missing role {name:?}"))?;
            if let Some(slot) = roles.slot(name) {
                *slot = c;
            }
        }
        return Ok(roles);
    }
    let need = |key: &str| color(key)?.ok_or_else(|| format!("missing key {key:?}"));
    let legacy = Legacy {
        bg: need("bg")?,
        fg: need("fg")?,
        accent: need("accent")?,
        muted: need("muted")?,
        surface: need("surface")?,
        error: need("error")?,
        ok: need("ok")?,
    };
    Ok(derive_legacy(&legacy, mode))
}

// ---- TEMPORARY legacy view: task 4 removes its daemon uses, task 7 its TUI
// uses, and the last of the two deletes it. ----

/// The 7-colour palette the daemon and the TUI paint with until tasks 4 and
/// 7 move them to `Roles`.
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

/// Unchanged from before the roles, so the daemon's and the TUI's pixel and
/// colour tests do not move until tasks 4 and 7.
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

impl From<Roles> for Palette {
    fn from(r: Roles) -> Self {
        Palette { bg: r.bar_bg, fg: r.fg, accent: r.accent, muted: r.fg_dim, surface: r.pill_bg, error: r.error, ok: r.ok }
    }
}

impl Palette {
    /// colors.toml (16 roles or legacy 7, read as dark) → the legacy view.
    /// Absent: the default, silently. Same messages as before the roles.
    pub fn load(path: &Path) -> (Palette, Option<String>) {
        let s = match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Palette::default(), None),
            Err(e) => return (Palette::default(), Some(format!("colors.toml unreadable: {e}"))),
            Ok(s) => s,
        };
        match parse_colors(&s, Mode::Dark) {
            Ok(roles) => (Palette::from(roles), None),
            Err(e) => (Palette::default(), Some(format!("invalid colors.toml: {e}"))),
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
    const VALID_LIGHT: &str = "bg = \"#f0f0f0\"\nfg = \"#202020\"\naccent = \"#8800ff\"\nmuted = \"#666666\"\nsurface = \"#e0e0e0\"\nerror = \"#cc0000\"\nok = \"#008800\"\n";

    /// The i-th role's colour in `sixteen`: all distinct.
    fn nth(i: usize) -> Rgb {
        Rgb((i * 16) as u8, i as u8, 200)
    }

    /// A 16-role file, role i = nth(i), without `skip`.
    fn sixteen(skip: Option<&str>) -> String {
        ROLE_NAMES
            .iter()
            .enumerate()
            .filter(|(_, n)| Some(**n) != skip)
            .map(|(i, n)| {
                let Rgb(r, g, b) = nth(i);
                format!("{n} = \"#{r:02x}{g:02x}{b:02x}\"\n")
            })
            .collect()
    }

    // ---- kept from the 7-colour palette ----

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

    // ---- 16 roles ----

    #[test]
    fn role_names_cover_every_field_once_in_order() {
        let mut r = Roles::default();
        for (i, n) in ROLE_NAMES.iter().enumerate() {
            *r.slot(n).unwrap() = nth(i);
        }
        let fields = [
            r.bar_bg,
            r.pill_bg,
            r.popup_bg,
            r.raised,
            r.selection,
            r.fg,
            r.fg_dim,
            r.on_accent,
            r.accent,
            r.accent_container,
            r.accent2,
            r.accent2_container,
            r.ok,
            r.warn,
            r.error,
            r.urgent,
        ];
        assert_eq!(fields, std::array::from_fn::<Rgb, 16, _>(nth));
        assert!(r.slot("bg").is_none());
        assert!(r.slot("border").is_none());
    }

    #[test]
    fn a_sixteen_role_file_is_read_as_is_in_both_modes() {
        let mut r = parse_colors(&sixteen(None), Mode::Dark).unwrap();
        for (i, n) in ROLE_NAMES.iter().enumerate() {
            assert_eq!(*r.slot(n).unwrap(), nth(i), "{n}");
        }
        assert_eq!(parse_colors(&sixteen(None), Mode::Light), Ok(r));
    }

    #[test]
    fn a_sixteen_role_file_missing_or_breaking_a_role_is_an_error() {
        let e = parse_colors(&sixteen(Some("urgent")), Mode::Dark).unwrap_err();
        assert!(e.contains("urgent"), "{e}");
        // nth(15) = #f00fc8.
        let e = parse_colors(&sixteen(None).replace("#f00fc8", "red"), Mode::Dark).unwrap_err();
        assert!(e.contains("urgent") && e.contains("red"), "{e}");
    }

    #[test]
    fn a_legacy_file_is_derived_for_dark() {
        let h = |x: u32| Rgb((x >> 16) as u8, (x >> 8) as u8, x as u8);
        let want = Roles {
            bar_bg: h(0x101010),
            pill_bg: h(0x202020),
            popup_bg: h(0x202020),
            raised: h(0x343434),    // surface + 20
            selection: h(0x484848), // surface + 40
            fg: h(0xeeeeee),
            fg_dim: h(0x777777),
            on_accent: h(0x101010),
            accent: h(0xff8800),
            accent_container: h(0xff8800),
            accent2: h(0x00ff00),
            accent2_container: h(0x00ff00),
            ok: h(0x00ff00),
            warn: h(0xffb000),
            error: h(0xff0000),
            urgent: h(0xff0000),
        };
        assert_eq!(parse_colors(VALID, Mode::Dark), Ok(want));
    }

    #[test]
    fn a_legacy_file_is_derived_for_light() {
        let r = parse_colors(VALID_LIGHT, Mode::Light).unwrap();
        assert_eq!((r.bar_bg, r.pill_bg, r.popup_bg), (Rgb(0xf0, 0xf0, 0xf0), Rgb(0xe0, 0xe0, 0xe0), Rgb(0xe0, 0xe0, 0xe0)));
        assert_eq!(r.raised, Rgb(0xcc, 0xcc, 0xcc)); // surface − 20
        assert_eq!(r.selection, Rgb(0xb8, 0xb8, 0xb8)); // surface − 40
        assert_eq!((r.fg_dim, r.on_accent), (Rgb(0x66, 0x66, 0x66), Rgb(0xf0, 0xf0, 0xf0)));
        assert_eq!(
            (r.accent_container, r.accent2, r.accent2_container),
            (Rgb(0x88, 0x00, 0xff), Rgb(0x00, 0x88, 0x00), Rgb(0x00, 0x88, 0x00))
        );
        assert_eq!(r.warn, Rgb(0x8a, 0x53, 0x00));
        assert_eq!(r.urgent, Rgb(0xcc, 0x00, 0x00));
    }

    #[test]
    fn a_step_moves_away_from_the_background_and_saturates() {
        assert_eq!(step(Rgb(250, 5, 128), Mode::Dark, 1), Rgb(255, 25, 148));
        assert_eq!(step(Rgb(250, 5, 128), Mode::Dark, 2), Rgb(255, 45, 168));
        assert_eq!(step(Rgb(250, 5, 128), Mode::Light, 1), Rgb(230, 0, 108));
        assert_eq!(step(Rgb(250, 5, 128), Mode::Light, 2), Rgb(210, 0, 88));
    }

    #[test]
    fn invalid_values_are_errors() {
        let e = parse_colors(&VALID.replace("\"#ff8800\"", "3"), Mode::Dark).unwrap_err();
        assert!(e.contains("accent"), "{e}");
        let e = parse_colors(&VALID.replace("#ff8800", "#ff88"), Mode::Dark).unwrap_err();
        assert!(e.contains("accent") && e.contains("#ff88"), "{e}");
        let e = parse_colors("bg = \"#101010\"\n", Mode::Dark).unwrap_err();
        assert!(e.contains("fg"), "{e}");
        assert!(parse_colors("bg = ", Mode::Dark).is_err());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        assert_eq!(parse_colors(&format!("{VALID}shadow = \"#000000\"\n"), Mode::Dark), parse_colors(VALID, Mode::Dark));
        assert_eq!(parse_colors(&format!("{}comment = 1\n", sixteen(None)), Mode::Dark), parse_colors(&sixteen(None), Mode::Dark));
    }

    #[test]
    fn the_border_is_fg_dim_at_40_percent_rounded() {
        let r = Roles { fg_dim: Rgb(200, 100, 0), ..Roles::default() };
        assert_eq!(r.border_over(Rgb(0, 0, 0)), Rgb(80, 40, 0));
        assert_eq!(r.border_over(Rgb(255, 255, 255)), Rgb(233, 193, 153));
        assert_eq!(r.border_over(r.fg_dim), r.fg_dim);
        let r = Roles { fg_dim: Rgb(2, 5, 255), ..Roles::default() };
        assert_eq!(r.border_over(Rgb(0, 0, 0)), Rgb(1, 2, 102)); // 0.8 → 1, 2.0 → 2, 102.0 → 102
    }

    #[test]
    fn default_roles_are_catppuccin_mocha() {
        let r = Roles::default();
        assert_eq!(r.bar_bg, Rgb(0x18, 0x18, 0x25)); // Mantle
        assert_eq!(r.popup_bg, Rgb(0x1e, 0x1e, 0x2e)); // Base
        assert_eq!(r.fg, Rgb(0xcd, 0xd6, 0xf4)); // Text
        assert_eq!(r.accent, Rgb(0xcb, 0xa6, 0xf7)); // Mauve
    }

    // ---- the temporary Palette shim (removed by tasks 4 and 7) ----

    #[test]
    fn palette_default_is_unchanged() {
        let p = Palette::default();
        assert_eq!((p.bg, p.muted, p.surface), (Rgb(0x1e, 0x1e, 0x2e), Rgb(0x6c, 0x70, 0x86), Rgb(0x31, 0x32, 0x44)));
    }

    #[test]
    fn the_palette_shim_round_trips_a_legacy_file() {
        let (p, w) = Palette::load(&file("roundtrip", VALID));
        assert_eq!(w, None);
        assert_eq!(
            p,
            Palette {
                bg: Rgb(0x10, 0x10, 0x10),
                fg: Rgb(0xee, 0xee, 0xee),
                accent: Rgb(0xff, 0x88, 0x00),
                muted: Rgb(0x77, 0x77, 0x77),
                surface: Rgb(0x20, 0x20, 0x20),
                error: Rgb(0xff, 0x00, 0x00),
                ok: Rgb(0x00, 0xff, 0x00),
            }
        );
    }

    #[test]
    fn the_palette_shim_reads_a_sixteen_role_file() {
        let (p, w) = Palette::load(&file("sixteen", &sixteen(None)));
        assert_eq!(w, None);
        // bar_bg = nth(0), pill_bg = nth(1), fg_dim = nth(6), fg = nth(5).
        assert_eq!((p.bg, p.surface, p.muted, p.fg), (nth(0), nth(1), nth(6), nth(5)));
    }
}
