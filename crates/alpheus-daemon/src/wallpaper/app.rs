//! The wallpaper on the daemon's state: a name applied through the setter, the setter's pidfd on the
//! event loop, and what a set's ending leads to.
//!
//! This is the wiring half (the setter is `setter.rs`, the pure part `wallpaper.rs`). It also
//! applies on the daemon's own: the remembered name once the loop exists at start (without `then`:
//! the colour files are already written), and the folder's next name for `alpheus-launch
//! --wallpaper next` (an external timer's). `wallpaper_apply`
//! runs on the loop's thread, so the setter's PDEATHSIG is the daemon's own. The pidfd of each
//! setter started is a `Generic` source, silent until that setter ends: no wake-up while it runs.
//! A source fires once and goes; the one of a setter replaced fires right after the drop that ended
//! it and finds nothing left to tell.
//!
//! Never:
//! - log a file name, the folder or an argv: one fixed word per ending (a guardrail,
//!   `the_wallpaper_logs_fixed_words_only`);
//! - end a setter any other way than `Setter` does (`the_wallpaper_ends_only_the_setter_it_started`).
//!
//! See: docs/features/wallpaper.md.

use super::setter::{Applied, Ended, Started, apply_outcome};
use super::{MAX_FILES, WallpaperConfig, is_image_name, list, next_after, resolve_dir};
use crate::launcher::config::{LauncherConfig, config_path};
use crate::launcher::run::{self, RunContext};
use crate::launcher::scan::ScanEnv;
use crate::log::write as log;
use crate::state_file::read_wallpaper;
use crate::wayland::App;
use alpheus_common::launcher_ipc::{Reply, Request};
use alpheus_common::paths;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction, RegistrationToken};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// A name the daemon applies on its own, and whether `then` follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    pub name: String,
    pub run_then: bool,
}

/// The name to apply at start: with the picker on and `remember` set, the remembered name when it
/// is still an image file of the folder. `then` does not follow (the colour files it writes are
/// already there). Off, nothing is read.
pub(crate) fn start_choice(w: &WallpaperConfig, state_dir: &Path, home: Option<&Path>) -> Option<Choice> {
    if !w.enabled() || !w.remember {
        return None;
    }
    let dir = resolve_dir(&w.dir, home)?;
    // `read_wallpaper` refuses a name with a `/`, `..` or NUL: the join stays in the folder.
    let name = read_wallpaper(state_dir)?;
    // `Setter::start` checks this again; checked here too so that a remembered file since removed
    // is skipped quietly instead of logged as a setter that could not start at every login.
    let file = std::fs::metadata(dir.join(&name)).is_ok_and(|m| m.is_file());
    (is_image_name(&name) && file).then_some(Choice { name, run_then: false })
}

/// The name for `--wallpaper next`: the folder's name after the current one (the first when none,
/// or when it is no longer listed), with `then`. The current one is the remembered name, or with
/// `remember = false` `last`, the name the running daemon last applied (nothing on disk). Err: the
/// fixed words of the reply.
pub(crate) fn next_choice(w: &WallpaperConfig, state_dir: &Path, home: Option<&Path>, last: Option<&str>) -> Result<Choice, &'static str> {
    if !w.enabled() {
        return Err("wallpaper is off");
    }
    let dir = resolve_dir(&w.dir, home).ok_or("the folder cannot be read")?;
    let listing = list(&dir, MAX_FILES).map_err(|_| "the folder cannot be read")?;
    let remembered = if w.remember { read_wallpaper(state_dir) } else { last.map(str::to_owned) };
    let name = next_after(&listing.names, remembered.as_deref()).ok_or("no image in the folder")?;
    Ok(Choice { name: name.to_string(), run_then: true })
}

/// The home a `~/` folder resolves against: `$HOME` when absolute.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())
}

