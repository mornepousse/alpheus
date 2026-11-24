//! The Power tab's shortcut hint.
//!
//! The tab's rows are not here: Home and Power both list one row per item with a blank row between
//! them, and that one arm is in `tabs.rs` (`Tab::Home | Tab::Power`). A button is drawn by the
//! shared item drawing in the same file, and the question it asks is drawn by `overlays.rs`
//! (`confirm`); only Log out, Reboot and Power off ask (`PowerAction::needs_confirm`). This file
//! holds only the key the shortcut bar (the keycaps row in the rail) offers on this tab.
//!
//! Never:
//! - change the state: everything here reads the keymap and fills a list of hints;
//! - run a tool or read a clock.
//!
//! See: docs/features/control-center.md.

use crate::app::action::Action;
use crate::app::keymap::Keymap;

/// The keys the shortcut bar (the keycaps row in the rail) offers on this tab: activate the focused button.
pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap) {
    pairs.push((km.label(Action::Activate), "Activate"));
}

#[cfg(test)]
mod tests;
