//! What the bar draws for a tray item (spec § 1, Icons): a Nerd Font glyph
//! from a small built-in table, or `[tray.icons]` overrides, matched against
//! what the item says about itself; an application nobody knows gets the
//! first letter of its name. Never an image: the item's pixmaps are never
//! even requested.
//!
//! Never:
//! - ask for an image: an item's pixmaps are never requested, so no application sends the bar a
//!   picture (`tray_d_bus_is_confined_and_allowlisted`);
//! - grow per item: the table is read-only data, the overrides come from `bar.toml`, and an unknown
//!   application costs a letter.
//!
//! See: docs/features/tray.md ("Icons"), docs/configuration.md (`[tray.icons]`).

use super::host::Item;
use super::{ItemKey, Status};
use crate::grid::text_width;
use std::collections::BTreeMap;

// Codepoints verified in Nerd Fonts 3.4.
pub const STEAM: char = '\u{f04d3}'; // md-steam
pub const DISCORD: char = '\u{f066f}'; // md-discord
pub const TELEGRAM: char = '\u{f2c6}'; // fa-telegram
pub const SPOTIFY: char = '\u{f04c7}'; // md-spotify
pub const NETWORK: char = '\u{f05a9}'; // md-wifi
pub const BLUETOOTH: char = '\u{f00af}'; // md-bluetooth
pub const KDE_CONNECT: char = '\u{f0317}'; // md-lan
/// (lower-case needle, glyph), first match wins.
pub const TABLE: &[(&str, char)] = &[
    ("steam", STEAM),
    ("discord", DISCORD),
    ("telegram", TELEGRAM),
    ("spotify", SPOTIFY),
    ("nm-applet", NETWORK),
    ("network", NETWORK),
    ("blueman", BLUETOOTH),
    ("bluetooth", BLUETOOTH),
    ("kdeconnect", KDE_CONNECT),
];
/// An item nothing is known about: no Title, Id nor process.
pub const UNKNOWN_LETTER: char = '?';

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Icon {
    /// A table glyph, or an override (1 or 2 cells).
    Glyph(String),
    /// Unknown application: drawn bold in the accent colour, no background.
    Letter(char),
}

/// What the bar draws for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIcon {
    pub key: ItemKey,
    pub icon: Icon,
    pub status: Status,
}

/// The four fields an item is recognised by, in priority order, lower-cased
/// with `str::to_lowercase` — no Unicode normalisation (NFC/NFKC), the same
/// folding `[tray.icons]` keys get at load; absent or empty fields
/// left out.
fn fields(item: &Item) -> Vec<String> {
    [&item.id, &item.icon_name, &item.tooltip, &item.process]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

/// The longest needle of `table` found in a field, fields in priority order:
/// among several needles matching the same field, the longest (most
/// specific) one wins — not whichever a `BTreeMap`'s key order happens to
/// put first (`[tray.icons]`'s overrides).
fn find<'a, G>(fields: &[String], table: impl Iterator<Item = (&'a str, G)> + Clone) -> Option<G> {
    fields
        .iter()
        .find_map(|f| table.clone().filter(|(needle, _)| f.contains(needle)).max_by_key(|(needle, _)| needle.len()).map(|(_, g)| g))
}

/// Id, IconName, ToolTip title, process name — each lower-cased and matched as
/// a substring, against `overrides` over all four, then against TABLE over all
/// four. No match: the item's letter.
pub fn icon(item: &Item, overrides: &BTreeMap<String, String>) -> Icon {
    let fields = fields(item);
    if let Some(g) = find(&fields, overrides.iter().map(|(k, v)| (k.as_str(), v))) {
        return Icon::Glyph(g.clone());
    }
    match find(&fields, TABLE.iter().copied()) {
        Some(c) => Icon::Glyph(c.to_string()),
        None => Icon::Letter(letter(item)),
    }
}

/// First letter (upper-cased) of Title, else process name, else Id, else
/// UNKNOWN_LETTER: the first alphanumeric character that takes exactly one cell
/// (never a lone combining mark, never a wide one) of the first of them that has
/// one. The process name
/// comes before the Id: an Electron item's Id (`chrome_status_icon_N`) says
/// nothing about the application.
pub fn letter(item: &Item) -> char {
    [&item.title, &item.process, &item.id]
        .into_iter()
        .flatten()
        .find_map(|s| s.chars().find(|c| c.is_alphanumeric() && text_width(c.encode_utf8(&mut [0; 4])) == 1))
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or(UNKNOWN_LETTER)
}

