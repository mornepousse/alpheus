//! The Home tab's items: two sliders, four switches and the energy profile.

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
