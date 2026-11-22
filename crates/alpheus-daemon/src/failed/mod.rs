//! The failed-units module's pure half: the total of the two managers' counts, and the notice's rule.
//!
//! The `failed` module shows `󰀦 N` in the bar when systemd has units in the failed state. `N` is
//! the system manager's count plus the user manager's, each when it is known (`total`). `Notice`
//! decides when `[failed] notify` sends its one notification (`Notice::feed`), and `incoming` builds
//! that notification. The counts come from other files: the system manager's from the D-Bus thread
//! (`sources/dbus/units.rs`), the user manager's from `app.rs` in this folder; `notify/app.rs`
//! feeds the notice and sends it.
//!
//! Never:
//! - read a bus, a file or a clock: it only works on the counts it is handed;
//! - start anything (a thread, a process, a timer).
//!
//! See: docs/specs/2026-11-21-alpheus-diagnostics-design.md (§ 6), docs/features/failed.md, and
//! docs/decisions.md ("A source that is down is not a state", "The failed-units notice waits for
//! both managers").

use crate::config::{BarConfig, ModuleId};
use crate::notify::{Incoming, Urgency};
use crate::state::Failed;

/// The system count plus the user count, each when known; None when neither is known.
///
/// A count never known is left out of the sum, and nothing is drawn while neither has ever been
/// known (decision 13 of the diagnostics spec's "Decisions written down"). See docs/decisions.md,
/// "A source that is down is not a state".
pub fn total(f: &Failed) -> Option<u32> {
    match (f.system, f.user) {
        (None, None) => None,
        // `saturating_add` stops at the largest `u32` instead of wrapping round.
        (system, user) => Some(system.unwrap_or(0).saturating_add(user.unwrap_or(0))),
    }
}

/// When to send `[failed] notify`'s notice: once when the total rises above the last total
/// notified; a total of 0 re-arms it. Like the battery alerts, a notice counts as given only while
/// the server is serving, and the latest total is kept to be offered again when it starts.
///
/// See docs/decisions.md, "The failed-units notice waits for both managers".
#[derive(Debug, Default)]
pub struct Notice {
    /// The highest total a notice was sent for since the total was last 0.
    notified: u32,
    /// The latest total seen, to be offered again when the server starts.
    last: Option<u32>,
    /// The system manager has answered once (an unknown count included: the bus down or no owner
    /// also publishes `Units(None)`). The notice exists only with the module, which wants the
    /// domain, so an answer always comes.
    system_seen: bool,
    /// The user manager has answered once (or will never: no session bus, `user_units = false`).
    /// Before both have, the total is half of what it will be, and the first notice would carry it.
    user_seen: bool,
}

impl Notice {
    /// The system manager answered.
    pub fn settle_system(&mut self) {
        self.system_seen = true;
    }

    /// The user manager answered (or no client will ask it).
    pub fn settle_user(&mut self) {
        self.user_seen = true;
    }

    /// The latest total seen.
    pub fn last(&self) -> Option<u32> {
        self.last
    }

    /// Gives the notice a total. None means the total is unknown and nothing changes; `Some(n)` in
    /// the answer means: send the notice for `n` units.
    pub fn feed(&mut self, total: Option<u32>, serving: bool) -> Option<u32> {
        // `?` after a value that may be None ends the function with None: no total, nothing to do.
        let n = total?;
        self.last = Some(n);
        // Wait for both managers: see the type's comment.
        if !(self.system_seen && self.user_seen) {
            return None;
        }
        // A total of 0 re-arms the notice.
        if n == 0 {
            self.notified = 0;
            return None;
        }
        // Not serving: nothing is marked as notified, so the notice stays armed.
        if n > self.notified && serving {
            self.notified = n;
            return Some(n);
        }
        None
    }
}

/// The notice runs only with `[failed] notify` on and both the `failed` and `notifications`
/// modules in `modules`.
pub fn notice_wanted(c: &BarConfig) -> bool {
    c.failed.notify && c.modules.contains(&ModuleId::Failed) && c.modules.contains(&ModuleId::Notifications)
}

/// The notification for `n` failed units: "N units failed" ("1 unit failed" for one), pointing at
/// the Diag tab. Normal urgency, the server's default timeout (`expire_ms` of -1) and no action.
pub fn incoming(n: u32) -> Incoming {
    Incoming {
        app_name: "alpheus".into(),
        replaces_id: 0,
        summary: if n == 1 { "1 unit failed".to_string() } else { format!("{n} units failed") },
        body: "Open Diag in the control center".into(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
        expire_ms: -1,
        desktop_entry: None,
        category: None,
        transient: false,
        resident: false,
        tag: None,
    }
}

pub mod app;

#[cfg(test)]
mod tests;
