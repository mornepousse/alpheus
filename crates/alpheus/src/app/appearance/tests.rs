use super::super::action::Tab;
use super::super::testing::*;
use super::*;
use alpheus_commun::temp_path::TempPath;
use alpheus_commun::theme::Rgb;
use alpheus_commun::theme::presets::preset;
use alpheus_commun::theme::settings::{BarStyle, ModuleColors};
use ratatui::style::Color;

/// A fresh config directory holding `files`, and an App opened on the Appearance tab with it.
/// The directory goes when the returned guard drops: bind it (`_dir`), never `_`.
pub fn open(name: &str, files: &[(&str, &str)]) -> (App, TempPath) {
    let dir = TempPath::dir(&format!("app-appearance-{name}"));
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

/// From the list, down past the last theme onto row `n`: 1 mode, 2 module colours, 3 style.
pub fn to_row(a: &mut App, n: usize) {
    a.on_key_action(Action::End);
    for _ in 0..n {
        a.on_key_action(Action::Down);
    }
}

fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

#[test]
fn nothing_is_computed_until_the_tab_is_shown() {
    let a = ready_app();
    assert!(a.appearance.entries.is_empty() && a.appearance.preview.is_none());
    let (a, _dir) = open("lazy", &[]);
    assert_eq!(a.appearance.entries.len(), 10);
    assert!(a.appearance.preview.is_some());
}

#[test]
fn it_opens_on_the_theme_in_effect() {
    let (a, _dir) = open("effect", &[("theme.toml", "theme = \"tokyo-night\"\nmode = \"light\"\n")]);
    let ap = &a.appearance;
    assert_eq!((ap.cursor, ap.applied, ap.mode, ap.applied_mode), (3, Some(3), ModeSetting::Light, ModeSetting::Light));
    assert_eq!(ap.preview, Some(preset("tokyo-night").unwrap().light.unwrap()), "light variant for a fixed light mode");
    let (a, _dir) = open("default", &[]);
    assert_eq!((a.appearance.cursor, a.appearance.applied), (1, Some(1)), "no file: catppuccin");
    let (a, _dir) = open(
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
    let dir = TempPath::dir("app-appearance-run");
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
    to_row(&mut a, 1); // back on the theme in effect
    act(&mut a, Action::Right); // Dark: the published mode no longer matters
    assert_eq!(a.appearance.preview, Some(preset("catppuccin").unwrap().dark));
}

#[test]
fn a_preset_without_a_light_variant_previews_its_dark_half() {
    let (mut a, _dir) = open("nord", &[("theme.toml", "mode = \"light\"\n")]);
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
    let (mut a, _dir) = open("bounds", &[]);
    act(&mut a, Action::Home);
    act(&mut a, Action::Up);
    assert_eq!(a.appearance.cursor, 0);
    act(&mut a, Action::PageDown);
    assert_eq!(a.appearance.cursor, 5);
    act(&mut a, Action::End);
    assert_eq!(a.appearance.cursor, 9, "the last theme; Down from there reaches the rows");
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
    to_row(&mut a, 1);
    assert!(a.appearance.mode_focus, "down past the last theme: the mode row");
    act(&mut a, Action::Right);
    act(&mut a, Action::Right);
    assert_eq!(a.appearance.mode, ModeSetting::Light);
    act(&mut a, Action::Right);
    assert_eq!(a.appearance.mode, ModeSetting::Light, "bounded");
    assert_eq!(theme_toml(&dir), "theme = \"nord\"\n", "choosing writes nothing");
    act(&mut a, Action::Activate);
    assert_eq!(theme_toml(&dir), "theme = \"nord\"\nmode = \"light\"\n");
    assert_eq!(a.appearance.applied_mode, ModeSetting::Light);
    act(&mut a, Action::Up);
    assert!(!a.appearance.mode_focus, "Up: back to the list");
}

#[test]
fn down_past_the_last_theme_walks_the_rows_and_up_comes_back() {
    let (mut a, _dir) = open("rows", &[]);
    let at = |a: &App| (a.appearance.mode_focus, a.appearance.colors_focus, a.appearance.style_focus);
    act(&mut a, Action::End);
    assert_eq!(at(&a), (false, false, false), "End stays in the list");
    act(&mut a, Action::Down);
    assert_eq!(at(&a), (true, false, false));
    act(&mut a, Action::Down);
    assert_eq!(at(&a), (false, true, false));
    act(&mut a, Action::Down);
    assert_eq!(at(&a), (false, false, true));
    act(&mut a, Action::Down);
    assert_eq!(at(&a), (false, false, true), "Down stops on the last row");
    act(&mut a, Action::Up);
    assert_eq!(at(&a), (false, true, false), "Up walks back one row");
    act(&mut a, Action::Up);
    act(&mut a, Action::Up);
    assert_eq!(at(&a), (false, false, false), "back in the list");
}

#[test]
fn tab_leaves_the_tab_from_the_list_and_from_the_rows() {
    let (mut a, _dir) = open("tab-leaves", &[]);
    act(&mut a, Action::NextTab);
    assert_ne!(a.tab, Tab::Appearance, "Tab: the next tab, as everywhere");
    a.go_tab(Tab::Appearance, Instant::now());
    to_row(&mut a, 2);
    act(&mut a, Action::PrevTab);
    assert_eq!(a.tab, Tab::Displays, "Shift+Tab from a row: the previous tab");
}

#[test]
fn going_down_to_the_rows_puts_the_cursor_back_on_the_theme_in_effect() {
    let (mut a, dir) = open("rows-cursor", &[("theme.toml", "theme = \"nord\"\n")]);
    let nord = a.appearance.cursor;
    to_row(&mut a, 1);
    assert_eq!(a.appearance.cursor, nord, "walking past the list chose no theme");
    assert_eq!(a.appearance.preview, Some(preset("nord").unwrap().dark), "the preview shows the theme in effect");
    act(&mut a, Action::Right);
    act(&mut a, Action::Activate);
    assert_eq!(theme_toml(&dir), "theme = \"nord\"\nmode = \"dark\"\n", "Enter on the mode row: the mode only");
}

#[test]
fn the_style_row_chooses_with_the_arrows_and_applies_with_enter_writing_only_its_key() {
    let (mut a, dir) = open("style-keys", &[("theme.toml", "# mine\ntheme = \"nord\"\nmode = \"dark\"\nopacity = 0.6\n")]);
    assert_eq!((a.appearance.style, a.appearance.applied_style), (BarStyle::Flat, BarStyle::Flat), "the default");
    to_row(&mut a, 3);
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
    to_row(&mut a, 2);
    act(&mut a, Action::Right);
    assert_eq!(a.appearance.module_colors, ModuleColors::Pills);
    act(&mut a, Action::Activate);
    assert_eq!(theme_toml(&dir), "theme = \"nord\"\nmodule_colors = \"pills\"\n");
}

#[test]
fn the_colours_row_chooses_with_the_arrows_and_applies_with_enter_writing_only_its_key() {
    let (mut a, dir) = open("colors-keys", &[("theme.toml", "# mine\ntheme = \"nord\"\nmode = \"dark\"\n")]);
    assert_eq!((a.appearance.module_colors, a.appearance.applied_module_colors), (ModuleColors::Icons, ModuleColors::Icons), "the default");
    to_row(&mut a, 2);
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
    to_row(&mut a, 1);
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
    to_row(&mut a, 1);
    act(&mut a, Action::Right); // Auto -> Dark: the same mode in effect, still cached
    assert_eq!(bg(&a), Rgb(0x10, 0x10, 0x10));
    act(&mut a, Action::Right);
    assert_eq!(bg(&a), Rgb(0xf0, 0xf0, 0xf0), "Light: its own file");
    act(&mut a, Action::Left);
    assert_eq!(bg(&a), Rgb(0, 0, 1), "back to Dark: read again");
}

#[test]
fn a_missing_wallpaper_file_is_said_on_the_preview() {
    let (mut a, _dir) = open("nowall", &[("theme.toml", "theme = \"wallpaper\"\nmode = \"dark\"\n")]);
    assert_eq!(a.appearance.cursor, 0);
    assert!(a.appearance.preview_note.is_some(), "{:?}", a.appearance.preview_note);
    act(&mut a, Action::Down);
    assert_eq!(a.appearance.preview_note, None);
}

#[test]
fn other_keys_fall_through_to_the_generic_handling() {
    let (mut a, _dir) = open("fall", &[]);
    assert!(!a.appearance_action(Action::Back, Instant::now()));
    assert!(!a.appearance_action(Action::GoTab(Tab::Home), Instant::now()));
    assert!(a.appearance_action(Action::Left, Instant::now()), "swallowed in the list, nothing else");
}

#[test]
fn the_color_helper_matches_roles() {
    let (a, _dir) = open("color", &[]);
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
    key(&mut a, KeyCode::Enter);
    assert_eq!(theme_toml(&dir), "theme = \"gruvbox\"\n");
    key(&mut a, KeyCode::End);
    key(&mut a, KeyCode::Char('h')); // Dvorak down, past the last theme
    assert!(a.appearance.mode_focus && a.tab == Tab::Appearance);
    key(&mut a, KeyCode::Char('n')); // Dvorak right
    assert_eq!(a.appearance.mode, ModeSetting::Dark);
    key(&mut a, KeyCode::Char('+'));
    assert_eq!(a.appearance.mode, ModeSetting::Light);
    key(&mut a, KeyCode::Char('-'));
    assert_eq!(a.appearance.mode, ModeSetting::Dark);
    key(&mut a, KeyCode::Enter);
    assert_eq!(theme_toml(&dir), "theme = \"gruvbox\"\nmode = \"dark\"\n");
    key(&mut a, KeyCode::Tab);
    assert_eq!(a.tab, Tab::Home, "Tab leaves the tab, as everywhere");
    key(&mut a, KeyCode::Char('7'));
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
    let (mut a, _dir) = open("overrides", &[("theme.toml", t)]);
    assert_eq!(a.appearance.preview.unwrap().fg, Rgb(0x12, 0x34, 0x56));
    act(&mut a, Action::Down); // Dracula
    assert_eq!(a.appearance.preview.unwrap().fg, Rgb(0x12, 0x34, 0x56), "on every theme");
    assert_eq!(a.appearance.preview.unwrap().bar_bg, preset("dracula").unwrap().dark.bar_bg, "only fg is overridden");
    let (a, _dir) = open("no-overrides", &[("theme.toml", "theme = \"nord\"\n")]);
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
