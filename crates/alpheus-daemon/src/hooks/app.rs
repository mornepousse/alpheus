//! The event hooks' wiring: it hands each change to the pure model, and starts the command of the
//! hook that fires.
//!
//! The `hooks_*` methods on `App` are the feed points: the places where something changes call
//! one of them, and it passes the change to `Hooks` (`mod.rs`), which decides whether a hook runs.
//! A `Fire` is then started like the launcher starts an app: in a systemd user scope when a user
//! manager exists, detached otherwise, with the event's variables added through the executor's
//! `spawn_with_env`. `hook_spawn` looks the program up first. The log says fixed words only.
//!
//! Never:
//! - wait for a hook, read its output or kill it;
//! - put event data in the command's arguments: it goes in the environment only;
//! - log a command, an argument, a variable's value or a name (a guardrail reads every log
//!   statement of this file);
//! - start a thread: nothing here runs off the event loop.
//!
//! See: docs/specs/2026-11-15-alpheus-hooks-design.md (§§ 3, 5), docs/features/hooks.md, and
//! docs/decisions.md ("A hook runs in its own systemd scope", "A hook's data reaches it through the
//! environment, never the arguments").

use crate::config::ModuleId;
use crate::hooks::{Event, Fire, HookId, Hooks, Verdict};
use crate::launcher::run::{SYSTEMD_RUN, Spawner, spawner};
use crate::launcher::scan::has_program;
use crate::log::write as log;
use crate::state::Msg;
use crate::wayland::App;
use alpheus_common::exec::{ExecError, Executor};
use alpheus_common::paths;
use alpheus_common::theme::Mode;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Instant;

/// The hooks' state on the `App`: the model, and how a command is started.
///
/// (`pub(crate)` means the item is visible inside the daemon, and nowhere outside it.)
pub(crate) struct HooksApp {
    pub(crate) model: Hooks,
    /// In a systemd scope, or detached: decided once at start.
    pub(crate) spawner: Spawner,
}

impl HooksApp {
    /// None when no hook is set: the daemon then keeps nothing of this module.
    pub(crate) fn new(cfg: &crate::config::HooksConfig) -> Option<HooksApp> {
        // `?` after a value that may be None ends the function with None.
        let model = Hooks::new(cfg, Instant::now())?;
        // The spawner is chosen once, at start, as the launcher does.
        Some(HooksApp { model, spawner: spawner(&paths::runtime_dir()) })
    }
}

/// Why a hook could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    NotFound,
    SystemdRun,
    Start,
}

impl Failure {
    /// The fixed words the log says for each failure.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Failure::NotFound => "program not found",
            Failure::SystemdRun => "systemd-run failed",
            Failure::Start => "could not start",
        }
    }
}

/// How a started hook ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spawned {
    /// As the spawner says: in a scope, or detached.
    Ran,
    /// In a scope was wanted, but `systemd-run` is missing: it ran detached.
    FellBack,
}

/// The directories of a `$PATH`, empty entries dropped. None (no `$PATH`) gives no directory.
pub(crate) fn path_dirs(path: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(path) = path else { return Vec::new() };
    std::env::split_paths(path).filter(|dir| !dir.as_os_str().is_empty()).collect()
}

