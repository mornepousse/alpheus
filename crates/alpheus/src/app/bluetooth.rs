//! The Bluetooth tab's items: the Bluetooth switch and the button that opens the Bluetooth tool.
//!
//! The Bluetooth data (`App::data.bluetooth`, read by the worker as `Domain::Bluetooth`), the
//! switch itself (`ToggleId::Bluetooth`, changed by `App::toggle`) and the opening of the external
//! tool (`Command::Open`, `App::external_argv`) live in `mod.rs`: Home has the same switch, so the
//! code that changes it is shared. This file holds only what is the Bluetooth tab's own, its list
//! of items.
//!
//! Never:
//! - run a tool: `bluetoothctl` is run by the worker, and the pairing tool (`bluetui` by default)
//!   by `external_tools.rs`;
//! - draw anything: `ui/bluetooth.rs` draws the tab from the same items, in the same order.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, the Bluetooth row), docs/features/control-center.md.

use super::action::{Command, External};
use super::{Item, ToggleId};

/// The focusable items of the Bluetooth tab, in the order they are drawn: the Bluetooth switch,
/// then the "Pair a device" button.
///
/// `ui/bluetooth.rs` draws the switch before the list of connected devices and the button after
/// it, and finds them by these two positions (0 and 1).
pub(super) fn items() -> Vec<Item> {
    vec![Item::Toggle(ToggleId::Bluetooth), Item::Button(Command::Open(External::Bluetooth))]
}
