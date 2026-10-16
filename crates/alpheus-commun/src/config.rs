//! Optional configuration: ~/.config/alpheus/config.toml. Safe defaults
//! without a file; invalid file = defaults + a warning on the status line
//! (never a stop).

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
    /// D/H/T/N = left/down/up/right (hjkl positions on Mae's Dvorak layout).
    pub dvorak_nav: bool,
    /// Overrides: action name → keys (replace the defaults).
    pub bindings: BTreeMap<String, Vec<String>>,
}

impl Default for KeysConfig {
    fn default() -> Self {
        KeysConfig { dvorak_nav: true, bindings: BTreeMap::new() }
    }
}

/// External tools opened in alpheus's window (program + arguments).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    pub mixer: Vec<String>,
    pub network: Vec<String>,
    pub bluetooth: Vec<String>,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        CommandsConfig { mixer: vec!["wiremix".into()], network: vec!["nmtui".into()], bluetooth: vec!["bluetui".into()] }
    }
}

impl Config {
    /// Absent → defaults with no message; unreadable or invalid → defaults + a message.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (Config::default(), Some(format!("config.toml unreadable: {e}"))),
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("invalid config.toml: {}", e.message()))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temp_path::TempPath;

    fn file(name: &str, content: &str) -> TempPath {
        let p = TempPath::dir(&format!("cfg-{name}")).below("config.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn absent_gives_the_defaults_with_no_message() {
        let (c, w) = Config::load(Path::new("/nonexistent/alpheus/config.toml"));
        assert_eq!(c, Config::default());
        assert_eq!(w, None);
        assert_eq!(c.commands.mixer, vec!["wiremix"]);
        assert!(c.keys.dvorak_nav);
    }

    #[test]
    fn partial_file() {
        let p = file(
            "partial",
            "[commands]\nmixer = [\"pavucontrol\"]\n\n[keys]\ndvorak_nav = false\n\n[keys.bindings]\npalette = [\"ctrl+k\"]\n",
        );
        let (c, w) = Config::load(&p);
        assert_eq!(w, None);
        assert_eq!(c.commands.mixer, vec!["pavucontrol"]);
        assert_eq!(c.commands.network, vec!["nmtui"]);
        assert!(!c.keys.dvorak_nav);
        assert_eq!(c.keys.bindings["palette"], vec!["ctrl+k"]);
    }

    #[test]
    fn invalid_gives_the_defaults_and_a_message() {
        let (c, w) = Config::load(&file("invalid", "[commands\nmixer = 3"));
        assert_eq!(c, Config::default());
        let w = w.unwrap();
        assert!(w.starts_with("invalid config.toml"), "{w}");
        assert!(w.contains("expected"), "{w}");
    }

    #[test]
    fn unknown_field_reported() {
        let (_, w) = Config::load(&file("unknown", "[commandes]\nmixer = [\"x\"]\n"));
        let w = w.unwrap();
        assert!(w.contains("commandes"), "{w}");
    }
}
