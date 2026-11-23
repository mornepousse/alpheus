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
    pub diagnostics: DiagnosticsConfig,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeysConfig {
    /// D/H/T/N = left/down/up/right (hjkl positions on a Dvorak layout). Off by default: a
    /// shareable product does not assume one keyboard layout.
    pub dvorak_nav: bool,
    /// Overrides: action name → keys (replace the defaults).
    pub bindings: BTreeMap<String, Vec<String>>,
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

/// The Diag tab (`[diagnostics]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagnosticsConfig {
    /// The bug report's clipboard program (argv; the report goes on its standard input); `[]`
    /// disables Copy.
    pub copy: Vec<String>,
    /// Restart and reset-failed on system units, in the TUI's own window (polkit asks there).
    pub system_actions: bool,
    /// `NRestarts` at or above this is "restarting in a loop" (1..=1000).
    pub restart_loop: u32,
    /// Entries read when the journal view opens (1..=5000).
    pub journal_lines: u32,
    /// Declared health checks (at most 8).
    pub checks: Vec<Check>,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        DiagnosticsConfig { copy: vec!["wl-copy".into()], system_actions: true, restart_loop: 3, journal_lines: 500, checks: Vec::new() }
    }
}

/// A check the user declared: `run` an argv, exit 0 is fine; `fix` is shown, never run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// 1 to 40 characters, no control character.
    pub name: String,
    pub run: Vec<String>,
    /// At most 200 characters.
    pub fix: Option<String>,
}

impl DiagnosticsConfig {
    /// Puts an out-of-range value back to its default and drops a bad or surplus check, one
    /// sentence each.
    fn normalise(&mut self) -> Vec<String> {
        let default = DiagnosticsConfig::default();
        let mut warnings = Vec::new();
        if !(1..=1000).contains(&self.restart_loop) {
            warnings.push(format!("[diagnostics] restart_loop {} is outside 1-1000: using {}", self.restart_loop, default.restart_loop));
            self.restart_loop = default.restart_loop;
        }
        if !(1..=5000).contains(&self.journal_lines) {
            warnings.push(format!("[diagnostics] journal_lines {} is outside 1-5000: using {}", self.journal_lines, default.journal_lines));
            self.journal_lines = default.journal_lines;
        }
        let ok = |c: &Check| {
            let n = c.name.chars().count();
            (1..=40).contains(&n)
                && !c.name.chars().any(|ch| ch.is_control() || crate::text::invisible(ch))
                && !c.run.is_empty()
                && c.fix
                    .as_ref()
                    .is_none_or(|f| f.chars().count() <= 200 && !f.chars().any(|ch| ch.is_control() || crate::text::invisible(ch)))
        };
        let before = self.checks.len();
        self.checks.retain(ok);
        if self.checks.len() < before {
            warnings.push("[diagnostics] a check with a bad name, an empty run or a fix over 200 characters was ignored".into());
        }
        if self.checks.len() > 8 {
            self.checks.truncate(8);
            warnings.push("[diagnostics] more than 8 checks: the first 8 are kept".into());
        }
        warnings
    }
}

