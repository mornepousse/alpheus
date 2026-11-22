//! The Sound tab's rows and its shortcut hints (the shortcut bar, or the keycaps row in the rail).
//!
//! `tabs::render` draws the rows this file lists; the items themselves come from
//! `app/sound.rs`, and a row is drawn by the shared item drawing in `tabs.rs`, like the other item
//! tabs.
//!
//! Never:
//! - change the state: everything here reads the `App` and fills a list of rows;
//! - run a tool or read a clock.
//!
//! See: docs/specs/2026-10-02-alpheus-v1-design.md (§ 3, the Sound row),
//! docs/specs/2026-11-15-alpheus-ui-refresh-design.md (the rail and the tab hues),
//! docs/features/control-center.md.

use super::tabs::Row;
use crate::app::action::Action;
use crate::app::keymap::Keymap;
use crate::app::{App, Item, Loadable};

/// Adds the Sound tab's rows to `rows`: "Outputs" and "Inputs", each followed by its nodes, and
/// the mixer button at the end.
///
/// A list that has not loaded shows `…`, a failed read shows its error, and an empty one shows
/// `none`.
pub(super) fn rows(app: &App, items: &[Item], rows: &mut Vec<Row>) {
    let (sinks, sources) = app.visible_nodes();
    // `i` counts through the nodes in the order of `sound::items`.
    let mut i = 0;
    for (title, count) in [("Outputs", sinks.len()), ("Inputs", sources.len())] {
        rows.push(Row::Heading(title));
        match &app.data.audio {
            Loadable::Loading => rows.push(Row::Text("…".into())),
            Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
            Loadable::Ready(_) if count == 0 => rows.push(Row::Text("none".into())),
            Loadable::Ready(_) => {
                for _ in 0..count {
                    rows.push(Row::Item(i));
                    i += 1;
                }
            }
        }
        rows.push(Row::Blank);
    }
    // The button is the last item (`sound::items`); the subtraction is guarded so the render
    // never panics even if that were to change.
    rows.push(Row::Item(items.len().saturating_sub(1)));
}

/// The keys the shortcut bar (the keycaps row in the rail) offers on this tab: choose the default,
/// change the volume (`adjust` is the pair of keys the caller built from the keymap), and search.
pub(super) fn hints(pairs: &mut Vec<(String, &'static str)>, km: &Keymap, adjust: String) {
    pairs.push((km.label(Action::Activate), "Default"));
    pairs.push((adjust, "Volume"));
    pairs.push((km.label(Action::Search), "Search"));
}

#[cfg(test)]
mod tests;
