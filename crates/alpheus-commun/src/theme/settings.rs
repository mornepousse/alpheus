//! theme.toml (spec § 2): which theme, which mode, and per-role overrides;
//! the resolver that turns them and the wallpaper colour files into `Roles`,
//! with spec § 6's fallbacks (one line each, never fatal); and the portal's
//! `color-scheme` value. Pure apart from reading the files it is given.

use super::presets::{self, DEFAULT_PRESET, PRESETS, Preset};
use super::{Mode, ROLE_NAMES, Rgb, Roles, parse_colors, parse_hex};
use std::path::Path;

pub const THEME_FILE: &str = "theme.toml";
pub const DARK_FILE: &str = "colors.toml";
pub const LIGHT_FILE: &str = "colors-light.toml";

/// `mode` in theme.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSetting {
    /// Follow the desktop (the portal's `color-scheme`).
    Auto,
    Dark,
    Light,
}

/// `theme` in theme.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    /// A compiled-in preset, by its name in `PRESETS`.
    Preset(&'static str),
    /// matugen's colors.toml / colors-light.toml.
    Wallpaper,
}

/// Validated `[colors]` entries, in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides(Vec<(&'static str, Rgb)>);

impl Overrides {
    /// Every override onto `roles`.
    pub(crate) fn apply(&self, roles: &mut Roles) {
        for (name, c) in &self.0 {
            if let Some(slot) = roles.slot(name) {
                *slot = *c;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSettings {
    pub source: ThemeSource,
    pub mode: ModeSetting,
    pub overrides: Overrides,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        ThemeSettings { source: ThemeSource::Preset(DEFAULT_PRESET.name), mode: ModeSetting::Auto, overrides: Overrides::default() }
    }
}

/// "catppuccin, gruvbox, …, dracula": for the unknown-theme line.
fn known() -> String {
    PRESETS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
}

/// theme.toml's text. Warnings: one line each, never fatal; whatever is
/// valid is kept. No `theme` key → catppuccin (`load_settings` applies the
/// wallpaper rule on top).
pub fn parse_settings(text: &str) -> (ThemeSettings, Vec<String>) {
    parse_settings_or(text, ThemeSource::Preset(DEFAULT_PRESET.name))
}

/// `parse_settings`, with `source` when the text has no `theme` key at
/// all. An unknown or invalid `theme` still means catppuccin.
fn parse_settings_or(text: &str, source: ThemeSource) -> (ThemeSettings, Vec<String>) {
    let mut s = ThemeSettings { source, ..ThemeSettings::default() };
    let mut w = Vec::new();
    let table: toml::Table = match toml::from_str(text) {
        Ok(t) => t,
        Err(e) => return (s, vec![format!("invalid {THEME_FILE}: {}", e.message().trim())]),
    };
    let fallback = DEFAULT_PRESET.name;
    for (key, value) in &table {
        match key.as_str() {
            "theme" => match value.as_str() {
                Some("wallpaper") => s.source = ThemeSource::Wallpaper,
                Some(name) => match presets::preset(name) {
                    Some(p) => s.source = ThemeSource::Preset(p.name),
                    None => {
                        s.source = ThemeSource::Preset(fallback);
                        w.push(format!("{THEME_FILE}: unknown theme {name:?}, using {fallback:?} (known: {}, wallpaper)", known()));
                    }
                },
                None => {
                    s.source = ThemeSource::Preset(fallback);
                    w.push(format!("{THEME_FILE}: theme must be a string, using {fallback:?}"));
                }
            },
            "mode" => match value.as_str() {
                Some("auto") => s.mode = ModeSetting::Auto,
                Some("dark") => s.mode = ModeSetting::Dark,
                Some("light") => s.mode = ModeSetting::Light,
                _ => w.push(format!("{THEME_FILE}: mode must be \"auto\", \"dark\" or \"light\", got {value}; using \"auto\"")),
            },
            "colors" => match value.as_table() {
                Some(colors) => {
                    for (role, v) in colors {
                        match ROLE_NAMES.iter().find(|n| **n == role.as_str()) {
                            None => w.push(format!("{THEME_FILE}: [colors] {role:?} is not a role, ignored")),
                            Some(name) => match v.as_str().and_then(parse_hex) {
                                Some(c) => s.overrides.0.push((*name, c)),
                                None => {
                                    w.push(format!("{THEME_FILE}: [colors] {role:?}: invalid colour {v} (expected \"#rrggbb\"), ignored"))
                                }
                            },
                        }
                    }
                }
                None => w.push(format!("{THEME_FILE}: colors must be a table ([colors]), ignored")),
            },
            other => w.push(format!("{THEME_FILE}: unknown key {other:?}, ignored")),
        }
    }
    (s, w)
}

/// The theme when theme.toml names none (spec § 2 as amended): the
/// wallpaper when matugen's colors.toml exists, as before themes existed, so
/// an upgrade changes nothing; else catppuccin.
fn default_source(dir: &Path) -> ThemeSource {
    if dir.join(DARK_FILE).is_file() { ThemeSource::Wallpaper } else { ThemeSource::Preset(DEFAULT_PRESET.name) }
}

/// `dir/theme.toml`; absent → Default with no warning; unreadable → Default
/// with one. Without a `theme` key, `default_source` decides.
pub fn load_settings(dir: &Path) -> (ThemeSettings, Vec<String>) {
    let source = default_source(dir);
    match std::fs::read_to_string(dir.join(THEME_FILE)) {
        Ok(text) => parse_settings_or(&text, source),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (ThemeSettings { source, ..ThemeSettings::default() }, Vec::new()),
        Err(e) => (ThemeSettings { source, ..ThemeSettings::default() }, vec![format!("{THEME_FILE} unreadable: {e}")]),
    }
}

/// `p`'s half for `mode`; a missing light half → its dark one, one line.
fn half(p: &Preset, mode: Mode, w: &mut Vec<String>) -> Roles {
    match (mode, p.light) {
        (Mode::Dark, _) => p.dark,
        (Mode::Light, Some(light)) => light,
        (Mode::Light, None) => {
            w.push(format!("theme {:?} has no light variant: using its dark one", p.name));
            p.dark
        }
    }
}

/// A colour file: read, absent, or unusable (with its line).
enum ColorFile {
    Found(Roles),
    Absent,
    Bad(String),
}

fn read_colors(dir: &Path, file: &str, mode: Mode) -> ColorFile {
    match std::fs::read_to_string(dir.join(file)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ColorFile::Absent,
        Err(e) => ColorFile::Bad(format!("{file} unreadable: {e}")),
        Ok(text) => match parse_colors(&text, mode) {
            Ok(roles) => ColorFile::Found(roles),
            Err(e) => ColorFile::Bad(format!("invalid {file}: {e}")),
        },
    }
}

/// theme = "wallpaper" (spec § 2): the light file in light mode, else the
/// dark file (read as dark, since that is what it is); any unusable file →
/// the default theme, and `bad` set (an absent file leaves it alone).
fn wallpaper(mode: Mode, dir: &Path, w: &mut Vec<String>, bad: &mut bool) -> Roles {
    let fallback = DEFAULT_PRESET.name;
    if mode == Mode::Light {
        match read_colors(dir, LIGHT_FILE, Mode::Light) {
            ColorFile::Found(roles) => return roles,
            ColorFile::Absent => w.push(format!("{LIGHT_FILE} not found: light mode uses {DARK_FILE}")),
            ColorFile::Bad(line) => {
                *bad = true;
                w.push(format!("{line}, using {fallback:?}"));
                return half(DEFAULT_PRESET, mode, w);
            }
        }
    }
    match read_colors(dir, DARK_FILE, Mode::Dark) {
        ColorFile::Found(roles) => roles,
        ColorFile::Absent => {
            w.push(format!("theme \"wallpaper\": {DARK_FILE} not found, using {fallback:?}"));
            half(DEFAULT_PRESET, mode, w)
        }
        ColorFile::Bad(line) => {
            *bad = true;
            w.push(format!("{line}, using {fallback:?}"));
            half(DEFAULT_PRESET, mode, w)
        }
    }
}

/// `resolve`'s result, plus whether a wallpaper colour file existed but was
/// unreadable or invalid (controller reconciliation 9: a hot reload keeps the
/// previous roles then; startup falls back to the default theme, spec § 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub roles: Roles,
    pub warnings: Vec<String>,
    pub bad_file: bool,
}

/// The roles for `settings` in `mode`. Reads `dir/colors.toml` /
/// `dir/colors-light.toml` for Wallpaper only; falls back per spec § 6;
/// overrides apply last. Each failure is one warning line.
pub fn resolve(settings: &ThemeSettings, mode: Mode, dir: &Path) -> (Roles, Vec<String>) {
    let r = resolve_detailed(settings, mode, dir);
    (r.roles, r.warnings)
}

/// `resolve`, with `bad_file`.
pub fn resolve_detailed(settings: &ThemeSettings, mode: Mode, dir: &Path) -> Resolved {
    let mut w = Vec::new();
    let mut bad_file = false;
    let mut roles = match &settings.source {
        ThemeSource::Wallpaper => wallpaper(mode, dir, &mut w, &mut bad_file),
        ThemeSource::Preset(name) => match presets::preset(name) {
            Some(p) => half(p, mode, &mut w),
            None => {
                w.push(format!("unknown theme {name:?}, using {:?}", DEFAULT_PRESET.name));
                half(DEFAULT_PRESET, mode, &mut w)
            }
        },
    };
    settings.overrides.apply(&mut roles);
    Resolved { roles, warnings: w, bad_file }
}

/// The portal's color-scheme value: 1 → Dark, 2 → Light, anything else
/// (0 = no preference) → Dark.
pub fn portal_mode(value: u32) -> Mode {
    match value {
        2 => Mode::Light,
        _ => Mode::Dark,
    }
}

/// The mode actually in effect: Dark/Light as set, Auto → `portal`
/// (None = no portal → Dark).
pub fn effective_mode(setting: ModeSetting, portal: Option<Mode>) -> Mode {
    match setting {
        ModeSetting::Dark => Mode::Dark,
        ModeSetting::Light => Mode::Light,
        ModeSetting::Auto => portal.unwrap_or(Mode::Dark),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// `$TMPDIR/alpheus-<pid>-<name>`, emptied and created; removed when
    /// dropped, so a run leaves nothing behind (like the launcher's TempDir).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let d = std::env::temp_dir().join(format!("alpheus-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }

        fn write(&self, file: &str, text: &str) {
            std::fs::write(self.0.join(file), text).unwrap();
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A 16-role colour file holding `roles`.
    fn file_of(mut roles: Roles) -> String {
        ROLE_NAMES
            .iter()
            .map(|n| {
                let Rgb(r, g, b) = *roles.slot(n).unwrap();
                format!("{n} = \"#{r:02x}{g:02x}{b:02x}\"\n")
            })
            .collect()
    }

    const LEGACY_LIGHT: &str = "bg = \"#f0f0f0\"\nfg = \"#202020\"\naccent = \"#8800ff\"\nmuted = \"#666666\"\nsurface = \"#e0e0e0\"\nerror = \"#cc0000\"\nok = \"#008800\"\n";

    fn named(name: &str) -> &'static Preset {
        presets::preset(name).unwrap()
    }

    fn wallpaper() -> ThemeSettings {
        ThemeSettings { source: ThemeSource::Wallpaper, ..ThemeSettings::default() }
    }

    const NONE: Vec<String> = Vec::new();

    // ---- parse_settings ----

    #[test]
    fn an_empty_file_is_the_default() {
        let d = ThemeSettings::default();
        assert_eq!(
            d,
            ThemeSettings { source: ThemeSource::Preset("catppuccin"), mode: ModeSetting::Auto, overrides: Overrides::default() }
        );
        assert_eq!(parse_settings(""), (d, NONE));
    }

    #[test]
    fn theme_mode_and_overrides_are_read() {
        let (s, w) = parse_settings("theme = \"tokyo-night\"\nmode = \"light\"\n\n[colors]\naccent = \"#ff9e64\"\n");
        assert_eq!(w, NONE);
        assert_eq!(s.source, ThemeSource::Preset("tokyo-night"));
        assert_eq!(s.mode, ModeSetting::Light);
        assert_eq!(s.overrides, Overrides(vec![("accent", Rgb(0xff, 0x9e, 0x64))]));
        let (s, w) = parse_settings("theme = \"wallpaper\"\nmode = \"dark\"\n");
        assert_eq!((s.source, s.mode, w), (ThemeSource::Wallpaper, ModeSetting::Dark, NONE));
        assert_eq!(parse_settings("mode = \"auto\"\n").0.mode, ModeSetting::Auto);
    }

    #[test]
    fn every_preset_name_is_accepted() {
        for p in PRESETS {
            let (s, w) = parse_settings(&format!("theme = \"{}\"\n", p.name));
            assert_eq!((s.source, w), (ThemeSource::Preset(p.name), NONE));
        }
    }

    #[test]
    fn an_unknown_theme_warns_and_keeps_the_default() {
        let (s, w) = parse_settings("theme = \"solarised\"\n");
        assert_eq!(s.source, ThemeSource::Preset("catppuccin"));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"solarised\"") && w[0].contains("tokyo-night") && w[0].contains("wallpaper"), "{w:?}");
        let (s, w) = parse_settings("theme = 3\n");
        assert_eq!((s.source, w.len()), (ThemeSource::Preset("catppuccin"), 1));
    }

    #[test]
    fn a_bad_mode_warns_and_keeps_auto() {
        let (s, w) = parse_settings("theme = \"nord\"\nmode = \"dim\"\n");
        assert_eq!((s.source, s.mode, w.len()), (ThemeSource::Preset("nord"), ModeSetting::Auto, 1));
        assert!(w[0].contains("mode") && w[0].contains("dim"), "{w:?}");
    }

    #[test]
    fn bad_overrides_are_ignored_one_line_each() {
        let (s, w) = parse_settings("[colors]\naccent = \"orange\"\nbar_bg = \"#000000\"\nshadow = \"#111111\"\nfg = 7\n");
        assert_eq!(s.overrides, Overrides(vec![("bar_bg", Rgb(0, 0, 0))]));
        assert_eq!(w.len(), 3, "{w:?}");
        for role in ["\"accent\"", "\"shadow\"", "\"fg\""] {
            assert!(w.iter().any(|l| l.contains(role)), "{role}: {w:?}");
        }
        let (s, w) = parse_settings("colors = \"#ffffff\"\n");
        assert_eq!((s.overrides, w.len()), (Overrides::default(), 1));
    }

    #[test]
    fn unknown_keys_and_invalid_toml_warn_without_failing() {
        let (s, w) = parse_settings("style = \"islands\"\ntheme = \"dracula\"\n");
        assert_eq!((s.source, w.len()), (ThemeSource::Preset("dracula"), 1));
        assert!(w[0].contains("\"style\""), "{w:?}");
        let (s, w) = parse_settings("theme = \n");
        assert_eq!((s, w.len()), (ThemeSettings::default(), 1));
        assert!(w[0].contains("theme.toml"), "{w:?}");
    }

    #[test]
    fn load_settings_reads_theme_toml_and_is_silent_when_absent() {
        let dir = TempDir::new("theme-load");
        assert_eq!(load_settings(&dir), (ThemeSettings::default(), NONE));
        dir.write(THEME_FILE, "theme = \"kanagawa\"\n");
        assert_eq!(load_settings(&dir), (ThemeSettings { source: ThemeSource::Preset("kanagawa"), ..ThemeSettings::default() }, NONE));
    }

    // ---- the default source (spec § 2 as amended) ----

    #[test]
    fn no_theme_toml_with_colors_toml_present_is_the_wallpaper() {
        let dir = TempDir::new("theme-default-wallpaper");
        dir.write(DARK_FILE, LEGACY_LIGHT);
        assert_eq!(load_settings(&dir), (wallpaper(), NONE));
    }

    #[test]
    fn no_theme_toml_and_no_colors_toml_is_catppuccin() {
        let dir = TempDir::new("theme-default-catppuccin");
        assert_eq!(load_settings(&dir), (ThemeSettings::default(), NONE));
        dir.write(LIGHT_FILE, LEGACY_LIGHT); // only the light file: not enough
        assert_eq!(load_settings(&dir).0.source, ThemeSource::Preset("catppuccin"));
    }

    #[test]
    fn a_theme_toml_without_a_theme_key_follows_the_same_rule() {
        let dir = TempDir::new("theme-default-no-key");
        dir.write(THEME_FILE, "mode = \"light\"\n");
        assert_eq!(load_settings(&dir), (ThemeSettings { mode: ModeSetting::Light, ..ThemeSettings::default() }, NONE));
        dir.write(DARK_FILE, LEGACY_LIGHT);
        assert_eq!(load_settings(&dir), (ThemeSettings { mode: ModeSetting::Light, ..wallpaper() }, NONE));
        // A theme named in the file wins; an unknown one is still catppuccin.
        dir.write(THEME_FILE, "theme = \"nord\"\n");
        assert_eq!(load_settings(&dir).0.source, ThemeSource::Preset("nord"));
        dir.write(THEME_FILE, "theme = \"nope\"\n");
        let (s, w) = load_settings(&dir);
        assert_eq!((s.source, w.len()), (ThemeSource::Preset("catppuccin"), 1));
        // Invalid TOML: the rule still applies.
        dir.write(THEME_FILE, "mode = \n");
        let (s, w) = load_settings(&dir);
        assert_eq!((s.source, w.len()), (ThemeSource::Wallpaper, 1));
    }

    // ---- resolve ----

    #[test]
    fn a_preset_resolves_per_mode() {
        let dir = TempDir::new("theme-preset");
        let s = ThemeSettings::default();
        assert_eq!(resolve(&s, Mode::Dark, &dir), (named("catppuccin").dark, NONE));
        assert_eq!(resolve(&s, Mode::Light, &dir), (named("catppuccin").light.unwrap(), NONE));
        for p in PRESETS {
            let s = ThemeSettings { source: ThemeSource::Preset(p.name), ..ThemeSettings::default() };
            assert_eq!(resolve(&s, Mode::Dark, &dir), (p.dark, NONE), "{}", p.name);
        }
    }

    #[test]
    fn a_dark_only_preset_in_light_mode_uses_its_dark_half_with_one_line() {
        let dir = TempDir::new("theme-dark-only");
        for name in ["nord", "dracula"] {
            let s = ThemeSettings { source: ThemeSource::Preset(name), ..ThemeSettings::default() };
            let (r, w) = resolve(&s, Mode::Light, &dir);
            assert_eq!(r, named(name).dark);
            assert_eq!(w.len(), 1, "{w:?}");
            assert!(w[0].contains(name) && w[0].contains("light"), "{w:?}");
        }
    }

    #[test]
    fn an_unknown_preset_name_resolves_to_the_default_with_one_line() {
        let dir = TempDir::new("theme-unknown");
        let s = ThemeSettings { source: ThemeSource::Preset("nope"), ..ThemeSettings::default() };
        let (r, w) = resolve(&s, Mode::Light, &dir);
        assert_eq!(r, named("catppuccin").light.unwrap());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"nope\""), "{w:?}");
    }

    #[test]
    fn overrides_apply_last_on_presets_and_on_the_wallpaper() {
        let dir = TempDir::new("theme-overrides");
        dir.write(DARK_FILE, &file_of(named("gruvbox").dark));
        let (mut s, w) = parse_settings("theme = \"dracula\"\n[colors]\naccent = \"#123456\"\nbar_bg = \"#000001\"\n");
        assert_eq!(w, NONE);
        let (r, w) = resolve(&s, Mode::Dark, &dir);
        assert_eq!(w, NONE);
        assert_eq!((r.accent, r.bar_bg, r.fg), (Rgb(0x12, 0x34, 0x56), Rgb(0, 0, 1), named("dracula").dark.fg));
        s.source = ThemeSource::Wallpaper;
        let (r, w) = resolve(&s, Mode::Dark, &dir);
        assert_eq!(w, NONE);
        assert_eq!((r.accent, r.bar_bg, r.fg), (Rgb(0x12, 0x34, 0x56), Rgb(0, 0, 1), named("gruvbox").dark.fg));
    }

    #[test]
    fn the_wallpaper_reads_the_dark_and_the_light_file() {
        let dir = TempDir::new("theme-wallpaper");
        dir.write(DARK_FILE, &file_of(named("kanagawa").dark));
        dir.write(LIGHT_FILE, LEGACY_LIGHT);
        assert_eq!(resolve(&wallpaper(), Mode::Dark, &dir), (named("kanagawa").dark, NONE));
        let (r, w) = resolve(&wallpaper(), Mode::Light, &dir);
        assert_eq!(w, NONE);
        assert_eq!(Ok(r), parse_colors(LEGACY_LIGHT, Mode::Light));
        assert_eq!(r.raised, Rgb(0xcc, 0xcc, 0xcc)); // the light file is derived for light: darkened
    }

    #[test]
    fn the_wallpaper_in_light_without_its_file_uses_the_dark_one_with_one_line() {
        let dir = TempDir::new("theme-wallpaper-dark-only");
        dir.write(DARK_FILE, "bg = \"#101010\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n");
        let (r, w) = resolve(&wallpaper(), Mode::Light, &dir);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains(LIGHT_FILE) && w[0].contains(DARK_FILE), "{w:?}");
        assert_eq!(r.raised, Rgb(0x34, 0x34, 0x34)); // read as the dark palette it is: lightened
    }

    #[test]
    fn the_wallpaper_without_a_usable_file_falls_back_to_the_default_theme() {
        let dir = TempDir::new("theme-wallpaper-none");
        let (r, w) = resolve(&wallpaper(), Mode::Dark, &dir);
        assert_eq!(r, named("catppuccin").dark);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains(DARK_FILE) && w[0].contains("catppuccin"), "{w:?}");
        // Light, neither file: the light file's line, then the dark file's.
        let (r, w) = resolve(&wallpaper(), Mode::Light, &dir);
        assert_eq!(r, named("catppuccin").light.unwrap());
        assert_eq!(w.len(), 2, "{w:?}");

        dir.write(DARK_FILE, "bg = \"#101010\"\n");
        let (r, w) = resolve(&wallpaper(), Mode::Dark, &dir);
        assert_eq!(r, named("catppuccin").dark);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("invalid colors.toml"), "{w:?}");

        dir.write(LIGHT_FILE, "fg = \"nope\"\n");
        let (r, w) = resolve(&wallpaper(), Mode::Light, &dir);
        assert_eq!(r, named("catppuccin").light.unwrap());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("invalid colors-light.toml"), "{w:?}");
    }

    /// Controller reconciliation 9: a file that exists but cannot be used is flagged, so a hot
    /// reload can keep the previous roles; an absent file is a normal fallback, never flagged.
    #[test]
    fn a_bad_wallpaper_file_is_flagged_an_absent_one_is_not() {
        let dir = TempDir::new("theme-bad-flag");
        let r = resolve_detailed(&wallpaper(), Mode::Dark, &dir);
        assert_eq!((r.roles, r.bad_file), (named("catppuccin").dark, false), "absent: the default theme, not bad");
        dir.write(DARK_FILE, "bg = \"#1010"); // truncated mid-write
        let r = resolve_detailed(&wallpaper(), Mode::Dark, &dir);
        assert!(r.bad_file, "{:?}", r.warnings);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert_eq!(resolve(&wallpaper(), Mode::Dark, &dir), (r.roles, r.warnings), "resolve is resolve_detailed without the flag");
        dir.write(DARK_FILE, &file_of(named("gruvbox").dark));
        dir.write(LIGHT_FILE, "fg = \"nope\"\n");
        assert!(resolve_detailed(&wallpaper(), Mode::Light, &dir).bad_file, "a bad light file too");
        assert!(!resolve_detailed(&wallpaper(), Mode::Dark, &dir).bad_file, "the dark file is fine");
        let preset = ThemeSettings::default();
        assert!(!resolve_detailed(&preset, Mode::Light, &dir).bad_file, "a preset never reads the files");
    }

    /// Review Focus 5: Mae's machine on upgrade day — a legacy 7-key colors.toml and no
    /// theme.toml. Through load_settings (the wallpaper rule) and resolve, the bar keeps its `bg`.
    #[test]
    fn a_legacy_colors_toml_alone_keeps_its_bg() {
        let dir = TempDir::new("theme-legacy-alone");
        dir.write(DARK_FILE, "bg = \"#101820\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n");
        assert_eq!(resolve(&load_settings(&dir).0, Mode::Dark, &dir).0.bar_bg, Rgb(0x10, 0x18, 0x20), "the file's bg, not catppuccin");
    }

    // ---- mode ----

    #[test]
    fn the_portal_value_maps_to_a_mode() {
        assert_eq!(portal_mode(0), Mode::Dark); // no preference
        assert_eq!(portal_mode(1), Mode::Dark);
        assert_eq!(portal_mode(2), Mode::Light);
        assert_eq!(portal_mode(3), Mode::Dark);
        assert_eq!(portal_mode(u32::MAX), Mode::Dark);
    }

    #[test]
    fn the_effective_mode() {
        for portal in [None, Some(Mode::Dark), Some(Mode::Light)] {
            assert_eq!(effective_mode(ModeSetting::Dark, portal), Mode::Dark);
            assert_eq!(effective_mode(ModeSetting::Light, portal), Mode::Light);
        }
        assert_eq!(effective_mode(ModeSetting::Auto, None), Mode::Dark);
        assert_eq!(effective_mode(ModeSetting::Auto, Some(Mode::Dark)), Mode::Dark);
        assert_eq!(effective_mode(ModeSetting::Auto, Some(Mode::Light)), Mode::Light);
    }
}
