//! TUI theme: the shared resolver (alpheus-commun::theme::settings) turned into ratatui colours. The
//! TUI is short-lived: it resolves once at start (theme.toml, the mode, the colour files). In mode
//! "auto" the mode is the one the daemon publishes in $XDG_RUNTIME_DIR/alpheus/mode: the TUI never
//! uses D-Bus (Mae, 2026-10-07). The file outlives the daemon, so a TUI started after the daemon
//! stopped reads the last published mode: accepted, no cleanup. The rest of the TUI only ever sees `Theme`.

use alpheus_commun::theme::mode_file::read_mode;
use alpheus_commun::theme::settings::resolve_dir;
use alpheus_commun::theme::{Rgb, Roles};
use ratatui::style::{Color, Modifier, Style};
use std::path::Path;

/// The roles the TUI draws with (spec § 4); see the plan's mapping table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// `bar_bg`: the screen's background.
    pub bg: Color,
    pub fg: Color,
    /// `fg_dim`.
    pub muted: Color,
    /// `popup_bg`: status line, overlays, dialogs.
    pub surface: Color,
    pub accent: Color,
    /// A selected tab or row.
    pub selection: Color,
    /// Text on `accent_container`.
    pub on_accent: Color,
    /// A filled control: an active choice, a focused button.
    pub accent_container: Color,
    pub error: Color,
    pub ok: Color,
}

