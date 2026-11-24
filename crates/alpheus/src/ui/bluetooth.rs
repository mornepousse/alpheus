//! The Bluetooth tab's rows and its shortcut hints (the shortcut bar, or the keycaps row in the rail).
//!
//! `tabs::render` draws the rows this file lists; the items themselves come from
//! `app/bluetooth.rs`, and a switch or a button is drawn by the shared item drawing in `tabs.rs`,
//! like the other item tabs. This file adds the list of connected devices between the two items.
//!
//! Never:
//! - change the state: everything here reads the `App` and fills a list of rows;
//! - run a tool or read a clock.
//!
//! See: docs/features/control-center.md.

use super::tabs::Row;
use crate::app::action::Action;
use crate::app::keymap::Keymap;
use crate::app::{App, Loadable};

/// Adds the Bluetooth tab's rows to `rows`: the Bluetooth switch, the "Connected devices" heading
/// with one line per device that passes the search, then the "Pair a device" button.
///
/// `rail` says the tab is drawn with the left rail (from 84 columns by 26 rows), where each line
/// starts with a glyph and the name is padded to 24, then comes the address; the classic layout
/// writes the name, then the address in parentheses. A list that has not loaded shows `…`, a failed read shows its error, an adapter
/// that is off shows `Bluetooth is off`, and an empty list shows `none`.
pub(super) fn rows(app: &App, rail: bool, rows: &mut Vec<Row>) {
    // The switch and the button are the two items of `items` in `app/bluetooth.rs`, by position.
    rows.push(Row::Item(0));
    rows.push(Row::Blank);
    rows.push(Row::Heading("Connected devices"));
    match &app.data.bluetooth {
        Loadable::Loading => rows.push(Row::Text("…".into())),
        Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
        Loadable::Ready(b) if !b.powered => rows.push(Row::Text("Bluetooth is off".into())),
        Loadable::Ready(b) => {
            let list: Vec<_> = b.connected.iter().filter(|d| app.matches_search(&d.name)).collect();
            if list.is_empty() {
                rows.push(Row::Text("none".into()));
            }
            for d in list {
                rows.push(Row::Text(if rail { format!("󰂱  {:<24} {}", d.name, d.mac) } else { format!("{}  ({})", d.name, d.mac) }));
            }
        }
    }
    rows.push(Row::Blank);
    rows.push(Row::Item(1));
}

/// The keys the shortcut bar (the keycaps row in the rail) offers on this tab: switch Bluetooth, open the pairing tool, and search
/// the devices.
pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap) {
    pairs.push((km.label(Action::Toggle), "Toggle"));
    pairs.push((km.label(Action::Activate), "Open"));
    pairs.push((km.label(Action::Search), "Search"));
}

#[cfg(test)]
mod tests;
