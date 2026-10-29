//! The Appearance tab's data and its one write: the theme list with its display names, and
//! `theme.toml`'s `theme`, `mode`, `module_colors` and `style` lines, edited in place (everything else byte
//! for byte).

use alpheus_commun::theme::presets::PRESETS;
use alpheus_commun::theme::settings::THEME_FILE;
use std::io;
use std::path::Path;

/// The `theme` value for the wallpaper (matugen's colour files).
pub const WALLPAPER: &str = "wallpaper";

/// Human names of the presets, by `PRESETS` name.
const NAMES: [(&str, &str); 9] = [
    ("catppuccin", "Catppuccin"),
    ("gruvbox", "Gruvbox"),
    ("tokyo-night", "Tokyo Night"),
    ("rose-pine", "Rosé Pine"),
    ("everforest", "Everforest"),
    ("kanagawa", "Kanagawa"),
    ("solarized", "Solarized"),
    ("nord", "Nord"),
    ("dracula", "Dracula"),
];

/// One line of the theme list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// `theme.toml`'s value: a `PRESETS` name, or `WALLPAPER`.
    pub key: &'static str,
    pub label: &'static str,
    /// No light variant: "dark only".
    pub dark_only: bool,
}

/// The list: the wallpaper first, then `PRESETS` in order. A preset with no entry in `NAMES`
/// shows under its own name (a test keeps the table whole).
pub fn entries() -> Vec<Entry> {
    let wallpaper = Entry { key: WALLPAPER, label: "Wallpaper (matugen)", dark_only: false };
    let presets = PRESETS.iter().map(|p| {
        let label = NAMES.iter().find(|(k, _)| *k == p.name).map_or(p.name, |(_, l)| *l);
        Entry { key: p.name, label, dark_only: p.light.is_none() }
    });
    std::iter::once(wallpaper).chain(presets).collect()
}

/// Index of the first `[table]` / `[[array]]` header line, or the line count.
fn first_header(lines: &[&str]) -> usize {
    lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len())
}

/// `text` with the top-level `key = "value"` line replaced in place (indentation and line ending
/// kept, the old value and any trailing comment dropped), or inserted: after the last top-level
/// key line; else before the first table (and the comment lines glued to it); else at the end.
fn set_one(text: &str, key: &str, value: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let header = first_header(&lines);
    let is_key = |l: &str| {
        let body = l.trim_start();
        !body.is_empty() && !body.starts_with('#')
    };
    let mine = lines[..header].iter().position(|l| l.trim_start().strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('=')));
    if let Some(i) = mine {
        let line = lines[i];
        let indent = &line[..line.len() - line.trim_start().len()];
        let ending = if line.ends_with("\r\n") {
            "\r\n"
        } else if line.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let mut out = lines[..i].concat();
        out.push_str(&format!("{indent}{key} = \"{value}\"{ending}"));
        out.push_str(&lines[i + 1..].concat());
        return out;
    }
    let after_key = lines[..header].iter().rposition(|l| is_key(l)).map(|i| i + 1);
    let (pos, before_header) = match after_key {
        Some(p) => (p, false),
        None => {
            let mut p = header;
            while p > 0 && header < lines.len() && lines[p - 1].trim_start().starts_with('#') {
                p -= 1;
            }
            (p, header < lines.len())
        }
    };
    let mut out = lines[..pos].concat();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{key} = \"{value}\"\n"));
    // A table (or its comment) right after: keep a blank line between.
    if before_header && lines.get(pos).is_some_and(|l| !l.trim().is_empty()) {
        out.push('\n');
    }
    out.push_str(&lines[pos..].concat());
    out
}

/// `theme.toml` text with each top-level `(key, value)` line set, in order (a key not listed is left
/// alone). Every other line is kept byte for byte: comments, blank lines, `opacity`, the
/// `[colors]` overrides. A replaced line loses its own trailing comment.
pub fn set_keys(text: &str, keys: &[(&str, &str)]) -> String {
    keys.iter().fold(text.to_string(), |out, (key, value)| set_one(&out, key, value))
}

