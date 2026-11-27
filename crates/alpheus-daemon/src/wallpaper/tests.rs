use super::*;
use crate::launcher::config::LauncherConfig;
use crate::launcher::testing::TempDir;

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn configured(set: &[&str]) -> WallpaperConfig {
    WallpaperConfig { dir: "/srv/walls".into(), set: argv(set), ..WallpaperConfig::default() }
}

#[test]
fn wallpaper_is_off_by_default() {
    let w = LauncherConfig::default().wallpaper;
    assert!(!w.enabled());
    assert!(w.restart && w.remember, "restart and remember default to true");
    assert_eq!(LauncherConfig::from_text("").unwrap().wallpaper, w);
    assert!(validate(&w).is_ok());
    // A dir with a setter is the feature on; parsed from the file as written.
    let c = LauncherConfig::from_text("[wallpaper]\ndir = \"~/Walls\"\nset = [\"swaybg\", \"-i\", \"{file}\"]\nrestart = false\n").unwrap();
    assert!(c.wallpaper.enabled());
    assert_eq!(c.wallpaper.set, argv(&["swaybg", "-i", "{file}"]));
    assert!(!c.wallpaper.restart && c.wallpaper.remember);
}

#[test]
fn set_needs_file_once_as_a_whole_element() {
    assert!(validate(&configured(&["swaybg", "-i", "{file}"])).is_ok());
    for bad in [
        &["swaybg", "-i"][..],
        &["swaybg", "-i", "{file}", "{file}"],
        &["swaybg", "-i", "x{file}"],
        &["{file}"],
        &["{file}", "-i", "{file}"],
        &[" ", "{file}"],
    ] {
        assert!(validate(&configured(bad)).is_err(), "{bad:?}");
    }
    // Through the file: an invalid [wallpaper] makes the file invalid, like any other key.
    let e = LauncherConfig::from_text("[wallpaper]\ndir = \"/srv/walls\"\nset = [\"swaybg\", \"-i\", \"x{file}\"]\n").unwrap_err();
    assert!(e.contains("wallpaper.set"), "{e}");
}

#[test]
fn a_dir_without_set_is_invalid() {
    let w = WallpaperConfig { dir: "/srv/walls".into(), ..WallpaperConfig::default() };
    assert!(!w.enabled());
    let e = validate(&w).unwrap_err();
    assert!(e.contains("wallpaper.set"), "{e}");
    // The folder is absolute or under `~/`, and never reaches the message.
    for dir in ["walls", "~walls", "/srv/wa\0lls"] {
        let e = validate(&WallpaperConfig { dir: dir.into(), ..configured(&["swaybg", "-i", "{file}"]) }).unwrap_err();
        assert!(e.contains("wallpaper.dir") && !e.contains("walls"), "{e}");
    }
    assert!(validate(&WallpaperConfig { dir: "~/Walls".into(), ..configured(&["wbg", "{file}"]) }).is_ok());
}

#[test]
fn then_takes_file_at_most_once_whole() {
    let with_then = |then: &[&str]| WallpaperConfig { then: argv(then), ..configured(&["swaybg", "-i", "{file}"]) };
    assert!(validate(&with_then(&[])).is_ok());
    assert!(validate(&with_then(&["matugen", "image", "{file}"])).is_ok());
    assert!(validate(&with_then(&["my-reload"])).is_ok(), "{{file}} is optional in then");
    for bad in [&["matugen", "{file}", "{file}"][..], &["matugen", "--image={file}"], &["{file}"], &["", "x"]] {
        let e = validate(&with_then(bad)).unwrap_err();
        assert!(e.contains("wallpaper.then"), "{bad:?}: {e}");
    }
}

#[test]
fn an_unknown_wallpaper_key_is_refused() {
    let d = TempDir::new("wallpaper-unknown-key");
    let path = d.join("launcher.toml");
    std::fs::write(&path, "rows = 12\n[wallpaper]\ndir = \"/srv/walls\"\nset = [\"swaybg\", \"-i\", \"{file}\"]\nevery = \"30m\"\n")
        .unwrap();
    let (c, warning) = LauncherConfig::load(&path);
    let warning = warning.expect("a warning");
    assert!(warning.contains("launcher.toml invalid") && warning.contains("every"), "{warning}");
    assert_eq!(c, LauncherConfig::default(), "defaults kept");
}

#[test]
fn resolve_dir_expands_home_and_refuses_relative() {
    let home = Path::new("/home/someone");
    assert_eq!(resolve_dir("~/Walls", Some(home)), Some(PathBuf::from("/home/someone/Walls")));
    assert_eq!(resolve_dir("/srv/walls", Some(home)), Some(PathBuf::from("/srv/walls")));
    assert_eq!(resolve_dir("/srv/walls", None), Some(PathBuf::from("/srv/walls")));
    assert_eq!(resolve_dir("~/Walls", None), None, "no home: never a relative folder");
    assert_eq!(resolve_dir("~/Walls", Some(Path::new("relative-home"))), None);
    for dir in ["", "walls", "~", "~walls", "./walls"] {
        assert_eq!(resolve_dir(dir, Some(home)), None, "{dir:?}");
    }
}

