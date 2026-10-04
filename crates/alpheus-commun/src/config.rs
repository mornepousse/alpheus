//! Configuration facultative : ~/.config/alpheus/config.toml. Défauts sûrs
//! sans fichier ; fichier invalide = défauts + avertissement dans la ligne
//! d'état (jamais d'arrêt).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub keys: KeysConfig,
    pub commands: CommandsConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeysConfig {
    /// D/H/T/N = gauche/bas/haut/droite (positions hjkl sur le Dvorak de Mae).
    pub dvorak_nav: bool,
    /// Surcharges : nom d'action → touches (remplacent celles par défaut).
    pub bindings: BTreeMap<String, Vec<String>>,
}

impl Default for KeysConfig {
    fn default() -> Self {
        KeysConfig { dvorak_nav: true, bindings: BTreeMap::new() }
    }
}

/// Outils externes ouverts dans la fenêtre d'alpheus (programme + arguments).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    pub mixer: Vec<String>,
    pub network: Vec<String>,
    pub bluetooth: Vec<String>,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        CommandsConfig {
            mixer: vec!["wiremix".into()],
            network: vec!["nmtui".into()],
            bluetooth: vec!["bluetui".into()],
        }
    }
}

impl Config {
    /// Absent → défauts sans message ; illisible ou invalide → défauts + message.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (Config::default(), Some(format!("config.toml illisible : {e}"))),
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("config.toml invalide : {}", e.message()))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(name: &str, content: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-cfg-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn absent_donne_les_defauts_sans_message() {
        let (c, w) = Config::load(Path::new("/nonexistent/alpheus/config.toml"));
        assert_eq!(c, Config::default());
        assert_eq!(w, None);
        assert_eq!(c.commands.mixer, vec!["wiremix"]);
        assert!(c.keys.dvorak_nav);
    }

    #[test]
    fn fichier_partiel() {
        let p = file("partiel", "[commands]\nmixer = [\"pavucontrol\"]\n\n[keys]\ndvorak_nav = false\n\n[keys.bindings]\npalette = [\"ctrl+k\"]\n");
        let (c, w) = Config::load(&p);
        assert_eq!(w, None);
        assert_eq!(c.commands.mixer, vec!["pavucontrol"]);
        assert_eq!(c.commands.network, vec!["nmtui"]);
        assert!(!c.keys.dvorak_nav);
        assert_eq!(c.keys.bindings["palette"], vec!["ctrl+k"]);
    }

    #[test]
    fn invalide_donne_les_defauts_et_un_message() {
        let (c, w) = Config::load(&file("invalide", "[commands\nmixer = 3"));
        assert_eq!(c, Config::default());
        let w = w.unwrap();
        assert!(w.starts_with("config.toml invalide"), "{w}");
        assert!(w.contains("expected"), "{w}");
    }

    #[test]
    fn champ_inconnu_signale() {
        let (_, w) = Config::load(&file("inconnu", "[commandes]\nmixer = [\"x\"]\n"));
        let w = w.unwrap();
        assert!(w.contains("commandes"), "{w}");
    }
}
