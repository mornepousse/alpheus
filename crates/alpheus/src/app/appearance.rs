//! The Appearance tab: choose a theme, the light/dark mode, the module colours and the bar style,
//! preview them, write theme.toml.
//! Nothing is computed until the tab is shown (`appearance_open`). Moving the cursor only changes
//! the preview; Enter writes (`crate::appearance::write_settings`) and reloads the TUI's own theme.
//! The daemon follows through its inotify reload; the TUI never touches D-Bus.

use super::App;
use super::action::Action;
use crate::appearance::{Entry, WALLPAPER, entries, write_settings};
use crate::theme::Theme;
use alpheus_commun::theme::mode_file::read_mode;
use alpheus_commun::theme::settings::{
    BarStyle, ModeSetting, ModuleColors, Overrides, ThemeSettings, ThemeSource, effective_mode, load_settings, resolve,
};
use alpheus_commun::theme::{Mode, Roles};
use std::path::PathBuf;
use std::time::Instant;

/// The mode chips, in display order.
pub const MODES: [ModeSetting; 3] = [ModeSetting::Auto, ModeSetting::Dark, ModeSetting::Light];

pub fn mode_label(m: ModeSetting) -> &'static str {
    match m {
        ModeSetting::Auto => "Auto",
        ModeSetting::Dark => "Dark",
        ModeSetting::Light => "Light",
    }
}

/// `theme.toml`'s value for `m`.
fn mode_value(m: ModeSetting) -> &'static str {
    match m {
        ModeSetting::Auto => "auto",
        ModeSetting::Dark => "dark",
        ModeSetting::Light => "light",
    }
}

/// The module-colours chips, in display order.
pub const COLORS: [ModuleColors; 4] = [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills];

pub fn colors_label(m: ModuleColors) -> &'static str {
    match m {
        ModuleColors::None => "None",
        ModuleColors::Icons => "Icons",
        ModuleColors::Full => "Full",
        ModuleColors::Pills => "Pills",
    }
}

/// `theme.toml`'s value for `m`.
fn colors_value(m: ModuleColors) -> &'static str {
    match m {
        ModuleColors::None => "none",
        ModuleColors::Icons => "icons",
        ModuleColors::Full => "full",
        ModuleColors::Pills => "pills",
    }
}

/// The bar styles, in display order (shapes spec § 4). theme.toml's value for a style is
/// `BarStyle::name()`: no second table to keep in step.
pub const STYLES: [BarStyle; 6] = BarStyle::ALL;

pub fn style_label(s: BarStyle) -> &'static str {
    match s {
        BarStyle::Flat => "Flat",
        BarStyle::Islands => "Islands",
        BarStyle::Powerline => "Powerline",
        BarStyle::Floating => "Floating",
        BarStyle::Underline => "Underline",
        BarStyle::Translucent => "Translucent",
    }
}

pub struct AppearanceTab {
    /// The config directory (theme.toml, colors.toml); None: nothing can be written.
    pub dir: Option<PathBuf>,
    /// $XDG_RUNTIME_DIR, where the daemon publishes its mode.
    pub runtime: Option<PathBuf>,
    /// Empty until the tab is first shown.
    pub entries: Vec<Entry>,
    /// The row under the cursor.
    pub cursor: usize,
    /// The theme in effect (the "●"), None when it is not in the list.
    pub applied: Option<usize>,
    /// The mode chosen (previewed; written by Enter).
    pub mode: ModeSetting,
    /// The mode in effect, as theme.toml says.
    pub applied_mode: ModeSetting,
    /// Keyboard focus is on the mode row.
    pub mode_focus: bool,
    /// The module colours chosen (previewed; written by Enter).
    pub module_colors: ModuleColors,
    /// The module colours in effect, as theme.toml says.
    pub applied_module_colors: ModuleColors,
    /// Keyboard focus is on the module-colours row (never together with `mode_focus`).
    pub colors_focus: bool,
    /// The bar style chosen (previewed; written by Enter).
    pub style: BarStyle,
    /// The bar style in effect, as theme.toml says.
    pub applied_style: BarStyle,
    /// Keyboard focus is on the style row (never together with `mode_focus` or `colors_focus`).
    pub style_focus: bool,
    /// The roles under the cursor, for the chosen mode.
    pub preview: Option<Roles>,
    /// The user's `[colors]` overrides: the preview shows what Enter will produce.
    overrides: Overrides,
    /// A note on the preview (the wallpaper's colour file missing or invalid).
    pub preview_note: Option<String>,
    /// The mode the daemon publishes, read when the tab opens and after each write.
    auto_mode: Mode,
    /// The wallpaper's roles for one mode, read once per mode.
    wallpaper: Option<(Mode, Roles, Option<String>)>,
}

