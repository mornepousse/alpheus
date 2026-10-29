use super::*;
use crate::theme::HUE_NAMES;
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

/// A 16-role colour file holding `roles`, hues included.
fn file_of(mut roles: Roles) -> String {
    let mut text: String = ROLE_NAMES
        .iter()
        .map(|n| {
            let Rgb(r, g, b) = *roles.slot(n).unwrap();
            format!("{n} = \"#{r:02x}{g:02x}{b:02x}\"\n")
        })
        .collect();
    for n in HUE_NAMES {
        let Rgb(r, g, b) = *roles.hues.slot(n).unwrap();
        text.push_str(&format!("{n} = \"#{r:02x}{g:02x}{b:02x}\"\n"));
    }
    text
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
        ThemeSettings {
            source: ThemeSource::Preset("catppuccin"),
            mode: ModeSetting::Auto,
            overrides: Overrides::default(),
            module_colors: ModuleColors::Icons,
            style: BarStyle::Flat,
            opacity: 0.8,
        }
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
fn module_colors_is_read_in_its_four_values_and_defaults_to_icons() {
    assert_eq!(ModuleColors::default(), ModuleColors::Icons);
    assert_eq!(parse_settings("").0.module_colors, ModuleColors::Icons);
    for (text, want) in
        [("none", ModuleColors::None), ("icons", ModuleColors::Icons), ("full", ModuleColors::Full), ("pills", ModuleColors::Pills)]
    {
        let (s, w) = parse_settings(&format!("module_colors = \"{text}\"\n"));
        assert_eq!((s.module_colors, w), (want, NONE), "{text}");
    }
}

#[test]
fn a_bad_module_colors_warns_once_and_falls_back_to_icons() {
    for text in [
        "module_colors = \"Pills\"\n",
        "module_colors = \"pill\"\n",
        "module_colors = \"Full\"\n",
        "module_colors = 3\n",
        "module_colors = true\n",
    ] {
        let (s, w) = parse_settings(text);
        assert_eq!(s.module_colors, ModuleColors::Icons, "{text}");
        assert_eq!(w.len(), 1, "{text}: {w:?}");
        assert!(w[0].contains("module_colors") && w[0].contains("\"none\"") && w[0].contains("\"pills\""), "{w:?}");
    }
    // The valid keys around it are kept.
    let (s, w) = parse_settings("theme = \"nord\"\nmodule_colors = \"pill\"\nmode = \"dark\"\n");
    assert_eq!((s.source, s.mode, s.module_colors, w.len()), (ThemeSource::Preset("nord"), ModeSetting::Dark, ModuleColors::Icons, 1));
}

#[test]
fn module_colors_is_carried_through_load_settings_and_resolve_dir() {
    let dir = TempDir::new("theme-module-colors");
    assert_eq!(load_settings(&dir).0.module_colors, ModuleColors::Icons);
    dir.write(THEME_FILE, "module_colors = \"full\"\n");
    let (s, w) = load_settings(&dir);
    assert_eq!((s.module_colors, w), (ModuleColors::Full, NONE));
    let (s, _) = resolve_dir(&dir, || None);
    assert_eq!(s.module_colors, ModuleColors::Full);
    dir.write(THEME_FILE, "module_colors = \"none\"\nmode = \"light\"\n");
    assert_eq!(resolve_dir(&dir, || None).0.module_colors, ModuleColors::None);
}

// ---- style and opacity (shapes spec § 1) ----

#[test]
fn every_style_is_read_and_flat_is_the_default() {
    assert_eq!((BarStyle::default(), parse_settings("").0.style), (BarStyle::Flat, BarStyle::Flat));
    let names: Vec<&str> = BarStyle::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["flat", "islands", "powerline", "floating", "underline", "translucent"]);
    for style in BarStyle::ALL {
        let (s, w) = parse_settings(&format!("style = \"{}\"\n", style.name()));
        assert_eq!((s.style, w), (style, NONE), "{style:?}");
        assert_eq!(BarStyle::from_name(style.name()), Some(style));
    }
}

#[test]
fn an_unknown_style_warns_once_and_falls_back_to_flat() {
    for text in ["style = \"pills\"\n", "style = \"Islands\"\n", "style = \"\"\n", "style = 2\n", "style = [\"flat\"]\n"] {
        let (s, w) = parse_settings(text);
        assert_eq!(s.style, BarStyle::Flat, "{text}");
        assert_eq!(w.len(), 1, "{text}: {w:?}");
        assert!(w[0].contains("style") && w[0].contains("\"translucent\"") && w[0].contains("using \"flat\""), "{w:?}");
    }
    // The valid keys around it are kept.
    let (s, w) = parse_settings("theme = \"nord\"\nstyle = \"round\"\nmodule_colors = \"full\"\n");
    assert_eq!((s.source, s.style, s.module_colors, w.len()), (ThemeSource::Preset("nord"), BarStyle::Flat, ModuleColors::Full, 1));
}

