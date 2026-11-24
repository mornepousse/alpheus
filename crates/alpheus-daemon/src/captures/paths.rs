//! Where a screenshot is saved and what it is called: pure arithmetic on strings and paths.
//!
//! The folder is `[captures] dir`, else `$XDG_PICTURES_DIR/screenshots`, else
//! `~/Pictures/screenshots`; the file is `%Y%m%d-%H%M%S.png` in local time. `user_dir` reads one
//! entry of `~/.config/user-dirs.dirs`, since a session often sets no `XDG_PICTURES_DIR`
//! variable. The caller reads the environment, the file and the clock and hands them in.
//!
//! Never:
//! - read the environment, a file or the clock: the glue (`app.rs`) does, so a test picks any value;
//! - expand more than `~/` and the `$HOME` that opens a `user-dirs.dirs` value.
//!
//! See: docs/features/captures.md and docs/configuration.md (`[captures] dir`).

use crate::clock::LocalTime;
use std::path::{Path, PathBuf};

/// The folder a screenshot is saved in: `configured` (`[captures] dir`: absolute or `~/…`) when set,
/// else `pictures/screenshots`, else `home/Pictures/screenshots`; with no home, a relative path.
pub fn save_dir(configured: &str, pictures: Option<&Path>, home: Option<&Path>) -> PathBuf {
    if !configured.is_empty() {
        return match configured.strip_prefix("~/") {
            Some(rest) => home.map_or_else(|| PathBuf::from(rest), |h| h.join(rest)),
            None => PathBuf::from(configured),
        };
    }
    match pictures {
        Some(p) => p.join("screenshots"),
        None => home.map_or_else(PathBuf::new, Path::to_path_buf).join("Pictures/screenshots"),
    }
}

/// The value of `key` (`XDG_PICTURES_DIR`) in the text of `user-dirs.dirs`: absolute, or starting
/// with `$HOME/` (then `home`). None for a missing, empty, relative or otherwise expanded value.
pub fn user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let line = text.lines().map(str::trim).find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix('='))?;
    let value = line.trim().trim_matches('"');
    if let Some(rest) = value.strip_prefix("$HOME/") {
        return (!rest.is_empty() && !rest.contains('$')).then(|| home.join(rest));
    }
    (value.starts_with('/') && value.len() > 1 && !value.contains('$')).then(|| PathBuf::from(value))
}

/// `%Y%m%d-%H%M%S.png` of `t`.
pub fn file_name(t: &LocalTime) -> String {
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}.png", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

/// `name` for the `n`-th try: itself at 0, else `-n` before the extension.
pub fn numbered(name: &str, n: u32) -> String {
    match (n, name.rsplit_once('.')) {
        (0, _) => name.to_string(),
        (n, Some((stem, ext))) => format!("{stem}-{n}.{ext}"),
        (n, None) => format!("{name}-{n}"),
    }
}

#[cfg(test)]
mod tests;