/// The ready items, in registration order.
pub fn icons(items: &[Item], overrides: &BTreeMap<String, String>) -> Vec<TrayIcon> {
    items.iter().filter(|i| i.is_ready()).map(|i| TrayIcon { key: i.key.clone(), icon: icon(i, overrides), status: i.status }).collect()
}

/// Items built the way the host builds them — registered, then their first
/// `Get`s answered — through `Host`'s public API only: what the tests
/// run against, independent of the host's internals.
#[cfg(test)]
pub(crate) mod testing {
    use crate::session_bus::bus::testing::FakeOutbox;
    use crate::session_bus::bus::{Call, CallError, Property};
    use crate::session_bus::codec::Reply;
    use crate::session_bus::host::{Host, Item};
    use crate::session_bus::{Prop, PropValue, Status};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temporary /proc root; removed on drop, so a test that panics above
    /// `host()` (an `unwrap()`, an assertion in the answer loop) still leaves
    /// nothing behind — modelled on `host.rs`'s own `Proc` guard.
    pub(crate) struct Proc(pub(crate) PathBuf);

    impl Drop for Proc {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// How a fake item answers its first `Get`s.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Answers {
        /// From its properties; any other property: UnknownProperty.
        Yes,
        /// Every `Get` times out (a silent application).
        Timeout,
        /// Never answered yet (not ready).
        Never,
    }

    pub(crate) struct Fake {
        props: Vec<(Prop, String)>,
        process: Option<String>,
        answers: Answers,
    }

    impl Fake {
        /// `Status` is parsed, `Menu` is a path, `ItemIsMenu` is "true" or not, the rest is text.
        pub(crate) fn new(props: &[(Prop, &str)]) -> Fake {
            Fake { props: props.iter().map(|(p, v)| (*p, v.to_string())).collect(), process: None, answers: Answers::Yes }
        }
        pub(crate) fn process(mut self, comm: &str) -> Fake {
            self.process = Some(comm.into());
            self
        }
        pub(crate) fn answers(mut self, a: Answers) -> Fake {
            self.answers = a;
            self
        }
    }

    fn value(p: Prop, v: &str) -> PropValue {
        match p {
            Prop::Status => PropValue::Status(Status::parse(v)),
            Prop::Menu => PropValue::Path(v.into()),
            Prop::ItemIsMenu => PropValue::Bool(v == "true"),
            _ => PropValue::Text(v.into()),
        }
    }

    /// The fake at `name` (":1.100" is the first).
    fn index(name: &str) -> Option<usize> {
        name.strip_prefix(":1.")?.parse::<usize>().ok()?.checked_sub(100)
    }

    fn answer(fakes: &[Fake], call: &Call) -> Option<Result<Reply, CallError>> {
        match call {
            Call::Get { destination, property: Property::Item(p), .. } => {
                let f = fakes.get(index(destination)?)?;
                match f.answers {
                    Answers::Never => None,
                    Answers::Timeout => Some(Err(CallError::Timeout)),
                    Answers::Yes => Some(match f.props.iter().find(|(q, _)| q == p) {
                        Some((_, v)) => Ok(Reply::Prop(value(*p, v))),
                        None => Err(CallError::Remote("org.freedesktop.DBus.Error.UnknownProperty".into())),
                    }),
                }
            }
            Call::GetConnectionUnixProcessID { name } => Some(Ok(Reply::Pid(1000 + index(name)? as u32))),
            _ => Some(Ok(Reply::Unit)),
        }
    }

