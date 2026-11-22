//! The state of a configured toggle (quick spec § 4), read when the panel opens and 300 ms after
//! its toggle ran, never watched: "file:<path>" is on while the file exists; "process:<name>" is on
//! while a process with that `comm` runs, found by one bounded scan of `/proc`. Reads only (a
//! guardrail); unknown or unreadable is None (the row shows `·`).

use crate::config::{StateForm, state_form};
use alpheus_common::theme::settings::read_capped;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `/proc` entries looked at, at most (spec § 6).
pub const MAX_PROC_ENTRIES: usize = 4096;

/// `path` with `$XDG_RUNTIME_DIR/` or `~/` expanded from `env`; None when that variable is unset
/// or empty, or the path is not absolute.
pub fn expand(path: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let var = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(rest) = path.strip_prefix("$XDG_RUNTIME_DIR/") {
        return Some(var("XDG_RUNTIME_DIR")?.join(rest));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(var("HOME")?.join(rest));
    }
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// Whether a process named `name` runs: the `comm` of each numeric entry of `proc_root`, each read
/// capped, MAX_PROC_ENTRIES entries at most.
pub fn process_running(proc_root: &Path, name: &str) -> bool {
    scan(proc_root, name, MAX_PROC_ENTRIES)
}

/// `process_running`, `limit` entries at most.
fn scan(proc_root: &Path, name: &str, limit: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(proc_root) else { return false };
    entries
        .take(limit)
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        .any(|e| read_capped(&e.path().join("comm")).is_ok_and(|c| c.trim_end_matches('\n') == name))
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

    /// Fix m1: a file whose existence cannot be told (here ENOTDIR: a parent is a plain file) is
    /// unknown (`·`), never "off"; only a missing file is.
    #[test]
    fn an_unreadable_file_state_is_unknown_never_off() {
        let root = tree("unreadable");
        fs::write(root.join("plain"), "").unwrap();
        assert_eq!(read(&format!("file:{}", root.join("plain/flag").display()), &root, &none), None, "cannot tell: ·");
        assert_eq!(read(&format!("file:{}", root.join("missing").display()), &root, &none), Some(false), "missing: off");
        let _ = fs::remove_dir_all(&root);
    }
}
