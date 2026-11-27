//! The wallpaper picker's pure part: `launcher.toml`'s `[wallpaper]`, the folder's listing, the
//! name guard, the setter's argv and the rotation's next name.
//!
//! alpheus never draws a wallpaper: the user's setter (swaybg, wbg, swww) does, started with
//! `{file}`, the absolute path of a name listed from `dir`. Off (`dir` empty) nothing here runs.
//! No log: a file name, the folder or an argv never reaches one (the callers log fixed words).
//!
//! See: docs/configuration.md (`[wallpaper]`), docs/superpowers/specs/2026-11-27-wallpaper-design.md.

pub mod app;
pub mod setter;

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// At most this many names listed: the level's bound (about 100 KB of names).
pub const MAX_FILES: usize = 2000;
/// The listed extensions, matched in any case.
pub const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];
/// The one element of `set` (and of `then`) replaced by the chosen file's absolute path, whole.
pub const FILE_PLACEHOLDER: &str = "{file}";

/// `[wallpaper]`: all off by default (`dir` empty: the feature is absent).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WallpaperConfig {
    /// The folder; a leading `~/` is the home. Absolute after expansion.
    pub dir: String,
    /// The setter's argv: `{file}` exactly once, a whole element, never the program.
    pub set: Vec<String>,
    /// The setter runs until replaced (swaybg, wbg); `false` for one that exits once applied.
    pub restart: bool,
    /// Run after a successful set: `{file}` at most once, a whole element, never the program.
    pub then: Vec<String>,
    /// Keep the last choice's name and apply it again when the daemon starts.
    pub remember: bool,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        WallpaperConfig { dir: String::new(), set: Vec::new(), restart: true, then: Vec::new(), remember: true }
    }
}

impl WallpaperConfig {
    /// Whether the picker exists: a folder and a setter.
    pub fn enabled(&self) -> bool {
        !self.dir.is_empty() && !self.set.is_empty()
    }
}

/// `dir` empty, or absolute or `~/…` without NUL, and then `set` required; `set` and `then` by
/// `check_command`. The messages name keys, never the folder nor an argument.
pub fn validate(w: &WallpaperConfig) -> Result<(), String> {
    if !w.dir.is_empty() {
        if !(w.dir.starts_with('/') || w.dir.starts_with("~/")) || w.dir.contains('\0') {
            return Err("wallpaper.dir must be an absolute path or start with \"~/\"".into());
        }
        if w.set.is_empty() {
            return Err(format!("wallpaper.set empty (expected: [\"program\", \"argument\", …, \"{FILE_PLACEHOLDER}\"])"));
        }
    }
    check_command("wallpaper.set", &w.set, true)?;
    check_command("wallpaper.then", &w.then, false)
}

/// An empty `argv` passes (its key's presence is checked by the caller); otherwise a non-blank
/// program that is not `{file}`, and `{file}` as a whole element only, exactly once when
/// `required`, at most once otherwise.
fn check_command(key: &str, argv: &[String], required: bool) -> Result<(), String> {
    let Some(program) = argv.first() else { return Ok(()) };
    if program.trim().is_empty() {
        return Err(format!("{key}: empty program"));
    }
    if program.contains(FILE_PLACEHOLDER) {
        return Err(format!("{key}: the program must not contain \"{FILE_PLACEHOLDER}\""));
    }
    let whole = argv.iter().filter(|a| a.as_str() == FILE_PLACEHOLDER).count();
    let partial = argv.iter().any(|a| a.contains(FILE_PLACEHOLDER) && a.as_str() != FILE_PLACEHOLDER);
    let count_ok = if required { whole == 1 } else { whole <= 1 };
    if !count_ok || partial {
        let how = if required { "exactly once" } else { "at most once" };
        return Err(format!("{key} must hold \"{FILE_PLACEHOLDER}\" {how}, as a whole argument"));
    }
    Ok(())
}

/// `dir` with a leading `~/` replaced by `home`; None unless the result is absolute (no home, a
/// relative home, `~user`, a relative `dir`): the folder is never relative to the daemon's. None
/// too for a home that is not UTF-8: `fill` puts the path in an argv as text, and the path must
/// reach the setter unchanged.
pub fn resolve_dir(dir: &str, home: Option<&Path>) -> Option<PathBuf> {
    let path = match dir.strip_prefix("~/") {
        Some(rest) => home?.join(rest),
        None => PathBuf::from(dir),
    };
    (path.is_absolute() && path.to_str().is_some()).then_some(path)
}

/// Whether `name` ends in `.<extension>` of EXTENSIONS (any case), with something before the dot.
pub fn is_image_name(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) => !stem.is_empty() && EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)),
        None => false,
    }
}

/// Whether `name` names an entry of the folder itself: not empty, no `/`, no NUL, not `.` or `..`.
/// A remembered or chosen name passes this before it is joined to the folder.
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// The folder's images by name, sorted; `cut` when there were more than the cap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub names: Vec<String>,
    pub cut: bool,
}

/// One `read_dir` of `dir`: the image names (`is_image_name`) of regular files and of links to
/// them, sorted, at most `cap`. A name that is not UTF-8, holds a control character or a line
/// separator (it would not show on one line) or an invisible formatting character (a bidirectional
/// override draws `evil\u{202E}gnp.exe.png` as another name) is skipped. Reading stops at the
/// `cap + 1`-th image: it only says the list was cut (which `cap` names are kept is then the
/// folder's order).
pub fn list(dir: &Path, cap: usize) -> std::io::Result<Listing> {
    let mut names = Vec::new();
    let mut cut = false;
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else { continue };
        if !is_plain_name(&name) || !is_image_name(&name) || name.contains(drawn_otherwise) {
            continue;
        }
        let file = match entry.file_type() {
            Ok(t) if t.is_file() => true,
            Ok(t) if t.is_symlink() => std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()),
            _ => false,
        };
        if !file {
            continue;
        }
        if names.len() == cap {
            cut = true;
            break;
        }
        names.push(name);
    }
    names.sort_unstable();
    Ok(Listing { names, cut })
}

/// Whether `c` would not draw as itself on one line: a control character, a line or paragraph
/// separator, or one of the invisible formatting characters (`crate::text::invisible`).
fn drawn_otherwise(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') || crate::text::invisible(c)
}

/// `argv` with the element exactly `{file}` replaced by `file`, whole: a name with spaces or a
/// leading dash stays one argument (and `file` is absolute, so it never starts with `-`). `file` is
/// a `resolve_dir` folder joined to a listed name, both UTF-8, so the conversion never alters it.
pub fn fill(argv: &[String], file: &Path) -> Vec<String> {
    argv.iter().map(|a| if a == FILE_PLACEHOLDER { file.to_string_lossy().into_owned() } else { a.clone() }).collect()
}

/// The name after `current` in `names`, wrapping; the first when `current` is absent or not listed.
pub fn next_after<'a>(names: &'a [String], current: Option<&str>) -> Option<&'a str> {
    let at = current.and_then(|c| names.iter().position(|n| n == c));
    let next = at.map_or(0, |i| (i + 1) % names.len());
    names.get(next).map(String::as_str)
}

#[cfg(test)]
mod tests;
