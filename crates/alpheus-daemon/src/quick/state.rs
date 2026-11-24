//! The state of a configured toggle: whether the keep-awake or night-light toggle is on, read from
//! the `state` setting you gave it.
//!
//! The panel asks when it opens and 300 ms after the toggle ran, and never watches. `file:<path>` is
//! on while the file exists; `process:<name>` is on while a process with that `comm` (the name the
//! kernel keeps for it) runs, found by one bounded scan of `/proc`. Anything that cannot be told is
//! `None`, and the row shows `·`.
//!
//! Never:
//! - write or create a file, or make a raw system call (guardrail
//!   `the_quick_model_and_its_state_reader_only_read`);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`);
//! - guess "off": a state that cannot be read is unknown, not off;
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md,
//! and docs/decisions.md ("A toggle's state that cannot be read shows `·`, never off").

use crate::config::{StateForm, state_form};
use alpheus_common::theme::settings::read_capped;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// `/proc` entries looked at, at most.
pub const MAX_PROC_ENTRIES: usize = 4096;

/// The environment variable `key` as a directory, or None when it is unset or empty.
fn env_dir(key: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    match env(key) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// `path` with `$XDG_RUNTIME_DIR/` or `~/` expanded from `env`; None when that variable is unset
/// or empty, or the path is not absolute.
pub fn expand(path: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    // `?` after a call that gives None ends the function with None: the path cannot be expanded.
    if let Some(rest) = path.strip_prefix("$XDG_RUNTIME_DIR/") {
        return Some(env_dir("XDG_RUNTIME_DIR", env)?.join(rest));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(env_dir("HOME", env)?.join(rest));
    }
    if path.starts_with('/') { Some(PathBuf::from(path)) } else { None }
}

/// Whether a process named `name` runs: the `comm` of each numeric entry of `proc_root`, each read
/// capped, MAX_PROC_ENTRIES entries at most.
pub fn process_running(proc_root: &Path, name: &str) -> bool {
    scan(proc_root, name, MAX_PROC_ENTRIES)
}

/// Whether a `/proc` entry's name is a process id: only ASCII digits.
fn is_process_id(entry_name: &OsStr) -> bool {
    match entry_name.to_str() {
        Some(text) => !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// `process_running`, `limit` entries at most. An entry that cannot be read counts toward the limit.
fn scan(proc_root: &Path, name: &str, limit: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(proc_root) else { return false };
    for entry in entries.take(limit) {
        let Ok(entry) = entry else { continue };
        if !is_process_id(&entry.file_name()) {
            continue;
        }
        // A process that ended since the listing has no `comm` to read: it is not the one.
        if let Ok(comm) = read_capped(&entry.path().join("comm"))
            && comm.trim_end_matches('\n') == name
        {
            return true;
        }
    }
    false
}

/// A toggle's state from its `state` setting: Some(on) for a file or a process; None for "", a form
/// bar.toml would have refused, a path that cannot be expanded, or a file whose existence cannot
/// be told (any error but "not found": never "off" on a guess).
pub fn read(state: &str, proc_root: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Option<bool> {
    match state_form(state).ok()? {
        StateForm::Unknown => None,
        StateForm::File(p) => expand(&p, env)?.try_exists().ok(),
        StateForm::Process(name) => Some(process_running(proc_root, &name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-quick-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake /proc: numeric entries with their comm, and entries the scan must skip.
    fn fake_proc(root: &Path) {
        for (pid, comm) in [("1", "systemd\n"), ("4242", "hyprsunset\n"), ("77", "foot\n")] {
            fs::create_dir_all(root.join(pid)).unwrap();
            fs::write(root.join(pid).join("comm"), comm).unwrap();
        }
        fs::create_dir_all(root.join("self")).unwrap();
        fs::write(root.join("self").join("comm"), "wlsunset\n").unwrap();
        fs::write(root.join("uptime"), "1.0 2.0\n").unwrap();
    }

    fn none(_: &str) -> Option<OsString> {
        None
    }

    #[test]
    fn a_process_state_is_on_while_one_runs() {
        let root = tree("proc");
        fake_proc(&root);
        assert_eq!(read("process:hyprsunset", &root, &none), Some(true));
        assert_eq!(read("process:wlsunset", &root, &none), Some(false), "only numeric entries are processes");
        assert_eq!(read("process:hyprsun", &root, &none), Some(false), "the whole name");
        assert!(!scan(&root, "hyprsunset", 0), "the scan stops at its limit");
        assert_eq!(read("process:hyprsunset", &root.join("missing"), &none), Some(false), "no /proc: not running");
        let _ = fs::remove_dir_all(&root);
    }

    /// An empty variable counts as unset, and a path that is not absolute has no expansion.
    #[test]
    fn an_empty_variable_is_unset_and_a_relative_path_is_not_expanded() {
        let empty = |_: &str| Some(OsString::new());
        assert_eq!(expand("$XDG_RUNTIME_DIR/flag", &empty), None);
        assert_eq!(expand("~/flag", &empty), None);
        assert_eq!(expand("relative/flag", &none), None);
        assert_eq!(expand("/abs/flag", &none), Some(PathBuf::from("/abs/flag")));
    }

    /// `/proc` entries that are not processes (an empty name, `self`, `12a`, a name that is not text)
    /// are skipped.
    #[test]
    fn only_a_name_of_digits_is_a_process_id() {
        use std::os::unix::ffi::OsStrExt;
        assert!(is_process_id(OsStr::new("4242")));
        assert!(!is_process_id(OsStr::new("")));
        assert!(!is_process_id(OsStr::new("self")));
        assert!(!is_process_id(OsStr::new("12a")));
        assert!(!is_process_id(OsStr::from_bytes(b"\xff12")));
    }

    /// Entries that are not processes do not end the scan: with 25 of them created before the
    /// process and 25 after, the process is found whether the directory lists them in creation
    /// order, in reverse, or (not controlled here) in some other order.
    #[test]
    fn entries_that_are_not_processes_do_not_end_the_scan() {
        let root = tree("skipped");
        for i in 0..25 {
            fs::write(root.join(format!("note{i}")), "").unwrap();
        }
        fs::create_dir_all(root.join("7")).unwrap();
        fs::write(root.join("7").join("comm"), "foot\n").unwrap();
        for i in 25..50 {
            fs::write(root.join(format!("note{i}")), "").unwrap();
        }
        assert!(scan(&root, "foot", MAX_PROC_ENTRIES));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_state_is_on_while_it_exists_and_expands_the_runtime_dir_and_home() {
        let (runtime, home) = (tree("runtime"), tree("home"));
        let env = |k: &str| match k {
            "XDG_RUNTIME_DIR" => Some(runtime.clone().into_os_string()),
            "HOME" => Some(home.clone().into_os_string()),
            _ => None,
        };
        let flag = "file:$XDG_RUNTIME_DIR/garder-eveille";
        assert_eq!(read(flag, &home, &env), Some(false));
        fs::write(runtime.join("garder-eveille"), "").unwrap();
        assert_eq!(read(flag, &home, &env), Some(true));
        fs::create_dir_all(home.join(".cache")).unwrap();
        fs::write(home.join(".cache/night"), "").unwrap();
        assert_eq!(read("file:~/.cache/night", &home, &env), Some(true));
        assert_eq!(read(&format!("file:{}", runtime.join("garder-eveille").display()), &home, &env), Some(true), "an absolute path");
        assert_eq!(read("", &home, &env), None, "unknown: ·");
        assert_eq!(read(flag, &home, &none), None, "the variable unset: unknown, never a guess");
        assert_eq!(read("pid:1", &home, &env), None, "not a form: unknown");
        let _ = fs::remove_dir_all(&runtime);
        let _ = fs::remove_dir_all(&home);
    }

    /// A file whose existence cannot be told (here ENOTDIR: a parent is a plain file) is unknown
    /// (`·`), never "off"; only a missing file is.
    #[test]
    fn an_unreadable_file_state_is_unknown_never_off() {
        let root = tree("unreadable");
        fs::write(root.join("plain"), "").unwrap();
        assert_eq!(read(&format!("file:{}", root.join("plain/flag").display()), &root, &none), None, "cannot tell: ·");
        assert_eq!(read(&format!("file:{}", root.join("missing").display()), &root, &none), Some(false), "missing: off");
        let _ = fs::remove_dir_all(&root);
    }
}
