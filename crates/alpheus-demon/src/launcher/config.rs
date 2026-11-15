//! ~/.config/alpheus/launcher.toml, optional, read at each open of the
//! launcher (nothing kept while it is closed). Absent: defaults, no message.
//! Invalid: defaults and one line in the daemon's log. Never an abort.
//!
//! `terminal` and `terminal_hold` are separate keys, one per flag style a terminal emulator
//! needs (plain vs. held-open): neither is derived from the other, so switching away from foot
//! means setting both.
//!
//! ```toml
//! terminal = ["foot", "-e"]                        # Terminal=true apps, plain (no hold)
//! terminal_hold = ["foot", "--hold", "-e"]          # a command or typed shell text, Enter (stays open)
//! copy = ["wl-copy"]                               # the calculator's Enter: the result on its stdin; [] hides the action
//! search = "https://duckduckgo.com/?q={query}"     # the `?` prefix's URL, {query} percent-encoded; "" disables `?`
//! open = ["xdg-open"]                              # opens that URL (the URL is appended); [] disables `?`
//! rows = 10                                        # result rows, 3 to 30
//! history = 200                                    # entries kept, 0 to 1000; 0 disables the history
//! focus = ["wm-set", "focus-window", "{window}"]   # {window}: address:0x… (Hyprland), id:N (niri)
//! [[sources]]                                      # menu scripts whose lines are searchable (at most 8)
//! name = "Outils"                                  # 1 to 32 characters, unique; in logs and the history key
//! list = ["tools-menu", "--list"]                  # argv, no shell, run once per open on the scan thread
//! run = ["tools-menu", "{line}"]                   # argv; the one element "{line}" becomes the chosen line
//! [prefixes]
//! commands = ">"
//! windows = "w "
//! ```

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const ROWS_MIN: u16 = 3;
pub const ROWS_MAX: u16 = 30;
/// Bounds the history file: HISTORY_MAX lines of at most ~680 bytes (history::ID_MAX).
pub const HISTORY_MAX: usize = 1_000;
/// At most this many `[[sources]]` (sources spec § 1).
pub const MAX_SOURCES: usize = 8;
/// A source's name: 1 to NAME_MAX characters (not bytes).
pub const NAME_MAX: usize = 32;
/// A source's line, cleaned, is at most this many bytes; longer ones are skipped (sources spec § 2).
pub const LINE_MAX: usize = 512;
/// The one element of `run` replaced by the chosen line, whole: never part of an argument.
pub const LINE_PLACEHOLDER: &str = "{line}";

/// One `[[sources]]` table: a menu script whose lines the launcher searches (sources spec § 1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub name: String,
    /// Run once per open, without a shell: its standard output's lines are the entries.
    pub list: Vec<String>,
    /// Run when an entry is chosen; its `{line}` element becomes the whole line, one argument.
    pub run: Vec<String>,
}

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
    /// `Terminal=true` apps: never held (the app manages its own terminal's lifetime).
    pub terminal: Vec<String>,
    /// A $PATH command or typed shell text, Enter (spec § 1): held open after the command
    /// exits, so its output stays readable. Ctrl+Enter never uses this (it runs detached).
    pub terminal_hold: Vec<String>,
    /// The calculator's copy command (small features 1): the text goes on its stdin, never on
    /// its argv; empty hides the action.
    pub copy: Vec<String>,
    /// The `?` prefix's URL template (launcher 2): `{query}` is replaced by the percent-encoded
    /// text; empty disables the prefix.
    pub search: String,
    /// Opens the search URL: the URL is appended as the last argument; empty disables the prefix.
    pub open: Vec<String>,
    pub rows: u16,
    /// Entries kept; 0 disables the history (never read, never written).
    pub history: usize,
    pub prefixes: Prefixes,
    pub focus: Vec<String>,
    /// Command sources (sources spec § 1): none by default.
    pub sources: Vec<SourceConfig>,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        LauncherConfig {
            terminal: vec!["foot".into(), "-e".into()],
            terminal_hold: vec!["foot".into(), "--hold".into(), "-e".into()],
            copy: vec!["wl-copy".into()],
            search: "https://duckduckgo.com/?q={query}".into(),
            open: vec!["xdg-open".into()],
            rows: 10,
            history: 200,
            prefixes: Prefixes::default(),
            focus: vec!["wm-set".into(), "focus-window".into(), "{window}".into()],
            sources: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    terminal: Vec<String>,
    terminal_hold: Vec<String>,
    copy: Vec<String>,
    search: String,
    open: Vec<String>,
    rows: u16,
    history: usize,
    prefixes: Prefixes,
    focus: Vec<String>,
    sources: Vec<SourceConfig>,
}

