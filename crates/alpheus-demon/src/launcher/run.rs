//! Activation → argv → the executor (spec § 1, "Launching"): the only place
//! the launcher builds a command line. Apps start in their own systemd
//! scope when a user manager exists (they never die with the bar), fully
//! detached otherwise; `Exec` is expanded per the Desktop Entry spec and
//! passed as argv, never through a shell; the shell only for what the user
//! typed in the Commands mode; windows are focused through the `focus`
//! template (`wm-set focus-window` by default), the only write path to the
//! compositor.

use crate::actions::substitute;
use crate::launcher::desktop::{BadExec, DesktopEntry, ExecContext, exec_argv};
use crate::launcher::history::{app_key, command_key, source_key};
use crate::launcher::model::{Activation, Launcher};
use crate::launcher::scan::has_program;
use crate::launcher::sources::run_argv;
use crate::log::write as log;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::launcher_ipc::{Reply, wipe};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spawner {
    SystemdScope,
    Detached,
}

/// Decided once per open: SystemdScope iff "<runtime_dir>/systemd/private" exists.
pub fn spawner(runtime_dir: &Path) -> Spawner {
    if runtime_dir.join("systemd/private").exists() { Spawner::SystemdScope } else { Spawner::Detached }
}

pub const SYSTEMD_RUN: [&str; 6] = ["systemd-run", "--user", "--scope", "--quiet", "--collect", "--"];

/// Main Exec or action `action`; Terminal=true → terminal + argv.
pub fn app_argv(entry: &DesktopEntry, action: Option<usize>, terminal: &[String]) -> Result<Vec<String>, String> {
    let exec = match action {
        None => &entry.exec,
        Some(i) => &entry.actions.get(i).ok_or_else(|| format!("{}: no action {i}", entry.name))?.exec,
    };
    let ctx = ExecContext { name: &entry.name, icon: entry.icon.as_deref(), path: &entry.path };
    let argv = exec_argv(exec, &ctx).map_err(|BadExec(m)| format!("{}: invalid Exec ({m})", entry.name))?;
    if argv.is_empty() {
        return Err(format!("{}: empty Exec", entry.name));
    }
    Ok(if entry.terminal { terminal.iter().cloned().chain(argv).collect() } else { argv })
}

/// SystemdScope → SYSTEMD_RUN + argv; Detached → argv.
pub fn wrap(spawner: Spawner, argv: Vec<String>) -> Vec<String> {
    match spawner {
        Spawner::SystemdScope => SYSTEMD_RUN.iter().map(|s| s.to_string()).chain(argv).collect(),
        Spawner::Detached => argv,
    }
}

/// The `focus` template with {window} replaced by the selector (single pass, like actions::substitute).
pub fn focus_argv(template: &[String], selector: &str) -> Vec<String> {
    template.iter().map(|a| substitute(a, &[("{window}", selector)])).collect()
}

/// ["sh", "-c", "--", text], prefixed by `terminal` when Some: typed text
/// starting with `-` or `+` would otherwise be read by `sh` as an option (a
/// controller decision, not something `sh` does on its own).
pub fn shell_argv(text: &str, terminal: Option<&[String]>) -> Vec<String> {
    let shell = ["sh", "-c", "--", text].map(String::from);
    terminal.unwrap_or(&[]).iter().cloned().chain(shell).collect()
}

pub struct RunContext<'a> {
    pub exec: &'a dyn Executor,
    pub spawner: Spawner,
    /// None: history disabled or no state dir.
    pub history_path: Option<&'a Path>,
    /// The `$PATH` the scan used (`ScanEnv::path_dirs`), kept as the single copy on
    /// `Launcher::path_dirs` and cloned out by the caller for this borrow (review item M5,
    /// final review slice 1: this used to say "task 12 keeps it in `OpenLauncher`", stale since
    /// the model, not `OpenLauncher`, became the single source of truth): a program is looked
    /// up there before it is started.
    pub path_dirs: &'a [PathBuf],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    Close,
    /// Script mode: send this (Reply::Chosen, or Reply::Index under `--index`) to the waiting
    /// client, then close.
    Reply(Reply),
}

