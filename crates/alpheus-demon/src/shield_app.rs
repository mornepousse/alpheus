//! Shield mode on the `App` (shield spec): the model, the start and the flag's re-reads. Nothing
//! exists without a configured `[shield]` (every call site is one `Option` check).

use crate::config::ShieldConfig;
use crate::log::write as log;
use crate::shield::{Edge, Shield, read_flag};
use crate::wayland::App;
use std::path::Path;
use std::time::Instant;

pub(crate) struct ShieldApp {
    pub(crate) model: Shield,
    pub(crate) config: ShieldConfig,
    /// The flag's directory is watched (the watch armed). Not: the flag is re-read after each
    /// command alpheus runs and at each panel open, never on a timer.
    pub(crate) watched: bool,
}

impl ShieldApp {
    /// None unless `[shield]` is configured.
    pub(crate) fn new(config: &ShieldConfig) -> Option<ShieldApp> {
        config.configured().then(|| ShieldApp { model: Shield::default(), config: config.clone(), watched: false })
    }

    pub(crate) fn flag(&self) -> &Path {
        Path::new(&self.config.flag)
    }
}

impl App {
    /// Reads the flag once and seeds the model (no notice, no hook), once the watch is armed.
    pub(crate) fn shield_start(&mut self) {
        let Some(sh) = self.shield.as_mut() else { return };
        let now = read_flag(sh.flag());
        sh.model.observe(now);
        if sh.model.first_unknown() {
            log("shield: the flag cannot be read");
        }
        self.shield_publish();
    }

    /// The flag's directory reported a change (or the queue overflowed), or a re-read is due: one
    /// reading, an edge logged, the view refreshed.
    pub(crate) fn shield_readable(&mut self) -> Option<Edge> {
        let sh = self.shield.as_mut()?;
        let now = read_flag(sh.flag());
        let edge = sh.model.observe(now);
        if sh.model.first_unknown() {
            log("shield: the flag cannot be read");
        }
        match edge {
            Some(Edge::On) => log("shield: now on"),
            Some(Edge::Off) => log("shield: now off"),
            None => {}
        }
        self.shield_publish();
        edge
    }

    /// `Bar.shield` follows the model; a change marks the bar dirty.
    pub(crate) fn shield_publish(&mut self) {
        let Some(sh) = self.shield.as_ref() else { return };
        let view = sh.model.view(&sh.config, Instant::now());
        if self.bar.shield != Some(view) {
            self.bar.shield = Some(view);
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests;
