//! The Power tab's items: one button per session action.
//!
//! This tab has no state of its own and reads nothing: its row in `tab.rs` (`TabSpec`) lists no
//! domain and no refresh period. What a button does is shared code in `mod.rs`: `App::run_command`
//! asks for a confirmation first when the action needs one (`PowerAction::needs_confirm`: log out,
//! reboot and power off), and `App::close_confirm` queues `Op::Power` only when the answer is
//! Confirm (`close_confirm(false)` sends nothing). The commands themselves are in the backend
//! (`SessionPower`).
//!
//! Never:
//! - run a tool: the worker runs the command of `Op::Power`;
//! - draw anything: `ui/tabs.rs` draws the buttons, in this order.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 2, the mandatory confirmation, and § 3, the
//! Power row), docs/features/control-center.md.

use super::Item;
use super::action::Command;
use crate::backends::PowerAction;

/// The focusable items of the Power tab: one button per action of `PowerAction::ALL`, in that
/// order (Lock, Log out, Suspend, Reboot, Power off).
pub(super) fn items() -> Vec<Item> {
    PowerAction::ALL.iter().map(|a| Item::Button(Command::Power(*a))).collect()
}
