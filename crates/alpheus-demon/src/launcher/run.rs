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
/// deliberate guard, not something `sh` does on its own).
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
    /// `Launcher::path_dirs` and cloned out by the caller for this borrow (the
    /// model, not `OpenLauncher`, is the single source of truth): a program is looked
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

pub(crate) fn failure(program: &str, e: ExecError) -> String {
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
        // The calculator's text on the copy program's stdin (never its argv, never a scope, never
        // recorded): a missing program is said, and the box stays open.
        Activation::Copy { text } => {
            let (prog, args) = l.config.copy.split_first().ok_or_else(|| "copy: no program configured".to_string())?;
            if !has_program(ctx.path_dirs, prog) {
                return Err(failure(prog, ExecError::NotFound));
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            ctx.exec.spawn_with_input(prog, &args, &[text.as_bytes()]).map_err(|e| failure(prog, e))?;
        }
        // The `?` row: `open` with the URL as its last argument, in a scope like an app; never
        // recorded, and a failure names the program only (the URL holds what the user typed).
        Activation::Open { url } => {
            launch(ctx, l.config.open.iter().cloned().chain([url]).collect())?;
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
mod tests;
