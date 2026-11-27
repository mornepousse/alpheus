//! Tests of the setter (`setter.rs`): its argv, the replacement that starts the new one before the
//! old handle goes, a one-shot setter judged by its exit, and what a set's success or failure leads
//! to (the remembered name, `then`). The executor is a `FakeExecutor`: no setter runs; the test
//! plays it by ending its fake child.

use super::*;
use crate::launcher::run::{RunContext, Spawner};
use crate::launcher::testing::TempDir;
use crate::state_file::{WALLPAPER_FILE, read_wallpaper, save_wallpaper};
use alpheus_common::exec::fake::FakeExecutor;
use alpheus_common::exec::{CaptureLimits, Captured, ExecError, Executor, Output, Stream};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// A folder holding these files (empty).
fn folder(name: &str, files: &[&str]) -> TempDir {
    let d = TempDir::new(name);
    for f in files {
        std::fs::write(d.join(f), b"").unwrap();
    }
    d
}

/// swaybg on `dir`, until replaced.
fn swaybg(dir: &Path) -> WallpaperConfig {
    WallpaperConfig {
        dir: dir.to_string_lossy().into_owned(),
        set: argv(&["swaybg", "-i", "{file}", "-m", "fill"]),
        ..WallpaperConfig::default()
    }
}

/// A one-shot setter (`swww img {file}`): applied once it exits 0.
fn swww(dir: &Path) -> WallpaperConfig {
    WallpaperConfig { set: argv(&["swww", "img", "{file}"]), restart: false, ..swaybg(dir) }
}

/// A `FakeExecutor` that also keeps each `spawn_watched`'s arguments as they were passed, so a test
/// can count them (the fake's own record joins them with spaces).
#[derive(Default)]
struct Spy {
    fake: FakeExecutor,
    args: Mutex<Vec<Vec<String>>>,
}

impl Executor for Spy {
    fn run(&self, prog: &str, args: &[&str]) -> Result<Output, ExecError> {
        self.fake.run(prog, args)
    }
    fn spawn(&self, prog: &str, args: &[&str]) -> Result<(), ExecError> {
        self.fake.spawn(prog, args)
    }
    fn spawn_with_input(&self, prog: &str, args: &[&str], input: &[&[u8]]) -> Result<(), ExecError> {
        self.fake.spawn_with_input(prog, args, input)
    }
    fn spawn_with_env(&self, prog: &str, args: &[&str], env: &[(&str, &str)], unset: &[&str]) -> Result<(), ExecError> {
        self.fake.spawn_with_env(prog, args, env, unset)
    }
    fn stream(&self, prog: &str, args: &[&str]) -> Result<Stream, ExecError> {
        self.fake.stream(prog, args)
    }
    fn capture(&self, prog: &str, args: &[&str], limits: CaptureLimits) -> Result<Captured, ExecError> {
        self.fake.capture(prog, args, limits)
    }
    fn run_silent(&self, prog: &str, args: &[&str]) -> Result<Option<i32>, ExecError> {
        self.fake.run_silent(prog, args)
    }
    fn spawn_watched(&self, prog: &str, args: &[&str], stdout: Option<&Path>) -> Result<Box<dyn Watched>, ExecError> {
        self.args.lock().unwrap().push(args.iter().map(|a| a.to_string()).collect());
        self.fake.spawn_watched(prog, args, stdout)
    }
}

#[test]
fn the_set_command_is_run_as_written_with_the_file_as_one_argument() {
    let dir = folder("wp-argv", &["my wall.png"]);
    let exec = Spy::default();
    exec.fake.watch_ok("swaybg", None);
    let mut setter = Setter::new();
    let started = setter.start(&exec, &swaybg(&dir), &dir, "my wall.png", true);
    let file = dir.join("my wall.png");
    assert!(matches!(started, Ok(Started::Running(_))), "a restart setter is applied once it started");
    assert_eq!(exec.fake.children()[0].line(), format!("swaybg -i {} -m fill", file.display()));
    let args = exec.args.lock().unwrap().clone();
    assert_eq!(args, [argv(&["-i", &file.to_string_lossy(), "-m", "fill"])], "the name with a space is one argument");
    assert_eq!(args[0].len(), 4);
}

#[test]
fn a_restart_drops_only_the_old_handle_after_the_new_one_started() {
    let dir = folder("wp-restart", &["a.png", "b.png", "c.png"]);
    let w = swaybg(&dir);
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let mut setter = Setter::new();
    assert!(setter.start(&exec, &w, &dir, "a.png", true).is_ok());
    assert!(setter.start(&exec, &w, &dir, "b.png", true).is_ok());
    let children = exec.children();
    assert!(children[0].killed(), "the old setter goes once the new one runs");
    assert!(!children[1].killed(), "the new one stays");
    // The next setter cannot start: the one on screen is kept.
    exec.watch_err("swaybg", ExecError::NotFound);
    assert!(setter.start(&exec, &w, &dir, "c.png", true).is_err());
    assert!(!exec.children()[1].killed(), "a failed start never ends the setter running");
    assert_eq!(setter.child_ended().map(|(e, _)| e), None, "it still runs");
}

