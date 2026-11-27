//! The wallpaper's setter: the one program the picker starts and the one it ever ends.
//!
//! `Setter::start` checks the name (a plain image name of a file in the folder), fills `[wallpaper]
//! set`'s `{file}` (one whole argument) and starts the setter as a watched child of the daemon
//! (`Executor::spawn_watched`, on the event loop's thread: its PDEATHSIG belongs to the launching
//! thread, so the setter dies with the daemon and nothing is left to chase). Only once the new one
//! started is the old handle dropped, which kills that child's own process group and reaps it: no
//! flash of an empty background, and a setter that cannot start leaves the wallpaper on screen as
//! it was. `restart = true` (swaybg, wbg) is applied at its start; `restart = false` (`swww img`)
//! once it exits 0, which the loop hears on its pidfd (`child_ended`). On success `apply_outcome`
//! saves the name (`remember`) and starts `then` in its scope; on failure neither happens.
//!
//! Never:
//! - end a process it did not start, or end its own any other way than dropping the handle: no
//!   signal, no name search, no PID read from a file (a guardrail,
//!   `the_wallpaper_ends_only_the_setter_it_started`);
//! - drop the setter running before the new one has started (the same guardrail, and
//!   `a_restart_drops_only_the_old_handle_after_the_new_one_started`);
//! - start a name that is not a plain image name of a file in the folder;
//! - log or panic: the glue (`app.rs`) logs one fixed word per ending.
//!
//! See: docs/features/wallpaper.md, docs/superpowers/specs/2026-11-27-wallpaper-design.md.

use super::{WallpaperConfig, fill, is_image_name, is_plain_name};
use crate::launcher::run::{self, RunContext};
use crate::state_file;
use alpheus_common::exec::{Executor, Watched};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// A set that worked: what follows it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The name in the folder, remembered for the next start.
    pub name: String,
    /// `dir/name`, for `then`'s `{file}`.
    pub file: PathBuf,
    /// `then` as configured, or empty when it must not run (the daemon's start re-applying).
    pub then: Vec<String>,
    pub remember: bool,
}

/// How a start went.
#[derive(Debug)]
pub enum Started {
    /// A setter that runs until replaced: applied now.
    Running(Applied),
    /// A one-shot setter: applied once it exits 0 (`Setter::child_ended`).
    Pending,
}

/// How a watched setter ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// A one-shot setter exited 0.
    Applied,
    /// A one-shot setter exited otherwise, or was ended by a signal.
    Failed,
    /// A setter meant to run until replaced ended on its own.
    Gone,
}

/// Why a start did nothing: the name was refused, or the setter could not start. Nothing was
/// dropped either way; the glue logs one fixed word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotStarted;

/// At most one running setter, and at most one one-shot setter not yet ended.
#[derive(Default)]
pub struct Setter {
    child: Option<Box<dyn Watched>>,
    pending: Option<(Box<dyn Watched>, Applied)>,
}

impl Setter {
    pub fn new() -> Setter {
        Setter::default()
    }

    /// Starts `w.set` on `dir/name`. Err (nothing started, nothing dropped) for a name that is not
    /// a plain image name of a file in `dir`, or a setter that cannot start. `run_then`: whether
    /// `then` follows this set.
    pub fn start(
        &mut self,
        exec: &dyn Executor,
        w: &WallpaperConfig,
        dir: &Path,
        name: &str,
        run_then: bool,
    ) -> Result<Started, NotStarted> {
        if !is_plain_name(name) || !is_image_name(name) {
            return Err(NotStarted);
        }
        let file = dir.join(name);
        if !std::fs::metadata(&file).is_ok_and(|m| m.is_file()) {
            return Err(NotStarted);
        }
        let argv = fill(&w.set, &file);
        let (program, args) = argv.split_first().ok_or(NotStarted)?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let started = exec.spawn_watched(program, &args, None).map_err(|_| NotStarted)?;
        let applied =
            Applied { name: name.to_string(), file, then: if run_then { w.then.clone() } else { Vec::new() }, remember: w.remember };
        if w.restart {
            // The new setter runs: the old one goes (killed by its handle, reaped), and a one-shot
            // still pending with it, whose ending would no longer mean anything.
            self.pending = None;
            self.child = Some(started);
            Ok(Started::Running(applied))
        } else {
            // The setter running stays until this one has worked (`child_ended`).
            self.pending = Some((started, applied));
            Ok(Started::Pending)
        }
    }

    /// The descriptor that turns readable when the setter just started ends, for the loop to watch.
    pub fn exit_fd(&self) -> Option<OwnedFd> {
        match &self.pending {
            Some((child, _)) => child.exit_fd(),
            None => self.child.as_ref().and_then(|c| c.exit_fd()),
        }
    }

    /// One setter's end, once one has ended (each told once; reaped here): a one-shot's exit 0 is
    /// `Applied` with what follows it, and then the setter that ran before it goes. None while every
    /// setter runs.
    pub fn child_ended(&mut self) -> Option<(Ended, Option<Applied>)> {
        if let Some(exit) = self.pending.as_mut().and_then(|(child, _)| child.try_exit()) {
            let (_, applied) = self.pending.take()?;
            if exit == Some(0) {
                self.child = None;
                return Some((Ended::Applied, Some(applied)));
            }
            return Some((Ended::Failed, None));
        }
        if self.child.as_mut().and_then(|child| child.try_exit()).is_some() {
            self.child = None;
            return Some((Ended::Gone, None));
        }
        None
    }
}

/// What followed a set that worked: whether the name was saved (None: not remembered) and whether
/// `then` started (None: no `then`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub saved: Option<bool>,
    pub then: Option<bool>,
}

/// After a set that worked: the name saved under `state_dir` when remembered, then `then` filled
/// with the file and started in its scope (`run::launch`, fire and forget). Called only on success:
/// a failed set keeps the remembered name and runs nothing.
pub fn apply_outcome(ctx: &RunContext, state_dir: &Path, applied: &Applied) -> Outcome {
    let saved = applied.remember.then(|| state_file::save_wallpaper(state_dir, &applied.name).is_ok());
    let then = (!applied.then.is_empty()).then(|| run::launch(ctx, fill(&applied.then, &applied.file)).is_ok());
    Outcome { saved, then }
}

#[cfg(test)]
mod tests;
