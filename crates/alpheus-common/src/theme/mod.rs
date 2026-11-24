//! Colours: 16 named roles and 7 named hues shared by the daemon and the TUI,
//! read from a colour file holding either the 16 roles or the legacy 7 keys
//! (derived per mode). Independent of any display: the TUI converts to
//! ratatui colours (alpheus::theme), the daemon to pixels.
//!
//! Never:
//! - name a display library: the roles are plain values, which the control center turns into
//!   ratatui colours and the daemon into pixels (`the_foundation_carries_no_display_dependency`);
//! - fail on a colour file: a missing or malformed key falls back with one log line
//!   (docs/features/themes.md).
//!
//! See: docs/features/themes.md, docs/configuration.md ("Roles", "Presets").

pub mod contrast;
pub mod mode_file;
pub mod presets;
pub mod settings;

/// 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// Which half of a theme is in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

/// The seven named hues: what the modules' icons and text take
/// when `module_colors` asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hues {
    pub red: Rgb,
    pub orange: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub purple: Rgb,
}

/// Hue names in field order: the optional hue keys of a colour file and of
/// `[colors]` in theme.toml.
pub const HUE_NAMES: [&str; 7] = ["red", "orange", "yellow", "green", "cyan", "blue", "purple"];

impl Hues {
    /// The field named `name` (one of `HUE_NAMES`), None otherwise.
    pub(crate) fn slot(&mut self, name: &str) -> Option<&mut Rgb> {
        Some(match name {
            "red" => &mut self.red,
            "orange" => &mut self.orange,
            "yellow" => &mut self.yellow,
            "green" => &mut self.green,
            "cyan" => &mut self.cyan,
            "blue" => &mut self.blue,
            "purple" => &mut self.purple,
            _ => return None,
        })
    }
}

/// The 16 colour roles and the 7 hues. The frame
/// colour is not a role: it is derived from `fg_dim` by `border_over`.
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
    pub hues: Hues,
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

/// Catppuccin Mocha's red, peach, yellow, green, teal, blue and mauve: the
/// reference hues of a dark colour file that carries none.
const HUES_DARK: Hues = Hues {
    red: Rgb(0xf3, 0x8b, 0xa8),
    orange: Rgb(0xfa, 0xb3, 0x87),
    yellow: Rgb(0xf9, 0xe2, 0xaf),
    green: Rgb(0xa6, 0xe3, 0xa1),
    cyan: Rgb(0x94, 0xe2, 0xd5),
    blue: Rgb(0x89, 0xb4, 0xfa),
    purple: Rgb(0xcb, 0xa6, 0xf7),
};
/// The same seven for a light colour file: Catppuccin Latte as the preset ships it, with the orange,
/// yellow and green darkened to reach 3:1 on a light bar (raw Latte does not).
const HUES_LIGHT: Hues = presets::CATPPUCCIN_LIGHT.hues;
/// How far a derived hue moves toward the accent: 30 %.
const HUE_MIX: u16 = 30;

/// `c` mixed `HUE_MIX` % toward `toward`: per 8-bit channel, linear in the
/// sRGB values (not in linear light), rounded to nearest.
fn mix_toward(c: Rgb, toward: Rgb) -> Rgb {
    let m = |a: u8, b: u8| ((u16::from(a) * (100 - HUE_MIX) + u16::from(b) * HUE_MIX + 50) / 100) as u8;
    Rgb(m(c.0, toward.0), m(c.1, toward.1), m(c.2, toward.2))
}

