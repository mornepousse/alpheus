//! `coredumpctl list --json=short`.
//!
//! Never:
//! - launch a tool or open a file: the backend does that, and these only read what it printed;
//! - keep a field the view does not show: what journald or `coredumpctl` adds is dropped at the
//!   parse, so a secret in the environment or a command line is never carried;
//! - trust a text: everything from outside passes `text::clean_line` before it is kept.
//!
//! See: docs/features/control-center.md ("Diag"), docs/decisions.md (the Diag entries),
//! AGENTS.md ("No private data in fixtures or logs").

use crate::text::clean_line;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crash {
    /// Microseconds since the epoch.
    pub time_us: u64,
    pub pid: u32,
    pub signal: u8,
    pub core_present: bool,
    /// The executable's basename.
    pub exe_name: String,
}

/// The most crashes kept.
pub const MAX_CRASHES: usize = 20;

/// The crashes of `coredumpctl list --json=short`, at most MAX_CRASHES. No signal reads 0, no
/// executable an empty name; a core stored in the journal counts as present.
pub fn parse_crashes(json: &[u8]) -> Result<Vec<Crash>, String> {
    let list: Vec<serde_json::Value> = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    Ok(list
        .iter()
        .take(MAX_CRASHES)
        .map(|v| {
            let exe = v.get("exe").and_then(|x| x.as_str()).unwrap_or("");
            // Cleaned first: a control character must not hide a `/` from the split.
            let exe = clean_line(exe, 4096);
            let base = exe.rsplit('/').next().unwrap_or("");
            Crash {
                time_us: v.get("time").and_then(|x| x.as_u64()).unwrap_or(0),
                pid: v.get("pid").and_then(|x| x.as_u64()).and_then(|x| u32::try_from(x).ok()).unwrap_or(0),
                signal: v.get("sig").and_then(|x| x.as_u64()).and_then(|x| u8::try_from(x).ok()).unwrap_or(0),
                core_present: matches!(v.get("corefile").and_then(|x| x.as_str()), Some("present" | "journal")),
                exe_name: clean_line(base, 100),
            }
        })
        .collect())
}

/// The conventional name of a Linux signal number.
pub fn signal_name(n: u8) -> &'static str {
    const NAMES: [&str; 32] = [
        "none",
        "SIGHUP",
        "SIGINT",
        "SIGQUIT",
        "SIGILL",
        "SIGTRAP",
        "SIGABRT",
        "SIGBUS",
        "SIGFPE",
        "SIGKILL",
        "SIGUSR1",
        "SIGSEGV",
        "SIGUSR2",
        "SIGPIPE",
        "SIGALRM",
        "SIGTERM",
        "SIGSTKFLT",
        "SIGCHLD",
        "SIGCONT",
        "SIGSTOP",
        "SIGTSTP",
        "SIGTTIN",
        "SIGTTOU",
        "SIGURG",
        "SIGXCPU",
        "SIGXFSZ",
        "SIGVTALRM",
        "SIGPROF",
        "SIGWINCH",
        "SIGIO",
        "SIGPWR",
        "SIGSYS",
    ];
    NAMES.get(usize::from(n)).copied().unwrap_or("SIG?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_basename_is_taken_after_cleaning() {
        let json = r#"[{"time":1,"pid":1,"sig":11,"corefile":"present","exe":"/bin/a\u001b]0;x/y\u0007b"}]"#;
        assert_eq!(parse_crashes(json.as_bytes()).unwrap()[0].exe_name, "ab", "a `/` inside an escape sequence is not a separator");
    }

    #[test]
    fn crashes_from_recorded_json() {
        let crashes = parse_crashes(include_bytes!("../../tests/fixtures/diag-coredumps.json")).unwrap();
        assert_eq!(crashes.len(), 3);
        assert_eq!(
            crashes[0],
            Crash { time_us: 1_763_700_000_000_000, pid: 4242, signal: 11, core_present: true, exe_name: "example-app".into() }
        );
        assert_eq!(crashes[1].exe_name, "example-sync", "basename, escape sequence removed");
        assert!(!crashes[1].core_present);
        assert_eq!((crashes[2].signal, crashes[2].exe_name.as_str()), (0, ""), "no signal, no executable");
        assert!(parse_crashes(b"garbage").is_err());
        assert_eq!(parse_crashes(b"[]"), Ok(vec![]));
    }

    #[test]
    fn signals_are_named() {
        assert_eq!(signal_name(11), "SIGSEGV");
        assert_eq!(signal_name(6), "SIGABRT");
        assert_eq!(signal_name(9), "SIGKILL");
        assert_eq!(signal_name(0), "none");
        assert_eq!(signal_name(200), "SIG?");
    }

    #[test]
    fn the_list_is_bounded() {
        let many: Vec<String> =
            (0..30).map(|i| format!(r#"{{"time":{i},"pid":1,"sig":11,"corefile":"present","exe":"/bin/x"}}"#)).collect();
        assert_eq!(parse_crashes(format!("[{}]", many.join(",")).as_bytes()).unwrap().len(), MAX_CRASHES);
    }
}
