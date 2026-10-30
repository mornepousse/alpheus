//! Vocabulary of the application: tabs, keyboard intents (Action), business
//! effects (Command), external tools, palette entries.

use super::tab::{TABS, TabSpec};
use super::worker::Domain;
use super::{ToggleId, toggle_label};
use crate::backends::PowerAction;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Home,
    Sound,
    Network,
    Bluetooth,
    Power,
    Displays,
    Appearance,
}

impl Tab {
    /// Every tab, in tab order (derived from `TABS`).
    pub const ALL: [Tab; TABS.len()] = {
        let mut all = [Tab::Home; TABS.len()];
        let mut i = 0;
        while i < TABS.len() {
            all[i] = TABS[i].tab;
            i += 1;
        }
        all
    };

    /// The tab's row in `TABS`.
    pub fn spec(self) -> &'static TabSpec {
        &TABS[self.index()]
    }

    pub fn title(self) -> &'static str {
        self.spec().title
    }

    pub fn index(self) -> usize {
        // The enum is `repr`-ordered like `TABS` (checked by a test).
        self as usize
    }

    pub fn next(self) -> Tab {
        Tab::ALL[(self.index() + 1) % Tab::ALL.len()]
    }

    pub fn prev(self) -> Tab {
        Tab::ALL[(self.index() + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }

    /// Tabs whose list is filtered with "/".
    pub fn searchable(self) -> bool {
        self.spec().searchable
    }

    /// Tab name on the command line (`alpheus --tab <name>`).
    pub fn slug(self) -> &'static str {
        self.spec().slug
    }

    /// Refresh period while the tab is visible.
    pub fn refresh(self) -> Option<Duration> {
        self.spec().refresh
    }

    /// Domains the tab shows.
    pub fn domains(self) -> &'static [Domain] {
        self.spec().domains
    }

    /// Tab named `name`, case- and accent-insensitive.
    pub fn from_slug(name: &str) -> Option<Tab> {
        let n = crate::fuzzy::fold_str(name.trim());
        TABS.iter().find(|s| s.slug == n || s.aliases.contains(&n.as_str())).map(|s| s.tab)
    }
}

/// External TUI tool opened in the same window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum External {
    Mixer,
    Network,
    Bluetooth,
}

impl External {
    pub fn label(self) -> &'static str {
        match self {
            External::Mixer => "Per-app mixer",
            External::Network => "Choose a network",
            External::Bluetooth => "Pair a device",
        }
    }
}

/// Business effect, triggered by a key, the mouse or the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    ToggleWifi,
    ToggleBluetooth,
    ToggleDnd,
    ToggleKeepAwake,
    SetProfile(String),
    Power(PowerAction),
    Open(External),
    GoTab(Tab),
    /// Applies a display profile (palette "Display profile: …").
    ApplyDisplayProfile(String),
}

impl Command {
    pub fn label(&self) -> String {
        match self {
            Command::ToggleWifi => toggle_label(ToggleId::Wifi).into(),
            Command::ToggleBluetooth => toggle_label(ToggleId::Bluetooth).into(),
            Command::ToggleDnd => toggle_label(ToggleId::Dnd).into(),
            Command::ToggleKeepAwake => toggle_label(ToggleId::KeepAwake).into(),
            Command::SetProfile(p) => format!("Profile: {}", profile_label(p)),
            Command::Power(a) => a.label().into(),
            Command::Open(e) => e.label().into(),
            Command::GoTab(t) => format!("Go to: {}", t.title()),
            Command::ApplyDisplayProfile(p) => format!("Display profile: {p}"),
        }
    }
}

/// Keyboard intent, configurable by name in config.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Back,
    Help,
    Palette,
    Search,
    NextTab,
    PrevTab,
    GoTab(Tab),
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Toggle,
    Activate,
    Decrease,
    Increase,
}

impl Action {
    /// Every action; the `GoTab` ones, one per tab, follow `PrevTab`.
    pub const ALL: [Action; 19 + TABS.len()] = {
        const BEFORE: [Action; 7] =
            [Action::Quit, Action::Back, Action::Help, Action::Palette, Action::Search, Action::NextTab, Action::PrevTab];
        const AFTER: [Action; 12] = [
            Action::Left,
            Action::Right,
            Action::Up,
            Action::Down,
            Action::PageUp,
            Action::PageDown,
            Action::Home,
            Action::End,
            Action::Toggle,
            Action::Activate,
            Action::Decrease,
            Action::Increase,
        ];
        let mut all = [Action::Quit; 19 + TABS.len()];
        let mut n = 0;
        let mut i = 0;
        while i < BEFORE.len() {
            all[n] = BEFORE[i];
            n += 1;
            i += 1;
        }
        i = 0;
        while i < TABS.len() {
            all[n] = Action::GoTab(TABS[i].tab);
            n += 1;
            i += 1;
        }
        i = 0;
        while i < AFTER.len() {
            all[n] = AFTER[i];
            n += 1;
            i += 1;
        }
        all
    };

    pub fn name(self) -> String {
        match self {
            Action::Quit => "quit".into(),
            Action::Back => "back".into(),
            Action::Help => "help".into(),
            Action::Palette => "palette".into(),
            Action::Search => "search".into(),
            Action::NextTab => "next_tab".into(),
            Action::PrevTab => "prev_tab".into(),
            Action::GoTab(t) => format!("tab_{}", t.index() + 1),
            Action::Left => "left".into(),
            Action::Right => "right".into(),
            Action::Up => "up".into(),
            Action::Down => "down".into(),
            Action::PageUp => "page_up".into(),
            Action::PageDown => "page_down".into(),
            Action::Home => "home".into(),
            Action::End => "end".into(),
            Action::Toggle => "toggle".into(),
            Action::Activate => "activate".into(),
            Action::Decrease => "decrease".into(),
            Action::Increase => "increase".into(),
        }
    }

