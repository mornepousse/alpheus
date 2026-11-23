//! Tests of the hooks' wiring (`app.rs`): how a hook is started (in a scope, detached, bare), what
//! the start-up lines say, how a `$PATH` is split. They use `FakeExecutor`, never a real program.

use super::*;
use crate::launcher::testing::TempDir;
use alpheus_common::exec::fake::FakeExecutor;

/// The command line `systemd-run` is given before the hook's own, as the fake executor sees it.
const SCOPE: &str = "systemd-run --user --scope --quiet --collect --";

/// A temporary directory with one executable program, `hook-x`.
fn bin(name: &str) -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new(&format!("hooks-{name}"));
    std::fs::write(d.join("hook-x"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(d.join("hook-x"), std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

/// A hook that fires: `hook-x --flag` for a do-not-disturb change.
fn fire() -> Fire {
    Fire {
        id: HookId::Dnd,
        argv: vec!["hook-x".into(), "--flag".into()],
        env: vec![("ALPHEUS_EVENT", "dnd".into()), ("ALPHEUS_DND", "on".into())],
        unset: vec!["ALPHEUS_DEVICE", "ALPHEUS_MODE"],
    }
}

/// The directories to look the program up in: just `d`.
fn dirs(d: &TempDir) -> Vec<PathBuf> {
    vec![d.to_path_buf()]
}

/// The variables `fire()` gives, as the fake executor records them.
fn vars() -> Vec<(String, String)> {
    vec![("ALPHEUS_EVENT".into(), "dnd".into()), ("ALPHEUS_DND".into(), "on".into())]
}

#[test]
fn a_hook_runs_in_a_systemd_scope() {
    let d = bin("scope");
    let f = FakeExecutor::new();
    f.ok(&format!("{SCOPE} hook-x --flag"), "");
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire()), Ok(Spawned::Ran));
    assert_eq!(f.envs(), [(format!("{SCOPE} hook-x --flag"), vars())]);
    assert_eq!(f.unsets(), [["ALPHEUS_DEVICE", "ALPHEUS_MODE"]], "the variables the event leaves unset are removed");
}

#[test]
fn a_hook_runs_detached_when_asked_to() {
    let d = bin("detached");
    let f = FakeExecutor::new();
    f.ok("hook-x --flag", "");
    assert_eq!(hook_spawn(&f, Spawner::Detached, &dirs(&d), &fire()), Ok(Spawned::Ran));
    assert_eq!(f.envs(), [("hook-x --flag".to_string(), vars())]);
    assert_eq!(f.unsets(), [["ALPHEUS_DEVICE", "ALPHEUS_MODE"]]);
}

#[test]
fn a_hook_runs_detached_when_systemd_run_is_missing() {
    let d = bin("fallback");
    let f = FakeExecutor::new();
    f.ok("hook-x --flag", "");
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire()), Ok(Spawned::FellBack));
    assert_eq!(f.calls(), [format!("{SCOPE} hook-x --flag"), "hook-x --flag".to_string()]);
}

#[test]
fn a_systemd_run_failure_is_named_and_the_hook_is_not_run_bare() {
    let d = bin("sdfail");
    let f = FakeExecutor::new();
    f.respond(&format!("{SCOPE} hook-x --flag"), Err(ExecError::Io("boom".into())));
    f.ok("hook-x --flag", "");
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire()), Err(Failure::SystemdRun));
    assert_eq!(f.calls(), [format!("{SCOPE} hook-x --flag")], "the argv alone is never spawned");
}

#[test]
fn a_missing_program_is_never_started() {
    let d = bin("missing");
    let f = FakeExecutor::new();
    let absent = Fire { argv: vec!["hook-absent".into()], ..fire() };
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &absent), Err(Failure::NotFound));
    assert_eq!(hook_spawn(&f, Spawner::Detached, &dirs(&d), &absent), Err(Failure::NotFound));
    assert!(f.calls().is_empty());
    let abs = d.join("hook-x").to_string_lossy().to_string();
    f.ok(&abs, "");
    let by_path = Fire { argv: vec![abs], ..fire() };
    assert_eq!(hook_spawn(&f, Spawner::Detached, &dirs(&d), &by_path), Ok(Spawned::Ran));
    let empty = Fire { argv: vec![], ..fire() };
    assert_eq!(hook_spawn(&f, Spawner::Detached, &dirs(&d), &empty), Err(Failure::Start));
}

#[test]
fn the_argv_never_holds_event_data() {
    let d = bin("argv");
    let f = FakeExecutor::new();
    f.ok(&format!("{SCOPE} hook-x --flag"), "");
    let mut fire = fire();
    fire.env.push(("ALPHEUS_DEVICE", "Test Headset".into()));
    hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire).unwrap();
    assert!(f.calls().iter().all(|c| !c.contains("Test Headset")));
}

#[test]
fn the_start_lines_name_each_hook_that_never_fires() {
    let cfg = crate::config::BarConfig::from_text("[hooks]\npower_profile = [\"a\"]\ndnd = [\"b\"]\n").unwrap();
    let model = Hooks::new(&cfg.hooks, Instant::now()).unwrap();
    assert_eq!(
        start_lines(&model, &[ModuleId::Clock], false),
        ["hooks: 2 set", "hooks: dnd never fires (needs notifications)", "hooks: power_profile never fires (needs quick)"]
    );
    assert_eq!(start_lines(&model, &crate::config::BarConfig::default().modules, false), ["hooks: 2 set"]);
}

#[test]
fn the_start_lines_name_the_shield_hook_when_no_shield_is_configured() {
    let cfg = crate::config::BarConfig::from_text("[hooks]\nshield = [\"a\"]\n").unwrap();
    let model = Hooks::new(&cfg.hooks, Instant::now()).unwrap();
    let modules = crate::config::BarConfig::default().modules;
    assert_eq!(start_lines(&model, &modules, false), ["hooks: 1 set", "hooks: shield never fires (needs [shield])"]);
    assert_eq!(start_lines(&model, &modules, true), ["hooks: 1 set"]);
}

#[test]
fn path_dirs_split_a_path_and_drop_empty_entries() {
    assert_eq!(path_dirs(Some(OsStr::new("/a::/b"))), [PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(path_dirs(None).is_empty());
}
