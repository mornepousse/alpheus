//! The failed-units module's pure parts (diagnostics spec § 6): the total of the two managers'
//! counts, and the notice's arming rule. Nothing here reads a bus; `failed_app.rs` and
//! `notify_app.rs` feed it.

use crate::config::{BarConfig, ModuleId};
use crate::notify::{Incoming, Urgency};
use crate::state::Failed;

/// The system count plus the user count, each when known; None when neither is (a source that is
/// down is not a state).
pub fn total(f: &Failed) -> Option<u32> {
    match (f.system, f.user) {
        (None, None) => None,
        (s, u) => Some(s.unwrap_or(0).saturating_add(u.unwrap_or(0))),
    }
}

/// When to send `[failed] notify`'s notice: once when the total rises above the last total
/// notified; a total of 0 re-arms it. Like the battery alerts, a notice counts as given only while
/// the server is serving, and the latest total is kept to be offered again when it starts.
#[derive(Debug, Default)]
pub struct Notice {
    notified: u32,
    last: Option<u32>,
    /// Each manager has answered once (or will never: no session bus, `user_units = false`): before
    /// that the total is half of what it will be, and the first notice would be the wrong one.
    system_seen: bool,
    user_seen: bool,
}

impl Notice {
    /// The system manager answered (or its domain says it never will).
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

    /// A total (None: unknown, nothing changes). Some(n): send the notice for `n` units.
    pub fn feed(&mut self, total: Option<u32>, serving: bool) -> Option<u32> {
        let n = total?;
        self.last = Some(n);
        if !(self.system_seen && self.user_seen) {
            return None;
        }
        if n == 0 {
            self.notified = 0;
            return None;
        }
        if n > self.notified && serving {
            self.notified = n;
            return Some(n);
        }
        None
    }
}

/// The notice runs with both modules and the option on.
pub fn notice_wanted(c: &BarConfig) -> bool {
    c.failed.notify && c.modules.contains(&ModuleId::Failed) && c.modules.contains(&ModuleId::Notifications)
}

/// The notification: "N units failed", pointing at the Diag tab.
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
