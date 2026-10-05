//! ~/.config/alpheus/launcher.toml, optional, read at each open of the
//! launcher (nothing kept while it is closed). Absent: defaults, no message.
//! Invalid: defaults and one line in the daemon's log. Never an abort.
//!
//! ```toml
//! terminal = ["foot", "-e"]                   # Terminal=true apps, Ctrl+Enter in Commands
//! rows = 10                                        # result rows, 3 to 30
//! history = 200                                    # entries kept, 0 to 1000; 0 disables the history
//! focus = ["wm-set", "focus-window", "{window}"]   # {window}: address:0x… (Hyprland), id:N (niri)
//! [prefixes]
//! commands = ">"
//! windows = "w "
//! ```

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const ROWS_MIN: u16 = 3;
pub const ROWS_MAX: u16 = 30;
/// Bounds the history file: HISTORY_MAX lines of at most ~290 bytes (history::ID_MAX).
pub const HISTORY_MAX: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Prefixes {
    pub commands: String,
    pub windows: String,
}

impl Default for Prefixes {
    fn default() -> Self {
        Prefixes { commands: ">".into(), windows: "w ".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherConfig {
    pub terminal: Vec<String>,
    pub rows: u16,
    /// Entries kept; 0 disables the history (never read, never written).
    pub history: usize,
    pub prefixes: Prefixes,
    pub focus: Vec<String>,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            terminal: vec!["foot".into(), "-e".into()],
            rows: 10,
            history: 200,
            prefixes: Prefixes::default(),
            focus: vec!["wm-set".into(), "focus-window".into(), "{window}".into()],
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    terminal: Vec<String>,
    rows: u16,
    history: usize,
    prefixes: Prefixes,
    focus: Vec<String>,
}

impl Default for Raw {
    fn default() -> Self {
        let d = LauncherConfig::default();
        Raw { terminal: d.terminal, rows: d.rows, history: d.history, prefixes: d.prefixes, focus: d.focus }
    }
}

impl LauncherConfig {
    /// Absent → (defaults, None); unreadable/invalid → (defaults, Some("launcher.toml …")).
    pub fn load(path: &Path) -> (LauncherConfig, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (LauncherConfig::default(), None),
            Err(e) => (LauncherConfig::default(), Some(format!("launcher.toml unreadable: {e}"))),
            Ok(s) => match Self::from_text(&s) {
                Ok(c) => (c, None),
                Err(m) => (LauncherConfig::default(), Some(format!("launcher.toml invalid: {m}"))),
            },
        }
    }

    /// Validation: terminal and focus non-empty with a non-blank program; focus contains "{window}";
    /// rows in ROWS_MIN..=ROWS_MAX; history ≤ HISTORY_MAX; prefixes non-empty, not starting with
    /// whitespace, neither one a prefix of the other (equal included).
    pub fn from_text(s: &str) -> Result<LauncherConfig, String> {
        let r: Raw = toml::from_str(s).map_err(|e| e.message().to_string())?;
        for (key, command) in [("terminal", &r.terminal), ("focus", &r.focus)] {
            if command.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(format!("{key} empty (expected: [\"program\", \"argument\", …])"));
            }
        }
        if r.focus.first().is_some_and(|p| p.contains("{window}")) {
            return Err("focus[0] (the program) must not contain \"{window}\" (it names the compositor binary, not a window)".into());
        }
        if !r.focus.iter().any(|a| a.contains("{window}")) {
            return Err("focus must contain \"{window}\" (the window to focus)".into());
        }
        if !(ROWS_MIN..=ROWS_MAX).contains(&r.rows) {
            return Err(format!("rows {} out of range {ROWS_MIN} to {ROWS_MAX}", r.rows));
        }
        if r.history > HISTORY_MAX {
            return Err(format!("history {} out of range 0 to {HISTORY_MAX}", r.history));
        }
        for (key, prefix) in [("prefixes.commands", &r.prefixes.commands), ("prefixes.windows", &r.prefixes.windows)] {
            if prefix.is_empty() {
                return Err(format!("{key} empty"));
            }
            if prefix.starts_with(char::is_whitespace) {
                return Err(format!("{key} starts with a space"));
            }
        }
        // One a prefix of the other ("w" and "w "): a query starting with the longer one
        // would match both modes.
        let (c, w) = (&r.prefixes.commands, &r.prefixes.windows);
        if c.starts_with(w.as_str()) || w.starts_with(c.as_str()) {
            return Err(format!("prefixes.commands {c:?} and prefixes.windows {w:?}: a query starting with both is ambiguous"));
        }
        Ok(LauncherConfig { terminal: r.terminal, rows: r.rows, history: r.history, prefixes: r.prefixes, focus: r.focus })
    }
}

pub fn config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("launcher.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::testing::TempDir;

    /// A launcher.toml in its own temporary dir (keep the TempDir alive while using the path).
    fn file(name: &str, content: &str) -> (TempDir, PathBuf) {
        let d = TempDir::new(&format!("launcher-config-{name}"));
        let p = config_path(&d);
        std::fs::write(&p, content).unwrap();
        (d, p)
    }

    #[test]
    fn absent_gives_the_spec_defaults_without_message() {
        let (c, w) = LauncherConfig::load(Path::new("/nonexistent/launcher.toml"));
        assert_eq!(w, None);
        assert_eq!(c, LauncherConfig::default());
        assert_eq!(c.terminal, ["foot", "-e"]);
        assert_eq!((c.rows, c.history), (10, 200));
        assert_eq!((c.prefixes.commands.as_str(), c.prefixes.windows.as_str()), (">", "w "));
        assert_eq!(c.focus, ["wm-set", "focus-window", "{window}"]);
    }

    #[test]
    fn settings_are_read() {
        let text = "terminal = [\"foot\"]\nrows = 6\nhistory = 0\nfocus = [\"my-wm\", \"focus\", \"--window={window}\"]\n[prefixes]\ncommands = \"!\"\nwindows = \"win:\"\n";
        let (_dir, p) = file("ok", text);
        let (c, w) = LauncherConfig::load(&p);
        assert_eq!(w, None);
        let expected = LauncherConfig {
            terminal: vec!["foot".into()],
            rows: 6,
            history: 0,
            prefixes: Prefixes { commands: "!".into(), windows: "win:".into() },
            focus: vec!["my-wm".into(), "focus".into(), "--window={window}".into()],
        };
        assert_eq!(c, expected);
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let c = LauncherConfig::from_text("rows = 5\n[prefixes]\ncommands = \"$\"\n").unwrap();
        assert_eq!((c.rows, c.history, c.prefixes.commands.as_str(), c.prefixes.windows.as_str()), (5, 200, "$", "w "));
        assert_eq!((c.terminal, c.focus), (LauncherConfig::default().terminal, LauncherConfig::default().focus));
        assert_eq!(LauncherConfig::from_text("").unwrap(), LauncherConfig::default());
    }

    #[test]
    fn each_rule_refuses_with_its_reason() {
        for (text, needle) in [
            ("terminal = []", "terminal empty"),
            ("terminal = [\" \", \"-e\"]", "terminal empty"),
            ("focus = []", "focus empty"),
            ("focus = [\"wm-set\", \"focus-window\"]", "{window}"),
            ("focus = [\"{window}\"]", "focus[0]"),
            ("rows = 2", "rows 2 out of range"),
            ("rows = 31", "rows 31 out of range"),
            ("rows = -1", "expected u16"),
            ("history = 1001", "history 1001 out of range"),
            ("[prefixes]\ncommands = \"\"", "prefixes.commands empty"),
            ("[prefixes]\nwindows = \" w\"", "prefixes.windows starts with a space"),
            ("[prefixes]\ncommands = \"w \"", "both"),
            ("[prefixes]\nwindows = \">\"", "both"),
            // One a prefix of the other: "w x" would open both modes, so it is refused too.
            ("[prefixes]\ncommands = \"w\"", "both"),
            ("[prefixes]\nwindows = \">>\"", "both"),
        ] {
            let e = LauncherConfig::from_text(text).unwrap_err();
            assert!(e.contains(needle), "{text:?}: {e}");
        }
    }

    #[test]
    fn the_limits_themselves_are_accepted() {
        for text in ["rows = 3", "rows = 30", "history = 0", "history = 1000", "[prefixes]\ncommands = \"w>\"\nwindows = \"w \""] {
            assert!(LauncherConfig::from_text(text).is_ok(), "{text}");
        }
    }

    #[test]
    fn unknown_keys_are_refused() {
        for text in ["prefix = \">\"", "[prefixes]\ncalc = \"=\"", "[launcher]\nrows = 4"] {
            let e = LauncherConfig::from_text(text).unwrap_err();
            assert!(e.contains("unknown field"), "{text:?}: {e}");
        }
    }

    #[test]
    fn invalid_or_unreadable_gives_defaults_and_one_message() {
        let (_bad, p) = file("bad", "rows = \"ten\"");
        let (c, w) = LauncherConfig::load(&p);
        assert_eq!(c, LauncherConfig::default());
        let w = w.unwrap();
        assert!(w.starts_with("launcher.toml invalid") && w.contains("u16"), "{w}");
        let d = TempDir::new("launcher-config-dir");
        std::fs::create_dir_all(config_path(&d)).unwrap(); // a directory where the file should be
        let (c, w) = LauncherConfig::load(&config_path(&d));
        assert_eq!(c, LauncherConfig::default());
        let w = w.unwrap();
        assert!(w.starts_with("launcher.toml unreadable"), "{w}");
    }

    #[test]
    fn config_path_is_launcher_toml() {
        assert_eq!(config_path(Path::new("/c/alpheus")), Path::new("/c/alpheus/launcher.toml"));
    }
}