impl Default for Raw {
    fn default() -> Self {
        let d = LauncherConfig::default();
        Raw {
            terminal: d.terminal,
            terminal_hold: d.terminal_hold,
            copy: d.copy,
            search: d.search,
            open: d.open,
            rows: d.rows,
            history: d.history,
            prefixes: d.prefixes,
            focus: d.focus,
            sources: d.sources,
        }
    }
}

impl LauncherConfig {
    /// Whether the `?` prefix is on: a URL template and a program to open it with.
    pub fn search_enabled(&self) -> bool {
        !self.search.is_empty() && !self.open.is_empty()
    }

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

    /// Validation:
    /// - terminal, terminal_hold and focus: non-empty, with a non-blank program (copy may be
    ///   empty, otherwise the same rule);
    /// - search empty, or holding "{query}"; open may be empty, otherwise the same rule as copy;
    /// - focus contains "{window}";
    /// - rows in ROWS_MIN..=ROWS_MAX, history ≤ HISTORY_MAX;
    /// - prefixes non-empty, not starting with whitespace, neither one a prefix of the other
    ///   (equal included);
    /// - sources: `check_sources`.
    pub fn from_text(s: &str) -> Result<LauncherConfig, String> {
        let r: Raw = toml::from_str(s).map_err(|e| e.message().to_string())?;
        for (key, command) in [("terminal", &r.terminal), ("terminal_hold", &r.terminal_hold), ("focus", &r.focus)] {
            if command.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(format!("{key} empty (expected: [\"program\", \"argument\", …])"));
            }
        }
        if r.copy.first().is_some_and(|p| p.trim().is_empty()) {
            return Err("copy: empty program (expected: [\"program\", \"argument\", …], or [] for no copy)".into());
        }
        if r.open.first().is_some_and(|p| p.trim().is_empty()) {
            return Err("open: empty program (expected: [\"program\", \"argument\", …], or [] to disable `?`)".into());
        }
        if !r.search.is_empty() && !r.search.contains("{query}") {
            return Err(
                "search: no \"{query}\" placeholder (expected a URL such as \"https://duckduckgo.com/?q={query}\", or \"\" to disable `?`)"
                    .into(),
            );
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
        check_sources(&r.sources)?;
        Ok(LauncherConfig {
            terminal: r.terminal,
            terminal_hold: r.terminal_hold,
            copy: r.copy,
            search: r.search,
            open: r.open,
            rows: r.rows,
            history: r.history,
            prefixes: r.prefixes,
            focus: r.focus,
            sources: r.sources,
        })
    }
}

