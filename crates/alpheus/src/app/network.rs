//! The Network tab's items: the Wi-Fi switch and the button that opens the network tool.
//!
//! The network data (`App::data.network`, read by the worker as `Domain::Network`), the Wi-Fi
//! switch itself (`ToggleId::Wifi`, changed by `App::toggle`) and the opening of the external tool
//! (`Command::Open`, `App::external_argv`) live in `mod.rs`: Home has the same Wi-Fi switch, so the
//! code that changes it is shared. This file holds only what is the Network tab's own, its list of
//! items.
//!
//! Never:
//! - run a tool: `nmcli` is run by the worker, and the network tool (`nmtui` by default) by
//!   `external_tools.rs`;
//! - draw anything: `ui/network.rs` draws the tab from the same items, in the same order.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, the Network row), docs/features/control-center.md.

use super::action::{Command, External};
use super::{Item, ToggleId};

/// The focusable items of the Network tab, in the order they are drawn: the Wi-Fi switch, then
/// the "Choose a network" button.
///
/// `ui/network.rs` draws the switch before the list of active connections and the button after
/// it, and finds them by these two positions (0 and 1).
pub(super) fn items() -> Vec<Item> {
    vec![Item::Toggle(ToggleId::Wifi), Item::Button(Command::Open(External::Network))]
}
