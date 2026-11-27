//! Tests of what the daemon applies on its own: the remembered name at start (without `then`) and
//! the folder's next name for `--wallpaper next`. No setter runs: a `FakeExecutor` plays it.

use super::*;
use crate::launcher::run::{RunContext, Spawner};
use crate::launcher::testing::TempDir;
use crate::state_file::{read_wallpaper, save_wallpaper};
use crate::wallpaper::setter::{Setter, Started, apply_outcome};
use alpheus_common::exec::fake::FakeExecutor;
use std::os::unix::fs::PermissionsExt;

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// A folder holding these files (empty), and a state directory beside it (not created).
fn folder(name: &str, files: &[&str]) -> (TempDir, PathBuf) {
    let d = TempDir::new(name);
    let walls = d.join("walls");
    std::fs::create_dir_all(&walls).unwrap();
    for f in files {
        std::fs::write(walls.join(f), b"").unwrap();
    }
    let state = d.join("state");
    (d, state)
}

/// swaybg on `walls`, matugen after it.
fn config(walls: &Path) -> WallpaperConfig {
    WallpaperConfig {
        dir: walls.to_string_lossy().into_owned(),
        set: argv(&["swaybg", "-i", "{file}"]),
        then: argv(&["matugen", "image", "{file}"]),
        ..WallpaperConfig::default()
    }
}

