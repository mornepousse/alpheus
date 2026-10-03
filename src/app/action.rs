//! Vocabulaire de l'application : onglets, intentions clavier (Action),
//! effets métier (Command), outils externes, entrées de la palette.

use crate::backends::PowerAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Accueil,
    Son,
    Reseau,
    Bluetooth,
    Alimentation,
    Ecrans,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Accueil, Tab::Son, Tab::Reseau, Tab::Bluetooth, Tab::Alimentation, Tab::Ecrans];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Accueil => "Accueil",
            Tab::Son => "Son",
            Tab::Reseau => "Réseau",
            Tab::Bluetooth => "Bluetooth",
            Tab::Alimentation => "Alimentation",
            Tab::Ecrans => "Écrans",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn next(self) -> Tab {
        Tab::ALL[(self.index() + 1) % Tab::ALL.len()]
    }

    pub fn prev(self) -> Tab {
        Tab::ALL[(self.index() + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }

    /// Onglets dont la liste se filtre avec « / ».
    pub fn searchable(self) -> bool {
        matches!(self, Tab::Son | Tab::Reseau | Tab::Bluetooth)
    }
}

/// Outil TUI externe ouvert dans la même fenêtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum External {
    Mixer,
    Network,
    Bluetooth,
}

impl External {
    pub fn label(self) -> &'static str {
        match self {
            External::Mixer => "Mixage par appli",
            External::Network => "Choisir un réseau",
            External::Bluetooth => "Appairer un appareil",
        }
    }
}

/// Effet métier, déclenché par une touche, la souris ou la palette.
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
}

impl Command {
    pub fn label(&self) -> String {
        match self {
            Command::ToggleWifi => "Wi-Fi".into(),
            Command::ToggleBluetooth => "Bluetooth".into(),
            Command::ToggleDnd => "Ne pas déranger".into(),
            Command::ToggleKeepAwake => "Garder éveillé".into(),
            Command::SetProfile(p) => format!("Profil : {}", profile_label(p)),
            Command::Power(a) => a.label().into(),
            Command::Open(e) => e.label().into(),
            Command::GoTab(t) => format!("Aller à : {}", t.title()),
        }
    }
}

/// Intention clavier, configurable par son nom dans config.toml.
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
    pub const ALL: [Action; 25] = [
        Action::Quit, Action::Back, Action::Help, Action::Palette, Action::Search,
        Action::NextTab, Action::PrevTab,
        Action::GoTab(Tab::Accueil), Action::GoTab(Tab::Son), Action::GoTab(Tab::Reseau),
        Action::GoTab(Tab::Bluetooth), Action::GoTab(Tab::Alimentation), Action::GoTab(Tab::Ecrans),
        Action::Left, Action::Right, Action::Up, Action::Down,
        Action::PageUp, Action::PageDown, Action::Home, Action::End,
        Action::Toggle, Action::Activate, Action::Decrease, Action::Increase,
    ];

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

    /// Libellé de l'aide.
    pub fn label(self) -> String {
        match self {
            Action::Quit => "Quitter".into(),
            Action::Back => "Retour".into(),
            Action::Help => "Aide".into(),
            Action::Palette => "Palette de commandes".into(),
            Action::Search => "Chercher".into(),
            Action::NextTab => "Onglet suivant".into(),
            Action::PrevTab => "Onglet précédent".into(),
            Action::GoTab(t) => format!("Onglet {}", t.title()),
            Action::Left => "Gauche".into(),
            Action::Right => "Droite".into(),
            Action::Up => "Haut".into(),
            Action::Down => "Bas".into(),
            Action::PageUp => "Page précédente".into(),
            Action::PageDown => "Page suivante".into(),
            Action::Home => "Début".into(),
            Action::End => "Fin".into(),
            Action::Toggle => "Basculer".into(),
            Action::Activate => "Activer".into(),
            Action::Decrease => "Diminuer".into(),
            Action::Increase => "Augmenter".into(),
        }
    }
}

pub fn profile_label(p: &str) -> String {
    match p {
        "performance" => "Performance",
        "balanced" => "Équilibré",
        "power-saver" => "Économie",
        other => other,
    }
    .to_string()
}

/// Toutes les commandes de la palette ; les profils dépendent du poste.
pub fn palette_entries(profiles: &[String]) -> Vec<(String, Command)> {
    let mut v: Vec<Command> = vec![Command::ToggleDnd, Command::ToggleWifi, Command::ToggleBluetooth, Command::ToggleKeepAwake];
    v.extend(Tab::ALL.map(Command::GoTab));
    v.extend([External::Mixer, External::Network, External::Bluetooth].map(Command::Open));
    v.extend(profiles.iter().cloned().map(Command::SetProfile));
    v.extend(PowerAction::ALL.map(Command::Power));
    v.into_iter().map(|c| (c.label(), c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onglets_circulaires() {
        assert_eq!(Tab::Accueil.prev(), Tab::Ecrans);
        assert_eq!(Tab::Alimentation.next(), Tab::Ecrans);
        assert_eq!(Tab::Ecrans.next(), Tab::Accueil);
        assert_eq!(Tab::Ecrans.title(), "Écrans");
        assert_eq!(Tab::Reseau.index(), 2);
    }

    #[test]
    fn noms_d_action_aller_retour() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(&a.name()), Some(a), "{}", a.name());
        }
        assert_eq!(Action::GoTab(Tab::Reseau).name(), "tab_3");
        assert_eq!(Action::PageUp.name(), "page_up");
        assert_eq!(Action::from_name("voler"), None);
    }

    #[test]
    fn libelles_d_aide() {
        assert_eq!(Action::Palette.label(), "Palette de commandes");
        assert_eq!(Action::GoTab(Tab::Son).label(), "Onglet Son");
    }

    #[test]
    fn palette_complete() {
        let p = palette_entries(&["performance".into(), "balanced".into(), "power-saver".into()]);
        let labels: Vec<&str> = p.iter().map(|(l, _)| l.as_str()).collect();
        for attendu in ["Ne pas déranger", "Wi-Fi", "Bluetooth", "Garder éveillé", "Verrouiller", "Éteindre",
            "Mixage par appli", "Choisir un réseau", "Appairer un appareil", "Aller à : Son", "Profil : Équilibré"] {
            assert!(labels.contains(&attendu), "{attendu} manque");
        }
        assert!(p.contains(&("Éteindre".to_string(), Command::Power(PowerAction::Poweroff))));
    }
}
