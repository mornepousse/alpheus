//! `systemctl --user show-environment`: `NAME=value` lines. The whole environment can hold secrets
//! (tokens exported by a shell profile), so only the names asked for are ever kept.

use crate::text::clean_line;

/// The `(name, value)` of each of `names` found in `stdout`, in the order of `names`.
pub fn session_vars(stdout: &str, names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .filter_map(|name| {
            stdout.lines().find_map(|line| {
                let value = line.strip_prefix(name)?.strip_prefix('=')?;
                // systemd quotes a value holding odd characters as $'…'.
                let value = value.strip_prefix("$'").and_then(|v| v.strip_suffix('\'')).unwrap_or(value);
                Some((name.to_string(), clean_line(value, 256)))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_environment_keeps_two_names_only() {
        let text = include_str!("../../tests/fixtures/diag-show-environment.txt");
        let vars = session_vars(text, &["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "MISSING"]);
        assert_eq!(
            vars,
            [("WAYLAND_DISPLAY".to_string(), "wayland-1".to_string()), ("XDG_CURRENT_DESKTOP".to_string(), "example".to_string())]
        );
        assert!(!format!("{vars:?}").contains("SECRET_TOKEN"));
        assert!(!format!("{vars:?}").contains("abc"));
    }

    #[test]
    fn quoted_values_and_hostile_text() {
        let vars = session_vars("A=$'x y'\nB=ok\u{1b}[2J\n=empty name\nA2=nope\n", &["A", "B"]);
        assert_eq!(vars, [("A".to_string(), "x y".to_string()), ("B".to_string(), "ok".to_string())]);
    }
}