impl Default for AppearanceTab {
    fn default() -> Self {
        AppearanceTab {
            dir: None,
            runtime: None,
            entries: Vec::new(),
            cursor: 0,
            applied: None,
            mode: ModeSetting::Auto,
            applied_mode: ModeSetting::Auto,
            mode_focus: false,
            module_colors: ModuleColors::default(),
            applied_module_colors: ModuleColors::default(),
            colors_focus: false,
            style: BarStyle::default(),
            applied_style: BarStyle::default(),
            style_focus: false,
            preview: None,
            preview_note: None,
            overrides: Overrides::default(),
            auto_mode: Mode::Dark,
            wallpaper: None,
        }
    }
}

impl AppearanceTab {
    /// The mode the preview is drawn in.
    pub fn effective_mode(&self) -> Mode {
        effective_mode(self.mode, Some(self.auto_mode))
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Re-resolves the preview for the cursor and the chosen mode. Presets read nothing; the
    /// wallpaper's files are read once per mode.
    fn refresh_preview(&mut self) {
        let Some(entry) = self.entries.get(self.cursor).copied() else { return };
        let mode = self.effective_mode();
        let dir = self.dir.clone().unwrap_or_default();
        let settings = |source| ThemeSettings {
            source,
            mode: self.mode,
            overrides: self.overrides.clone(),
            module_colors: self.module_colors,
            ..ThemeSettings::default()
        };
        if entry.key == WALLPAPER {
            if self.wallpaper.as_ref().is_none_or(|(m, ..)| *m != mode) {
                let (roles, warnings) = resolve(&settings(ThemeSource::Wallpaper), mode, &dir);
                self.wallpaper = Some((mode, roles, warnings.into_iter().next()));
            }
            if let Some((_, roles, note)) = &self.wallpaper {
                self.preview = Some(*roles);
                self.preview_note = note.clone();
            }
        } else {
            let (roles, _) = resolve(&settings(ThemeSource::Preset(entry.key)), mode, &dir);
            self.preview = Some(roles);
            self.preview_note = None;
        }
    }
}

impl App {
    /// The tab is being shown: reads theme.toml (what is in effect), the daemon's mode and the
    /// preview. Everything the tab computes starts here.
    pub(crate) fn appearance_open(&mut self) {
        let a = &mut self.appearance;
        a.entries = entries();
        let (settings, _) = match &a.dir {
            Some(dir) => load_settings(dir),
            None => (ThemeSettings::default(), Vec::new()),
        };
        let key = match &settings.source {
            ThemeSource::Wallpaper => WALLPAPER,
            ThemeSource::Preset(name) => name,
        };
        a.overrides = settings.overrides.clone();
        a.applied = a.entries.iter().position(|e| e.key == key);
        a.cursor = a.applied.unwrap_or(0);
        a.mode = settings.mode;
        a.applied_mode = settings.mode;
        a.mode_focus = false;
        a.module_colors = settings.module_colors;
        a.applied_module_colors = settings.module_colors;
        a.colors_focus = false;
        a.style = settings.style;
        a.applied_style = settings.style;
        a.style_focus = false;
        a.auto_mode = read_mode(a.runtime.as_deref());
        a.wallpaper = None;
        a.refresh_preview();
    }

    /// A key of the Appearance tab. false: not ours, the generic handling goes on (Esc, 1–7, help…).
    pub(crate) fn appearance_action(&mut self, a: Action, now: Instant) -> bool {
        let last = self.appearance.entries.len().saturating_sub(1);
        let ap = &mut self.appearance;
        // Focus: 0 the list, 1 the mode row, 2 the module-colours row, 3 the style row. Tab cycles,
        // Up and Down walk.
        let focus = if ap.mode_focus {
            1
        } else if ap.colors_focus {
            2
        } else if ap.style_focus {
            3
        } else {
            0
        };
        let set_focus = |ap: &mut AppearanceTab, f: usize| {
            ap.mode_focus = f == 1;
            ap.colors_focus = f == 2;
            ap.style_focus = f == 3;
        };
        match a {
            Action::NextTab => set_focus(ap, (focus + 1) % 4),
            Action::PrevTab => set_focus(ap, (focus + 3) % 4),
            Action::Up if focus > 0 => set_focus(ap, focus - 1),
            Action::Down if focus > 0 => set_focus(ap, (focus + 1).min(3)),
            Action::PageUp | Action::PageDown | Action::Home | Action::End if focus > 0 => {}
            Action::Up => self.appearance_move(-1),
            Action::Down => self.appearance_move(1),
            Action::PageUp => self.appearance_move(-5),
            Action::PageDown => self.appearance_move(5),
            Action::Home => self.appearance_move(-(last as i32)),
            Action::End => self.appearance_move(last as i32),
            Action::Left | Action::Decrease if focus == 1 => self.appearance_step(-1),
            Action::Right | Action::Increase if focus == 1 => self.appearance_step(1),
            Action::Left | Action::Decrease if focus == 2 => self.appearance_step_colors(-1),
            Action::Right | Action::Increase if focus == 2 => self.appearance_step_colors(1),
            Action::Left | Action::Decrease if focus == 3 => self.appearance_step_style(-1),
            Action::Right | Action::Increase if focus == 3 => self.appearance_step_style(1),
            Action::Left | Action::Right | Action::Decrease | Action::Increase => {}
            Action::Activate => self.appearance_apply(now),
            _ => return false,
        }
        true
    }

