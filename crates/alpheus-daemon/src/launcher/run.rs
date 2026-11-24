//! Activation → argv → the executor: the only place
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
use crate::launcher::tools::catalog::{BUILTINS, Choices, EntryKind};
use crate::launcher::tools::config::{CHOICE_PLACEHOLDER, EntryConfig, TEXT_PLACEHOLDER};
use crate::launcher::tools::nav::ToolAction;
use crate::log::write as log;
use alpheus_common::exec::{ExecError, Executor};
use alpheus_common::launcher_ipc::{Reply, wipe};
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

/// A user entry's argv: `run` with `terminal` or `terminal_hold` in front, and the
/// one element that is exactly `{choice}` replaced by the chosen line, and the one that is exactly
/// `{text}` by the typed text, each whole: a space, a quote, `;`, `-rf` or `{choice}` in them stays
/// inside that one argument (no shell is involved).
pub fn tool_argv(e: &EntryConfig, choice: Option<&str>, text: Option<&str>, terminal: &[String], terminal_hold: &[String]) -> Vec<String> {
    let front: &[String] = if e.hold {
        terminal_hold
    } else if e.terminal {
        terminal
    } else {
        &[]
    };
    let run = e.run.iter().map(|a| match (choice, text) {
        (Some(c), _) if a == CHOICE_PLACEHOLDER => c.to_string(),
        (_, Some(t)) if a == TEXT_PLACEHOLDER => t.to_string(),
        _ => a.clone(),
    });
    front.iter().cloned().chain(run).collect()
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
    /// A built-in of the Tools menu (an index into `BUILTINS`): the box closes, then the glue
    /// replays its verb.
    Builtin(usize),
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
/// find would fail after the launcher closed, silently (the box
/// says so instead, and stays open).
pub(crate) fn launch(ctx: &RunContext, argv: Vec<String>) -> Result<(), String> {
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

/// Ids only, never typed text; a save error is logged, not shown.
/// On the event loop: one small write and a rename, no fsync (`History::save`;
/// a crash may lose the last use, never the file).
fn remember(l: &mut Launcher, ctx: &RunContext, key: &str) {
    let Some(path) = ctx.history_path else { return };
    l.history.record(key, l.now);
    if let Err(e) = l.history.save(path) {
        log(&format!("launcher history not saved ({})", crate::log::error_word(&e)));
    }
}

/// A Tools menu row. A built-in is recorded under `tool:builtin:<id>` and handed
/// back for the glue to replay; a user entry is launched through the scope, recorded under its
/// key (a fixed choice under its own; a `choices_cmd` line never), and logged by position only.
// one row per kind of choices: each case reads on its own.
#[allow(clippy::match_same_arms)]
fn perform_tool(l: &mut Launcher, action: ToolAction, ctx: &RunContext) -> Result<Done, String> {
    let gone = || "the list changed: try again".to_string();
    match action {
        // A built-in's fixed choice is kept as `tool:builtin:<id>/<choice>` (the catalog's own key);
        // what was typed never is.
        ToolAction::Builtin { index, choice, text: _ } => {
            let id = BUILTINS.get(index).ok_or_else(gone)?.id;
            match choice {
                Some(c) => remember(l, ctx, &format!("tool:builtin:{id}/{c}")),
                None => remember(l, ctx, &format!("tool:builtin:{id}")),
            }
            Ok(Done::Builtin(index))
        }
        ToolAction::User { entry, choice, text } => {
            let catalog = &l.tools().ok_or_else(gone)?.catalog;
            let e = catalog.entries.get(entry).ok_or_else(gone)?;
            let EntryKind::User(at) = e.kind else { return Err(gone()) };
            let config = l.config.tools.entries.get(at).ok_or_else(gone)?;
            let argv = tool_argv(config, choice.as_deref(), text.as_deref(), &l.config.terminal, &l.config.terminal_hold);
            let key = match (&e.choices, &choice) {
                (Choices::Fixed(_), Some(c)) => Some(catalog.choice_key(entry, c)),
                // A process list or a file name is never kept.
                (Choices::Command, _) => None,
                (_, None) => Some(e.key.clone()),
                (Choices::None, Some(_)) => None,
            };
            let i = config.position;
            launch(ctx, argv)?;
            log(&format!("tools: entry {i} run"));
            if let Some(key) = key {
                remember(l, ctx, &key);
            }
            Ok(Done::Close)
        }
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
        // Never recorded: no typed text, and a dmenu line is not an app.
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
        // A history row never runs here: the app copies it from its own history first.
        Activation::Clip { .. } => return Err("not a launcher action".to_string()),
        Activation::Tool(action) => return perform_tool(l, action, ctx),
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
