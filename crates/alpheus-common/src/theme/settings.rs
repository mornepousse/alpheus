//! theme.toml (spec § 2, § 9): which theme, which mode, how the modules take the hues, and per-role and per-hue overrides;
//! the resolver that turns them and the wallpaper colour files into `Roles`,
//! with spec § 6's fallbacks (one line each, never fatal); and the portal's
//! `color-scheme` value. Pure apart from reading the files it is given.

use super::presets::{self, DEFAULT_PRESET, PRESETS, Preset};
use super::{HUE_NAMES, Mode, ROLE_NAMES, Rgb, Roles, parse_colors, parse_hex};
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

impl ModeSetting {
    /// Every mode, in the Theme tab's order.
    pub const ALL: [ModeSetting; 3] = [ModeSetting::Auto, ModeSetting::Dark, ModeSetting::Light];

    /// Its value in theme.toml.
    pub fn name(self) -> &'static str {
        match self {
            ModeSetting::Auto => "auto",
            ModeSetting::Dark => "dark",
            ModeSetting::Light => "light",
        }
    }

    /// The mode theme.toml names `name` (exact, lower case), None otherwise.
    pub fn from_name(name: &str) -> Option<ModeSetting> {
        ModeSetting::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// `theme` in theme.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeSource {
    /// A compiled-in preset, by its name in `PRESETS`.
    Preset(&'static str),
    /// matugen's colors.toml / colors-light.toml.
    Wallpaper,
}

/// `module_colors` in theme.toml (spec § 9): how much of each module takes its hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModuleColors {
    /// Delivery 1's look: no hue.
    None,
    /// The module's icon takes its hue.
    #[default]
    Icons,
    /// The icon and the text.
    Full,
    /// The block styles fill each block with its first module's hue, its text and icons in
    /// `bar_bg` or `fg`, whichever contrasts more (shapes spec § 1); under `flat` and
    /// `underline`, as `Full`.
    Pills,
}

impl ModuleColors {
    /// Every value, in the Theme tab's order.
    pub const ALL: [ModuleColors; 4] = [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills];

    /// Its value in theme.toml.
    pub fn name(self) -> &'static str {
        match self {
            ModuleColors::None => "none",
            ModuleColors::Icons => "icons",
            ModuleColors::Full => "full",
            ModuleColors::Pills => "pills",
        }
    }

    /// The value theme.toml names `name` (exact, lower case), None otherwise.
    pub fn from_name(name: &str) -> Option<ModuleColors> {
        ModuleColors::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// `style` in theme.toml (shapes spec § 1): the bar's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BarStyle {
    /// Today's bar.
    #[default]
    Flat,
    Islands,
    Powerline,
    Floating,
    Underline,
    Translucent,
}

impl BarStyle {
    /// Every style, in the Theme tab's order.
    pub const ALL: [BarStyle; 6] =
        [BarStyle::Flat, BarStyle::Islands, BarStyle::Powerline, BarStyle::Floating, BarStyle::Underline, BarStyle::Translucent];

    /// Its value in theme.toml.
    pub fn name(self) -> &'static str {
        match self {
            BarStyle::Flat => "flat",
            BarStyle::Islands => "islands",
            BarStyle::Powerline => "powerline",
            BarStyle::Floating => "floating",
            BarStyle::Underline => "underline",
            BarStyle::Translucent => "translucent",
        }
    }

    /// The style theme.toml names `name` (exact, lower case), None otherwise.
    pub fn from_name(name: &str) -> Option<BarStyle> {
        BarStyle::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// `opacity` in theme.toml (shapes spec § 1): the bar's alpha under `translucent`.
pub const OPACITY_DEFAULT: f32 = 0.8;
/// The lowest `opacity`; below, clamped with one warning.
pub const OPACITY_MIN: f32 = 0.3;
/// The highest `opacity`; above, clamped with one warning.
pub const OPACITY_MAX: f32 = 1.0;
// The same bounds in f64, the type TOML reads: clamping in f32 would turn `0.3` into a warning
// (0.3 in f32 is 0.30000001…, not the 0.3 the file holds).
const OPACITY_MIN_F64: f64 = 0.3;
const OPACITY_MAX_F64: f64 = 1.0;

/// Validated `[colors]` entries, in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overrides(Vec<(&'static str, Rgb)>);

impl Overrides {
    /// Every override onto `roles`, a role or a hue.
    pub(crate) fn apply(&self, roles: &mut Roles) {
        for (name, c) in &self.0 {
            if let Some(slot) = roles.slot(name) {
                *slot = *c;
            } else if let Some(slot) = roles.hues.slot(name) {
                *slot = *c;
            }
        }
    }
}

/// Not `Eq`: `opacity` is a float (always within `OPACITY_MIN..=OPACITY_MAX`, never NaN, once parsed).
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeSettings {
    pub source: ThemeSource,
    pub mode: ModeSetting,
    pub overrides: Overrides,
    pub module_colors: ModuleColors,
    pub style: BarStyle,
    pub opacity: f32,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        ThemeSettings {
            source: ThemeSource::Preset(DEFAULT_PRESET.name),
            mode: ModeSetting::Auto,
            overrides: Overrides::default(),
            module_colors: ModuleColors::default(),
            style: BarStyle::default(),
            opacity: OPACITY_DEFAULT,
        }
    }
}