    /// Moves the cursor: the preview only, nothing is written.
    fn appearance_move(&mut self, delta: i32) {
        let a = &mut self.appearance;
        a.cursor = (a.cursor as i32 + delta).clamp(0, a.entries.len().saturating_sub(1) as i32) as usize;
        a.refresh_preview();
    }

    fn appearance_step(&mut self, delta: i32) {
        let a = &mut self.appearance;
        let i = MODES.iter().position(|m| *m == a.mode).unwrap_or(0) as i32;
        a.mode = MODES[(i + delta).clamp(0, MODES.len() as i32 - 1) as usize];
        a.refresh_preview();
    }

    fn appearance_step_colors(&mut self, delta: i32) {
        let a = &mut self.appearance;
        let i = COLORS.iter().position(|m| *m == a.module_colors).unwrap_or(0) as i32;
        a.module_colors = COLORS[(i + delta).clamp(0, COLORS.len() as i32 - 1) as usize];
        a.refresh_preview();
    }

    /// The preview's roles do not depend on the style: nothing to refresh, the next draw shows it.
    fn appearance_step_style(&mut self, delta: i32) {
        let a = &mut self.appearance;
        let i = STYLES.iter().position(|s| *s == a.style).unwrap_or(0) as i32;
        a.style = STYLES[(i + delta).clamp(0, STYLES.len() as i32 - 1) as usize];
    }

    /// A click on a theme: selects it; a click on the row already under the cursor applies it.
    pub(crate) fn appearance_click_theme(&mut self, i: usize, now: Instant) {
        let a = &mut self.appearance;
        a.mode_focus = false;
        a.colors_focus = false;
        a.style_focus = false;
        if i >= a.entries.len() {
            return;
        }
        if a.cursor == i {
            self.appearance_apply(now);
        } else {
            a.cursor = i;
            a.refresh_preview();
        }
    }

    /// A click on a mode applies it at once.
    pub(crate) fn appearance_click_mode(&mut self, m: ModeSetting, now: Instant) {
        let a = &mut self.appearance;
        a.mode_focus = true;
        a.colors_focus = false;
        a.style_focus = false;
        a.mode = m;
        a.refresh_preview();
        self.appearance_apply(now);
    }

    /// A click on a module-colours value applies it at once.
    pub(crate) fn appearance_click_colors(&mut self, m: ModuleColors, now: Instant) {
        let a = &mut self.appearance;
        a.colors_focus = true;
        a.mode_focus = false;
        a.style_focus = false;
        a.module_colors = m;
        a.refresh_preview();
        self.appearance_apply(now);
    }

    /// A click on a style applies it at once.
    pub(crate) fn appearance_click_style(&mut self, s: BarStyle, now: Instant) {
        let a = &mut self.appearance;
        a.style_focus = true;
        a.mode_focus = false;
        a.colors_focus = false;
        a.style = s;
        self.appearance_apply(now);
    }