fn spawn(exec: &dyn Executor, argv: &[String]) -> Result<(), ExecError> {
    let Some((prog, args)) = argv.split_first() else { return Err(ExecError::Io("empty command".into())) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.spawn(prog, &args)
}

fn failure(program: &str, e: ExecError) -> String {
    match e {
        ExecError::NotFound => format!("{program}: program not found"),
        ExecError::Timeout => format!("{program}: timed out"),
        ExecError::Io(m) => format!("{program}: {m}"),
    }
}

/// A program for the user: in its systemd scope, or bare when systemd-run
/// itself is missing. Looked up first (argv[0], the terminal for
/// Terminal=true): `systemd-run` starts detached, so a program it cannot
/// find would fail after the launcher closed, silently (spec § 3: the box
/// says so instead, and stays open).
fn launch(ctx: &RunContext, argv: Vec<String>) -> Result<(), String> {
    let program = argv.first().cloned().unwrap_or_default();
    if !has_program(ctx.path_dirs, &program) {
        return Err(failure(&program, ExecError::NotFound));
    }
    match spawn(ctx.exec, &wrap(ctx.spawner, argv.clone())) {
        Ok(()) => Ok(()),
        Err(ExecError::NotFound) if ctx.spawner == Spawner::SystemdScope => {
            log("systemd-run not found: launching detached");
            spawn(ctx.exec, &argv).map_err(|e| failure(&program, e))
        }
        // Any other failure wrapping in a scope: systemd-run itself is the
        // program that failed to run, not the app it was asked to start.
        Err(e) if ctx.spawner == Spawner::SystemdScope => Err(failure("systemd-run", e)),
        Err(e) => Err(failure(&program, e)),
    }
}

/// Ids only, never typed text (spec § 3); a save error is logged, not shown.
/// On the event loop: one small write and a rename, no fsync (`History::save`;
/// a crash may lose the last use, never the file).
fn remember(l: &mut Launcher, ctx: &RunContext, key: &str) {
    let Some(path) = ctx.history_path else { return };
    l.history.record(key, l.now);
    if let Err(e) = l.history.save(path) {
        log(&format!("launcher history not saved ({}): {e}", path.display()));
    }
}

/// Looks the program up in `path_dirs` (argv[0] after the terminal is prepended; windows
/// excepted), spawns through the executor (systemd-run NotFound → retried Detached), records
/// history (apps under app_key, actions under their app, commands under command_key; Shell and
/// windows never recorded; a source entry runs its source's `run` with the whole line as one
/// argument, recorded under source_key) then saves it (a save error is logged, not shown; no fsync on the event
/// loop). Err(message) on a missing program or a spawn failure: the caller shows it in the box
/// and stays open.
pub fn perform(l: &mut Launcher, a: Activation, ctx: &RunContext) -> Result<Done, String> {
    let gone = || "the list changed: try again".to_string();
    match a {
        // Never recorded (spec § 3: no typed text, and a dmenu line is not an app).
        Activation::Choose { mut text, line } if l.index => {
            wipe(&mut text);
            return Ok(Done::Reply(Reply::Index(line)));
        }
        Activation::Choose { text, .. } => return Ok(Done::Reply(Reply::Chosen(text))),
        Activation::App { index, action } => {
            let entry = l.catalog.as_ref().and_then(|c| c.apps.get(index)).ok_or_else(gone)?;
            let argv = app_argv(entry, action, &l.config.terminal)?;
            let key = app_key(&entry.id);
            launch(ctx, argv)?;
            remember(l, ctx, &key);
        }
        Activation::Command { index, terminal } => {
            let name = l.catalog.as_ref().and_then(|c| c.commands.get(index)).ok_or_else(gone)?.clone();
            // By itself, never through `sh -c`: the held terminal runs the name as its argv.
            let terminal = if terminal { &l.config.terminal_hold[..] } else { &[] };
            launch(ctx, terminal.iter().cloned().chain([name.clone()]).collect())?;
            remember(l, ctx, &command_key(&name));
        }
        Activation::Entry(i) => {
            let entries = l.entries.as_ref().ok_or_else(gone)?;
            let item = entries.items.get(i).ok_or_else(gone)?;
            let source = entries.sources.get(item.source).ok_or_else(gone)?;
            let argv = run_argv(&source.run, &item.line);
            let key = source_key(&source.name, &item.line);
            launch(ctx, argv)?;
            remember(l, ctx, &key);
        }
        Activation::Shell { text, terminal } => {
            let terminal = terminal.then_some(&l.config.terminal_hold[..]);
            launch(ctx, shell_argv(&text, terminal))?;
        }
        Activation::Window(i) => {
            let selector = &l.windows.get(i).ok_or_else(gone)?.selector;
            let argv = focus_argv(&l.config.focus, selector);
            spawn(ctx.exec, &argv).map_err(|e| failure(argv.first().map_or("focus", String::as_str), e))?;
        }
    }
    Ok(Done::Close)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::Toplevel;
    use crate::launcher::config::{LauncherConfig, SourceConfig};
    use crate::launcher::desktop::DesktopAction;
    use crate::launcher::history::History;
    use crate::launcher::scan::Catalog;
    use crate::launcher::sources::{Entries, SourceEntry};
    use crate::launcher::testing::TempDir;
    use alpheus_commun::exec::fake::FakeExecutor;
    use alpheus_commun::launcher_ipc::DmenuFlags;
    use std::path::PathBuf;

    const NOW: u64 = 1_800_000_000;
    const SCOPE: &str = "systemd-run --user --scope --quiet --collect --";

    fn entry(id: &str, name: &str, exec: &str) -> DesktopEntry {
        DesktopEntry {
            id: id.into(),
            path: PathBuf::from(format!("/usr/share/applications/{id}")),
            name: name.into(),
            generic_name: None,
            comment: None,
            keywords: vec![],
            icon: None,
            exec: exec.into(),
            terminal: false,
            startup_wm_class: None,
            actions: vec![],
        }
    }

    fn catalog() -> Catalog {
        let mut firefox = entry("org.mozilla.firefox.desktop", "Firefox", "firefox %u");
        firefox.actions = vec![
            DesktopAction { key: "new-window".into(), name: "New Window".into(), exec: "firefox --new-window %u".into() },
            DesktopAction {
                key: "new-private-window".into(),
                name: "New Private Window".into(),
                exec: "firefox --private-window %u".into(),
            },
        ];
        let mut btop = entry("btop.desktop", "btop++", "btop");
        btop.terminal = true;
        Catalog { apps: vec![firefox, btop], commands: vec!["htop".into(), "nvim".into()] }
    }

    fn launcher() -> Launcher {
        let w = |selector: &str| Toplevel {
            selector: selector.into(),
            class: "firefox".into(),
            title: "t".into(),
            workspace: "1".into(),
            focused: false,
        };
        let mut l = Launcher::for_apps(LauncherConfig::default(), vec![w("address:0x583d9c60da90"), w("id:12")], History::empty(200), NOW);
        l.set_catalog(catalog());
        l
    }

    /// A directory of executable stubs (never run: the executor is fake);
    /// removed when the returned guard drops, so a test run leaves nothing
    /// in /tmp (unlike a plain `std::env::temp_dir()` join with no cleanup).
    fn stubs(name: &str, programs: &[&str]) -> TempDir {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new(&format!("run-{name}"));
        for p in programs {
            std::fs::write(d.join(p), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(d.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        d
    }

    /// The `$PATH` of these tests: every program they may launch. `name`
    /// disambiguates the temporary directory from other tests running in
    /// the same process (shared `name`s would race on the same path).
    fn bin(name: &str) -> TempDir {
        stubs(name, &["firefox", "foot", "nautilus", "htop", "nvim", "sh"])
    }

    fn ctx<'a>(exec: &'a FakeExecutor, spawner: Spawner, history: Option<&'a Path>, path_dirs: &'a [PathBuf]) -> RunContext<'a> {
        RunContext { exec, spawner, history_path: history, path_dirs }
    }

    fn app(index: usize, action: Option<usize>) -> Activation {
        Activation::App { index, action }
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn spawner_follows_the_user_manager_s_socket() {
        let d = TempDir::new("run-spawner");
        assert_eq!(spawner(&d), Spawner::Detached);
        std::fs::create_dir_all(d.join("systemd")).unwrap();
        std::fs::write(d.join("systemd/private"), "").unwrap();
        assert_eq!(spawner(&d), Spawner::SystemdScope);
    }

    #[test]
    fn argv_builders() {
        assert_eq!(wrap(Spawner::Detached, v(&["foot"])), v(&["foot"]));
        assert_eq!(
            wrap(Spawner::SystemdScope, v(&["foot", "-e", "x"])),
            v(&["systemd-run", "--user", "--scope", "--quiet", "--collect", "--", "foot", "-e", "x"])
        );
        let template = v(&["wm-set", "focus-window", "{window}"]);
        assert_eq!(focus_argv(&template, "address:0x583d9c60da90"), v(&["wm-set", "focus-window", "address:0x583d9c60da90"]));
        assert_eq!(focus_argv(&template, "id:12"), v(&["wm-set", "focus-window", "id:12"]));
        assert_eq!(focus_argv(&v(&["my-focus", "--id={window}"]), "x{window}"), v(&["my-focus", "--id=x{window}"]), "single pass");
        assert_eq!(shell_argv("ls -la | wc -l", None), v(&["sh", "-c", "--", "ls -la | wc -l"]), "one argument, the shell parses it");
        assert_eq!(shell_argv("htop", Some(&v(&["foot", "-e"]))), v(&["foot", "-e", "sh", "-c", "--", "htop"]));
        assert_eq!(
            shell_argv("-v foo", None),
            v(&["sh", "-c", "--", "-v foo"]),
            "typed text starting with - is not read by sh as an option"
        );
    }

    #[test]
    fn app_argv_main_action_terminal_and_field_codes() {
        let c = catalog();
        let term = v(&["foot", "-e"]);
        assert_eq!(app_argv(&c.apps[0], None, &term), Ok(v(&["firefox"])), "%u removed: no file passed");
        assert_eq!(app_argv(&c.apps[0], Some(1), &term), Ok(v(&["firefox", "--private-window"])));
        assert!(app_argv(&c.apps[0], Some(5), &term).unwrap_err().contains("Firefox"), "an action that is not there");
        assert_eq!(app_argv(&c.apps[1], None, &term), Ok(v(&["foot", "-e", "btop"])), "Terminal=true");
        let mut e = entry("my.desktop", "My App", "my-app --name %c --file %k %i");
        e.icon = Some("my-icon".into());
        assert_eq!(
            app_argv(&e, None, &term),
            Ok(v(&["my-app", "--name", "My App", "--file", "/usr/share/applications/my.desktop", "--icon", "my-icon"]))
        );
        e.icon = None;
        assert_eq!(
            app_argv(&e, None, &term),
            Ok(v(&["my-app", "--name", "My App", "--file", "/usr/share/applications/my.desktop"])),
            "%i without an icon: removed"
        );
        let bad = entry("bad.desktop", "Bad", "bad %z");
        assert!(app_argv(&bad, None, &term).unwrap_err().contains("Bad"));
    }

    #[test]
    fn an_app_starts_in_its_scope_or_detached() {
        let bin = bin("app-starts");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        f.ok(&format!("{SCOPE} firefox"), "");
        let mut l = launcher();
        assert_eq!(perform(&mut l, app(0, None), &ctx(&f, Spawner::SystemdScope, None, &dirs)), Ok(Done::Close));
        let f2 = FakeExecutor::new();
        f2.ok("firefox --new-window", "");
        assert_eq!(perform(&mut l, app(0, Some(0)), &ctx(&f2, Spawner::Detached, None, &dirs)), Ok(Done::Close));
        assert_eq!((f.calls(), f2.calls()), (vec![format!("{SCOPE} firefox")], vec!["firefox --new-window".to_string()]));
        let f3 = FakeExecutor::new();
        f3.ok(&format!("{SCOPE} foot -e btop"), "");
        assert_eq!(perform(&mut l, app(1, None), &ctx(&f3, Spawner::SystemdScope, None, &dirs)), Ok(Done::Close), "Terminal=true");
    }

    #[test]
    fn without_systemd_run_the_app_is_retried_detached() {
        let bin = bin("without-systemd-run");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new(); // systemd-run not programmed: NotFound
        f.ok("firefox", "");
        let mut l = launcher();
        assert_eq!(perform(&mut l, app(0, None), &ctx(&f, Spawner::SystemdScope, None, &dirs)), Ok(Done::Close));
        assert_eq!(f.calls(), [format!("{SCOPE} firefox"), "firefox".to_string()]);
    }

    #[test]
    fn a_systemd_run_failure_other_than_not_found_names_systemd_run() {
        let bin = bin("systemd-run-failure");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        f.respond(&format!("{SCOPE} firefox"), Err(ExecError::Io("permission denied".into())));
        let mut l = launcher();
        assert_eq!(
            perform(&mut l, app(0, None), &ctx(&f, Spawner::SystemdScope, None, &dirs)),
            Err("systemd-run: permission denied".to_string()),
            "systemd-run itself failed, not firefox"
        );
    }

    #[test]
    fn a_spawn_failure_is_an_error_message_and_nothing_is_recorded() {
        let d = TempDir::new("run-failure-state");
        let h = d.join("launcher-history");
        let bin = bin("failure");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new(); // firefox not programmed: NotFound
        let mut l = launcher();
        let r = perform(&mut l, app(0, None), &ctx(&f, Spawner::Detached, Some(&h), &dirs));
        assert_eq!(r, Err("firefox: program not found".to_string()));
        assert_eq!(l.message, None, "the caller shows it, not perform");
        assert!(!h.exists() && l.history.entries().is_empty(), "a failed launch is not a use");
        f.respond("firefox", Err(ExecError::Io("permission denied".into())));
        assert_eq!(perform(&mut l, app(0, None), &ctx(&f, Spawner::Detached, None, &dirs)), Err("firefox: permission denied".to_string()));
    }

    #[test]
    fn a_missing_program_is_reported_before_systemd_run() {
        let d = TempDir::new("run-missing");
        let h = d.join("launcher-history");
        let missing_bin = stubs("missing-bin", &["htop"]);
        let only_htop = [missing_bin.to_path_buf()];
        let f = FakeExecutor::new();
        for c in ["firefox", "foot -e btop", "foot --hold -e htop", "/opt/none/abs", "sh -c -- ls"] {
            f.ok(&format!("{SCOPE} {c}"), ""); // systemd-run itself would start: the failure would come later, unseen
        }
        let c = RunContext { exec: &f, spawner: Spawner::SystemdScope, history_path: Some(&h), path_dirs: &only_htop };
        let mut l = launcher();
        assert_eq!(perform(&mut l, app(0, None), &c), Err("firefox: program not found".to_string()));
        assert_eq!(perform(&mut l, app(1, None), &c), Err("foot: program not found".to_string()), "Terminal=true: the terminal");
        let to_terminal = Activation::Command { index: 0, terminal: true };
        assert_eq!(perform(&mut l, to_terminal, &c), Err("foot: program not found".to_string()));
        let shell = Activation::Shell { text: "ls".into(), terminal: false };
        assert_eq!(perform(&mut l, shell, &c), Err("sh: program not found".to_string()));
        l.set_catalog(Catalog { apps: vec![entry("abs.desktop", "Abs", "/opt/none/abs")], commands: vec![] });
        assert_eq!(perform(&mut l, app(0, None), &c), Err("/opt/none/abs: program not found".to_string()), "an absolute path");
        assert!(f.calls().is_empty(), "nothing started: {:?}", f.calls());
        assert!(!h.exists() && l.history.entries().is_empty(), "nothing recorded");
    }

    #[test]
    fn windows_are_focused_through_the_template_never_in_a_scope() {
        let bin = bin("windows-focus");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        f.ok("wm-set focus-window address:0x583d9c60da90", "");
        f.ok("wm-set focus-window id:12", "");
        let d = TempDir::new("run-focus");
        let h = d.join("launcher-history");
        let mut l = launcher();
        for i in [0, 1] {
            assert_eq!(perform(&mut l, Activation::Window(i), &ctx(&f, Spawner::SystemdScope, Some(&h), &dirs)), Ok(Done::Close));
        }
        assert_eq!(f.calls(), ["wm-set focus-window address:0x583d9c60da90", "wm-set focus-window id:12"]);
        assert!(!h.exists(), "windows are never recorded");
        let f = FakeExecutor::new();
        assert_eq!(
            perform(&mut l, Activation::Window(0), &ctx(&f, Spawner::Detached, None, &dirs)),
            Err("wm-set: program not found".to_string())
        );
    }

    #[test]
    fn commands_and_the_shell() {
        let bin = bin("commands");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        for c in ["htop", "foot --hold -e htop", "sh -c -- htop -d 5", "foot --hold -e sh -c -- htop -d 5"] {
            f.ok(&format!("{SCOPE} {c}"), "");
        }
        let d = TempDir::new("run-commands-state");
        let h = d.join("launcher-history");
        let mut l = launcher();
        let c = ctx(&f, Spawner::SystemdScope, Some(&h), &dirs);
        // terminal: true held the running command open on screen (Enter); false: detached, no
        // terminal at all (Ctrl+Enter) — Mae, 2026-10-05.
        assert_eq!(perform(&mut l, Activation::Command { index: 0, terminal: false }, &c), Ok(Done::Close));
        assert_eq!(perform(&mut l, Activation::Command { index: 0, terminal: true }, &c), Ok(Done::Close));
        assert_eq!(perform(&mut l, Activation::Shell { text: "htop -d 5".into(), terminal: false }, &c), Ok(Done::Close));
        assert_eq!(perform(&mut l, Activation::Shell { text: "htop -d 5".into(), terminal: true }, &c), Ok(Done::Close));
        assert_eq!(
            f.calls(),
            [
                format!("{SCOPE} htop"),
                format!("{SCOPE} foot --hold -e htop"),
                format!("{SCOPE} sh -c -- htop -d 5"),
                format!("{SCOPE} foot --hold -e sh -c -- htop -d 5")
            ],
            "a $PATH command held in the terminal: its name as argv, never sh -c"
        );
        let text = std::fs::read_to_string(&h).unwrap();
        assert!(text.contains("\t2\tcmd:htop\n"), "{text:?}");
        assert!(!text.contains("-d 5"), "typed text never reaches the history: {text:?}");
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn script_mode_replies_without_running_anything() {
        let f = FakeExecutor::new();
        let mut l = Launcher::for_script(LauncherConfig::default(), None, v(&["a"]));
        assert_eq!(
            perform(&mut l, Activation::Choose { text: "a".into(), line: Some(0) }, &ctx(&f, Spawner::SystemdScope, None, &[])),
            Ok(Done::Reply(Reply::Chosen("a".into())))
        );
        assert!(f.calls().is_empty());
    }

    /// `--index` answers the position (None for typed text), `--password` the text as typed; a
    /// script menu never writes the history, whatever its flags.
    #[test]
    fn script_mode_answers_the_index_or_the_text_and_never_records() {
        let f = FakeExecutor::new();
        let d = TempDir::new("run-script-history");
        let h = d.join("launcher-history");
        let c = ctx(&f, Spawner::SystemdScope, Some(&h), &[]);
        let index = DmenuFlags { index: true, password: false, visible: None };
        let mut l = Launcher::for_dmenu(LauncherConfig::default(), None, v(&["a", "b"]), index);
        assert_eq!(perform(&mut l, Activation::Choose { text: "b".into(), line: Some(1) }, &c), Ok(Done::Reply(Reply::Index(Some(1)))));
        assert_eq!(perform(&mut l, Activation::Choose { text: "zz".into(), line: None }, &c), Ok(Done::Reply(Reply::Index(None))));
        let password = DmenuFlags { index: false, password: true, visible: None };
        let mut l = Launcher::for_dmenu(LauncherConfig::default(), None, vec![], password);
        let typed = Activation::Choose { text: " hunter 2".into(), line: None };
        assert_eq!(perform(&mut l, typed, &c), Ok(Done::Reply(Reply::Chosen(" hunter 2".into()))));
        assert!(!h.exists(), "nothing recorded");
        assert!(f.calls().is_empty());
    }

    #[test]
    fn apps_and_their_actions_are_recorded_under_the_app() {
        let bin = bin("history");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        f.ok("firefox", "");
        f.ok("firefox --private-window", "");
        let d = TempDir::new("run-history-state");
        let h = d.join("state/launcher-history");
        let mut l = launcher();
        let c = ctx(&f, Spawner::Detached, Some(&h), &dirs);
        perform(&mut l, app(0, None), &c).unwrap();
        perform(&mut l, app(0, Some(1)), &c).unwrap();
        assert_eq!(std::fs::read_to_string(&h).unwrap(), format!("{NOW}\t2\tapp:org.mozilla.firefox.desktop\n"));
        let mut off = launcher();
        perform(&mut off, app(0, None), &ctx(&f, Spawner::Detached, None, &dirs)).unwrap();
        assert!(off.history.entries().is_empty(), "no history path: nothing recorded");
    }

    #[test]
    fn a_history_save_error_does_not_fail_the_launch() {
        let bin = bin("save-error");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        f.ok("firefox", "");
        let d = TempDir::new("run-save-error-state");
        std::fs::write(d.join("file"), "").unwrap();
        let h = d.join("file/launcher-history"); // a file where a directory should be
        let mut l = launcher();
        assert_eq!(perform(&mut l, app(0, None), &ctx(&f, Spawner::Detached, Some(&h), &dirs)), Ok(Done::Close));
    }

    #[test]
    fn a_stale_index_is_refused_without_running_anything() {
        let f = FakeExecutor::new();
        let mut l = launcher();
        let c = ctx(&f, Spawner::Detached, None, &[]);
        for a in [app(9, None), Activation::Command { index: 9, terminal: false }, Activation::Window(9)] {
            assert!(perform(&mut l, a, &c).unwrap_err().contains("list changed"));
        }
        assert!(f.calls().is_empty());
    }

    fn entries_launcher() -> Launcher {
        let mut l = launcher();
        let source = SourceConfig { name: "Outils".into(), list: v(&["tools-menu", "--list"]), run: v(&["tools-menu", "--run", "{line}"]) };
        l.set_entries(Entries { sources: vec![source], items: vec![SourceEntry::new(0, "Captures · Screenshot zone")] });
        l
    }

    #[test]
    fn a_source_entry_runs_its_run_in_its_scope_and_is_remembered_by_line() {
        let d = TempDir::new("run-source-state");
        let h = d.join("launcher-history");
        let bin = stubs("source", &["tools-menu"]);
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        f.ok(&format!("{SCOPE} tools-menu --run Captures · Screenshot zone"), "");
        let mut l = entries_launcher();
        let r = perform(&mut l, Activation::Entry(0), &ctx(&f, Spawner::SystemdScope, Some(&h), &dirs));
        assert_eq!(r, Ok(Done::Close));
        assert_eq!(f.calls(), [format!("{SCOPE} tools-menu --run Captures · Screenshot zone")]);
        let saved = std::fs::read_to_string(&h).unwrap();
        assert!(saved.lines().any(|x| x.ends_with("\tsource:Outils:Captures · Screenshot zone")), "{saved}");
    }

    #[test]
    fn a_source_entry_whose_program_is_missing_says_so_and_records_nothing() {
        let d = TempDir::new("run-source-missing");
        let h = d.join("launcher-history");
        let bin = bin("source-missing");
        let dirs = [bin.to_path_buf()];
        let f = FakeExecutor::new();
        let mut l = entries_launcher();
        let r = perform(&mut l, Activation::Entry(0), &ctx(&f, Spawner::Detached, Some(&h), &dirs));
        assert_eq!(r, Err("tools-menu: program not found".to_string()));
        assert!(f.calls().is_empty() && !h.exists(), "nothing ran, nothing recorded");
        let r = perform(&mut l, Activation::Entry(5), &ctx(&f, Spawner::Detached, Some(&h), &dirs));
        assert_eq!(r, Err("the list changed: try again".to_string()));
    }
}
