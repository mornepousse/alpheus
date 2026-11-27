//! The wallpaper on the daemon's state: a name applied through the setter, the setter's pidfd on the
//! event loop, and what a set's ending leads to.
//!
//! This is the wiring half (the setter is `setter.rs`, the pure part `wallpaper.rs`). `wallpaper_apply`
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
use super::{WallpaperConfig, resolve_dir};
use crate::launcher::run::{self, RunContext};
use crate::launcher::scan::ScanEnv;
use crate::log::write as log;
use crate::wayland::App;
use alpheus_common::paths;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, Mode, PostAction, RegistrationToken};
use std::os::fd::OwnedFd;
use std::path::PathBuf;

impl App {
    /// Applies `name` of `w`'s folder: the setter started (the old one dropped only once it runs),
    /// watched, and on success the name saved and `then` started when `run_then`. A setter that
    /// cannot start leaves the wallpaper as it was.
    pub(crate) fn wallpaper_apply(&mut self, w: &WallpaperConfig, name: &str, run_then: bool) {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        let Some(dir) = resolve_dir(&w.dir, home.as_deref()) else {
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