    /// Writes theme.toml: the theme when the list has the focus (or the cursor moved off the one in
    /// effect), the mode when the mode row has it (or the chosen mode differs from the one in
    /// effect), the module colours likewise, the style likewise. Then the TUI reloads its own theme;
    /// the daemon reloads from the file. A failed write changes nothing but the status line.
    pub(crate) fn appearance_apply(&mut self, now: Instant) {
        let a = &self.appearance;
        let Some(entry) = a.cursor_entry().copied() else { return };
        let on_list = !a.mode_focus && !a.colors_focus && !a.style_focus;
        let theme = (on_list || a.applied != Some(a.cursor)).then_some(entry.key);
        let mode = (a.mode_focus || a.mode != a.applied_mode).then(|| mode_value(a.mode));
        let colors = (a.colors_focus || a.module_colors != a.applied_module_colors).then(|| colors_value(a.module_colors));
        let style = (a.style_focus || a.style != a.applied_style).then(|| a.style.name());
        let written = match &a.dir {
            Some(dir) => write_settings(dir, theme, mode, colors, style).map_err(|e| e.to_string()),
            None => Err("no config directory".to_string()),
        };
        if let Err(e) = written {
            let msg = format!("Appearance: {e}");
            self.status.error = Some(msg.clone());
            self.toast(msg, true, now);
            return;
        }
        let a = &mut self.appearance;
        if theme.is_some() {
            a.applied = Some(a.cursor);
        }
        a.applied_mode = a.mode;
        a.applied_module_colors = a.module_colors;
        a.applied_style = a.style;
        a.auto_mode = read_mode(a.runtime.as_deref());
        a.refresh_preview();
        if let Some(dir) = a.dir.clone() {
            self.theme = Theme::load(&dir, self.appearance.runtime.as_deref()).0;
        }
        self.status.error = None;
        let mut msg = format!("Theme: {}, {}", entry.label, mode_label(self.appearance.mode).to_lowercase());
        if let Some(c) = colors {
            msg.push_str(&format!(", module colours {c}"));
        }
        if let Some(s) = style {
            msg.push_str(&format!(", style {s}"));
        }
        self.status.last = Some(msg.clone());
        self.toast(msg, false, now);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::action::Tab;
    use super::super::testing::*;
    use super::*;
    use alpheus_commun::theme::Rgb;
    use alpheus_commun::theme::presets::preset;
    use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
    use ratatui::style::Color;

    /// A fresh config directory holding `files`, and an App opened on the Appearance tab with it.
    pub fn open(name: &str, files: &[(&str, &str)]) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("alpheus-{}-app-appearance-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, text) in files {
            std::fs::write(dir.join(f), text).unwrap();
        }
        let mut a = ready_app();
        a.appearance.dir = Some(dir.clone());
        a.go_tab(Tab::Appearance, Instant::now());
        (a, dir)
    }

    fn theme_toml(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("theme.toml")).unwrap_or_default()
    }

    fn act(a: &mut App, action: Action) {
        a.on_key_action(action);
    }

    fn rgb(c: Rgb) -> Color {
        Color::Rgb(c.0, c.1, c.2)
    }

    #[test]
    fn nothing_is_computed_until_the_tab_is_shown() {
        let a = ready_app();
        assert!(a.appearance.entries.is_empty() && a.appearance.preview.is_none());
        let (a, _) = open("lazy", &[]);
        assert_eq!(a.appearance.entries.len(), 10);
        assert!(a.appearance.preview.is_some());
    }

    #[test]
    fn it_opens_on_the_theme_in_effect() {
        let (a, _) = open("effect", &[("theme.toml", "theme = \"tokyo-night\"\nmode = \"light\"\n")]);
        let ap = &a.appearance;
        assert_eq!((ap.cursor, ap.applied, ap.mode, ap.applied_mode), (3, Some(3), ModeSetting::Light, ModeSetting::Light));
        assert_eq!(ap.preview, Some(preset("tokyo-night").unwrap().light.unwrap()), "light variant for a fixed light mode");
        let (a, _) = open("default", &[]);
        assert_eq!((a.appearance.cursor, a.appearance.applied), (1, Some(1)), "no file: catppuccin");
        let (a, _) = open(
            "wall",
            &[(
                "colors.toml",
                "bg = \"#101010\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n",
            )],
        );
        assert_eq!((a.appearance.cursor, a.appearance.applied), (0, Some(0)), "colors.toml alone: the wallpaper, as the bar sees it");
        assert_eq!(a.appearance.preview.unwrap().bar_bg, Rgb(0x10, 0x10, 0x10));
    }

