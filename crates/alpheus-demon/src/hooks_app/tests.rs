use super::*;
use crate::launcher::testing::TempDir;
use alpheus_commun::exec::fake::FakeExecutor;

const SCOPE: &str = "systemd-run --user --scope --quiet --collect --";

fn bin(name: &str) -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new(&format!("hooks-{name}"));
    std::fs::write(d.join("hook-x"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(d.join("hook-x"), std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

fn fire() -> Fire {
    Fire {
        id: HookId::Dnd,
        argv: vec!["hook-x".into(), "--flag".into()],
        env: vec![("ALPHEUS_EVENT", "dnd".into()), ("ALPHEUS_DND", "on".into())],
    }
}

fn dirs(d: &TempDir) -> Vec<PathBuf> {
    vec![d.to_path_buf()]
}

fn vars() -> Vec<(String, String)> {
    vec![("ALPHEUS_EVENT".into(), "dnd".into()), ("ALPHEUS_DND".into(), "on".into())]
}

#[test]
fn hook_spawn_in_a_scope() {
    let d = bin("scope");
    let f = FakeExecutor::new();
    f.ok(&format!("{SCOPE} hook-x --flag"), "");
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire()), Ok(Spawned::Ran));
    assert_eq!(f.envs(), [(format!("{SCOPE} hook-x --flag"), vars())]);
}

#[test]
fn hook_spawn_detached() {
    let d = bin("detached");
    let f = FakeExecutor::new();
    f.ok("hook-x --flag", "");
    assert_eq!(hook_spawn(&f, Spawner::Detached, &dirs(&d), &fire()), Ok(Spawned::Ran));
    assert_eq!(f.envs(), [("hook-x --flag".to_string(), vars())]);
}

#[test]
fn hook_spawn_falls_back_without_systemd_run() {
    let d = bin("fallback");
    let f = FakeExecutor::new();
    f.ok("hook-x --flag", "");
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire()), Ok(Spawned::FellBack));
    assert_eq!(f.calls(), [format!("{SCOPE} hook-x --flag"), "hook-x --flag".to_string()]);
}

#[test]
fn hook_spawn_names_a_systemd_run_failure() {
    let d = bin("sdfail");
    let f = FakeExecutor::new();
    f.respond(&format!("{SCOPE} hook-x --flag"), Err(ExecError::Io("boom".into())));
    f.ok("hook-x --flag", "");
    assert_eq!(hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire()), Err(Failure::SystemdRun));
    assert_eq!(f.calls(), [format!("{SCOPE} hook-x --flag")], "the argv alone is never spawned");
}

#[test]
fn hook_spawn_never_starts_a_missing_program() {
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
fn the_argv_is_the_configured_one() {
    let d = bin("argv");
    let f = FakeExecutor::new();
    f.ok(&format!("{SCOPE} hook-x --flag"), "");
    let mut fire = fire();
    fire.env.push(("ALPHEUS_DEVICE", "Test Headset".into()));
    hook_spawn(&f, Spawner::SystemdScope, &dirs(&d), &fire).unwrap();
    assert!(f.calls().iter().all(|c| !c.contains("Test Headset")));
}

#[test]
fn start_lines_name_what_never_fires() {
    let cfg = crate::config::BarConfig::from_text("[hooks]\npower_profile = [\"a\"]\ndnd = [\"b\"]\n").unwrap();
    let model = Hooks::new(&cfg.hooks, Instant::now()).unwrap();
    assert_eq!(
        start_lines(&model, &[ModuleId::Clock]),
        ["hooks: 2 set", "hooks: dnd never fires (needs notifications)", "hooks: power_profile never fires (needs quick)"]
    );
    assert_eq!(start_lines(&model, &crate::config::BarConfig::default().modules), ["hooks: 2 set"]);
}

#[test]
fn path_dirs_split_path() {
    assert_eq!(path_dirs(Some(OsStr::new("/a::/b"))), [PathBuf::from("/a"), PathBuf::from("/b")]);
    assert!(path_dirs(None).is_empty());
}