/// Writes the `(key, value)` lines into `dir/theme.toml`: reads it (absent: empty), edits it with
/// `set_keys`, creates `dir` if needed, then writes a temporary file beside it and renames it.
/// An unreadable existing file is an error, never overwritten.
pub fn write_settings(dir: &Path, keys: &[(&str, &str)]) -> io::Result<()> {
    // A symlinked theme.toml (dotfiles managers) keeps its link: the target is what gets replaced,
    // so a read-only target (a store path) is an error, not a silent swap for a regular file.
    let link = dir.join(THEME_FILE);
    let path = std::fs::canonicalize(&link).unwrap_or(link);
    let old = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let parent = path.parent().unwrap_or(dir);
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{THEME_FILE}.{}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, set_keys(&old, keys)).and_then(|()| std::fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_the_wallpaper_then_every_preset_with_a_human_name() {
        let e = entries();
        assert_eq!(e.len(), PRESETS.len() + 1);
        assert_eq!((e[0].key, e[0].label), (WALLPAPER, "Wallpaper (matugen)"));
        for (entry, p) in e[1..].iter().zip(PRESETS) {
            assert_eq!(entry.key, p.name);
            assert_ne!(entry.label, p.name, "{} has a display name", p.name);
            assert_eq!(entry.dark_only, p.light.is_none());
        }
        let labels: Vec<_> = e.iter().map(|x| x.label).collect();
        assert_eq!(&labels[1..4], ["Catppuccin", "Gruvbox", "Tokyo Night"]);
        let dark_only: Vec<_> = e.iter().filter(|x| x.dark_only).map(|x| x.label).collect();
        assert_eq!(dark_only, ["Nord", "Dracula"]);
    }

    fn scratch(name: &str) -> alpheus_commun::temp_path::TempPath {
        alpheus_commun::temp_path::TempPath::new(&format!("appearance-{name}"))
    }

    #[test]
    fn writing_creates_the_directory_and_leaves_no_temporary_file() {
        let tmp = scratch("create");
        let dir = tmp.join("not").join("yet");
        write_settings(&dir, &[("theme", "nord"), ("mode", "dark")]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("theme.toml")).unwrap(), "theme = \"nord\"\nmode = \"dark\"\n");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["theme.toml"], "the temporary file was renamed away");
    }

    #[test]
    fn writing_edits_the_existing_file_and_keeps_the_rest() {
        let dir = scratch("edit");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("theme.toml"), "# mine\ntheme = \"gruvbox\"\n[colors]\nfg = \"#ffffff\"\n").unwrap();
        write_settings(&dir, &[("theme", "dracula")]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("theme.toml")).unwrap(), "# mine\ntheme = \"dracula\"\n[colors]\nfg = \"#ffffff\"\n");
    }

    #[test]
    fn a_read_only_directory_is_an_error_and_the_file_is_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("ro");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("theme.toml"), "theme = \"nord\"\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = write_settings(&dir, &[("theme", "gruvbox")]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err(), "{r:?}");
        assert_eq!(std::fs::read_to_string(dir.join("theme.toml")).unwrap(), "theme = \"nord\"\n");
    }

    #[test]
    fn a_symlinked_theme_toml_stays_a_link_and_its_target_is_written() {
        let dir = scratch("link");
        let real = scratch("link-real");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("mine.toml"), "# mine\ntheme = \"nord\"\n").unwrap();
        std::os::unix::fs::symlink(real.join("mine.toml"), dir.join("theme.toml")).unwrap();
        write_settings(&dir, &[("theme", "gruvbox")]).unwrap();
        assert!(std::fs::symlink_metadata(dir.join("theme.toml")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(real.join("mine.toml")).unwrap(), "# mine\ntheme = \"gruvbox\"\n");
    }

    #[test]
    fn a_symlink_to_a_read_only_target_is_an_error_and_stays_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("link-ro");
        let real = scratch("link-ro-real");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("mine.toml"), "theme = \"nord\"\n").unwrap();
        std::os::unix::fs::symlink(real.join("mine.toml"), dir.join("theme.toml")).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = write_settings(&dir, &[("theme", "gruvbox")]);
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.is_err(), "{r:?}");
        assert!(std::fs::symlink_metadata(dir.join("theme.toml")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(real.join("mine.toml")).unwrap(), "theme = \"nord\"\n");
    }

    #[test]
    fn an_unreadable_theme_toml_is_never_overwritten() {
        let dir = scratch("dir-as-file");
        std::fs::create_dir_all(dir.join("theme.toml")).unwrap();
        assert!(write_settings(&dir, &[("theme", "nord")]).is_err());
        assert!(dir.join("theme.toml").is_dir());
    }

    #[test]
    fn an_empty_file_gets_both_lines() {
        assert_eq!(set_keys("", &[("theme", "nord"), ("mode", "dark")]), "theme = \"nord\"\nmode = \"dark\"\n");
        assert_eq!(set_keys("", &[("mode", "auto")]), "mode = \"auto\"\n");
        assert_eq!(set_keys("", &[]), "");
    }

    #[test]
    fn a_file_with_only_colors_keeps_them_below_the_new_lines() {
        let t = "[colors]\naccent = \"#ff8800\"\n";
        assert_eq!(set_keys(t, &[("theme", "nord")]), "theme = \"nord\"\n\n[colors]\naccent = \"#ff8800\"\n");
    }

    #[test]
    fn an_existing_line_is_replaced_in_place_and_the_rest_kept_byte_for_byte() {
        let t = "# my look\n\n  theme   =   \"gruvbox\"  # old\n# mode next\nmode = \"auto\"\n\n[colors]\n# theme = \"x\"\naccent = \"#ff8800\"";
        let out = set_keys(t, &[("theme", "tokyo-night"), ("mode", "light")]);
        assert_eq!(
            out,
            "# my look\n\n  theme = \"tokyo-night\"\n# mode next\nmode = \"light\"\n\n[colors]\n# theme = \"x\"\naccent = \"#ff8800\""
        );
    }

    #[test]
    fn a_theme_key_inside_a_table_is_never_touched() {
        let t = "[colors]\ntheme = \"#101010\"\n";
        let out = set_keys(t, &[("theme", "nord")]);
        assert_eq!(out, "theme = \"nord\"\n\n[colors]\ntheme = \"#101010\"\n");
        let t = "mode = \"dark\"\n[other]\nmode = \"keep\"\n";
        assert_eq!(set_keys(t, &[("mode", "light")]), "mode = \"light\"\n[other]\nmode = \"keep\"\n");
    }

    #[test]
    fn a_missing_mode_is_inserted_after_the_last_top_level_line() {
        let t = "theme = \"nord\"\n# note\n[colors]\nfg = \"#ffffff\"\n";
        assert_eq!(set_keys(t, &[("mode", "dark")]), "theme = \"nord\"\nmode = \"dark\"\n# note\n[colors]\nfg = \"#ffffff\"\n");
    }

    #[test]
    fn with_no_table_the_lines_go_at_the_end_even_without_a_final_newline() {
        assert_eq!(set_keys("# hi", &[("theme", "nord")]), "# hi\ntheme = \"nord\"\n");
        assert_eq!(set_keys("theme = \"nord\"", &[("mode", "auto")]), "theme = \"nord\"\nmode = \"auto\"\n");
    }

    #[test]
    fn setting_the_same_values_twice_changes_nothing() {
        let t = "# c\n[colors]\nfg = \"#fff\"\n";
        let once = set_keys(t, &[("theme", "nord"), ("mode", "dark")]);
        assert_eq!(set_keys(&once, &[("theme", "nord"), ("mode", "dark")]), once);
    }

    #[test]
    fn the_comment_glued_to_a_table_stays_with_it() {
        let t = "# colour overrides\n[colors]\nfg = \"#fff\"\n";
        assert_eq!(set_keys(t, &[("mode", "dark")]), "mode = \"dark\"\n\n# colour overrides\n[colors]\nfg = \"#fff\"\n");
    }

    #[test]
    fn a_key_that_only_starts_with_theme_is_not_theme() {
        let t = "theme_extra = 1\n";
        assert_eq!(set_keys(t, &[("theme", "nord")]), "theme_extra = 1\ntheme = \"nord\"\n");
    }

    #[test]
    fn crlf_lines_keep_their_ending_when_replaced() {
        assert_eq!(set_keys("theme = \"a\"\r\nmode = \"b\"\r\n", &[("theme", "nord")]), "theme = \"nord\"\r\nmode = \"b\"\r\n");
    }

    #[test]
    fn module_colors_is_set_replaced_and_inserted_like_the_others() {
        assert_eq!(set_keys("", &[("module_colors", "full")]), "module_colors = \"full\"\n");
        assert_eq!(
            set_keys("", &[("theme", "nord"), ("mode", "dark"), ("module_colors", "none")]),
            "theme = \"nord\"\nmode = \"dark\"\nmodule_colors = \"none\"\n"
        );
        let t = "# mine\nmodule_colors = \"icons\"  # old\ntheme = \"nord\"\n\n[colors]\nmodule_colors = \"keep\"\nfg = \"#fff\"\n";
        assert_eq!(
            set_keys(t, &[("module_colors", "full")]),
            "# mine\nmodule_colors = \"full\"\ntheme = \"nord\"\n\n[colors]\nmodule_colors = \"keep\"\nfg = \"#fff\"\n",
            "replaced in place, the one in [colors] untouched"
        );
        assert_eq!(
            set_keys("theme = \"nord\"\n[colors]\nfg = \"#fff\"\n", &[("module_colors", "none")]),
            "theme = \"nord\"\nmodule_colors = \"none\"\n[colors]\nfg = \"#fff\"\n"
        );
        assert_eq!(set_keys("module_colors_x = 1\n", &[("module_colors", "none")]), "module_colors_x = 1\nmodule_colors = \"none\"\n");
        assert_eq!(set_keys("module_colors = \"icons\"\r\n", &[("module_colors", "full")]), "module_colors = \"full\"\r\n");
    }

    #[test]
    fn module_colors_is_written_atomically_through_a_symlink() {
        let dir = scratch("colors-link");
        let real = scratch("colors-link-real");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("mine.toml"), "# mine\ntheme = \"nord\"\n").unwrap();
        std::os::unix::fs::symlink(real.join("mine.toml"), dir.join("theme.toml")).unwrap();
        write_settings(&dir, &[("module_colors", "full")]).unwrap();
        assert!(std::fs::symlink_metadata(dir.join("theme.toml")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(real.join("mine.toml")).unwrap(), "# mine\ntheme = \"nord\"\nmodule_colors = \"full\"\n");
        let names: Vec<_> = std::fs::read_dir(&real).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["mine.toml"], "no temporary file left");
    }

    #[test]
    fn style_is_set_replaced_and_inserted_like_the_others() {
        assert_eq!(set_keys("", &[("style", "islands")]), "style = \"islands\"\n");
        assert_eq!(
            set_keys("", &[("theme", "nord"), ("mode", "dark"), ("module_colors", "pills"), ("style", "powerline")]),
            "theme = \"nord\"\nmode = \"dark\"\nmodule_colors = \"pills\"\nstyle = \"powerline\"\n"
        );
        let t = "# mine\nstyle = \"flat\"  # old\nopacity = 0.6\n\n[colors]\nstyle = \"keep\"\n";
        assert_eq!(
            set_keys(t, &[("style", "translucent")]),
            "# mine\nstyle = \"translucent\"\nopacity = 0.6\n\n[colors]\nstyle = \"keep\"\n",
            "replaced in place, opacity kept byte for byte, the one in [colors] untouched"
        );
        assert_eq!(set_keys("style_x = 1\n", &[("style", "flat")]), "style_x = 1\nstyle = \"flat\"\n");
        assert_eq!(set_keys("style = \"flat\"\r\n", &[("style", "floating")]), "style = \"floating\"\r\n");
    }

    #[test]
    fn style_is_written_atomically_and_alone() {
        let dir = scratch("style");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("theme.toml"), "theme = \"nord\"\nopacity = 0.5\n").unwrap();
        write_settings(&dir, &[("style", "underline")]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("theme.toml")).unwrap(), "theme = \"nord\"\nopacity = 0.5\nstyle = \"underline\"\n");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["theme.toml"], "no temporary file left");
    }
}
