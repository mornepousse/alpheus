//! The Network tab's rows and its shortcut hints.
//!
//! `tabs::render` draws the rows this file lists; the items themselves come from
//! `app/network.rs`, and a switch or a button is drawn by the shared item drawing in `tabs.rs`,
//! like the other item tabs. This file adds the list of active connections between the two items.
//!
//! Never:
//! - change the state: everything here reads the `App` and fills a list of rows;
//! - run a tool or read a clock.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, the Network row),
//! docs/specs/2026-11-15-alpheus-ui-refresh-design.md (the rail and the tab hues),
//! docs/features/control-center.md.

use super::tabs::Row;
use crate::app::action::Action;
use crate::app::keymap::Keymap;
use crate::app::{App, Loadable};

/// Adds the Network tab's rows to `rows`: the Wi-Fi switch, the "Active connections" heading with
/// one line per connection that passes the search, then the "Choose a network" button.
///
/// `rail` says the tab is drawn with the left rail (from 84 columns by 26 rows), where each line
/// starts with a glyph for the kind of connection and the columns are wider; the classic layout
/// writes the kind first and the device in parentheses. A list that has not loaded shows `…`, a
/// failed read shows its error, and an empty one shows `none`.
pub(super) fn rows(app: &App, rail: bool, rows: &mut Vec<Row>) {
    // The switch and the button are the two items of `network::items`, by position.
    rows.push(Row::Item(0));
    rows.push(Row::Blank);
    rows.push(Row::Heading("Active connections"));
    match &app.data.network {
        Loadable::Loading => rows.push(Row::Text("…".into())),
        Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
        Loadable::Ready(n) => {
            let list: Vec<_> = n.active.iter().filter(|c| app.matches_search(&c.name)).collect();
            if list.is_empty() {
                rows.push(Row::Text("none".into()));
            }
            for c in list {
                rows.push(Row::Text(if rail {
                    let glyph = match c.kind.as_str() {
                        "Wi-Fi" => "󰖩",
                        "Ethernet" => "󰈀",
                        "WireGuard" | "VPN" => "󰦝",
                        _ => "󰛳",
                    };
                    format!("{glyph}  {:<24} {:<10} {}", c.name, c.kind, c.device)
                } else {
                    format!("{:<10} {}  ({})", c.kind, c.name, c.device)
                }));
            }
        }
    }
    rows.push(Row::Blank);
    rows.push(Row::Item(1));
}

/// The keys the status line offers on this tab: switch Wi-Fi, open the network tool, and search
/// the connections.
pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap) {
    pairs.push((km.label(Action::Toggle), "Toggle"));
    pairs.push((km.label(Action::Activate), "Open"));
    pairs.push((km.label(Action::Search), "Search"));
}

#[cfg(test)]
mod tests;