    #[test]
    fn auto_previews_the_mode_the_daemon_publishes() {
        let dir = std::env::temp_dir().join(format!("alpheus-{}-app-appearance-run", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("alpheus")).unwrap();
        std::fs::write(dir.join("alpheus/mode"), "light\n").unwrap();
        let mut a = ready_app();
        a.appearance.dir = Some(dir.join("cfg"));
        a.appearance.runtime = Some(dir.clone());
        a.go_tab(Tab::Appearance, Instant::now());
        assert_eq!(a.appearance.mode, ModeSetting::Auto);
        assert_eq!(a.appearance.preview, Some(preset("catppuccin").unwrap().light.unwrap()));
        act(&mut a, Action::Down);
        assert_eq!(a.appearance.preview, Some(preset("gruvbox").unwrap().light.unwrap()));
        act(&mut a, Action::NextTab);
        act(&mut a, Action::Right); // Dark: the published mode no longer matters
        assert_eq!(a.appearance.preview, Some(preset("gruvbox").unwrap().dark));
    }

    #[test]
    fn a_preset_without_a_light_variant_previews_its_dark_half() {
        let (mut a, _) = open("nord", &[("theme.toml", "mode = \"light\"\n")]);
        act(&mut a, Action::End);
        assert_eq!(a.appearance.cursor_entry().unwrap().label, "Dracula");
        assert_eq!(a.appearance.preview, Some(preset("dracula").unwrap().dark));
    }

    #[test]
    fn moving_the_cursor_changes_the_preview_and_writes_nothing() {
        let (mut a, dir) = open("move", &[]);
        let before = a.theme;
        for action in [Action::Down, Action::Down, Action::PageDown, Action::Up, Action::End, Action::Home] {
            act(&mut a, action);
        }
        act(&mut a, Action::Home);
        act(&mut a, Action::Down);
        assert_eq!(a.appearance.cursor, 1);
        assert_eq!(a.appearance.preview, Some(preset("catppuccin").unwrap().dark), "the preview follows the cursor");
        assert!(!dir.join("theme.toml").exists(), "no theme.toml written by moving");
        assert_eq!((a.theme, a.appearance.applied), (before, Some(1)), "the TUI's theme and the marker stay");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing at all in the config directory");
    }

    #[test]
    fn the_cursor_is_bounded_and_page_keys_jump() {
        let (mut a, _) = open("bounds", &[]);
        act(&mut a, Action::Home);
        act(&mut a, Action::Up);
        assert_eq!(a.appearance.cursor, 0);
        act(&mut a, Action::PageDown);
        assert_eq!(a.appearance.cursor, 5);
        act(&mut a, Action::End);
        act(&mut a, Action::Down);
        assert_eq!(a.appearance.cursor, 9);
    }

    #[test]
    fn enter_applies_the_theme_and_recolours_the_tui() {
        let (mut a, dir) = open("apply", &[("theme.toml", "# mine\nmode = \"dark\"\n")]);
        act(&mut a, Action::Down); // from catppuccin (1): gruvbox, then tokyo-night (3)
        act(&mut a, Action::Down);
        assert_eq!(a.appearance.cursor_entry().unwrap().key, "tokyo-night");
        assert_eq!(theme_toml(&dir), "# mine\nmode = \"dark\"\n", "still nothing written");
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "# mine\nmode = \"dark\"\ntheme = \"tokyo-night\"\n");
        assert_eq!(a.appearance.applied, Some(3), "the marker moves");
        assert_eq!(a.theme, Theme::from(preset("tokyo-night").unwrap().dark), "recoloured at once");
        assert_eq!(a.status.last.as_deref(), Some("Theme: Tokyo Night, dark"));
        assert_eq!(a.status.error, None);
    }