#[test]
fn opacity_is_read_and_defaults_to_0_8() {
    assert_eq!(parse_settings("").0.opacity, 0.8);
    for (text, want) in [("0.3", 0.3), ("0.55", 0.55), ("1.0", 1.0), ("1", 1.0)] {
        let (s, w) = parse_settings(&format!("opacity = {text}\n"));
        assert_eq!((s.opacity, w), (want, NONE), "{text}");
    }
}

#[test]
fn an_opacity_out_of_range_is_clamped_with_one_warning() {
    for (text, want) in [("0.1", 0.3), ("0", 0.3), ("-2", 0.3), ("1.5", 1.0), ("7", 1.0), ("inf", 1.0), ("-inf", 0.3)] {
        let (s, w) = parse_settings(&format!("opacity = {text}\n"));
        assert_eq!(s.opacity, want, "{text}");
        assert_eq!(w.len(), 1, "{text}: {w:?}");
        assert!(w[0].contains("opacity") && w[0].contains("0.3..=1.0"), "{w:?}");
    }
}

#[test]
fn a_bad_opacity_warns_once_and_keeps_0_8() {
    for text in ["opacity = \"0.5\"\n", "opacity = nan\n", "opacity = true\n", "opacity = [0.5]\n"] {
        let (s, w) = parse_settings(text);
        assert_eq!(s.opacity, 0.8, "{text}");
        assert_eq!(w.len(), 1, "{text}: {w:?}");
        assert!(w[0].contains("opacity must be a number"), "{w:?}");
    }
}

#[test]
fn style_and_opacity_are_carried_through_load_settings_and_resolve_dir() {
    let dir = TempDir::new("theme-style");
    let (s, w) = load_settings(&dir);
    assert_eq!((s.style, s.opacity, w), (BarStyle::Flat, 0.8, NONE));
    dir.write(THEME_FILE, "style = \"translucent\"\nopacity = 0.6\n");
    let (s, w) = load_settings(&dir);
    assert_eq!((s.style, s.opacity, w), (BarStyle::Translucent, 0.6, NONE));
    let (s, r) = resolve_dir(&dir, || None);
    assert_eq!((s.style, s.opacity), (BarStyle::Translucent, 0.6));
    assert_eq!(r.warnings, NONE);
    dir.write(THEME_FILE, "style = \"islands\"\nopacity = 0.1\n");
    let (s, r) = resolve_dir(&dir, || None);
    assert_eq!((s.style, s.opacity), (BarStyle::Islands, 0.3));
    assert!(r.warnings.len() == 1 && r.warnings[0].contains("opacity"), "the clamp's line reaches the daemon's log: {r:?}");
}

#[test]
fn a_hue_can_be_overridden_in_colors() {
    let (s, w) = parse_settings("[colors]\nblue = \"#010203\"\npurple = \"#040506\"\nmagenta = \"#000000\"\naccent = \"#070809\"\n");
    assert_eq!(s.overrides, Overrides(vec![("accent", Rgb(7, 8, 9)), ("blue", Rgb(1, 2, 3)), ("purple", Rgb(4, 5, 6))]));
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("\"magenta\""), "{w:?}");
    let (r, w) = resolve(&s, Mode::Dark, Path::new("/nonexistent-alpheus-theme-dir"));
    assert_eq!(w, NONE);
    let base = named("catppuccin").dark;
    assert_eq!((r.hues.blue, r.hues.purple, r.accent), (Rgb(1, 2, 3), Rgb(4, 5, 6), Rgb(7, 8, 9)));
    assert_eq!((r.hues.red, r.hues.green), (base.hues.red, base.hues.green));
    // An invalid hue is ignored like an invalid role.
    let (s, w) = parse_settings("[colors]\ncyan = \"teal\"\n");
    assert_eq!((s.overrides, w.len()), (Overrides::default(), 1));
    assert!(w[0].contains("\"cyan\""), "{w:?}");
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
    let (s, w) = parse_settings("shape = \"islands\"\ntheme = \"dracula\"\n");
    assert_eq!((s.source, w.len()), (ThemeSource::Preset("dracula"), 1));
    assert!(w[0].contains("\"shape\""), "{w:?}");
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

