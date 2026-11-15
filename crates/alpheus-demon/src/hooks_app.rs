//! The event hooks' glue (hooks spec §§ 3 and 5): the feed points hand what changed to the pure
//! model (`hooks.rs`), and a `Fire` is started like the launcher starts an app, in a systemd scope
//! when a user manager exists and detached otherwise, with the event's variables added. The log
//! says fixed words only: never a command, a variable or a name.

use crate::config::ModuleId;
use crate::hooks::{Event, Fire, HookId, Hooks, Verdict};
use crate::launcher::run::{SYSTEMD_RUN, Spawner, spawner};
use crate::launcher::scan::has_program;
use crate::log::write as log;
use crate::state::Msg;
use crate::wayland::App;
use alpheus_commun::exec::{ExecError, Executor};
use alpheus_commun::paths;
use alpheus_commun::theme::Mode;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Instant;

pub(crate) struct HooksApp {
    pub(crate) model: Hooks,
    pub(crate) spawner: Spawner,
}

impl HooksApp {
    pub(crate) fn new(cfg: &crate::config::HooksConfig) -> Option<HooksApp> {
        Hooks::new(cfg, Instant::now()).map(|model| HooksApp { model, spawner: spawner(&paths::runtime_dir()) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    NotFound,
    SystemdRun,
    Start,
}

impl Failure {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Failure::NotFound => "program not found",
            Failure::SystemdRun => "systemd-run failed",
            Failure::Start => "could not start",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spawned {
    Ran,
    FellBack,
}

/// The directories of a `$PATH`, empty entries dropped.
pub(crate) fn path_dirs(path: Option<&OsStr>) -> Vec<PathBuf> {
    path.map(|p| std::env::split_paths(p).filter(|d| !d.as_os_str().is_empty()).collect()).unwrap_or_default()
}

fn start(exec: &dyn Executor, argv: &[String], env: &[(&str, &str)]) -> Result<(), ExecError> {
    let Some((prog, args)) = argv.split_first() else { return Err(ExecError::Io("empty command".into())) };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    exec.spawn_with_env(prog, &args, env)
}

/// Starts the hook: the program is looked up first (`systemd-run` starts detached, so a missing one
/// would fail unseen), then run in a scope, or bare when `systemd-run` itself is missing.
pub(crate) fn hook_spawn(exec: &dyn Executor, spawner: Spawner, path_dirs: &[PathBuf], fire: &Fire) -> Result<Spawned, Failure> {
    let Some(program) = fire.argv.first() else { return Err(Failure::Start) };
    if !has_program(path_dirs, program) {
        return Err(Failure::NotFound);
    }
    let env: Vec<(&str, &str)> = fire.env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    match spawner {
        Spawner::SystemdScope => {
            let scoped: Vec<String> = SYSTEMD_RUN.iter().map(|s| s.to_string()).chain(fire.argv.iter().cloned()).collect();
            match start(exec, &scoped, &env) {
                Ok(()) => Ok(Spawned::Ran),
                Err(ExecError::NotFound) => match start(exec, &fire.argv, &env) {
                    Ok(()) => Ok(Spawned::FellBack),
                    Err(_) => Err(Failure::Start),
                },
                Err(_) => Err(Failure::SystemdRun),
            }
        }
        Spawner::Detached => match start(exec, &fire.argv, &env) {
            Ok(()) => Ok(Spawned::Ran),
            Err(ExecError::NotFound) => Err(Failure::NotFound),
            Err(_) => Err(Failure::Start),
        },
    }
}

/// The start-up lines: how many hooks are set, then each one the configured `modules` never feed.
pub(crate) fn start_lines(model: &Hooks, modules: &[ModuleId]) -> Vec<String> {
    let mut lines = vec![format!("hooks: {} set", model.set())];
    for id in HookId::ALL {
        if model.wants(id) && !id.reads(modules) {
            lines.push(format!("hooks: {} never fires (needs {})", id.key(), id.needs()));
        }
    }
    lines
}

impl App {
    pub(crate) fn hooks_msg(&mut self, m: &Msg) {
        let Some(h) = self.hooks.as_mut() else { return };
        for e in h.model.on_msg(m) {
            self.hooks_event(e);
        }
    }

    pub(crate) fn hooks_mode(&mut self, mode: Mode) {
        let Some(h) = self.hooks.as_mut() else { return };
        if let Some(e) = h.model.on_mode(mode) {
            self.hooks_event(e);
        }
    }

    pub(crate) fn hooks_capture(&mut self) {
        let Some(h) = self.hooks.as_mut() else { return };
        for e in h.model.on_capture(self.bar.privacy.shown()) {
            self.hooks_event(e);
        }
    }

    pub(crate) fn hooks_dnd(&mut self, on: bool) {
        let Some(_) = self.hooks.as_ref() else { return };
        self.hooks_event(Event::Dnd(on));
    }

    pub(crate) fn hooks_started(&mut self) {
        let Some(h) = self.hooks.as_ref() else { return };
        for line in start_lines(&h.model, &self.bar.config.modules) {
            log(&line);
        }
        self.hooks_event(Event::Started);
    }

    fn hooks_event(&mut self, e: Event) {
        let Some(h) = self.hooks.as_mut() else { return };
        let spawner = h.spawner;
        match h.model.fire(e, Instant::now()) {
            Verdict::Run(f) => {
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