    pub fn from_name(s: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == s)
    }

    /// Help label.
    pub fn label(self) -> String {
        match self {
            Action::Quit => "Quit".into(),
            Action::Back => "Back".into(),
            Action::Help => "Help".into(),
            Action::Palette => "Command palette".into(),
            Action::Search => "Search".into(),
            Action::NextTab => "Next tab".into(),
            Action::PrevTab => "Previous tab".into(),
            Action::GoTab(t) => format!("Tab {}", t.title()),
            Action::Left => "Left".into(),
            Action::Right => "Right".into(),
            Action::Up => "Up".into(),
            Action::Down => "Down".into(),
            Action::PageUp => "Page up".into(),
            Action::PageDown => "Page down".into(),
            Action::Home => "Home".into(),
            Action::End => "End".into(),
            Action::Toggle => "Toggle".into(),
            Action::Activate => "Activate".into(),
            Action::Decrease => "Decrease".into(),
            Action::Increase => "Increase".into(),
        }
    }
}

pub fn profile_label(p: &str) -> String {
    match p {
        "performance" => "Performance",
        "balanced" => "Balanced",
        "power-saver" => "Power saver",
        other => other,
    }
    .to_string()
}

/// All palette commands; the profiles (energy, displays) depend on the machine.
pub fn palette_entries(profiles: &[String], display_profiles: &[String]) -> Vec<(String, Command)> {
    let mut v: Vec<Command> = vec![Command::ToggleDnd, Command::ToggleWifi, Command::ToggleBluetooth, Command::ToggleKeepAwake];
    v.extend(Tab::ALL.map(Command::GoTab));
    v.extend([External::Mixer, External::Network, External::Bluetooth].map(Command::Open));
    v.extend(profiles.iter().cloned().map(Command::SetProfile));
    v.extend(display_profiles.iter().cloned().map(Command::ApplyDisplayProfile));
    v.extend(PowerAction::ALL.map(Command::Power));
    v.into_iter().map(|c| (c.label(), c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_wrap_around() {
        assert_eq!(Tab::Home.prev(), Tab::Appearance);
        assert_eq!(Tab::Power.next(), Tab::Displays);
        assert_eq!(Tab::Displays.next(), Tab::Appearance);
        assert_eq!(Tab::Appearance.next(), Tab::Home);
        assert_eq!(Tab::Appearance.title(), "Theme");
        assert_eq!(Tab::Bluetooth.title(), "BT");
        assert_eq!(Tab::Displays.title(), "Displays");
        assert_eq!(Tab::Network.index(), 2);
    }

    #[test]
    fn tab_names_on_the_command_line() {
        let mut names: Vec<&str> = Tab::ALL.iter().map(|t| t.slug()).collect();
        names.sort();
        assert_eq!(names, ["appearance", "bluetooth", "displays", "home", "network", "power", "sound"]);
        for t in Tab::ALL {
            assert_eq!(Tab::from_slug(t.slug()), Some(t));
        }
        assert_eq!(Tab::from_slug("Network"), Some(Tab::Network));
        assert_eq!(Tab::from_slug(" DISPLAYS "), Some(Tab::Displays));
        assert_eq!(Tab::from_slug("Appearance"), Some(Tab::Appearance));
        assert_eq!(Tab::from_slug("theme"), Some(Tab::Appearance));
        assert_eq!(Tab::from_slug("Bluetooth"), Some(Tab::Bluetooth));
        assert_eq!(Tab::from_slug("weather"), None);
        assert_eq!(Tab::from_slug(""), None);
    }

    #[test]
    fn action_names_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(&a.name()), Some(a), "{}", a.name());
        }
        assert_eq!(Action::GoTab(Tab::Network).name(), "tab_3");
        assert_eq!(Action::GoTab(Tab::Appearance).name(), "tab_7");
        assert_eq!(Action::PageUp.name(), "page_up");
        assert_eq!(Action::from_name("fly"), None);
    }

    #[test]
    fn one_go_tab_action_per_tab_in_tab_order() {
        let go: Vec<Tab> = Action::ALL.iter().filter_map(|a| if let Action::GoTab(t) = a { Some(*t) } else { None }).collect();
        assert_eq!(go, Tab::ALL);
        assert_eq!(Action::ALL.len(), 19 + Tab::ALL.len());
    }

    #[test]
    fn help_labels() {
        assert_eq!(Action::Palette.label(), "Command palette");
        assert_eq!(Action::GoTab(Tab::Sound).label(), "Tab Sound");
    }

    #[test]
    fn full_palette() {
        let p = palette_entries(&["performance".into(), "balanced".into(), "power-saver".into()], &["Bureau".into()]);
        let labels: Vec<&str> = p.iter().map(|(l, _)| l.as_str()).collect();
        for expected in [
            "Do not disturb",
            "Wi-Fi",
            "Bluetooth",
            "Keep awake",
            "Lock",
            "Power off",
            "Per-app mixer",
            "Choose a network",
            "Pair a device",
            "Go to: Sound",
            "Profile: Balanced",
            "Go to: Displays",
            "Go to: Theme",
            "Go to: BT",
            "Display profile: Bureau",
        ] {
            assert!(labels.contains(&expected), "{expected} missing");
        }
        assert!(p.contains(&("Power off".to_string(), Command::Power(PowerAction::Poweroff))));
        assert!(p.contains(&("Display profile: Bureau".to_string(), Command::ApplyDisplayProfile("Bureau".into()))));
    }
}