/// A FIFO named as a theme or colour file is read as nothing, at once: it must not block the
/// daemon's loop (a writer never comes), and it is "unreadable", not a crash.
#[test]
fn a_fifo_is_unreadable_and_never_blocks() {
    let dir = TempDir::new("theme-fifo");
    for file in [THEME_FILE, DARK_FILE] {
        let c = std::ffi::CString::new(dir.join(file).to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path in a directory this test owns.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }
    let d = dir.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (s, w) = load_settings(&d);
        let r = resolve_detailed(&wallpaper(), Mode::Dark, &d);
        let _ = tx.send((s, w, r));
    });
    let (s, w, r) = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("a FIFO blocked the read");
    assert_eq!(s.mode, ModeSetting::Auto);
    assert!(w.len() == 1 && w[0].starts_with("theme.toml unreadable:"), "{w:?}");
    assert!(r.bad_file && r.warnings.iter().any(|l| l.starts_with("colors.toml unreadable:")), "{r:?}");
    assert_eq!(r.roles, named("catppuccin").dark, "the existing fallback");
}

/// Only the first 64 KB of a file are read: `/dev/zero` as a symlink target would otherwise grow
/// the daemon without bound. The cut text is then (almost surely) not valid TOML: one warning.
#[test]
fn an_oversized_file_is_cut_at_the_cap() {
    let dir = TempDir::new("theme-big");
    let big = format!("# {}\ntheme = \"nord\"\n", "x".repeat(MAX_FILE as usize));
    std::fs::write(dir.join(THEME_FILE), &big).unwrap();
    let text = read_capped(&dir.join(THEME_FILE)).unwrap();
    assert_eq!(text.len() as u64, MAX_FILE, "exactly the cap, not the whole file");
    let (s, w) = load_settings(&dir);
    assert_eq!(s.source, ThemeSource::Preset("catppuccin"), "the `theme` line is past the cap");
    assert!(w.is_empty(), "a comment cut short is still valid TOML: {w:?}");
    std::os::unix::fs::symlink("/dev/zero", dir.join(DARK_FILE)).unwrap();
    let r = resolve_detailed(&wallpaper(), Mode::Dark, &dir);
    assert!(r.bad_file && !r.warnings.is_empty(), "/dev/zero: not a regular file, no growth: {r:?}");
}

/// The informational lines (no light file, no light variant) are `notes`; faults are `warnings`.
#[test]
fn informational_lines_are_notes_and_faults_are_warnings() {
    let dir = TempDir::new("theme-notes");
    std::fs::write(dir.join(DARK_FILE), LEGACY_LIGHT).unwrap();
    let r = resolve_detailed(&wallpaper(), Mode::Light, &dir);
    assert_eq!((r.warnings.len(), r.notes.len()), (0, 1), "{r:?}");
    assert!(r.notes[0].contains("colors-light.toml not found"), "{r:?}");
    let nord = ThemeSettings { source: ThemeSource::Preset("nord"), ..ThemeSettings::default() };
    let r = resolve_detailed(&nord, Mode::Light, &dir);
    assert_eq!((r.warnings.len(), r.notes.len()), (0, 1), "{r:?}");
    assert!(r.notes[0].contains("no light variant"), "{r:?}");
    let unknown = ThemeSettings { source: ThemeSource::Preset("nosuch"), ..ThemeSettings::default() };
    assert_eq!(resolve_detailed(&unknown, Mode::Dark, &dir).warnings.len(), 1, "a fault stays a warning");
    assert_eq!(r.lines(), r.notes, "the daemon logs both");
}

/// `resolve_dir` is load_settings + resolve_detailed: the settings' lines come first, the portal
/// is asked in mode auto only.
#[test]
fn resolve_dir_is_the_one_path() {
    let dir = TempDir::new("theme-resolve-dir");
    std::fs::write(dir.join(THEME_FILE), "theme = \"nord\"\nbogus = 1\n").unwrap();
    let (s, r) = resolve_dir(&dir, || Some(Mode::Light));
    assert_eq!(s.source, ThemeSource::Preset("nord"));
    assert!(r.warnings[0].contains("bogus"), "settings lines first: {r:?}");
    assert!(r.notes[0].contains("no light variant"), "auto asked the portal: light: {r:?}");
    std::fs::write(dir.join(THEME_FILE), "theme = \"nord\"\nmode = \"dark\"\n").unwrap();
    let (_, r) = resolve_dir(&dir, || panic!("a fixed mode never asks the portal"));
    assert_eq!(r.roles, named("nord").dark);
}
