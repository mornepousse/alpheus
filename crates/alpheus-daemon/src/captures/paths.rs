//! Where a screenshot or a recording is saved and what it is called, and the recorder's geometry:
//! pure arithmetic on strings and paths.
//!
//! The folder is `[captures] dir`, else `$XDG_PICTURES_DIR/screenshots`, else
//! `~/Pictures/screenshots`; the file is `%Y%m%d-%H%M%S.png` in local time. A recording goes to
//! `$XDG_VIDEOS_DIR`, else `~/Videos`, as `screenrecord-%Y%m%d-%H%M%S.mp4`. `user_dir` reads one
//! entry of `~/.config/user-dirs.dirs`, since a session often sets no `XDG_PICTURES_DIR` or
//! `XDG_VIDEOS_DIR` variable. The caller reads the environment, the file and the clock and hands
//! them in.
//!
//! Never:
//! - read the environment, a file or the clock: the glue (`app.rs`) does, so a test picks any value;
//! - expand more than `~/` and the `$HOME` that opens a `user-dirs.dirs` value.
//!
//! See: docs/features/captures.md and docs/configuration.md (`[captures] dir`).

use super::Rect;
use crate::clock::LocalTime;
use std::path::{Path, PathBuf};

/// The folder a screenshot is saved in: `configured` (`[captures] dir`: absolute or `~/…`) when set,
/// else `pictures/screenshots`, else `home/Pictures/screenshots`. None when it needs a home that
/// there is not: a screenshot never lands in a relative path.
pub fn save_dir(configured: &str, pictures: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if !configured.is_empty() {
        return match configured.strip_prefix("~/") {
            Some(rest) => home.map(|h| h.join(rest)),
            None => Some(PathBuf::from(configured)),
        };
    }
    match pictures {
        Some(p) => Some(p.join("screenshots")),
        None => home.map(|h| h.join("Pictures/screenshots")),
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

/// The folder a recording goes to: `videos` (`$XDG_VIDEOS_DIR`, or the videos entry of
/// `user-dirs.dirs`) itself, else `home/Videos`. None without a home: never a relative path.
pub fn videos_dir(videos: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match videos {
        Some(v) => Some(v.to_path_buf()),
        None => home.map(|h| h.join("Videos")),
    }
}

/// `screenrecord-%Y%m%d-%H%M%S.mp4` of `t`, the recording's start.
pub fn video_name(t: &LocalTime) -> String {
    format!("screenrecord-{:04}{:02}{:02}-{:02}{:02}{:02}.mp4", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

/// The recorder's geometry, `x,y wxh` as slurp prints it: `area` (an output's logical pixels) moved
/// by `origin`, that output's place in the compositor's layout.
pub fn geometry(origin: (i32, i32), area: Rect) -> String {
    // `saturating_add` stops at the limit instead of wrapping round: a layout that far out is not
    // a real one, and the recorder refuses the geometry.
    let (x, y) = (origin.0.saturating_add(area.x), origin.1.saturating_add(area.y));
    format!("{x},{y} {}x{}", area.width, area.height)
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
