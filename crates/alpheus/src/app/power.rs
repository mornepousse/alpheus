//! The Power tab's items: one button per session action.
//!
//! This tab has no state of its own and reads nothing: its row in `tab.rs` (`TabSpec`) lists no
//! domain and no refresh period. What a button does is shared code in `mod.rs`: `App::run_command`
//! asks for a confirmation first when the action needs one (`PowerAction::needs_confirm`: log out,
//! reboot and power off), and `App::close_confirm` queues `Op::Power` only when the answer is
//! Confirm (`close_confirm(false)` sends nothing). The commands themselves are in the backend
//! (`SessionPower`), which runs bar.toml's `[quick]` commands; an action whose command is empty is
//! not an item here and not a palette entry.
//!
//! Never:
//! - run a tool: the worker runs the command of `Op::Power`;
//! - draw anything: `ui/tabs.rs` draws the buttons, in this order.
//!
//! See: docs/features/control-center.md.

use super::Item;
use super::action::Command;
use crate::backends::{PowerAction, SessionCommands};

/// The focusable items of the Power tab: one button per action of `PowerAction::ALL` that
/// `session` offers (bar.toml's `[quick]`: an empty command hides its action), in that order
/// (Lock, Log out, Suspend, Reboot, Power off).
pub(super) fn items(session: &SessionCommands) -> Vec<Item> {
    PowerAction::ALL.iter().filter(|a| session.shows(**a)).map(|a| Item::Button(Command::Power(*a))).collect()
}