/// Starts `argv` detached with `env` added to the inherited environment and the `unset` variables
/// removed from it.
fn start(exec: &dyn Executor, argv: &[String], env: &[(&str, &str)], unset: &[&str]) -> Result<(), ExecError> {
    let Some((prog, args)) = argv.split_first() else { return Err(ExecError::Io("empty command".into())) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.spawn_with_env(prog, &args, env, unset)
}

/// Starts the hook: the program is looked up first (a scope starts asynchronously, so a missing
/// program would otherwise fail unseen), then run in a scope, or bare when `systemd-run` itself is
/// missing.
pub(crate) fn hook_spawn(exec: &dyn Executor, spawner: Spawner, path_dirs: &[PathBuf], fire: &Fire) -> Result<Spawned, Failure> {
    let Some(program) = fire.argv.first() else { return Err(Failure::Start) };
    if !has_program(path_dirs, program) {
        return Err(Failure::NotFound);
    }
    let env: Vec<(&str, &str)> = fire.env.iter().map(|(name, value)| (*name, value.as_str())).collect();
    let unset = fire.unset.as_slice();
    match spawner {
        Spawner::SystemdScope => {
            // The scope's command line, then the hook's own argv after it.
            let mut scoped: Vec<String> = SYSTEMD_RUN.iter().map(|word| word.to_string()).collect();
            scoped.extend(fire.argv.iter().cloned());
            match start(exec, &scoped, &env, unset) {
                Ok(()) => Ok(Spawned::Ran),
                // `systemd-run` is not there: run the hook bare.
                Err(ExecError::NotFound) => match start(exec, &fire.argv, &env, unset) {
                    Ok(()) => Ok(Spawned::FellBack),
                    Err(_) => Err(Failure::Start),
                },
                Err(_) => Err(Failure::SystemdRun),
            }
        }
        Spawner::Detached => match start(exec, &fire.argv, &env, unset) {
            Ok(()) => Ok(Spawned::Ran),
            Err(ExecError::NotFound) => Err(Failure::NotFound),
            Err(_) => Err(Failure::Start),
        },
    }
}

/// The start-up lines: how many hooks are set, then each one the configured `modules` never feed
/// (a hook never adds a source: see docs/decisions.md, "A hook never adds a source").
pub(crate) fn start_lines(model: &Hooks, modules: &[ModuleId], shield_configured: bool) -> Vec<String> {
    let mut lines = vec![format!("hooks: {} set", model.set())];
    for id in HookId::ALL {
        if model.wants(id) && !id.reads(modules, shield_configured) {
            lines.push(format!("hooks: {} never fires (needs {})", id.key(), id.needs()));
        }
    }
    lines
}

impl App {
    /// A source's message: the model works out the edges it makes, and each one is run.
    pub(crate) fn hooks_msg(&mut self, m: &Msg) {
        let Some(h) = self.hooks.as_mut() else { return };
        for e in h.model.on_msg(m) {
            self.hooks_event(e);
        }
    }

    /// `settled`: the mode is a chosen one or the portal has spoken (an `auto` fallback seeds nothing).
    pub(crate) fn hooks_mode(&mut self, mode: Mode, settled: bool) {
        let Some(h) = self.hooks.as_mut() else { return };
        if let Some(e) = h.model.on_mode(mode, settled) {
            self.hooks_event(e);
        }
    }

    /// The privacy indicators changed.
    pub(crate) fn hooks_capture(&mut self) {
        let Some(h) = self.hooks.as_mut() else { return };
        for e in h.model.on_capture(self.bar.privacy.shown()) {
            self.hooks_event(e);
        }
    }

    /// Do-not-disturb turned on or off, by any path.
    pub(crate) fn hooks_dnd(&mut self, on: bool) {
        let Some(_) = self.hooks.as_ref() else { return };
        self.hooks_event(Event::Dnd(on));
    }

    /// The daemon started its sources: the start-up lines, then the `started` hook.
    pub(crate) fn hooks_started(&mut self) {
        let Some(h) = self.hooks.as_ref() else { return };
        for line in start_lines(&h.model, &self.bar.config.modules, self.shield.is_some()) {
            log(&line);
        }
        self.hooks_event(Event::Started);
    }

    /// Shield mode changed (a known state turning into the other known one: `shield_readable`).
    pub(crate) fn hooks_shield(&mut self, on: bool) {
        let Some(_) = self.hooks.as_ref() else { return };
        self.hooks_event(Event::Shield(on));
    }

    /// A profile the compositor accepted (the name is cleaned for the variable).
    pub(crate) fn hooks_displays(&mut self, profile: &str) {
        let Some(_) = self.hooks.as_ref() else { return };
        self.hooks_event(Event::Displays(profile.to_string()));
    }

    /// Runs `e`'s hook if the model says it fires now, and writes the one log line for it.
    fn hooks_event(&mut self, e: Event) {
        let Some(h) = self.hooks.as_mut() else { return };
        let spawner = h.spawner;
        let now = Instant::now();
        if e.id() != HookId::Started && h.model.quiet_over(now) {
            log("hooks: quiet window over");
        }
        match h.model.fire(e, now) {
            Verdict::Run(f) => {
                // `$PATH` is read at each run (a few lookups on a path that is rate-limited), so a
                // program installed after the start is found.
                let dirs = path_dirs(std::env::var_os("PATH").as_deref());
                let key = f.id.key();
                match hook_spawn(&*self.exec, spawner, &dirs, &f) {
                    Ok(Spawned::Ran) => log(&format!("hook: {key}")),
                    Ok(Spawned::FellBack) => log(&format!("hook: {key} detached (systemd-run not found)")),
                    Err(why) => log(&format!("hook: {key} failed ({})", why.word())),
                }
            }
            Verdict::Throttled(id) => log(&format!("hook: {} throttled", id.key())),
            Verdict::Dropped => {}
        }
    }
}

#[cfg(test)]
mod tests;
