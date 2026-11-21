//! The per-tab facts that are plain data: one `TabSpec` row per `Tab`, in
//! tab order. To add a tab: add its `Tab` variant (action.rs) and one row
//! here (a test checks there is exactly one, in variant order), then its
//! items (`App::items`), its view (`ui/tabs.rs`) and its shortcut hints
//! (`ui/mod.rs`), which are code, not data.

use super::action::Tab;
use super::worker::Domain;
use std::time::Duration;

pub struct TabSpec {
    /// The tab this row describes; equals the row's index in `TABS`.
    pub tab: Tab,
    /// Label in the tab bar, the status line, the palette and the help.
    pub title: &'static str,
    /// Name on the command line (`alpheus --tab <slug>`).
    pub slug: &'static str,
    /// Other names `--tab` accepts, folded (lowercase, no accent).
    pub aliases: &'static [&'static str],
    /// The list is filtered with "/".
    pub searchable: bool,
    /// Refresh period while the tab is visible; `None` when there is nothing to read.
    pub refresh: Option<Duration>,
    /// Domains the tab shows, read on a refresh (the slowest last).
    pub domains: &'static [Domain],
    /// Width of the label column, in cells.
    pub label_width: u16,
}

/// One row per tab, in tab order.
pub const TABS: [TabSpec; 8] = [
    TabSpec {
        tab: Tab::Home,
        title: "Home",
        slug: "home",
        aliases: &[],
        searchable: false,
        refresh: Some(Duration::from_secs(1)),
        domains: &[
            Domain::Volume,
            Domain::Brightness,
            Domain::Dnd,
            Domain::KeepAwake,
            Domain::Profiles,
            Domain::Network,
            Domain::Bluetooth,
        ],
        label_width: 20,
    },
    TabSpec {
        tab: Tab::Sound,
        title: "Sound",
        slug: "sound",
        aliases: &[],
        searchable: true,
        refresh: Some(Duration::from_secs(2)),
        domains: &[Domain::Audio],
        label_width: 34,
    },
    TabSpec {
        tab: Tab::Network,
        title: "Network",
        slug: "network",
        aliases: &[],
        searchable: true,
        refresh: Some(Duration::from_secs(2)),
        domains: &[Domain::Network],
        label_width: 20,
    },
    TabSpec {
        tab: Tab::Bluetooth,
        title: "BT",
        slug: "bluetooth",
        aliases: &[],
        searchable: true,
        refresh: Some(Duration::from_secs(2)),
        domains: &[Domain::Bluetooth],
        label_width: 20,
    },
    TabSpec {
        tab: Tab::Power,
        title: "Power",
        slug: "power",
        aliases: &[],
        searchable: false,
        refresh: None,
        domains: &[],
        label_width: 20,
    },
    TabSpec {
        tab: Tab::Displays,
        title: "Displays",
        slug: "displays",
        aliases: &[],
        searchable: false,
        refresh: Some(Duration::from_secs(2)),
        domains: &[Domain::Displays],
        label_width: 20,
    },
    TabSpec {
        tab: Tab::Appearance,
        title: "Theme",
        slug: "appearance",
        // "theme" is the tab's title; "appearance" stays its long name.
        aliases: &["theme"],
        searchable: false,
        refresh: None,
        domains: &[],
        label_width: 20,
    },
    TabSpec {
        tab: Tab::Diagnostics,
        title: "Diag",
        slug: "diagnostics",
        aliases: &["diag"],
        searchable: false,
        refresh: Some(Duration::from_secs(5)),
        domains: &[Domain::Failed, Domain::Health],
        label_width: 20,
    },
];

/// Key name bound to tab number `index + 1` by default: "1" to "9". A tenth
/// tab and beyond get no default key (they stay reachable by Tab, the mouse
/// and the palette).
pub fn digit(index: usize) -> Option<&'static str> {
    const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    DIGITS.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_row_per_tab_in_order() {
        for (i, spec) in TABS.iter().enumerate() {
            assert_eq!(spec.tab.index(), i, "{}", spec.title);
            assert_eq!(Tab::ALL[i], spec.tab);
        }
        assert_eq!(Tab::ALL.len(), TABS.len());
    }

    #[test]
    fn digits_for_the_first_nine_tabs_only() {
        for i in 0..TABS.len() {
            assert_eq!(digit(i), Some(["1", "2", "3", "4", "5", "6", "7", "8"][i]));
        }
        assert_eq!(digit(8), Some("9"));
        assert_eq!(digit(9), None);
        assert_eq!(digit(usize::MAX), None);
    }

    #[test]
    fn aliases_are_folded_and_do_not_shadow_a_slug() {
        for spec in &TABS {
            for a in spec.aliases {
                assert_eq!(crate::fuzzy::fold_str(a), *a);
                assert!(TABS.iter().all(|s| s.slug != *a));
            }
        }
    }
}
