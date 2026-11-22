//! The Home tab's items: two sliders, four switches and the energy profile.
//!
//! This tab has no state and no code of its own beyond its list of items. What the items do is
//! shared code in `mod.rs`: `App::adjust` moves the two sliders, `App::toggle` changes the four
//! switches, and `Command::SetProfile` changes the energy profile. Their data (`App::data`) is
//! read by the worker, and the tab's row in `tab.rs` says which domains and how often.
//!
//! Never:
//! - run a tool: the worker runs `wpctl`, `brightnessctl`, `nmcli` and the others;
//! - draw anything: `ui/home.rs` draws the dashboard and `ui/tabs.rs` the rows, from these items,
//!   and both find an item by its position in this list (`ui/home.rs` names the seven positions).
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, the Home row),
//! docs/specs/2026-11-15-alpheus-ui-refresh-design.md (Home as a dashboard: "Same items in the same
//! order as today's Home"), docs/features/control-center.md.

use super::{Item, SliderId, ToggleId};

/// The focusable items of the Home tab, in the order they are drawn: volume, brightness, Wi-Fi,
/// Bluetooth, do not disturb, keep awake, the energy profile.
pub(super) fn items() -> Vec<Item> {
    vec![
        Item::Slider(SliderId::Volume),
        Item::Slider(SliderId::Brightness),
        Item::Toggle(ToggleId::Wifi),
        Item::Toggle(ToggleId::Bluetooth),
        Item::Toggle(ToggleId::Dnd),
        Item::Toggle(ToggleId::KeepAwake),
        Item::Profile,
    ]
}
