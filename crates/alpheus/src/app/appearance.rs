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

/// Where the keyboard focus is: the theme list or one of the setting rows below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    List,
    Mode,
    Colors,
    Style,
}

impl Focus {
    /// The row below, stopping on the last one.
    pub fn next(self) -> Focus {
        match self {
            Focus::List => Focus::Mode,
            Focus::Mode => Focus::Colors,
            Focus::Colors | Focus::Style => Focus::Style,
        }
    }

    /// The row above, stopping on the list.
    pub fn prev(self) -> Focus {
        match self {
            Focus::List | Focus::Mode => Focus::List,
            Focus::Colors => Focus::Mode,
            Focus::Style => Focus::Colors,
        }
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
    /// Where the keyboard focus is (exactly one place).
    pub focus: Focus,
    /// The module colours chosen (previewed; written by Enter).
    pub module_colors: ModuleColors,
    /// The module colours in effect, as theme.toml says.
    pub applied_module_colors: ModuleColors,
    /// The bar style chosen (previewed; written by Enter).
    pub style: BarStyle,
    /// The bar style in effect, as theme.toml says.
    pub applied_style: BarStyle,
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
            focus: Focus::List,
            module_colors: ModuleColors::default(),
            applied_module_colors: ModuleColors::default(),
            style: BarStyle::default(),
            applied_style: BarStyle::default(),
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
        a.focus = Focus::List;
        a.module_colors = settings.module_colors;
        a.applied_module_colors = settings.module_colors;
        a.style = settings.style;
        a.applied_style = settings.style;
        a.auto_mode = read_mode(a.runtime.as_deref());
        a.wallpaper = None;
        a.refresh_preview();
    }

    /// A key of the Appearance tab. false: not ours, the generic handling goes on (Esc, 1–7, help…).
    pub(crate) fn appearance_action(&mut self, a: Action, now: Instant) -> bool {
        let last = self.appearance.entries.len().saturating_sub(1);
        let ap = &mut self.appearance;
        // Up and Down walk the list then the rows, Down past the last theme reaching them; Tab
        // changes tab, as everywhere.
        let focus = ap.focus;
        match a {
            // Walking past the list chose no theme: the cursor goes back to the one in effect, so
            // the preview shows it and Enter on a row applies that row only.
            Action::Down if focus == Focus::List && ap.cursor == last => {
                ap.focus = Focus::Mode;
                ap.cursor = ap.applied.unwrap_or(ap.cursor);
                ap.refresh_preview();
            }
            Action::Up if focus != Focus::List => ap.focus = focus.prev(),
            Action::Down if focus != Focus::List => ap.focus = focus.next(),
            Action::PageUp | Action::PageDown | Action::Home | Action::End if focus != Focus::List => {}
            Action::Up => self.appearance_move(-1),
            Action::Down => self.appearance_move(1),
            Action::PageUp => self.appearance_move(-5),
            Action::PageDown => self.appearance_move(5),
            Action::Home => self.appearance_move(-(last as i32)),
            Action::End => self.appearance_move(last as i32),
            Action::Left | Action::Decrease if focus == Focus::Mode => self.appearance_step(-1),
            Action::Right | Action::Increase if focus == Focus::Mode => self.appearance_step(1),
            Action::Left | Action::Decrease if focus == Focus::Colors => self.appearance_step_colors(-1),
            Action::Right | Action::Increase if focus == Focus::Colors => self.appearance_step_colors(1),
            Action::Left | Action::Decrease if focus == Focus::Style => self.appearance_step_style(-1),
            Action::Right | Action::Increase if focus == Focus::Style => self.appearance_step_style(1),
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
        a.focus = Focus::List;
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
        a.focus = Focus::Mode;
        a.mode = m;
        a.refresh_preview();
        self.appearance_apply(now);
    }

    /// A click on a module-colours value applies it at once.
    pub(crate) fn appearance_click_colors(&mut self, m: ModuleColors, now: Instant) {
        let a = &mut self.appearance;
        a.focus = Focus::Colors;
        a.module_colors = m;
        a.refresh_preview();
        self.appearance_apply(now);
    }

    /// A click on a style applies it at once.
    pub(crate) fn appearance_click_style(&mut self, s: BarStyle, now: Instant) {
        let a = &mut self.appearance;
        a.focus = Focus::Style;
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
        let on_list = a.focus == Focus::List;
        let theme = (on_list || a.applied != Some(a.cursor)).then_some(entry.key);
        let mode = (a.focus == Focus::Mode || a.mode != a.applied_mode).then(|| mode_value(a.mode));
        let colors = (a.focus == Focus::Colors || a.module_colors != a.applied_module_colors).then(|| colors_value(a.module_colors));
        let style = (a.focus == Focus::Style || a.style != a.applied_style).then(|| a.style.name());
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
pub(crate) mod tests;