    #[test]
    fn the_mode_row_applies_the_mode_and_only_the_mode() {
        let (mut a, dir) = open("mode", &[("theme.toml", "theme = \"nord\"\n")]);
        act(&mut a, Action::NextTab);
        assert!(a.appearance.mode_focus, "Tab moves to the mode row");
        act(&mut a, Action::Right);
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.mode, ModeSetting::Light);
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.mode, ModeSetting::Light, "bounded");
        assert_eq!(theme_toml(&dir), "theme = \"nord\"\n", "choosing writes nothing");
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "theme = \"nord\"\nmode = \"light\"\n");
        assert_eq!(a.appearance.applied_mode, ModeSetting::Light);
        act(&mut a, Action::PrevTab);
        assert!(!a.appearance.mode_focus, "Tab again: back to the list");
    }

    #[test]
    fn tab_walks_the_list_the_mode_the_colours_the_style_rows_and_back() {
        let (mut a, _) = open("tab4", &[]);
        let at = |a: &App| (a.appearance.mode_focus, a.appearance.colors_focus, a.appearance.style_focus);
        assert_eq!(at(&a), (false, false, false));
        act(&mut a, Action::NextTab);
        assert_eq!(at(&a), (true, false, false));
        act(&mut a, Action::NextTab);
        assert_eq!(at(&a), (false, true, false));
        act(&mut a, Action::NextTab);
        assert_eq!(at(&a), (false, false, true));
        act(&mut a, Action::NextTab);
        assert_eq!(at(&a), (false, false, false), "around to the list");
        act(&mut a, Action::PrevTab);
        assert_eq!(at(&a), (false, false, true), "backwards: the style row");
        act(&mut a, Action::Up);
        assert_eq!(at(&a), (false, true, false), "Up walks back one row");
        act(&mut a, Action::Down);
        act(&mut a, Action::Down);
        assert_eq!(at(&a), (false, false, true), "Down stops on the last row");
        act(&mut a, Action::Up);
        act(&mut a, Action::Up);
        act(&mut a, Action::Up);
        assert_eq!(at(&a), (false, false, false));
    }

    #[test]
    fn the_style_row_chooses_with_the_arrows_and_applies_with_enter_writing_only_its_key() {
        let (mut a, dir) = open("style-keys", &[("theme.toml", "# mine\ntheme = \"nord\"\nmode = \"dark\"\nopacity = 0.6\n")]);
        assert_eq!((a.appearance.style, a.appearance.applied_style), (BarStyle::Flat, BarStyle::Flat), "the default");
        for _ in 0..3 {
            act(&mut a, Action::NextTab);
        }
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.style, BarStyle::Islands);
        for _ in 0..9 {
            act(&mut a, Action::Right);
        }
        assert_eq!(a.appearance.style, BarStyle::Translucent, "bounded");
        act(&mut a, Action::Left);
        act(&mut a, Action::Left);
        assert_eq!(a.appearance.style, BarStyle::Floating);
        assert_eq!(theme_toml(&dir), "# mine\ntheme = \"nord\"\nmode = \"dark\"\nopacity = 0.6\n", "choosing writes nothing");
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "# mine\ntheme = \"nord\"\nmode = \"dark\"\nopacity = 0.6\nstyle = \"floating\"\n");
        assert_eq!(a.appearance.applied_style, BarStyle::Floating);
        assert_eq!(a.status.last.as_deref(), Some("Theme: Nord, dark, style floating"));
        // A fresh read of the file gives it back.
        a.go_tab(Tab::Home, Instant::now());
        a.go_tab(Tab::Appearance, Instant::now());
        assert_eq!(a.appearance.style, BarStyle::Floating);
    }

    #[test]
    fn the_loaded_style_survives_a_theme_write_and_a_chosen_one_rides_along() {
        let (mut a, dir) = open("style-carried", &[("theme.toml", "theme = \"nord\"\nstyle = \"powerline\"\n")]);
        assert_eq!((a.appearance.style, a.appearance.applied_style), (BarStyle::Powerline, BarStyle::Powerline));
        act(&mut a, Action::Home); // Wallpaper
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "theme = \"wallpaper\"\nstyle = \"powerline\"\n", "a theme write leaves the style alone");
        a.appearance.style_focus = true;
        act(&mut a, Action::Right); // Powerline -> Floating
        a.appearance.style_focus = false;
        act(&mut a, Action::Activate); // Enter in the list: the chosen style is applied too
        assert_eq!(theme_toml(&dir), "theme = \"wallpaper\"\nstyle = \"floating\"\n");
    }

    #[test]
    fn the_colours_row_offers_pills() {
        let (mut a, dir) = open("pills", &[("theme.toml", "theme = \"nord\"\nmodule_colors = \"full\"\n")]);
        act(&mut a, Action::NextTab);
        act(&mut a, Action::NextTab);
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.module_colors, ModuleColors::Pills);
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "theme = \"nord\"\nmodule_colors = \"pills\"\n");
    }

    #[test]
    fn the_colours_row_chooses_with_the_arrows_and_applies_with_enter_writing_only_its_key() {
        let (mut a, dir) = open("colors-keys", &[("theme.toml", "# mine\ntheme = \"nord\"\nmode = \"dark\"\n")]);
        assert_eq!(
            (a.appearance.module_colors, a.appearance.applied_module_colors),
            (ModuleColors::Icons, ModuleColors::Icons),
            "the default"
        );
        act(&mut a, Action::NextTab);
        act(&mut a, Action::NextTab);
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.module_colors, ModuleColors::Full);
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.module_colors, ModuleColors::Pills, "Pills after Full");
        act(&mut a, Action::Right);
        assert_eq!(a.appearance.module_colors, ModuleColors::Pills, "bounded");
        act(&mut a, Action::Left);
        act(&mut a, Action::Left);
        act(&mut a, Action::Left);
        assert_eq!(a.appearance.module_colors, ModuleColors::None);
        assert_eq!(theme_toml(&dir), "# mine\ntheme = \"nord\"\nmode = \"dark\"\n", "choosing writes nothing");
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "# mine\ntheme = \"nord\"\nmode = \"dark\"\nmodule_colors = \"none\"\n");
        assert_eq!(a.appearance.applied_module_colors, ModuleColors::None);
        assert_eq!(a.status.last.as_deref(), Some("Theme: Nord, dark, module colours none"));
        // A fresh read of the file gives it back.
        a.go_tab(Tab::Home, Instant::now());
        a.go_tab(Tab::Appearance, Instant::now());
        assert_eq!(a.appearance.module_colors, ModuleColors::None);
    }

    #[test]
    fn the_loaded_module_colours_survive_a_theme_or_mode_write() {
        let (mut a, dir) = open("colors-carried", &[("theme.toml", "theme = \"nord\"\nmodule_colors = \"full\"\n")]);
        assert_eq!((a.appearance.module_colors, a.appearance.applied_module_colors), (ModuleColors::Full, ModuleColors::Full));
        act(&mut a, Action::Home); // Wallpaper
        act(&mut a, Action::Activate);
        assert!(theme_toml(&dir).contains("module_colors = \"full\""), "{}", theme_toml(&dir));
        act(&mut a, Action::NextTab);
        act(&mut a, Action::Right);
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "theme = \"wallpaper\"\nmodule_colors = \"full\"\nmode = \"dark\"\n");
        assert_eq!(a.appearance.module_colors, ModuleColors::Full, "still what the preview uses");
    }

    #[test]
    fn enter_in_the_list_also_applies_a_mode_that_was_chosen() {
        let (mut a, dir) = open("both", &[]);
        a.appearance.mode_focus = true;
        act(&mut a, Action::Right); // Auto -> Dark
        a.appearance.mode_focus = false;
        act(&mut a, Action::Activate);
        assert_eq!(theme_toml(&dir), "theme = \"catppuccin\"\nmode = \"dark\"\n");
    }

    #[test]
    fn a_failed_write_is_one_status_line_and_changes_nothing_else() {
        let (mut a, dir) = open("fail", &[]);
        std::fs::create_dir_all(dir.join("theme.toml")).unwrap(); // unreadable as a file
        let theme = a.theme;
        act(&mut a, Action::Down);
        act(&mut a, Action::Activate);
        assert!(a.status.error.as_deref().is_some_and(|e| e.starts_with("Appearance: ")), "{:?}", a.status.error);
        assert_eq!((a.theme, a.appearance.applied, a.appearance.applied_mode), (theme, Some(1), ModeSetting::Auto));
        assert_eq!(a.status.last, None);
    }

    fn legacy(bg: &str) -> String {
        format!(
            "bg = \"{bg}\"\nfg = \"#eeeeee\"\naccent = \"#ff8800\"\nmuted = \"#777777\"\nsurface = \"#202020\"\nerror = \"#ff0000\"\nok = \"#00ff00\"\n"
        )
    }

    #[test]
    fn the_wallpaper_is_read_once_per_mode_not_per_move() {
        let (mut a, dir) = open("once", &[("colors.toml", &legacy("#101010")), ("colors-light.toml", &legacy("#f0f0f0"))]);
        let bg = |a: &App| a.appearance.preview.unwrap().bar_bg;
        assert_eq!(bg(&a), Rgb(0x10, 0x10, 0x10));
        std::fs::write(dir.join("colors.toml"), legacy("#000001")).unwrap();
        act(&mut a, Action::Down);
        act(&mut a, Action::Up);
        assert_eq!(bg(&a), Rgb(0x10, 0x10, 0x10), "cached: the file is not read again on a move");
        act(&mut a, Action::NextTab);
        act(&mut a, Action::Right); // Auto -> Dark: the same mode in effect, still cached
        assert_eq!(bg(&a), Rgb(0x10, 0x10, 0x10));
        act(&mut a, Action::Right);
        assert_eq!(bg(&a), Rgb(0xf0, 0xf0, 0xf0), "Light: its own file");
        act(&mut a, Action::Left);
        assert_eq!(bg(&a), Rgb(0, 0, 1), "back to Dark: read again");
    }

    #[test]
    fn a_missing_wallpaper_file_is_said_on_the_preview() {
        let (mut a, _) = open("nowall", &[("theme.toml", "theme = \"wallpaper\"\nmode = \"dark\"\n")]);
        assert_eq!(a.appearance.cursor, 0);
        assert!(a.appearance.preview_note.is_some(), "{:?}", a.appearance.preview_note);
        act(&mut a, Action::Down);
        assert_eq!(a.appearance.preview_note, None);
    }

    #[test]
    fn other_keys_fall_through_to_the_generic_handling() {
        let (mut a, _) = open("fall", &[]);
        assert!(!a.appearance_action(Action::Back, Instant::now()));
        assert!(!a.appearance_action(Action::GoTab(Tab::Home), Instant::now()));
        assert!(a.appearance_action(Action::Left, Instant::now()), "swallowed in the list, nothing else");
    }

    #[test]
    fn the_color_helper_matches_roles() {
        let (a, _) = open("color", &[]);
        assert_eq!(a.theme.bg, rgb(a.appearance.preview.unwrap().bar_bg));
    }

    // Through the real key table, as Mae types it.
    fn key(a: &mut App, code: ratatui::crossterm::event::KeyCode) {
        a.on_key(ratatui::crossterm::event::KeyEvent::new(code, ratatui::crossterm::event::KeyModifiers::NONE), Instant::now());
    }

    #[test]
    fn the_keys_of_the_tab() {
        use ratatui::crossterm::event::KeyCode;
        let (mut a, dir) = open("keys", &[]);
        key(&mut a, KeyCode::Char('h')); // Dvorak down, like the other tabs
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Char('t')); // Dvorak up
        assert_eq!(a.appearance.cursor, 2);
        key(&mut a, KeyCode::Tab);
        assert!(a.appearance.mode_focus && a.tab == Tab::Appearance, "Tab moves the focus, it does not leave the tab");
        key(&mut a, KeyCode::Char('n')); // Dvorak right
        assert_eq!(a.appearance.mode, ModeSetting::Dark);
        key(&mut a, KeyCode::Char('+'));
        assert_eq!(a.appearance.mode, ModeSetting::Light);
        key(&mut a, KeyCode::Char('-'));
        assert_eq!(a.appearance.mode, ModeSetting::Dark);
        assert!(!dir.join("theme.toml").exists(), "still nothing written");
        key(&mut a, KeyCode::Enter);
        assert_eq!(theme_toml(&dir), "theme = \"gruvbox\"\nmode = \"dark\"\n", "the theme moved off the one in effect: both are applied");
        key(&mut a, KeyCode::Esc);
        assert_eq!(a.tab, Tab::Home, "Esc as in the other tabs");
        key(&mut a, KeyCode::Char('7'));
        assert_eq!(a.tab, Tab::Appearance);
        assert_eq!(a.appearance.cursor, 2, "reopened on the theme in effect");
    }

    #[test]
    fn the_tab_costs_nothing_while_other_tabs_are_shown() {
        let mut a = ready_app();
        a.go_tab(Tab::Sound, Instant::now());
        a.go_tab(Tab::Power, Instant::now());
        assert!(a.appearance.entries.is_empty() && a.appearance.preview.is_none());
        assert!(a.take_requests().iter().all(|r| !matches!(r, super::super::worker::Request::Do(_))));
    }

    #[test]
    fn the_preview_includes_the_users_colour_overrides() {
        let t = "theme = \"nord\"\nmode = \"dark\"\n[colors]\nfg = \"#123456\"\n";
        let (mut a, _) = open("overrides", &[("theme.toml", t)]);
        assert_eq!(a.appearance.preview.unwrap().fg, Rgb(0x12, 0x34, 0x56));
        act(&mut a, Action::Down); // Dracula
        assert_eq!(a.appearance.preview.unwrap().fg, Rgb(0x12, 0x34, 0x56), "on every theme");
        assert_eq!(a.appearance.preview.unwrap().bar_bg, preset("dracula").unwrap().dark.bar_bg, "only fg is overridden");
        let (a, _) = open("no-overrides", &[("theme.toml", "theme = \"nord\"\n")]);
        assert_eq!(a.appearance.preview, Some(preset("nord").unwrap().dark));
    }

    /// Every module-colours value the TUI can write is one theme.toml reads back, `pills` included.
    #[test]
    fn every_module_colours_value_reads_back() {
        use alpheus_commun::theme::settings::parse_settings;
        for m in [ModuleColors::None, ModuleColors::Icons, ModuleColors::Full, ModuleColors::Pills] {
            let (s, w) = parse_settings(&format!("module_colors = \"{}\"\n", colors_value(m)));
            assert_eq!((s.module_colors, w), (m, Vec::<String>::new()), "{m:?}");
        }
        assert_eq!(colors_label(ModuleColors::Pills), "Pills");
    }
}