impl Roles {
    /// fg_dim at 40 % over `bg`, rounded: frames and borders.
    pub fn border_over(&self, bg: Rgb) -> Rgb {
        let mix = |f: u8, b: u8| ((u16::from(f) * BORDER_ALPHA + u16::from(b) * (255 - BORDER_ALPHA) + 127) / 255) as u8;
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

/// The 16 roles from a legacy file (backward compatibility).
fn derive_legacy(l: &Legacy, mode: Mode, hues: Hues) -> Roles {
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
        hues,
    }
}

/// A colour file's text: the 16 role keys, or the legacy 7 (`bg fg accent
/// muted surface error ok`), derived for `mode`. Either may add the
/// seven hue keys (`red` … `purple`); a missing one is Catppuccin's
/// (Mocha for dark, the contrast-fixed Latte of the preset for light) mixed 30 % toward `accent`. Unknown keys
/// are ignored. Err: one human line.
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
    // Each missing hue is derived from the accent once it is known.
    let hues = |accent: Rgb| -> Result<Hues, String> {
        let reference = match mode {
            Mode::Dark => HUES_DARK,
            Mode::Light => HUES_LIGHT,
        };
        let mut hues = reference;
        for name in HUE_NAMES {
            if let Some(slot) = hues.slot(name) {
                *slot = match color(name)? {
                    Some(c) => c,
                    None => mix_toward(*slot, accent),
                };
            }
        }
        Ok(hues)
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
        roles.hues = hues(roles.accent)?;
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
    let hues = hues(legacy.accent)?;
    Ok(derive_legacy(&legacy, mode, hues))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Hue i = nth(20 + i): distinct from every role of `nth`.
    fn hue_lines(skip: Option<&str>) -> String {
        HUE_NAMES
            .iter()
            .enumerate()
            .filter(|(_, n)| Some(**n) != skip)
            .map(|(i, n)| {
                let Rgb(r, g, b) = nth(20 + i);
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
        let text = format!("{}{}", sixteen(None), hue_lines(None));
        let mut r = parse_colors(&text, Mode::Dark).unwrap();
        for (i, n) in ROLE_NAMES.iter().enumerate() {
            assert_eq!(*r.slot(n).unwrap(), nth(i), "{n}");
        }
        assert_eq!(parse_colors(&text, Mode::Light), Ok(r));
        // Without hues, the roles are the same and only the derived hues follow the mode.
        let (d, l) = (parse_colors(&sixteen(None), Mode::Dark).unwrap(), parse_colors(&sixteen(None), Mode::Light).unwrap());
        assert_eq!((d.accent, d.urgent), (l.accent, l.urgent));
        assert_ne!(d.hues, l.hues);
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
        let mut want = Roles {
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
            hues: HUES_DARK, // replaced below: the derived hues have their own test
        };
        let got = parse_colors(VALID, Mode::Dark).unwrap();
        want.hues = got.hues;
        assert_eq!(got, want);
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

    // ---- seven hues ----

    #[test]
    fn hue_names_cover_every_field_once_in_order() {
        let mut h = Roles::default().hues;
        for (i, n) in HUE_NAMES.iter().enumerate() {
            *h.slot(n).unwrap() = nth(i);
        }
        let fields = [h.red, h.orange, h.yellow, h.green, h.cyan, h.blue, h.purple];
        assert_eq!(fields, std::array::from_fn::<Rgb, 7, _>(nth));
        assert!(h.slot("accent").is_none());
        assert!(h.slot("magenta").is_none());
    }

    #[test]
    fn hues_in_a_sixteen_role_or_a_legacy_file_are_read_as_is() {
        let want = |r: &Roles| {
            std::array::from_fn::<Rgb, 7, _>(|i| nth(20 + i))
                == [r.hues.red, r.hues.orange, r.hues.yellow, r.hues.green, r.hues.cyan, r.hues.blue, r.hues.purple]
        };
        for mode in [Mode::Dark, Mode::Light] {
            assert!(want(&parse_colors(&format!("{}{}", sixteen(None), hue_lines(None)), mode).unwrap()));
            assert!(want(&parse_colors(&format!("{VALID}{}", hue_lines(None)), mode).unwrap()));
        }
    }

    #[test]
    fn missing_hues_are_the_catppuccin_reference_mixed_30_percent_toward_the_accent() {
        // Mocha red #f38ba8 with accent #ff8800: (243*70 + 255*30 + 50) / 100 = 247, (139*70 + 136*30 + 50) / 100 = 138,
        // (168*70 + 0*30 + 50) / 100 = 118.
        let r = parse_colors(VALID, Mode::Dark).unwrap();
        assert_eq!(r.hues.red, Rgb(247, 138, 118));
        // Latte red #d20f39 with accent #8800ff: (210*70 + 136*30 + 50) / 100 = 188, (15*70 + 50) / 100 = 11,
        // (57*70 + 255*30 + 50) / 100 = 116.
        let r = parse_colors(VALID_LIGHT, Mode::Light).unwrap();
        assert_eq!(r.hues.red, Rgb(188, 11, 116));
        // The same for a 16-role file without hues (accent = nth(8) = #8008c8), in dark: Mocha blue #89b4fa.
        let r = parse_colors(&sixteen(None), Mode::Dark).unwrap();
        assert_eq!(
            r.hues.blue,
            Rgb(((137 * 70 + 128 * 30 + 50) / 100) as u8, ((180 * 70 + 8 * 30 + 50) / 100) as u8, ((250 * 70 + 200 * 30 + 50) / 100) as u8)
        );
        // One present hue is kept while the others are derived.
        let r = parse_colors(&format!("{VALID}green = \"#010203\"\n"), Mode::Dark).unwrap();
        assert_eq!((r.hues.green, r.hues.red), (Rgb(1, 2, 3), Rgb(247, 138, 118)));
    }

    #[test]
    fn a_missing_light_hue_starts_from_the_darkened_reference() {
        // Latte's raw orange #fe640b is 2.45:1 on a light bar; the reference is the darkened #e0580a. Toward the
        // accent #8800ff: (224*70 + 136*30 + 50) / 100 = 198, (88*70 + 50) / 100 = 62, (10*70 + 255*30 + 50) / 100 = 84.
        let r = parse_colors(VALID_LIGHT, Mode::Light).unwrap();
        assert_eq!(r.hues.orange, Rgb(198, 62, 84));
    }

    #[test]
    fn an_invalid_hue_is_an_error_naming_the_key() {
        let e = parse_colors(&format!("{VALID}blue = \"nope\"\n"), Mode::Dark).unwrap_err();
        assert!(e.contains("blue") && e.contains("nope"), "{e}");
        let e = parse_colors(&format!("{}{}", sixteen(None), "cyan = 3\n"), Mode::Dark).unwrap_err();
        assert!(e.contains("cyan"), "{e}");
        let e = parse_colors(&format!("{}{}", sixteen(None), hue_lines(None).replace("#", "")), Mode::Dark).unwrap_err();
        assert!(e.contains("red"), "{e}");
    }
}
