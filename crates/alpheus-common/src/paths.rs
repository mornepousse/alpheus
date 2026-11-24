//! alpheus's directories, shared by the TUI and the daemon: configuration
//! (~/.config/alpheus), runtime ($XDG_RUNTIME_DIR) and state
//! ($XDG_STATE_HOME/alpheus: the launcher's history).
//!
//! Never:
//! - spell a directory out twice: both programs ask here, so a path cannot drift between them;
//! - write outside them: the state files go through the daemon's hardened writer.
//!
//! See: docs/configuration.md ("Where the files are and when they are read"), docs/cli.md.

use std::ffi::OsString;
use std::path::PathBuf;

/// The XDG variable `name` when it holds an absolute path; unset, empty or relative: None, as the
/// XDG Base Directory spec requires. (Environment injected, like the `_from` functions below.)
pub(crate) fn xdg_abs(var: impl Fn(&str) -> Option<OsString>, name: &str) -> Option<PathBuf> {
    var(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// $XDG_CONFIG_HOME/alpheus, else ~/.config/alpheus, else .config/alpheus.
///
/// Unlike `state_dir` and `runtime_dir`, this does NOT go through `xdg_abs`: a relative (or empty)
/// $XDG_CONFIG_HOME is honoured as is today, and the TUI and the daemon both rely on finding the
/// same directory. Applying the spec's "absolute only" rule here would be a behaviour change for
/// anyone who sets one, so it is left as it is until someone decides to make it.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("alpheus")
}

pub fn runtime_dir() -> PathBuf {
    runtime_dir_from(|k| std::env::var_os(k))
}

/// `runtime_dir` with the environment injected. An empty or relative
/// $XDG_RUNTIME_DIR is ignored, as the XDG Base Directory spec requires
/// (the same rule as `state_dir_from`).
fn runtime_dir_from(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    xdg_abs(var, "XDG_RUNTIME_DIR")
        // SAFETY: getuid(2) cannot fail.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

/// $XDG_STATE_HOME/alpheus, else ~/.local/state/alpheus, else .local/state/alpheus.
pub fn state_dir() -> PathBuf {
    state_dir_from(|k| std::env::var_os(k))
}

/// `state_dir` with the environment injected. An empty or relative
/// $XDG_STATE_HOME is ignored, as the XDG Base Directory spec requires.
fn state_dir_from(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    xdg_abs(&var, "XDG_STATE_HOME")
        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
        .join("alpheus")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(*v))
    }

    #[test]
    fn xdg_abs_keeps_only_absolute_paths() {
        assert_eq!(xdg_abs(vars(&[("X", "/a/b")]), "X"), Some(PathBuf::from("/a/b")));
        assert_eq!(xdg_abs(vars(&[("X", "a/b")]), "X"), None, "relative");
        assert_eq!(xdg_abs(vars(&[("X", "")]), "X"), None, "empty");
        assert_eq!(xdg_abs(vars(&[]), "X"), None, "unset");
    }

    #[test]
    fn runtime_dir_prefers_xdg_runtime_dir() {
        assert_eq!(runtime_dir_from(vars(&[("XDG_RUNTIME_DIR", "/run/user/1000")])), Path::new("/run/user/1000"));
    }

    #[test]
    fn runtime_dir_falls_back_to_run_user_uid_when_empty_or_relative() {
        // SAFETY: getuid(2) cannot fail.
        let fallback = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        assert_eq!(runtime_dir_from(vars(&[])), fallback);
        assert_eq!(runtime_dir_from(vars(&[("XDG_RUNTIME_DIR", "")])), fallback, "empty: ignored");
        assert_eq!(runtime_dir_from(vars(&[("XDG_RUNTIME_DIR", "rel/run")])), fallback, "relative: ignored");
    }

    #[test]
    fn state_dir_prefers_xdg_state_home() {
        assert_eq!(state_dir_from(vars(&[("XDG_STATE_HOME", "/x/state"), ("HOME", "/home/u")])), Path::new("/x/state/alpheus"));
    }

    #[test]
    fn state_dir_falls_back_to_home_then_relative() {
        let home = Path::new("/home/u/.local/state/alpheus");
        assert_eq!(state_dir_from(vars(&[("HOME", "/home/u")])), home);
        assert_eq!(state_dir_from(vars(&[("XDG_STATE_HOME", ""), ("HOME", "/home/u")])), home, "empty: ignored");
        assert_eq!(state_dir_from(vars(&[("XDG_STATE_HOME", "rel/state"), ("HOME", "/home/u")])), home, "relative: ignored");
        assert_eq!(state_dir_from(vars(&[])), Path::new(".local/state/alpheus"));
    }
}