/// At most MAX_SOURCES; each name 1 to NAME_MAX characters, not blank, without a control character
/// or U+FFFD, unique; `list` and `run`
/// non-empty with a non-blank program; `run[0]` without "{line}"; "{line}" exactly once in `run`,
/// as a whole element (no partial substitution: a line is never split nor reinterpreted).
fn check_sources(sources: &[SourceConfig]) -> Result<(), String> {
    if sources.len() > MAX_SOURCES {
        return Err(format!("sources: {} configured, at most {MAX_SOURCES}", sources.len()));
    }
    for (i, s) in sources.iter().enumerate() {
        let at = format!("sources[{i}]");
        if !(1..=NAME_MAX).contains(&s.name.chars().count()) {
            return Err(format!("{at}.name must be 1 to {NAME_MAX} characters"));
        }
        if s.name.trim().is_empty() {
            return Err(format!("{at}.name must not be blank"));
        }
        // The name is part of every history key of the source ("source:<name>:<line>"), and the
        // history refuses ids holding either: the source's frecency would be silently off.
        if s.name.contains(|c: char| c.is_control() || c == '\u{FFFD}') {
            return Err(format!("{at}.name must not hold a control character or U+FFFD"));
        }
        if sources[..i].iter().any(|o| o.name == s.name) {
            return Err(format!("{at}.name {:?} is used twice", s.name));
        }
        for (key, command) in [("list", &s.list), ("run", &s.run)] {
            if command.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(format!("{at}.{key} empty (expected: [\"program\", \"argument\", …])"));
            }
        }
        if s.run[0].contains(LINE_PLACEHOLDER) {
            return Err(format!("{at}.run[0] (the program) must not contain \"{LINE_PLACEHOLDER}\""));
        }
        let whole = s.run.iter().filter(|a| a.as_str() == LINE_PLACEHOLDER).count();
        let partial = s.run.iter().any(|a| a.contains(LINE_PLACEHOLDER) && a.as_str() != LINE_PLACEHOLDER);
        if whole != 1 || partial {
            return Err(format!("{at}.run must hold \"{LINE_PLACEHOLDER}\" exactly once, as a whole argument"));
        }
    }
    Ok(())
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
        assert_eq!(c.terminal_hold, ["foot", "--hold", "-e"]);
        assert_eq!((c.rows, c.history), (10, 200));
        assert_eq!((c.prefixes.commands.as_str(), c.prefixes.windows.as_str()), (">", "w "));
        assert_eq!(c.focus, ["wm-set", "focus-window", "{window}"]);
        assert!(c.sources.is_empty(), "no source by default");
    }

    #[test]
    fn settings_are_read() {
        let text = "terminal = [\"foot\"]\nterminal_hold = [\"foot\", \"--hold\"]\nrows = 6\nhistory = 0\nfocus = [\"my-wm\", \"focus\", \"--window={window}\"]\n[prefixes]\ncommands = \"!\"\nwindows = \"win:\"\n";
        let (_dir, p) = file("ok", text);
        let (c, w) = LauncherConfig::load(&p);
        assert_eq!(w, None);
        let expected = LauncherConfig {
            terminal: vec!["foot".into()],
            terminal_hold: vec!["foot".into(), "--hold".into()],
            copy: vec!["wl-copy".into()],
            search: "https://duckduckgo.com/?q={query}".into(),
            open: vec!["xdg-open".into()],
            rows: 6,
            history: 0,
            prefixes: Prefixes { commands: "!".into(), windows: "win:".into() },
            focus: vec!["my-wm".into(), "focus".into(), "--window={window}".into()],
            sources: vec![],
        };
        assert_eq!(c, expected);
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let c = LauncherConfig::from_text("rows = 5\n[prefixes]\ncommands = \"$\"\n").unwrap();
        assert_eq!((c.rows, c.history, c.prefixes.commands.as_str(), c.prefixes.windows.as_str()), (5, 200, "$", "w "));
        assert_eq!((c.terminal, c.focus), (LauncherConfig::default().terminal, LauncherConfig::default().focus));
        assert_eq!(c.terminal_hold, LauncherConfig::default().terminal_hold);
        assert_eq!(LauncherConfig::from_text("").unwrap(), LauncherConfig::default());
    }

    #[test]
    fn each_rule_refuses_with_its_reason() {
        for (text, needle) in [
            ("terminal = []", "terminal empty"),
            ("terminal = [\" \", \"-e\"]", "terminal empty"),
            ("terminal_hold = []", "terminal_hold empty"),
            ("terminal_hold = [\" \", \"-e\"]", "terminal_hold empty"),
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

    /// A `[[sources]]` table (sources spec § 1), `list` as tools-menu's, `run` given as TOML.
    fn source(name: &str, run: &str) -> String {
        format!("[[sources]]\nname = {name:?}\nlist = [\"tools-menu\", \"--list\"]\nrun = {run}\n")
    }

    #[test]
    fn sources_are_read_in_order() {
        let text = format!(
            "{}{}",
            source("Outils", r#"["tools-menu", "{line}"]"#),
            source("Réseau", r#"["net-menu", "--run", "{line}", "--quiet"]"#)
        );
        let c = LauncherConfig::from_text(&text).unwrap();
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            c.sources,
            [
                SourceConfig { name: "Outils".into(), list: v(&["tools-menu", "--list"]), run: v(&["tools-menu", "{line}"]) },
                SourceConfig {
                    name: "Réseau".into(),
                    list: v(&["tools-menu", "--list"]),
                    run: v(&["net-menu", "--run", "{line}", "--quiet"])
                },
            ]
        );
        assert_eq!((c.rows, c.history), (10, 200), "the other keys keep their defaults");
    }

    #[test]
    fn each_source_rule_refuses_with_its_reason() {
        let nine: String = (0..9).map(|i| source(&format!("s{i}"), r#"["x", "{line}"]"#)).collect();
        let cases = [
            (source("Outils", r#"["tools-menu"]"#), "exactly once, as a whole argument"),
            (source("Outils", r#"["tools-menu", "--line={line}"]"#), "exactly once, as a whole argument"),
            (source("Outils", r#"["tools-menu", "{line}", "--line={line}"]"#), "exactly once, as a whole argument"),
            (source("Outils", r#"["tools-menu", "{line}", "{line}"]"#), "exactly once, as a whole argument"),
            (source("Outils", r#"["{line}"]"#), "run[0] (the program)"),
            (source("Outils", r#"[]"#), "sources[0].run empty"),
            (source("Outils", r#"[" ", "{line}"]"#), "sources[0].run empty"),
            (source("", r#"["x", "{line}"]"#), "sources[0].name must be 1 to 32 characters"),
            (source("   ", r#"["x", "{line}"]"#), "must not be blank"),
            // Either would make every history key of the source invalid: frecency silently off.
            (source("Out\tils", r#"["x", "{line}"]"#), "sources[0].name must not hold a control character or U+FFFD"),
            (source("Out\nils", r#"["x", "{line}"]"#), "must not hold a control character"),
            (source("Out\u{FFFD}ils", r#"["x", "{line}"]"#), "must not hold a control character or U+FFFD"),
            (source(&"n".repeat(33), r#"["x", "{line}"]"#), "1 to 32 characters"),
            (
                format!("{}{}", source("Outils", r#"["x", "{line}"]"#), source("Outils", r#"["y", "{line}"]"#)),
                "sources[1].name \"Outils\" is used twice",
            ),
            (nine, "sources: 9 configured, at most 8"),
            ("[[sources]]\nname = \"Outils\"\nlist = []\nrun = [\"x\", \"{line}\"]\n".to_string(), "sources[0].list empty"),
            ("[[sources]]\nname = \"Outils\"\nrun = [\"x\", \"{line}\"]\n".to_string(), "missing field `list`"),
            (format!("{}icon = \"x\"\n", source("Outils", r#"["x", "{line}"]"#)), "unknown field `icon`"),
        ];
        for (text, needle) in cases {
            let e = LauncherConfig::from_text(&text).unwrap_err();
            assert!(e.contains(needle), "{text:?}: {e}");
        }
    }

    #[test]
    fn the_source_limits_themselves_are_accepted() {
        let eight: String = (0..8).map(|i| source(&format!("s{i}"), r#"["x", "{line}"]"#)).collect();
        assert_eq!(LauncherConfig::from_text(&eight).unwrap().sources.len(), 8);
        let long = "é".repeat(NAME_MAX); // 32 characters, 64 bytes: the rule counts characters
        assert!(LauncherConfig::from_text(&source(&long, r#"["x", "{line}"]"#)).is_ok());
        assert!(
            LauncherConfig::from_text(&source("a", r#"["x", "--", "{line}", "--then"]"#)).is_ok(),
            "{{line}} anywhere after the program"
        );
    }

    #[test]
    fn config_path_is_launcher_toml() {
        assert_eq!(config_path(Path::new("/c/alpheus")), Path::new("/c/alpheus/launcher.toml"));
    }

    #[test]
    fn copy_defaults_to_wl_copy_and_accepts_empty() {
        assert_eq!(LauncherConfig::default().copy, ["wl-copy"]);
        assert_eq!(LauncherConfig::from_text("").unwrap().copy, ["wl-copy"]);
        assert!(LauncherConfig::from_text("copy = []").unwrap().copy.is_empty());
        assert_eq!(LauncherConfig::from_text("copy = [\"xclip\", \"-sel\", \"c\"]").unwrap().copy, ["xclip", "-sel", "c"]);
        for text in ["copy = [\"\"]", "copy = [\" \", \"x\"]"] {
            let e = LauncherConfig::from_text(text).unwrap_err();
            assert!(e.contains("copy"), "{text}: {e}");
        }
    }

    #[test]
    fn search_and_open_defaults_and_validation() {
        let d = LauncherConfig::default();
        assert_eq!((d.search.as_str(), d.open.as_slice()), ("https://duckduckgo.com/?q={query}", ["xdg-open".to_string()].as_slice()));
        assert!(d.search_enabled());
        assert!(!LauncherConfig::from_text("search = \"\"").unwrap().search_enabled());
        assert!(!LauncherConfig::from_text("open = []").unwrap().search_enabled());
        let c = LauncherConfig::from_text("search = \"https://x/?q={query}&a={query}\"\nopen = [\"firefox\", \"--new-tab\"]").unwrap();
        assert_eq!(
            (c.search.as_str(), c.open.as_slice()),
            ("https://x/?q={query}&a={query}", ["firefox".to_string(), "--new-tab".to_string()].as_slice())
        );
        let e = LauncherConfig::from_text("search = \"https://x/\"").unwrap_err();
        assert!(e.contains("{query}"), "{e}");
        for text in ["open = [\"\"]", "open = [\" \", \"x\"]"] {
            let e = LauncherConfig::from_text(text).unwrap_err();
            assert!(e.contains("open"), "{text}: {e}");
        }
    }
}
