//! Tests of where a screenshot goes and what it is called (`paths.rs`): pure, nothing is read.

use super::*;
use std::path::Path;

fn home() -> Option<&'static Path> {
    Some(Path::new("/home/test"))
}

#[test]
fn the_folder_is_the_configured_one_then_pictures_then_home() {
    let some = |p: &str| Some(PathBuf::from(p));
    // The key wins, `~/` is the home.
    assert_eq!(save_dir("/srv/shots", Some(Path::new("/p")), home()), some("/srv/shots"));
    assert_eq!(save_dir("~/Shots", Some(Path::new("/p")), home()), some("/home/test/Shots"));
    // No key: the pictures folder's `screenshots`, else the home's `Pictures/screenshots`.
    assert_eq!(save_dir("", Some(Path::new("/data/Pictures")), home()), some("/data/Pictures/screenshots"));
    assert_eq!(save_dir("", None, home()), some("/home/test/Pictures/screenshots"));
    // A folder that needs a home nobody gave is no folder: never a relative path under the working
    // directory.
    assert_eq!(save_dir("", None, None), None);
    assert_eq!(save_dir("~/Shots", None, None), None);
    assert_eq!(save_dir("/srv/shots", None, None), some("/srv/shots"), "an absolute key needs no home");
    assert_eq!(save_dir("", Some(Path::new("/data/Pictures")), None), some("/data/Pictures/screenshots"));
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
    assert_eq!(
        user_dir("XDG_PICTURES_DIR=\"$HOME/\"\n", "XDG_PICTURES_DIR", Path::new("/h")),
        None,
        "the home itself is no pictures folder"
    );
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

/// A recording goes to the videos folder itself, else the home's `Videos`; never a relative path.
#[test]
fn a_recording_goes_to_the_videos_folder_then_the_homes() {
    assert_eq!(videos_dir(Some(Path::new("/data/Films")), home()), Some(PathBuf::from("/data/Films")));
    assert_eq!(videos_dir(None, home()), Some(PathBuf::from("/home/test/Videos")));
    assert_eq!(videos_dir(None, None), None);
    let text = "XDG_VIDEOS_DIR=\"$HOME/Vidéos\"\n";
    assert_eq!(user_dir(text, "XDG_VIDEOS_DIR", Path::new("/home/test")), Some("/home/test/Vidéos".into()));
}

#[test]
fn a_recording_is_named_by_its_start() {
    let t = LocalTime { year: 2026, month: 11, day: 5, weekday: 4, hour: 9, minute: 3, second: 7 };
    assert_eq!(video_name(&t), "screenrecord-20261105-090307.mp4");
    assert_eq!(numbered(&video_name(&t), 2), "screenrecord-20261105-090307-2.mp4");
}

/// The recorder's geometry is `x,y wxh` in the compositor's logical pixels, as slurp prints it: the
/// area moved by its output's place in the layout.
#[test]
fn the_geometry_is_the_area_in_the_layout() {
    let area = Rect { x: 10, y: 20, width: 300, height: 200 };
    assert_eq!(geometry((0, 0), area), "10,20 300x200");
    assert_eq!(geometry((1920, 0), area), "1930,20 300x200");
    assert_eq!(geometry((-1280, 360), area), "-1270,380 300x200");
}
