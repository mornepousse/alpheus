//! Tests of where a screenshot goes and what it is called (`paths.rs`): pure, nothing is read.

use super::*;
use std::path::Path;

fn home() -> Option<&'static Path> {
    Some(Path::new("/home/test"))
}

#[test]
fn the_folder_is_the_configured_one_then_pictures_then_home() {
    // The key wins, `~/` is the home.
    assert_eq!(save_dir("/srv/shots", Some(Path::new("/p")), home()), Path::new("/srv/shots"));
    assert_eq!(save_dir("~/Shots", Some(Path::new("/p")), home()), Path::new("/home/test/Shots"));
    // No key: the pictures folder's `screenshots`, else the home's `Pictures/screenshots`.
    assert_eq!(save_dir("", Some(Path::new("/data/Pictures")), home()), Path::new("/data/Pictures/screenshots"));
    assert_eq!(save_dir("", None, home()), Path::new("/home/test/Pictures/screenshots"));
    // No home either: a relative path, never the working directory's parent or a guess.
    assert_eq!(save_dir("", None, None), Path::new("Pictures/screenshots"));
    assert_eq!(save_dir("~/Shots", None, None), Path::new("Shots"));
}

#[test]
fn user_dirs_names_the_pictures_folder() {
    let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_PICTURES_DIR=\"$HOME/Images\"\nXDG_VIDEOS_DIR=\"$HOME/Videos\"\n";
    assert_eq!(user_dir(text, "XDG_PICTURES_DIR", Path::new("/home/test")), Some("/home/test/Images".into()));
    assert_eq!(user_dir(text, "XDG_VIDEOS_DIR", Path::new("/home/test")), Some("/home/test/Videos".into()));
    assert_eq!(
        user_dir("XDG_PICTURES_DIR=\"/mnt/pics\"\n", "XDG_PICTURES_DIR", Path::new("/h")),
        Some("/mnt/pics".into()),
        "an absolute path"
    );
    // A name that only starts like it, a relative value, an empty one, a comment: nothing.
    assert_eq!(user_dir("XDG_PICTURES_DIR_X=\"/a\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None);
    assert_eq!(user_dir("XDG_PICTURES_DIR=\"Images\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None);
    assert_eq!(user_dir("XDG_PICTURES_DIR=\"\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None);
    assert_eq!(user_dir("# XDG_PICTURES_DIR=\"/a\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None);
    // `$HOME` must open the value and be followed by a plain path: nothing else is expanded.
    assert_eq!(user_dir("XDG_PICTURES_DIR=\"$HOME/$OTHER\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None);
    assert_eq!(user_dir("XDG_PICTURES_DIR=\"$HOME/\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None, "the home itself is no pictures folder");
    // `$HOME` anywhere but the start is not expanded: it is not a path then.
    assert_eq!(user_dir("XDG_PICTURES_DIR=\"/x/$HOME\"\n", "XDG_PICTURES_DIR", Path::new("/h")), None);
}

#[test]
fn a_file_is_named_by_its_second() {
    use crate::clock::LocalTime;
    let t = LocalTime { year: 2026, month: 3, day: 4, weekday: 3, hour: 9, minute: 5, second: 7 };
    assert_eq!(file_name(&t), "20260304-090507.png", "every field is zero-padded");
    // A second shot in the same second gets a number before the extension, never over the first.
    assert_eq!(numbered("20260304-090507.png", 0), "20260304-090507.png");
    assert_eq!(numbered("20260304-090507.png", 2), "20260304-090507-2.png");
}