#[test]
fn the_remembered_wallpaper_is_applied_at_start_without_then() {
    let (d, state) = folder("wp-start", &["a.png", "b.png"]);
    let walls = d.join("walls");
    let w = config(&walls);
    save_wallpaper(&state, "b.png").unwrap();
    let choice = start_choice(&w, &state, None);
    assert_eq!(choice, Some(Choice { name: "b.png".into(), run_then: false }), "the colour files are already written");
    // Applied as the daemon does: the setter starts, `then` never does.
    let choice = choice.unwrap();
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let Ok(Started::Running(applied)) = Setter::new().start(&exec, &w, &walls, &choice.name, choice.run_then) else {
        panic!("the setter starts")
    };
    // A `matugen` on the `$PATH`, so a `then` asked for would start and be recorded.
    let bin = [d.join("bin")];
    std::fs::create_dir_all(&bin[0]).unwrap();
    std::fs::write(bin[0].join("matugen"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(bin[0].join("matugen"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let ctx = RunContext { exec: &exec, spawner: Spawner::Detached, history_path: None, path_dirs: &bin };
    let outcome = apply_outcome(&ctx, &state, &applied);
    assert_eq!(outcome.then, None, "no then at start");
    assert!(exec.calls().iter().all(|c| c.starts_with("swaybg ")), "nothing but the setter: {:?}", exec.calls());
    // Not remembered: nothing is applied at start.
    assert_eq!(start_choice(&WallpaperConfig { remember: false, ..w }, &state, None), None);
}

#[test]
fn a_remembered_name_outside_the_folder_is_not_applied() {
    let (d, state) = folder("wp-outside", &["a.png"]);
    let w = config(&d.join("walls"));
    save_wallpaper(&state, "gone.png").unwrap();
    assert_eq!(start_choice(&w, &state, None), None, "a name no longer in the folder");
    // A file beside the folder, not in it.
    std::fs::write(d.join("beside.png"), b"").unwrap();
    save_wallpaper(&state, "beside.png").unwrap();
    assert_eq!(start_choice(&w, &state, None), None);
    // A directory with an image's name is no file.
    std::fs::create_dir_all(d.join("walls/dir.png")).unwrap();
    save_wallpaper(&state, "dir.png").unwrap();
    assert_eq!(start_choice(&w, &state, None), None);
    // A path the state file was edited to hold.
    std::fs::write(state.join(crate::state_file::WALLPAPER_FILE), "../beside.png\n").unwrap();
    assert_eq!(start_choice(&w, &state, None), None);
}

#[test]
fn nothing_is_read_at_start_when_off() {
    let (d, _) = folder("wp-off", &["a.png"]);
    // A state "directory" that cannot be read as one: any read of it would fail loudly in a test
    // that expected a name, and here nothing reads it.
    let missing = d.join("no-such-state");
    assert_eq!(start_choice(&WallpaperConfig::default(), &missing, None), None, "off: no dir");
    let w = WallpaperConfig { dir: "~/walls".into(), ..config(&d.join("walls")) };
    assert_eq!(start_choice(&w, &missing, None), None, "no home: the folder does not resolve");
    assert!(!missing.exists(), "nothing created");
}

#[test]
fn next_applies_the_name_after_the_remembered_one() {
    let (d, state) = folder("wp-next", &["c.png", "a.png", "b.png", "notes.txt"]);
    let w = config(&d.join("walls"));
    let next = |state: &Path| next_choice(&w, state, None, None);
    assert_eq!(next(&state), Ok(Choice { name: "a.png".into(), run_then: true }), "none remembered: the first");
    save_wallpaper(&state, "a.png").unwrap();
    assert_eq!(next(&state).map(|c| c.name), Ok("b.png".into()));
    save_wallpaper(&state, "c.png").unwrap();
    assert_eq!(next(&state).map(|c| c.name), Ok("a.png".into()), "wraps");
    save_wallpaper(&state, "gone.png").unwrap();
    assert_eq!(next(&state).map(|c| c.name), Ok("a.png".into()), "a name no longer listed: the first");
    assert_eq!(next_choice(&WallpaperConfig::default(), &state, None, None), Err("wallpaper is off"));
    let (e, state) = folder("wp-next-empty", &["notes.txt"]);
    assert_eq!(next_choice(&config(&e.join("walls")), &state, None, None), Err("no image in the folder"));
    assert_eq!(next_choice(&config(&e.join("missing")), &state, None, None), Err("the folder cannot be read"));
}

/// `remember = false`: `next` steps from the name the setter last applied, held by the daemon
/// alone; a stale state file is not read, and nothing is written.
#[test]
fn next_without_remember_steps_from_the_name_last_applied() {
    let (d, state) = folder("wp-next-forget", &["a.png", "b.png", "c.png"]);
    let walls = d.join("walls");
    let w = WallpaperConfig { remember: false, ..config(&walls) };
    // A name left by an earlier `remember = true`: read, it would make the first `next` b.png.
    save_wallpaper(&state, "a.png").unwrap();
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let mut setter = Setter::new();
    for want in ["a.png", "b.png", "c.png", "a.png"] {
        let choice = next_choice(&w, &state, None, setter.last_applied()).unwrap();
        assert_eq!(choice.name, want, "steps from the name last applied");
        let Ok(Started::Running(applied)) = setter.start(&exec, &w, &walls, &choice.name, false) else { panic!("started") };
        let ctx = RunContext { exec: &exec, spawner: Spawner::Detached, history_path: None, path_dirs: &[] };
        assert_eq!(apply_outcome(&ctx, &state, &applied).saved, None, "not remembered: nothing saved");
    }
    assert_eq!(read_wallpaper(&state).as_deref(), Some("a.png"), "the state file is left as it was");
}

/// `remember = true` with a save that fails: `next` steps from the name last applied in memory,
/// not from the stale name left on disk, so it does not land on the same image again and again.
#[test]
fn next_with_a_failed_save_steps_from_the_name_last_applied() {
    let (d, state) = folder("wp-next-unsaved", &["a.png", "b.png", "c.png"]);
    let walls = d.join("walls");
    let w = config(&walls);
    // A stale name, then the state directory made read-only: every save from here fails.
    save_wallpaper(&state, "c.png").unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let mut setter = Setter::new();
    for want in ["a.png", "b.png"] {
        let choice = next_choice(&w, &state, None, setter.last_applied()).unwrap();
        assert_eq!(choice.name, want, "steps from the name last applied, not the stale one");
        let Ok(Started::Running(applied)) = setter.start(&exec, &w, &walls, &choice.name, false) else { panic!("started") };
        let ctx = RunContext { exec: &exec, spawner: Spawner::Detached, history_path: None, path_dirs: &[] };
        assert_eq!(apply_outcome(&ctx, &state, &applied).saved, Some(false), "the save fails");
    }
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(read_wallpaper(&state).as_deref(), Some("c.png"), "the stale name is still there");
}