impl Config {
    /// Absent → defaults with no message; unreadable or invalid → defaults + a message.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (Config::default(), Some(format!("config.toml unreadable: {e}"))),
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(mut c) => {
                    let warnings = c.diagnostics.normalise();
                    (c, (!warnings.is_empty()).then(|| warnings.join("; ")))
                }
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
        assert!(!c.keys.dvorak_nav);
    }

    #[test]
    fn partial_file() {
        let p = file(
            "partial",
            "[commands]\nmixer = [\"pavucontrol\"]\n\n[keys]\ndvorak_nav = true\n\n[keys.bindings]\npalette = [\"ctrl+k\"]\n",
        );
        let (c, w) = Config::load(&p);
        assert_eq!(w, None);
        assert_eq!(c.commands.mixer, vec!["pavucontrol"]);
        assert_eq!(c.commands.network, vec!["nmtui"]);
        assert!(c.keys.dvorak_nav);
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
    fn diagnostics_defaults_and_bounds() {
        let (c, w) = Config::load(Path::new("/nonexistent/alpheus/config.toml"));
        assert_eq!(w, None);
        assert_eq!(c.diagnostics.copy, vec!["wl-copy"]);
        assert!(c.diagnostics.system_actions);
        assert_eq!((c.diagnostics.restart_loop, c.diagnostics.journal_lines), (3, 500));
        assert!(c.diagnostics.checks.is_empty());

        let (c, w) = Config::load(&file(
            "diag-ok",
            "[diagnostics]\ncopy = []\nsystem_actions = false\nrestart_loop = 1000\njournal_lines = 5000\n\n[[diagnostics.checks]]\nname = \"One hypridle\"\nrun = [\"true\"]\nfix = \"pkill -x hypridle\"\n\n[[diagnostics.checks]]\nname = \"No fix\"\nrun = [\"true\"]\n",
        ));
        assert_eq!(w, None);
        assert!(c.diagnostics.copy.is_empty() && !c.diagnostics.system_actions);
        assert_eq!((c.diagnostics.restart_loop, c.diagnostics.journal_lines), (1000, 5000));
        assert_eq!(c.diagnostics.checks.len(), 2);
        assert_eq!(c.diagnostics.checks[1].fix, None);

        for bad in ["restart_loop = 0", "restart_loop = 1001", "journal_lines = 0", "journal_lines = 5001"] {
            let (c, w) = Config::load(&file("diag-range", &format!("[diagnostics]\n{bad}\n")));
            assert_eq!((c.diagnostics.restart_loop, c.diagnostics.journal_lines), (3, 500), "{bad}");
            assert!(w.unwrap().contains("[diagnostics]"), "{bad}");
        }
        // One bad value does not reset the others.
        let (c, w) = Config::load(&file("diag-one", "[diagnostics]\nrestart_loop = 0\njournal_lines = 20\n"));
        assert_eq!((c.diagnostics.restart_loop, c.diagnostics.journal_lines), (3, 20));
        assert!(w.is_some());
    }

    #[test]
    fn bad_and_surplus_checks_are_dropped_with_a_warning() {
        let one = |name: &str, run: &str| format!("[[diagnostics.checks]]\nname = {name}\nrun = {run}\n\n");
        let long = format!("\"{}\"", "n".repeat(41));
        let text = [
            one("\"\"", "[\"x\"]"),
            one(&long, "[\"x\"]"),
            one("\"tab\\there\"", "[\"x\"]"),
            one("\"bidi\\u202Ename\"", "[\"x\"]"),
            one("\"empty run\"", "[]"),
            one("\"fine\"", "[\"x\"]"),
        ]
        .concat();
        let (c, w) = Config::load(&file("diag-checks", &text));
        assert_eq!(c.diagnostics.checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["fine"]);
        assert!(w.is_some());
        let nine: String = (0..9).map(|i| one(&format!("\"c{i}\""), "[\"x\"]")).collect();
        let (c, w) = Config::load(&file("diag-nine", &nine));
        assert_eq!(c.diagnostics.checks.len(), 8);
        assert!(w.unwrap().contains("8"));
        let fix = format!("[[diagnostics.checks]]\nname = \"a\"\nrun = [\"x\"]\nfix = \"{}\"\n", "f".repeat(201));
        assert!(Config::load(&file("diag-fix", &fix)).0.diagnostics.checks.is_empty());
    }

    #[test]
    fn unknown_diagnostics_keys_are_rejected() {
        let (c, w) = Config::load(&file("diag-unknown", "[diagnostics]\ncpoy = []\n"));
        assert_eq!(c, Config::default());
        assert!(w.unwrap().contains("cpoy"));
    }

    #[test]
    fn unknown_field_reported() {
        let (_, w) = Config::load(&file("unknown", "[commandes]\nmixer = [\"x\"]\n"));
        let w = w.unwrap();
        assert!(w.contains("commandes"), "{w}");
    }
}
