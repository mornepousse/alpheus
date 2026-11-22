//! The light/dark mode in effect, as the daemon publishes it for the TUI (Mae, 2026-10-07: the TUI
//! never uses D-Bus): `$XDG_RUNTIME_DIR/alpheus/mode`, holding `dark` or `light` and a newline. The
//! daemon writes it (`alpheus_daemon::appearance::write_mode`); the TUI reads it in mode "auto".

use super::Mode;
use super::settings::read_capped;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The directory under $XDG_RUNTIME_DIR, and the file in it.
pub const MODE_DIR: &str = "alpheus";
pub const MODE_FILE: &str = "mode";

/// $XDG_RUNTIME_DIR, absolute only. Unset, empty or relative: None, so nothing is written or read
/// (never `paths::runtime_dir`'s /run/user/<uid> guess).
pub fn runtime_from(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    crate::paths::xdg_abs(var, "XDG_RUNTIME_DIR")
}

/// `runtime/alpheus/mode`.
pub fn mode_path(runtime: &Path) -> PathBuf {
    runtime.join(MODE_DIR).join(MODE_FILE)
}

/// The file's whole text for `mode`.
pub fn mode_text(mode: Mode) -> &'static str {
    match mode {
        Mode::Dark => "dark\n",
        Mode::Light => "light\n",
    }
}

/// `light` (surrounding whitespace ignored) is Light; anything else is Dark.
pub fn parse_mode(text: &str) -> Mode {
    if text.trim() == "light" { Mode::Light } else { Mode::Dark }
}

/// The published mode under `runtime` (None: XDG_RUNTIME_DIR unset). No file, unreadable: Dark.
pub fn read_mode(runtime: Option<&Path>) -> Mode {
    runtime.and_then(|r| read_capped(&mode_path(r)).ok()).map_or(Mode::Dark, |t| parse_mode(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn the_runtime_directory_only_when_set_and_absolute() {
        assert_eq!(runtime_from(vars(&[("XDG_RUNTIME_DIR", "/run/user/1000")])), Some(PathBuf::from("/run/user/1000")));
        assert_eq!(runtime_from(vars(&[])), None, "unset: no file, never the /run/user/<uid> guess");
        assert_eq!(runtime_from(vars(&[("XDG_RUNTIME_DIR", "")])), None, "empty: ignored");
        assert_eq!(runtime_from(vars(&[("XDG_RUNTIME_DIR", "rel/run")])), None, "relative: ignored");
        assert_eq!(mode_path(Path::new("/run/user/1000")), Path::new("/run/user/1000/alpheus/mode"));
    }

    #[test]
    fn the_text_round_trips_and_anything_else_is_dark() {
        assert_eq!((mode_text(Mode::Dark), mode_text(Mode::Light)), ("dark\n", "light\n"));
        for m in [Mode::Dark, Mode::Light] {
            assert_eq!(parse_mode(mode_text(m)), m);
        }
        assert_eq!(parse_mode("light"), Mode::Light, "no newline");
        assert_eq!(parse_mode("  light \n"), Mode::Light, "whitespace");
        for other in ["", "dark", "Light", "LIGHT\n", "lightish", "2", "light\nlight"] {
            assert_eq!(parse_mode(other), Mode::Dark, "{other:?}");
        }
    }

    #[test]
    fn reading_falls_back_to_dark() {
        let run = std::env::temp_dir().join(format!("alpheus-{}-mode-read", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        assert_eq!(read_mode(None), Mode::Dark, "XDG_RUNTIME_DIR unset");
        assert_eq!(read_mode(Some(&run)), Mode::Dark, "no file (the daemon is not running)");
        std::fs::create_dir_all(run.join(MODE_DIR)).unwrap();
        std::fs::write(mode_path(&run), "light\n").unwrap();
        assert_eq!(read_mode(Some(&run)), Mode::Light);
        std::fs::write(mode_path(&run), "garbage").unwrap();
        assert_eq!(read_mode(Some(&run)), Mode::Dark);
        let _ = std::fs::remove_dir_all(&run);
    }

    #[test]
    fn a_fifo_or_an_oversized_mode_file_is_dark_and_never_blocks() {
        let run = std::env::temp_dir().join(format!("alpheus-{}-mode-fifo", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(run.join(MODE_DIR)).unwrap();
        let c = std::ffi::CString::new(mode_path(&run).to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path in a directory this test owns.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let r = run.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_mode(Some(&r)));
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)).expect("a FIFO blocked the read"), Mode::Dark);
        std::fs::remove_file(mode_path(&run)).unwrap();
        std::fs::write(mode_path(&run), format!("{}light\n", " ".repeat(70_000))).unwrap();
        assert_eq!(read_mode(Some(&run)), Mode::Dark, "`light` lies past the cap");
        let _ = std::fs::remove_dir_all(&run);
    }
}