#[test]
fn a_one_shot_setter_succeeds_only_on_exit_zero() {
    let dir = folder("wp-oneshot", &["a.png", "b.png"]);
    let w = swww(&dir);
    let exec = FakeExecutor::new();
    exec.watch_ok("swww", None);
    let mut setter = Setter::new();
    assert!(matches!(setter.start(&exec, &w, &dir, "a.png", true), Ok(Started::Pending)));
    assert!(setter.child_ended().is_none(), "nothing ended yet");
    exec.children()[0].end(Some(1));
    match setter.child_ended() {
        Some((Ended::Failed, None)) => {}
        other => panic!("exit 1 is a failure: {:?}", other.map(|(e, a)| (e, a.map(|a| a.name)))),
    }
    assert!(matches!(setter.start(&exec, &w, &dir, "b.png", true), Ok(Started::Pending)));
    exec.children()[1].end(Some(0));
    match setter.child_ended() {
        Some((Ended::Applied, Some(applied))) => {
            assert_eq!(applied.name, "b.png");
            assert_eq!(applied.file, dir.join("b.png"));
        }
        other => panic!("exit 0 applies: {:?}", other.map(|(e, a)| (e, a.map(|a| a.name)))),
    }
    assert!(setter.child_ended().is_none(), "each end is told once");
}

/// A one-shot set while a setter runs: the running one survives the one-shot's start and its
/// failure, and goes only once a one-shot exits 0.
#[test]
fn a_one_shot_replaces_the_running_setter_only_on_exit_zero() {
    let dir = folder("wp-oneshot-over", &["a.png", "b.png", "c.png"]);
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    exec.watch_ok("swww", None);
    let mut setter = Setter::new();
    assert!(matches!(setter.start(&exec, &swaybg(&dir), &dir, "a.png", true), Ok(Started::Running(_))));
    assert!(matches!(setter.start(&exec, &swww(&dir), &dir, "b.png", true), Ok(Started::Pending)));
    assert!(!exec.children()[0].killed(), "a pending one-shot leaves the running setter");
    exec.children()[1].end(Some(1));
    assert!(matches!(setter.child_ended(), Some((Ended::Failed, None))));
    assert!(!exec.children()[0].killed(), "a failed one-shot leaves the running setter");
    assert!(matches!(setter.start(&exec, &swww(&dir), &dir, "c.png", true), Ok(Started::Pending)));
    exec.children()[2].end(Some(0));
    assert!(matches!(setter.child_ended(), Some((Ended::Applied, Some(_)))));
    assert!(exec.children()[0].killed(), "the running setter goes once a one-shot worked");
}

/// A restart setter that cannot start while a one-shot is pending keeps that one-shot: its exit 0
/// still applies.
#[test]
fn a_failed_restart_keeps_the_pending_one_shot() {
    let dir = folder("wp-pending-kept", &["a.png", "b.png"]);
    let exec = FakeExecutor::new();
    exec.watch_ok("swww", None);
    exec.watch_err("swaybg", ExecError::NotFound);
    let mut setter = Setter::new();
    assert!(matches!(setter.start(&exec, &swww(&dir), &dir, "a.png", true), Ok(Started::Pending)));
    assert!(setter.start(&exec, &swaybg(&dir), &dir, "b.png", true).is_err());
    assert!(!exec.children()[0].killed(), "a failed start never drops the pending one-shot");
    exec.children()[0].end(Some(0));
    match setter.child_ended() {
        Some((Ended::Applied, Some(applied))) => assert_eq!(applied.name, "a.png"),
        other => panic!("the one-shot still applies: {:?}", other.map(|(e, a)| (e, a.map(|a| a.name)))),
    }
}

#[test]
fn a_running_setter_that_ends_on_its_own_is_gone() {
    let dir = folder("wp-gone", &["a.png"]);
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let mut setter = Setter::new();
    assert!(setter.start(&exec, &swaybg(&dir), &dir, "a.png", true).is_ok());
    exec.children()[0].end(Some(1));
    assert!(matches!(setter.child_ended(), Some((Ended::Gone, None))));
    assert!(setter.child_ended().is_none());
}

/// A context whose `$PATH` holds an executable `matugen`, starting programs in their scope.
fn with_matugen<'a>(exec: &'a dyn Executor, bin: &'a [PathBuf]) -> RunContext<'a> {
    let tool = bin[0].join("matugen");
    std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    RunContext { exec, spawner: Spawner::SystemdScope, history_path: None, path_dirs: bin }
}

