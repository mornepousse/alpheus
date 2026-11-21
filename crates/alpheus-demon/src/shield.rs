//! Shield mode's model (shield spec §§ 3 and 7): the state read from the flag file, its edges, and
//! what the bar and the quick panel read of it. Pure apart from `read_flag`: the commands, the
//! watch and the notices live in `shield_app.rs`, `shield_run.rs` and `watch.rs`.

use crate::config::ShieldConfig;
use std::path::Path;
use std::time::Instant;

/// The flag's existence: `Some(true)` while it exists, `Some(false)` when it does not, None when it
/// cannot be told (any error but "not found": never "off" on a guess).
pub fn read_flag(flag: &Path) -> Option<bool> {
    flag.try_exists().ok()
}

/// A change between the two known states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    On,
    Off,
}

/// What the bar and the panel read: a `Copy` view kept up to date by `ShieldApp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShieldView {
    pub state: Option<bool>,
    /// A switch waits for its flag.
    pub switching: bool,
    /// The row reads `failed` until then.
    pub failed_until: Option<Instant>,
    /// `blocked` is set: the detail exists.
    pub can_list: bool,
    /// `allow` is set: a device can be allowed.
    pub can_allow: bool,
}

#[derive(Debug, Default)]
pub struct Shield {
    state: Option<bool>,
    last_known: Option<bool>,
    /// The unknown spell already logged.
    unknown_noted: bool,
}

impl Shield {
    pub fn state(&self) -> Option<bool> {
        self.state
    }

    /// A reading of the flag. The first known one seeds (`last_known` empty: no edge); afterwards a known state
    /// turning into the other known state is an edge; unknown in between changes nothing (`on,
    /// unknown, on` is no change, `on, unknown, off` is one).
    pub fn observe(&mut self, now: Option<bool>) -> Option<Edge> {
        self.state = now;
        let now = now?;
        self.unknown_noted = false;
        let edge = match self.last_known {
            Some(before) if before != now => Some(if now { Edge::On } else { Edge::Off }),
            _ => None,
        };
        self.last_known = Some(now);
        edge
    }

    /// True once per unknown spell, for the log line.
    pub fn first_unknown(&mut self) -> bool {
        if self.state.is_some() || self.unknown_noted {
            return false;
        }
        self.unknown_noted = true;
        true
    }

    pub fn view(&self, config: &ShieldConfig, _now: Instant) -> ShieldView {
        ShieldView {
            state: self.state,
            switching: false,
            failed_until: None,
            can_list: !config.blocked_argv().is_empty(),
            can_allow: !config.allow.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests;