pub(crate) fn color(Rgb(r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}

impl From<Roles> for Theme {
    fn from(r: Roles) -> Self {
        Theme {
            bg: color(r.bar_bg),
            fg: color(r.fg),
            muted: color(r.fg_dim),
            surface: color(r.popup_bg),
            accent: color(r.accent),
            selection: color(r.selection),
            on_accent: color(r.on_accent),
            accent_container: color(r.accent_container),
            error: color(r.error),
            ok: color(r.ok),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Roles::default().into()
    }
}

impl Theme {
    /// `dir`'s theme.toml through the shared resolver (so colors.toml alone means the wallpaper),
    /// the mode (in mode auto, the daemon's mode file under `runtime`; None or no file: dark), then
    /// the colour files. Warnings: one line each, for the status bar; only errors, never the
    /// informational notes (a missing light file, a preset without a light variant), which the
    /// daemon logs once.
    pub fn load(dir: &Path, runtime: Option<&Path>) -> (Theme, Vec<String>) {
        let (_, resolved) = resolve_dir(dir, || Some(read_mode(runtime)));
        (resolved.roles.into(), resolved.warnings)
    }

    /// A selected tab, row or choice: normal text on `selection`.
    pub fn selected(&self) -> Style {
        Style::default().fg(self.fg).bg(self.selection).add_modifier(Modifier::BOLD)
    }

    /// A filled control (an active choice, a focused button): `on_accent` on `accent_container`.
    pub fn filled(&self) -> Style {
        Style::default().fg(self.on_accent).bg(self.accent_container).add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpheus_commun::theme::mode_file::mode_path;
    use alpheus_commun::theme::presets::preset;
    use std::path::PathBuf;

    /// A fresh config directory holding `files`.
    fn dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-tui-theme-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (f, text) in files {
            std::fs::write(d.join(f), text).unwrap();
        }
        d
    }

    /// A fresh runtime directory (as $XDG_RUNTIME_DIR) whose mode file holds `text`; None: no file.
    fn runtime(name: &str, text: Option<&str>) -> PathBuf {
        let r = std::env::temp_dir().join(format!("alpheus-{}-tui-run-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&r);
        std::fs::create_dir_all(&r).unwrap();
        if let Some(t) = text {
            let p = mode_path(&r);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        }
        r
    }

    fn rgb(c: Rgb) -> Color {
        Color::Rgb(c.0, c.1, c.2)
    }

    const LEGACY: &str = "bg = \"#101010\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n";

    #[test]
    fn every_field_takes_its_role() {
        let r = Roles::default();
        let t = Theme::from(r);
        assert_eq!(
            [t.bg, t.fg, t.muted, t.surface, t.accent, t.selection, t.on_accent, t.accent_container, t.error, t.ok],
            [r.bar_bg, r.fg, r.fg_dim, r.popup_bg, r.accent, r.selection, r.on_accent, r.accent_container, r.error, r.ok].map(rgb)
        );
        let other = Roles { selection: Rgb(1, 2, 3), ..r };
        assert_eq!(Theme::from(other).selection, Color::Rgb(1, 2, 3));
        assert_eq!(Theme::default(), Theme::from(Roles::default()));
    }

    #[test]
    fn no_file_is_catppuccin_dark() {
        let dark = Theme::from(preset("catppuccin").unwrap().dark);
        assert_eq!(Theme::load(&dir("none", &[]), None), (dark, vec![]), "XDG_RUNTIME_DIR unset: dark");
        let (t, w) = Theme::load(&dir("none-run", &[]), Some(&runtime("none", None)));
        assert_eq!((t, w), (dark, vec![]), "no mode file (the daemon is not running): dark");
    }

    #[test]
    fn auto_follows_the_daemons_mode_file() {
        let light = Theme::from(preset("catppuccin").unwrap().light.unwrap());
        let dark = Theme::from(preset("catppuccin").unwrap().dark);
        assert_eq!(Theme::load(&dir("auto-light", &[]), Some(&runtime("light", Some("light\n")))).0, light);
        assert_eq!(Theme::load(&dir("auto-dark", &[]), Some(&runtime("dark", Some("dark\n")))).0, dark);
        assert_eq!(Theme::load(&dir("auto-other", &[]), Some(&runtime("other", Some("bright")))).0, dark, "anything else: dark");
    }

    #[test]
    fn a_fixed_mode_ignores_the_mode_file() {
        let (t, w) = Theme::load(&dir("fixed-dark", &[("theme.toml", "mode = \"dark\"\n")]), Some(&runtime("fixed-l", Some("light\n"))));
        assert_eq!((t, w), (Theme::from(preset("catppuccin").unwrap().dark), vec![]));
        let (t, _) = Theme::load(&dir("fixed-light", &[("theme.toml", "mode = \"light\"\n")]), Some(&runtime("fixed-d", Some("dark\n"))));
        assert_eq!(t, Theme::from(preset("catppuccin").unwrap().light.unwrap()));
    }

    #[test]
    fn a_preset_by_name() {
        let (t, w) = Theme::load(&dir("tokyo", &[("theme.toml", "theme = \"tokyo-night\"\nmode = \"dark\"\n")]), None);
        assert_eq!((t, w), (Theme::from(preset("tokyo-night").unwrap().dark), vec![]));
    }

    #[test]
    fn a_legacy_colors_toml_in_wallpaper_mode_looks_as_before() {
        let d = dir("legacy", &[("theme.toml", "theme = \"wallpaper\"\nmode = \"dark\"\n"), ("colors.toml", LEGACY)]);
        let (t, w) = Theme::load(&d, None);
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(
            (t.bg, t.fg, t.muted, t.surface, t.accent),
            (
                Color::Rgb(0x10, 0x10, 0x10),
                Color::Rgb(0xee, 0xee, 0xee),
                Color::Rgb(0x77, 0x77, 0x77),
                Color::Rgb(0x20, 0x20, 0x20),
                Color::Rgb(0xff, 0x88, 0x00)
            ),
            "bg, surface and muted as the old 7 keys gave them"
        );
    }

    /// Controller reconciliation 1–2: with colors.toml alone and no theme.toml, the TUI resolves the
    /// wallpaper (load_settings' default-source rule), as before themes existed.
    #[test]
    fn colors_toml_alone_is_the_wallpaper() {
        let (t, w) = Theme::load(&dir("colors-alone", &[("colors.toml", LEGACY)]), None);
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(
            (t.bg, t.surface, t.accent),
            (Color::Rgb(0x10, 0x10, 0x10), Color::Rgb(0x20, 0x20, 0x20), Color::Rgb(0xff, 0x88, 0x00)),
            "the file's colours, not catppuccin"
        );
    }

    #[test]
    fn an_unknown_theme_warns_and_falls_back() {
        let (t, w) = Theme::load(&dir("unknown", &[("theme.toml", "theme = \"nosuch\"\nmode = \"dark\"\n")]), None);
        assert_eq!(t, Theme::from(preset("catppuccin").unwrap().dark));
        assert!(!w.is_empty(), "one line for the status bar");
    }

    #[test]
    fn an_unreadable_theme_toml_warns_and_falls_back() {
        let d = dir("unreadable", &[]);
        std::fs::create_dir_all(d.join("theme.toml")).unwrap(); // a directory: unreadable as text
        let (t, w) = Theme::load(&d, None);
        assert_eq!(t, Theme::default());
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("theme.toml unreadable:"), "{w:?}");
    }

    #[test]
    fn selection_and_filled_styles() {
        let t = Theme::default();
        assert_eq!(t.selected(), Style::default().fg(t.fg).bg(t.selection).add_modifier(Modifier::BOLD));
        assert_eq!(t.filled(), Style::default().fg(t.on_accent).bg(t.accent_container).add_modifier(Modifier::BOLD));
    }

    /// The status bar shows faults only: the informational lines (no light file, a preset with no
    /// light variant) are the daemon's to log, once, not the TUI's on every Mod+A.
    #[test]
    fn informational_lines_stay_out_of_the_status_bar() {
        let light = runtime("info-light", Some("light\n"));
        let d = dir("info-wall", &[("theme.toml", "theme = \"wallpaper\"\n"), ("colors.toml", LEGACY)]);
        let (_, w) = Theme::load(&d, Some(&light));
        assert_eq!(w, Vec::<String>::new(), "no colors-light.toml: a note, not a fault");
        let d = dir("info-nord", &[("theme.toml", "theme = \"nord\"\n")]);
        let (_, w) = Theme::load(&d, Some(&light));
        assert_eq!(w, Vec::<String>::new(), "nord has no light variant: a note");
        let d = dir("info-bad", &[("theme.toml", "theme = \"nosuch\"\n")]);
        assert_eq!(Theme::load(&d, Some(&light)).1.len(), 1, "a real fault still shows");
    }
}