    /// A host holding `fakes`, registered path-only by the unique names
    /// ":1.100", ":1.101"… in order (pids 1000, 1001…, their `comm` in a
    /// temporary proc root), every call answered as each fake says, until
    /// nothing more is asked. The outbox comes back empty.
    pub(crate) fn host(fakes: &[Fake]) -> (Host, FakeOutbox) {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!("alpheus-{}-tray-proc-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _proc = Proc(root.clone());
        for (i, f) in fakes.iter().enumerate() {
            if let Some(comm) = &f.process {
                let dir = root.join((1000 + i).to_string());
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
            }
        }
        // Our own unique name (never one of the fakes: they are registered as
        // ":1.100", ":1.101"…), as `Host::new` needs it.
        let mut host = Host::new(root.clone(), ":1.0".into());
        let mut out = FakeOutbox::default();
        for i in 0..fakes.len() {
            host.register("/StatusNotifierItem", &format!(":1.{}", 100 + i), &mut out);
        }
        loop {
            let calls = out.take();
            if calls.is_empty() {
                break;
            }
            for (id, call) in calls {
                if let Some(r) = answer(fakes, &call) {
                    host.answer(id, r, &mut out);
                }
            }
        }
        out.sent.clear();
        (host, out)
        // `_proc` drops here, removing `root` whether we got here normally
        // or (were this not the last statement) by an earlier panic.
    }

    pub(crate) fn items(fakes: &[Fake]) -> Vec<Item> {
        host(fakes).0.items().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Answers, Fake, items};
    use super::*;
    use crate::grid::text_width;
    use crate::session_bus::Prop;

    fn no_overrides() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn one(fake: Fake) -> Item {
        items(&[fake]).remove(0)
    }

    fn glyph(c: char) -> Icon {
        Icon::Glyph(c.to_string())
    }

    #[test]
    fn glyph_by_id_then_icon_name_then_tooltip_then_process() {
        let all = Fake::new(&[(Prop::Id, "steam"), (Prop::IconName, "discord"), (Prop::ToolTip, "Telegram")]).process("spotify");
        assert_eq!(icon(&one(all), &no_overrides()), glyph(STEAM), "the Id first");
        let no_id = Fake::new(&[(Prop::Id, "nimbus"), (Prop::IconName, "discord"), (Prop::ToolTip, "Telegram")]).process("spotify");
        assert_eq!(icon(&one(no_id), &no_overrides()), glyph(DISCORD), "then the IconName");
        let tooltip = Fake::new(&[(Prop::Id, "nimbus"), (Prop::ToolTip, "Telegram Desktop")]).process("spotify");
        assert_eq!(icon(&one(tooltip), &no_overrides()), glyph(TELEGRAM), "then the tooltip title");
        let process = Fake::new(&[(Prop::Id, "nimbus")]).process("spotify");
        assert_eq!(icon(&one(process), &no_overrides()), glyph(SPOTIFY), "then the process name");
        let title_only = Fake::new(&[(Prop::Title, "Steam")]);
        assert_eq!(icon(&one(title_only), &no_overrides()), Icon::Letter('S'), "Title is not a key of the table");
    }

    #[test]
    fn electron_items_found_by_process_name() {
        let discord =
            Fake::new(&[(Prop::Id, "chrome_status_icon_1"), (Prop::Title, ""), (Prop::Menu, "/org/chromium/DbusMenu")]).process("Discord");
        assert_eq!(icon(&one(discord), &no_overrides()), glyph(DISCORD));
    }

    #[test]
    fn case_insensitive_substring_with_suffixes() {
        assert_eq!(icon(&one(Fake::new(&[(Prop::IconName, "blueman-active")])), &no_overrides()), glyph(BLUETOOTH));
        assert_eq!(icon(&one(Fake::new(&[(Prop::Id, "steam_tray_123")])), &no_overrides()), glyph(STEAM));
        assert_eq!(icon(&one(Fake::new(&[(Prop::Id, "KDEConnect")])), &no_overrides()), glyph(KDE_CONNECT));
        assert_eq!(icon(&one(Fake::new(&[(Prop::IconName, "nm-applet")])), &no_overrides()), glyph(NETWORK));
    }

    #[test]
    fn overrides_win_over_the_table() {
        let overrides = BTreeMap::from([("discord".to_string(), "D".to_string()), ("kettle".to_string(), "\u{f030b}".to_string())]);
        assert_eq!(icon(&one(Fake::new(&[(Prop::Id, "discord")])), &overrides), Icon::Glyph("D".into()));
        let kettle = Fake::new(&[(Prop::Id, "telegram")]).process("kettle");
        assert_eq!(
            icon(&one(kettle), &overrides),
            Icon::Glyph("\u{f030b}".into()),
            "an override on the process name beats the table on the Id"
        );
        let wide = BTreeMap::from([("lumen".to_string(), "日".to_string())]);
        assert_eq!(icon(&one(Fake::new(&[(Prop::Id, "lumen-tray")])), &wide), Icon::Glyph("日".into()), "two cells");
        assert_eq!(icon(&one(Fake::new(&[(Prop::Id, "steam")])), &overrides), glyph(STEAM), "no override: the table");
    }

    #[test]
    fn unknown_app_gets_its_first_letter() {
        assert_eq!(icon(&one(Fake::new(&[(Prop::Title, "nimbus Sync"), (Prop::Id, "quill")])), &no_overrides()), Icon::Letter('N'));
        assert_eq!(letter(&one(Fake::new(&[(Prop::Title, "  · lumen"), (Prop::Id, "quill")]))), 'L', "first alphanumeric");
        assert_eq!(letter(&one(Fake::new(&[(Prop::Title, ""), (Prop::Id, "quill")]))), 'Q', "empty Title, no process: the Id");
        assert_eq!(letter(&one(Fake::new(&[(Prop::Title, "··"), (Prop::Id, "ferry")]))), 'F', "nothing usable in the Title");
        assert_eq!(letter(&one(Fake::new(&[(Prop::Title, "élan")]))), 'É');
        let electron = Fake::new(&[(Prop::Id, "chrome_status_icon_1"), (Prop::Title, "")]).process("nimbus");
        assert_eq!(icon(&one(electron), &no_overrides()), Icon::Letter('N'), "Electron: the process name before the meaningless Id");
        // U+0941 (a Devanagari vowel sign) is alphanumeric but takes no cell.
        assert_eq!(letter(&one(Fake::new(&[(Prop::Title, "\u{941}lumen")]))), 'L', "a letter takes a cell");
        assert_eq!(letter(&one(Fake::new(&[(Prop::Title, "微信"), (Prop::Id, "quill")]))), 'Q', "a letter takes one cell, never two");
        assert_eq!(letter(&one(Fake::new(&[(Prop::Status, "Active")]))), UNKNOWN_LETTER, "nothing known (question 6)");
    }

    #[test]
    fn letter_of_a_silent_item_comes_from_its_process() {
        let silent = Fake::new(&[(Prop::Title, "Nimbus")]).process("kettle").answers(Answers::Timeout);
        let item = one(silent);
        assert!(item.is_ready(), "every Get timed out: shown");
        assert_eq!(icon(&item, &no_overrides()), Icon::Letter('K'));
    }

    #[test]
    fn only_ready_items_are_drawn_in_registration_order() {
        let list = items(&[
            Fake::new(&[(Prop::Id, "steam"), (Prop::Status, "Passive")]),
            Fake::new(&[(Prop::Id, "nimbus")]).answers(Answers::Never),
            Fake::new(&[(Prop::Title, "Kettle"), (Prop::Status, "NeedsAttention")]),
        ]);
        assert_eq!(list.len(), 3);
        let drawn = icons(&list, &no_overrides());
        assert_eq!(
            drawn,
            vec![
                TrayIcon { key: list[0].key.clone(), icon: glyph(STEAM), status: Status::Passive },
                TrayIcon { key: list[2].key.clone(), icon: Icon::Letter('K'), status: Status::NeedsAttention },
            ]
        );
    }

    #[test]
    fn every_table_glyph_is_one_cell() {
        for (needle, c) in TABLE {
            assert_eq!(text_width(&c.to_string()), 1, "{needle}");
            assert_eq!(*needle, needle.to_lowercase(), "needles are lower-case");
        }
        assert_eq!(text_width(&UNKNOWN_LETTER.to_string()), 1);
    }

    #[test]
    fn the_test_proc_root_is_removed_even_after_a_panic() {
        use super::testing::Proc;
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let root = std::env::temp_dir().join(format!("alpheus-{}-tray-proc-guard-test", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _proc = Proc(root.clone());
            panic!("a test above host() panicking, e.g. an unwrap() or a failed assertion");
        }));
        assert!(result.is_err(), "the panic must still have happened");
        assert!(!root.exists(), "the guard must remove the proc root even so");
    }