impl App {
    /// Applies `name` of `w`'s folder: the setter started (the old one dropped only once it runs),
    /// watched, and on success the name saved and `then` started when `run_then`. A setter that
    /// cannot start leaves the wallpaper as it was.
    pub(crate) fn wallpaper_apply(&mut self, w: &WallpaperConfig, name: &str, run_then: bool) {
        let Some(dir) = resolve_dir(&w.dir, home().as_deref()) else {
            log("wallpaper: setter could not start");
            return;
        };
        let Ok(started) = self.wallpaper.start(self.exec.as_ref(), w, &dir, name, run_then) else {
            log("wallpaper: setter could not start");
            return;
        };
        // Without a loop or a descriptor the setter still runs; its end is then heard only when the
        // next start drops it (a zombie at most until then).
        let fd = self.wallpaper.exit_fd();
        let _ = self.wallpaper_watch(fd);
        if let Started::Running(applied) = started {
            self.wallpaper_succeeded(&applied);
        }
    }

    /// At start, once the loop exists: the remembered wallpaper applied again, `then` not run.
    pub(crate) fn wallpaper_at_start(&mut self, w: &WallpaperConfig) {
        if let Some(choice) = start_choice(w, &paths::state_dir(), home().as_deref()) {
            self.wallpaper_apply(w, &choice.name, choice.run_then);
        }
    }

    /// `alpheus-launch --wallpaper next` (`launcher/socket_glue.rs`): `launcher.toml` read again (as
    /// an open does), the folder listed once, the name after the remembered one applied. Done once
    /// the setter was asked to start (its failure is logged, as a pick's is); an error in fixed
    /// words when the picker is off or the folder holds no image.
    pub(crate) fn wallpaper_verb(&mut self, request: &Request) -> Reply {
        let Request::Wallpaper(_) = request else { return Reply::Done };
        let (config, _) = LauncherConfig::load(&config_path(&paths::config_dir()));
        let w = config.wallpaper;
        match next_choice(&w, &paths::state_dir(), home().as_deref(), self.wallpaper.last_applied()) {
            Ok(choice) => {
                self.wallpaper_apply(&w, &choice.name, choice.run_then);
                Reply::Done
            }
            Err(why) => {
                log("wallpaper: next refused");
                Reply::Error(why.into())
            }
        }
    }

    /// Puts a setter's pidfd on the loop: readable once that setter has ended, quiet until then.
    /// None without a loop or a descriptor.
    fn wallpaper_watch(&mut self, fd: Option<OwnedFd>) -> Option<RegistrationToken> {
        let (Some(handle), Some(fd)) = (self.loop_handle.clone(), fd) else { return None };
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        handle
            .insert_source(source, |_, _, app: &mut App| {
                app.wallpaper_child_ready();
                Ok(PostAction::Remove)
            })
            .ok()
    }

    /// A setter's pidfd turned readable: every ending there is to tell is told (and reaped). The
    /// source goes: its setter has ended, whether this call or an earlier one told it.
    fn wallpaper_child_ready(&mut self) {
        while let Some((ended, applied)) = self.wallpaper.child_ended() {
            match (ended, applied) {
                (Ended::Applied, Some(applied)) => self.wallpaper_succeeded(&applied),
                (Ended::Applied | Ended::Failed, _) => log("wallpaper: setter failed"),
                (Ended::Gone, _) => log("wallpaper: setter ended"),
            }
        }
    }

    /// A set worked: the name saved when remembered, `then` started when there is one.
    fn wallpaper_succeeded(&mut self, applied: &Applied) {
        let path_dirs = if applied.then.is_empty() { Vec::new() } else { ScanEnv::from_env(|k| std::env::var_os(k)).path_dirs };
        let ctx = RunContext {
            exec: self.exec.as_ref(),
            spawner: run::spawner(&paths::runtime_dir()),
            history_path: None,
            path_dirs: &path_dirs,
        };
        let outcome = apply_outcome(&ctx, &paths::state_dir(), applied);
        log("wallpaper: applied");
        if outcome.saved == Some(false) {
            log("wallpaper: not saved");
        }
        if outcome.then == Some(false) {
            log("wallpaper: then could not start");
        }
    }
}

#[cfg(test)]
mod tests;