#[test]
fn a_failed_set_runs_no_then_and_keeps_the_remembered_name() {
    let dir = folder("wp-failed", &["a.png", "b.png"]);
    let state = TempDir::new("wp-failed-state");
    let state_dir = state.join("alpheus");
    save_wallpaper(&state_dir, "a.png").unwrap();
    let bin = TempDir::new("wp-failed-bin");
    let bin = [bin.to_path_buf()];
    let w = WallpaperConfig { then: argv(&["matugen", "image", "{file}"]), ..swww(&dir) };
    let exec = FakeExecutor::new();
    exec.watch_ok("swww", None);
    let ctx = with_matugen(&exec, &bin);
    let mut setter = Setter::new();
    assert!(setter.start(&exec, &w, &dir, "b.png", true).is_ok());
    exec.children()[0].end(Some(1));
    let (ended, applied) = setter.child_ended().expect("it ended");
    assert!(matches!(ended, Ended::Failed));
    if let Some(applied) = &applied {
        apply_outcome(&ctx, &state_dir, applied);
    }
    assert_eq!(read_wallpaper(&state_dir).as_deref(), Some("a.png"), "the remembered name is kept");
    assert!(!exec.calls().iter().any(|c| c.contains("matugen")), "no then: {:?}", exec.calls());
}

#[test]
fn then_runs_after_a_successful_set_with_the_file() {
    let dir = folder("wp-then", &["b.png"]);
    let state = TempDir::new("wp-then-state");
    let state_dir = state.join("alpheus");
    let bin = TempDir::new("wp-then-bin");
    let bin = [bin.to_path_buf()];
    let w = WallpaperConfig { then: argv(&["matugen", "image", "{file}"]), ..swaybg(&dir) };
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let file = dir.join("b.png");
    let scoped = format!("systemd-run --user --scope --quiet --collect -- matugen image {}", file.display());
    exec.ok(&scoped, "");
    let ctx = with_matugen(&exec, &bin);
    let mut setter = Setter::new();
    let Ok(Started::Running(applied)) = setter.start(&exec, &w, &dir, "b.png", true) else { panic!("started") };
    let outcome = apply_outcome(&ctx, &state_dir, &applied);
    assert_eq!(outcome, Outcome { saved: Some(true), then: Some(true) });
    assert!(exec.calls().contains(&scoped), "{:?}", exec.calls());
    assert_eq!(read_wallpaper(&state_dir).as_deref(), Some("b.png"), "remembered");
    // At the daemon's start the colour files are already written: `then` is not run again.
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let ctx = with_matugen(&exec, &bin);
    let Ok(Started::Running(applied)) = Setter::new().start(&exec, &w, &dir, "b.png", false) else { panic!("started") };
    assert_eq!(apply_outcome(&ctx, &state_dir, &applied).then, None);
    assert!(!exec.calls().iter().any(|c| c.contains("matugen")), "{:?}", exec.calls());
    // `remember = false`: nothing is written.
    let other = TempDir::new("wp-then-forget");
    let forget = WallpaperConfig { remember: false, ..w };
    let Ok(Started::Running(applied)) = Setter::new().start(&exec, &forget, &dir, "b.png", false) else { panic!("started") };
    assert_eq!(apply_outcome(&ctx, &other.join("alpheus"), &applied).saved, None);
    assert!(!other.join("alpheus").join(WALLPAPER_FILE).exists());
}

#[test]
fn a_name_not_in_the_folder_is_not_started() {
    let dir = folder("wp-absent", &["a.png", "notes.txt"]);
    std::fs::create_dir(dir.join("sub.png")).unwrap();
    let outside = folder("wp-absent-outside", &["x.png"]);
    let exec = FakeExecutor::new();
    exec.watch_ok("swaybg", None);
    let w = swaybg(&dir);
    let mut setter = Setter::new();
    let escape = format!("../{}/x.png", outside.file_name().unwrap().to_string_lossy());
    for bad in ["b.png", "notes.txt", "sub.png", "..", ".", "", "a\0.png", escape.as_str()] {
        assert!(setter.start(&exec, &w, &dir, bad, true).is_err(), "{bad:?}");
    }
    assert!(exec.calls().is_empty(), "nothing started: {:?}", exec.calls());
}

#[test]
fn the_wallpaper_file_round_trips_and_refuses_a_path() {
    let state = TempDir::new("wp-state");
    let state_dir = state.join("alpheus");
    assert_eq!(read_wallpaper(&state_dir), None, "nothing saved yet");
    save_wallpaper(&state_dir, " my wall.png").unwrap();
    assert_eq!(read_wallpaper(&state_dir).as_deref(), Some(" my wall.png"), "a name with spaces kept as it is");
    assert!(save_wallpaper(&state_dir, "../x.png").is_err(), "a path is never written");
    state_file::write_private(&state_dir, WALLPAPER_FILE, b"../x.png\n").unwrap();
    assert_eq!(read_wallpaper(&state_dir), None, "a path is never read back");
    state_file::write_private(&state_dir, WALLPAPER_FILE, &[b'a'; 5000]).unwrap();
    assert_eq!(read_wallpaper(&state_dir), None, "too long");
}

/// At rest the setter on `App` is two empty options: no allocation, a fixed size on the state.
#[test]
fn the_setter_at_rest_is_two_empty_options() {
    let setter = Setter::new();
    assert!(setter.child.is_none() && setter.pending.is_none() && setter.exit_fd().is_none());
    eprintln!("size_of::<Setter>() = {}", size_of::<Setter>());
    assert!(size_of::<Setter>() <= 128);
}