    #[test]
    fn the_longest_matching_override_wins() {
        // "steam" is the alphabetically first key of the two, but
        // "steamwebhelper" is the more specific (longer) match for this Id —
        // the longer needle must win, not whichever a BTreeMap iterates first.
        let overrides = BTreeMap::from([("steam".to_string(), "A".to_string()), ("steamwebhelper".to_string(), "B".to_string())]);
        let item = one(Fake::new(&[(Prop::Id, "steamwebhelper")]));
        assert_eq!(icon(&item, &overrides), Icon::Glyph("B".into()));
        let plain_steam = one(Fake::new(&[(Prop::Id, "steam")]));
        assert_eq!(icon(&plain_steam, &overrides), Icon::Glyph("A".into()), "only the shorter needle matches here");
    }

    /// U+0130 (capital dotted I) lower-cases, with `str::to_lowercase`, to two
    /// code points (U+0069 U+0307): the config loader lower-cases
    /// `[tray.icons]` keys the same way (T7 review), so the matcher must
    /// lower-case the item's fields with that same `str::to_lowercase` —
    /// never a one-char-to-one-char fold — or an override key whose
    /// lower-casing expands would never match.
    #[test]
    fn case_folding_that_expands_the_haystack_still_matches() {
        let key = "İstanbul".to_lowercase();
        let overrides = BTreeMap::from([(key, "\u{f0317}".to_string())]);
        let item = one(Fake::new(&[(Prop::Id, "İstanbul Tray")]));
        assert_eq!(icon(&item, &overrides), Icon::Glyph("\u{f0317}".into()), "the expanded fold still matches as a substring");
    }
}
