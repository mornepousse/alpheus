//! The Network tab's items: the Wi-Fi switch and the button that opens the network tool.
//!
//! Three things live in `mod.rs` and `action.rs`, each because another place uses it too:
//! - the Wi-Fi switch (`ToggleId::Wifi`, changed by `App::toggle`): Home has the same switch
//!   (`home::items`);
//! - the network data (`App::data.network`, read by the worker as `Domain::Network`): Home reads
//!   that domain too (its row in `tab.rs`);
//! - the opening of the external tool (`Command::Open`, `App::external_argv`): the palette's
//!   "Choose a network" entry (`palette_entries`) and Sound's mixer button run the same command.
//!
//! This file holds only what is the Network tab's own, its list of items.
//!
//! Never:
//! - run a tool: `nmcli` is run by the worker, and the network tool (`nmtui` by default) by
//!   `external_tools.rs`;
//! - draw anything: `ui/network.rs` draws the tab from the same items, in the same order.
//!
//! See: docs/features/control-center.md.

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