#[test]
fn the_folder_listing_keeps_images_sorted_and_bounded() {
    let d = TempDir::new("wallpaper-listing");
    for name in ["b.JPG", "a.png", "notes.txt", "line\nbreak.png"] {
        std::fs::write(d.join(name), b"x").unwrap();
    }
    std::fs::create_dir(d.join("c.png")).unwrap();
    std::os::unix::fs::symlink(d.join("a.png"), d.join("link.webp")).unwrap();
    std::os::unix::fs::symlink(d.join("c.png"), d.join("dirlink.gif")).unwrap();
    std::os::unix::fs::symlink(d.join("absent.png"), d.join("dangling.bmp")).unwrap();

    let l = list(&d, 3).unwrap();
    assert_eq!(l.names, ["a.png", "b.JPG", "link.webp"], "images only, files or links to files, sorted");
    assert!(!l.cut);

    std::fs::write(d.join("0.jpeg"), b"x").unwrap();
    let l = list(&d, 3).unwrap();
    assert!(l.cut, "four images, cap 3");
    assert_eq!(l.names.len(), 3);
    assert!(l.names.is_sorted(), "{:?}", l.names);

    let d = TempDir::new("wallpaper-listing-cap");
    for name in ["b.JPG", "a.png"] {
        std::fs::write(d.join(name), b"x").unwrap();
    }
    let l = list(&d, 2).unwrap();
    assert_eq!((l.names.as_slice(), l.cut), (&["a.png".to_string(), "b.JPG".to_string()][..], false));
    std::fs::write(d.join("c.gif"), b"x").unwrap();
    let l = list(&d, 2).unwrap();
    assert!(l.cut);
    assert_eq!(l.names.len(), 2);

    assert!(list(&d.join("absent"), 2).is_err());
}

#[test]
fn image_names_are_matched_by_extension_in_any_case() {
    for name in ["a.png", "a.JPG", "a.Jpeg", "a.webp", "a.BMP", "a.gif", "my wall.png"] {
        assert!(is_image_name(name), "{name}");
    }
    for name in ["png", ".png", "a.png.txt", "a.tiff", "a", "a."] {
        assert!(!is_image_name(name), "{name}");
    }
}

#[test]
fn a_name_with_a_slash_or_dotdot_is_refused() {
    for name in ["a/b.png", "..", ".", "", "a\0.png", "/a.png"] {
        assert!(!is_plain_name(name), "{name:?}");
    }
    for name in ["my wall.png", "..a.png", ".hidden.png", "a..png"] {
        assert!(is_plain_name(name), "{name:?}");
    }
}

#[test]
fn fill_keeps_a_name_with_spaces_in_one_argument() {
    let file = Path::new("/srv/walls/my wall -x.png");
    assert_eq!(
        fill(&argv(&["swaybg", "-m", "fill", "-i", "{file}"]), file),
        argv(&["swaybg", "-m", "fill", "-i", "/srv/walls/my wall -x.png"])
    );
    // Only the whole element: a part is never substituted.
    assert_eq!(fill(&argv(&["my-reload", "x{file}"]), file), argv(&["my-reload", "x{file}"]));
    assert_eq!(fill(&argv(&["my-reload"]), file), argv(&["my-reload"]));
}

#[test]
fn next_after_wraps_and_starts_at_the_first() {
    let names = argv(&["a.png", "b.png", "c.png"]);
    assert_eq!(next_after(&names, None), Some("a.png"));
    assert_eq!(next_after(&names, Some("a.png")), Some("b.png"));
    assert_eq!(next_after(&names, Some("c.png")), Some("a.png"), "wraps");
    assert_eq!(next_after(&names, Some("gone.png")), Some("a.png"), "unknown: the first");
    assert_eq!(next_after(&[], Some("a.png")), None);
    assert_eq!(next_after(&argv(&["a.png"]), Some("a.png")), Some("a.png"));
}

/// The configuration reference's `[wallpaper]` example is valid as written (the test reads
/// docs/configuration.md itself, so the example cannot drift).
#[test]
fn the_configuration_references_wallpaper_example_parses() {
    let reference = include_str!("../../../../docs/configuration.md");
    let block = reference
        .split("```toml\n")
        .skip(1)
        .map(|b| b.split("```").next().unwrap_or(""))
        .find(|b| b.contains("[wallpaper]"))
        .expect("a toml block with [wallpaper] in docs/configuration.md");
    let c = LauncherConfig::from_text(block).unwrap();
    assert!(c.wallpaper.enabled() && c.wallpaper.restart && c.wallpaper.remember);
}