/// "catppuccin, gruvbox, …, dracula": for the unknown-theme line.
fn known() -> String {
    PRESETS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
}

/// "flat", "islands", …: for the bad-style line.
fn style_names() -> String {
    BarStyle::ALL.iter().map(|s| format!("{:?}", s.name())).collect::<Vec<_>>().join(", ")
}

/// theme.toml's text. Warnings: one line each, never fatal; whatever is
/// valid is kept. No `theme` key → catppuccin (`load_settings` applies the
/// wallpaper rule on top).
///
/// Production goes through `load_settings`; this stays public (hidden) for the tests of the crates.
#[doc(hidden)]
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
            "mode" => match value.as_str().and_then(ModeSetting::from_name) {
                Some(mode) => s.mode = mode,
                None => w.push(format!("{THEME_FILE}: mode must be \"auto\", \"dark\" or \"light\", got {value}; using \"auto\"")),
            },
            "module_colors" => match value.as_str().and_then(ModuleColors::from_name) {
                Some(colors) => s.module_colors = colors,
                None => w.push(format!(
                    "{THEME_FILE}: module_colors must be \"none\", \"icons\", \"full\" or \"pills\", got {value}; using \"icons\""
                )),
            },
            "style" => match value.as_str().and_then(BarStyle::from_name) {
                Some(style) => s.style = style,
                None => w.push(format!("{THEME_FILE}: style must be one of {}, got {value}; using \"flat\"", style_names())),
            },
            "opacity" => match value.as_float().or_else(|| value.as_integer().map(|i| i as f64)) {
                Some(v) if !v.is_nan() => {
                    let clamped = v.clamp(OPACITY_MIN_F64, OPACITY_MAX_F64);
                    if clamped != v {
                        w.push(format!("{THEME_FILE}: opacity {v} is outside 0.3..=1.0, using {clamped}"));
                    }
                    s.opacity = clamped as f32;
                }
                _ => w.push(format!("{THEME_FILE}: opacity must be a number from 0.3 to 1.0, got {value}; using {OPACITY_DEFAULT}")),
            },
            "colors" => match value.as_table() {
                Some(colors) => {
                    for (role, v) in colors {
                        match ROLE_NAMES.iter().chain(&HUE_NAMES).find(|n| **n == role.as_str()) {
                            None => w.push(format!("{THEME_FILE}: [colors] {role:?} is not a role or a hue, ignored")),
                            Some(name) => match v.as_str().and_then(parse_hex) {
                                Some(c) => s.overrides.0.push((*name, c)),
                                None => {
                                    w.push(format!("{THEME_FILE}: [colors] {role:?}: invalid colour {v} (expected \"#rrggbb\"), ignored"));
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

/// The most a theme or colour file may hold: far above any real one, so a `/dev/zero` named as a
/// theme file cannot grow the daemon.
pub const MAX_FILE: u64 = 64 * 1024;

/// A regular file's text, at most `MAX_FILE` bytes (symlinks followed). A FIFO, a device or a
/// directory is an error (`InvalidInput`) without being opened, so nothing blocks.
pub fn read_capped(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut text = String::new();
    std::fs::File::open(path)?.take(MAX_FILE).read_to_string(&mut text)?;
    Ok(text)
}

/// `dir/theme.toml`; absent → Default with no warning; unreadable → Default
/// with one. Without a `theme` key, `default_source` decides.
pub fn load_settings(dir: &Path) -> (ThemeSettings, Vec<String>) {
    let source = default_source(dir);
    match read_capped(&dir.join(THEME_FILE)) {
        Ok(text) => parse_settings_or(&text, source),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (ThemeSettings { source, ..ThemeSettings::default() }, Vec::new()),
        Err(e) => (ThemeSettings { source, ..ThemeSettings::default() }, vec![format!("{THEME_FILE} unreadable: {e}")]),
    }
}

/// The resolver's lines: `warnings` are errors (something is wrong), `notes` informational
/// (a documented fallback that is no fault: the TUI does not repeat them on every launch).
#[derive(Default)]
struct Lines {
    warnings: Vec<String>,
    notes: Vec<String>,
}

/// `p`'s half for `mode`; a missing light half → its dark one, one note.
fn half(p: &Preset, mode: Mode, w: &mut Lines) -> Roles {
    match (mode, p.light) {
        (Mode::Dark, _) => p.dark,
        (Mode::Light, Some(light)) => light,
        (Mode::Light, None) => {
            w.notes.push(format!("theme {:?} has no light variant: using its dark one", p.name));
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
    match read_capped(&dir.join(file)) {
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
fn wallpaper(mode: Mode, dir: &Path, w: &mut Lines, bad: &mut bool) -> Roles {
    let fallback = DEFAULT_PRESET.name;
    if mode == Mode::Light {
        match read_colors(dir, LIGHT_FILE, Mode::Light) {
            ColorFile::Found(roles) => return roles,
            ColorFile::Absent => w.notes.push(format!("{LIGHT_FILE} not found: light mode uses {DARK_FILE}")),
            ColorFile::Bad(line) => {
                *bad = true;
                w.warnings.push(format!("{line}, using {fallback:?}"));
                return half(DEFAULT_PRESET, mode, w);
            }
        }
    }
    match read_colors(dir, DARK_FILE, Mode::Dark) {
        ColorFile::Found(roles) => roles,
        ColorFile::Absent => {
            w.warnings.push(format!("theme \"wallpaper\": {DARK_FILE} not found, using {fallback:?}"));
            half(DEFAULT_PRESET, mode, w)
        }
        ColorFile::Bad(line) => {
            *bad = true;
            w.warnings.push(format!("{line}, using {fallback:?}"));
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
    /// Something is wrong (an unusable file, an unknown theme, a settings line).
    pub warnings: Vec<String>,
    /// Informational: a documented fallback, no fault (no light file, no light variant).
    pub notes: Vec<String>,
    pub bad_file: bool,
}

impl Resolved {
    /// Every line, errors first: what the daemon logs.
    pub fn lines(&self) -> Vec<String> {
        self.warnings.iter().chain(&self.notes).cloned().collect()
    }
}

/// The roles for `settings` in `mode`. Reads `dir/colors.toml` /
/// `dir/colors-light.toml` for Wallpaper only; falls back per spec § 6;
/// overrides apply last. Each failure is one warning line.
pub fn resolve(settings: &ThemeSettings, mode: Mode, dir: &Path) -> (Roles, Vec<String>) {
    let r = resolve_detailed(settings, mode, dir);
    let lines = r.lines();
    (r.roles, lines)
}

/// `resolve`, with `bad_file` and the lines split into errors and notes.
pub fn resolve_detailed(settings: &ThemeSettings, mode: Mode, dir: &Path) -> Resolved {
    let mut w = Lines::default();
    let mut bad_file = false;
    let mut roles = match &settings.source {
        ThemeSource::Wallpaper => wallpaper(mode, dir, &mut w, &mut bad_file),
        ThemeSource::Preset(name) => match presets::preset(name) {
            Some(p) => half(p, mode, &mut w),
            None => {
                w.warnings.push(format!("unknown theme {name:?}, using {:?}", DEFAULT_PRESET.name));
                half(DEFAULT_PRESET, mode, &mut w)
            }
        },
    };
    settings.overrides.apply(&mut roles);
    Resolved { roles, warnings: w.warnings, notes: w.notes, bad_file }
}

/// The one way to the theme in effect, at startup and on every reload: theme.toml (`load_settings`,
/// whose lines come first among the warnings), the mode (`portal`: the portal's word, asked in mode auto only),
/// the colour files.
pub fn resolve_dir(dir: &Path, portal: impl FnOnce() -> Option<Mode>) -> (ThemeSettings, Resolved) {
    let (settings, mut warnings) = load_settings(dir);
    // `portal` is asked only in mode auto (the TUI reads a file there).
    let portal = if settings.mode == ModeSetting::Auto { portal() } else { None };
    let mut resolved = resolve_detailed(&settings, effective_mode(settings.mode, portal), dir);
    warnings.append(&mut resolved.warnings);
    resolved.warnings = warnings;
    (settings, resolved)
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
mod tests;
