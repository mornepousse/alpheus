//! The Tools menu's catalog (spec §§ 2, 3): the built-in table, the `Context` of booleans that
//! decides which rows show, and the merged categories and entries (built-ins first, then the
//! user's). Built at each open, dropped at close: nothing is cached. Pure: no thread, no
//! command, no log (the notes go through a callback the glue logs).

use super::config::{BUILTIN_CATEGORIES, DEFAULT_ORDER, ToolsConfig};
use crate::quick::Toggle;
use alpheus_common::launcher_ipc::QuickVerb;

/// A category with no glyph of its own.
pub const FOLDER: char = '\u{F024B}';

/// The built-in categories' orders (spec § 3), in `BUILTIN_CATEGORIES`' order.
const BUILTIN_ORDERS: [u16; 4] = [10, 20, 30, 90];
/// Their glyphs (Nerd Font Material Design), in the same order.
const BUILTIN_GLYPHS: [char; 4] = ['\u{F0493}', '\u{F056E}', '\u{F05B7}', '\u{F0343}'];

/// What a built-in does when chosen (spec § 3). Run by `app.rs`, after the box closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAction {
    /// The quick panel at that row or detail (`--quick <word>`).
    Quick(QuickVerb),
    /// Do-not-disturb, toggled.
    Dnd,
    /// A `[quick]` toggle's command.
    Toggle(Toggle),
    /// The control center's tab named by the id after `tab.` (see `Builtin::tab_slug`).
    Tab,
    Notes,
    Timer,
    Clipboard,
    Emoji,
    Lock,
    Power,
    Reload,
    Shield,
}

#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    pub id: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub glyph: char,
    pub action: BuiltinAction,
}

impl Builtin {
    /// The control center tab a `Tab` built-in opens: its id after `tab.` (`tab.network` is
    /// `network`). Derived, not written twice, so the diagnostics tab's name lives in
    /// `join.rs` alone (the branches' hook points are named only there).
    pub fn tab_slug(&self) -> Option<&'static str> {
        match self.action {
            BuiltinAction::Tab => self.id.strip_prefix("tab."),
            _ => None,
        }
    }
}

const fn b(id: &'static str, label: &'static str, category: &'static str, glyph: char, action: BuiltinAction) -> Builtin {
    Builtin { id, label, category, glyph, action }
}

// `BUILTIN_CATEGORIES`' names, spelled out (a const cannot be indexed here without a panicking
// path the guardrail refuses); `the_table_is_the_specs` pins them to it.
const QUICK: &str = "Quick settings";
const CONTROL: &str = "Control center";
const UTILITIES: &str = "Utilities";
const SESSION: &str = "Session";

/// Spec § 3's table, in its order.
pub const BUILTINS: [Builtin; 23] = [
    b("quick.sound", "Sound", QUICK, '\u{F057E}', BuiltinAction::Quick(QuickVerb::Sound)),
    b("quick.wifi", "Wi-Fi", QUICK, '\u{F05A9}', BuiltinAction::Quick(QuickVerb::Wifi)),
    b("quick.bluetooth", "Bluetooth", QUICK, '\u{F00AF}', BuiltinAction::Quick(QuickVerb::Bluetooth)),
    b("quick.vpn", "VPN", QUICK, '\u{F0582}', BuiltinAction::Quick(QuickVerb::Vpn)),
    b("quick.shield", "Shield", QUICK, '\u{F0498}', BuiltinAction::Shield),
    b("dnd", "Do not disturb", QUICK, '\u{F009B}', BuiltinAction::Dnd),
    b("keep_awake", "Keep awake", QUICK, '\u{F0176}', BuiltinAction::Toggle(Toggle::KeepAwake)),
    b("night_light", "Night light", QUICK, '\u{F0594}', BuiltinAction::Toggle(Toggle::NightLight)),
    b("tab.home", "Home", CONTROL, '\u{F02DC}', BuiltinAction::Tab),
    b("tab.sound", "Sound", CONTROL, '\u{F057E}', BuiltinAction::Tab),
    b("tab.network", "Network", CONTROL, '\u{F06F3}', BuiltinAction::Tab),
    b("tab.bluetooth", "Bluetooth", CONTROL, '\u{F00AF}', BuiltinAction::Tab),
    b("tab.power", "Power", CONTROL, '\u{F0079}', BuiltinAction::Tab),
    b("tab.displays", "Displays", CONTROL, '\u{F0379}', BuiltinAction::Tab),
    b("tab.appearance", "Theme", CONTROL, '\u{F03D8}', BuiltinAction::Tab),
    b("tab.diagnostics", "Diagnostics", CONTROL, '\u{F04D9}', BuiltinAction::Tab),
    b("notes", "Notes", UTILITIES, '\u{F039E}', BuiltinAction::Notes),
    b("timer", "Timer", UTILITIES, '\u{F051B}', BuiltinAction::Timer),
    b("clipboard", "Clipboard", UTILITIES, '\u{F014C}', BuiltinAction::Clipboard),
    b("emoji", "Emoji", UTILITIES, '\u{F0C68}', BuiltinAction::Emoji),
    b("lock", "Lock", SESSION, '\u{F033E}', BuiltinAction::Lock),
    b("power", "Power menu", SESSION, '\u{F0425}', BuiltinAction::Power),
    b("reload", "Reload the bar", SESSION, '\u{F0450}', BuiltinAction::Reload),
];

/// What decides which built-ins show (spec § 3's "Shown when" column): plain booleans, so the
/// rule is testable with no `App`. The glue fills it from `bar.toml`, `[tools] reload` and the
/// three answers of `join.rs`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// `quick` in `modules`.
    pub quick: bool,
    /// `notifications` in `modules`.
    pub notifications: bool,
    /// `timer` in `modules`.
    pub timer: bool,
    /// `[quick] keep_awake.toggle` set.
    pub keep_awake: bool,
    /// `[quick] night_light.toggle` set.
    pub night_light: bool,
    /// `[quick] lock` set.
    pub lock: bool,
    /// Any of `[quick]` lock, logout, reboot, poweroff set.
    pub session: bool,
    /// `[tools] reload` set.
    pub reload: bool,
    /// The shield hook says so.
    pub shield: bool,
    /// The clipboard history is available.
    pub clipboard: bool,
    /// The diagnostics hook says so.
    pub diagnostics: bool,
}

/// Whether `b` shows under `c` (spec § 3).
pub fn shown(b: &Builtin, c: &Context) -> bool {
    match b.action {
        BuiltinAction::Quick(_) => c.quick,
        BuiltinAction::Shield => c.shield,
        BuiltinAction::Dnd => c.notifications,
        BuiltinAction::Toggle(Toggle::KeepAwake) => c.keep_awake,
        BuiltinAction::Toggle(Toggle::NightLight) => c.night_light,
        // Only the diagnostics tab is conditional, and it is the one with a hook.
        BuiltinAction::Tab => b.id != "tab.diagnostics" || c.diagnostics,
        BuiltinAction::Notes | BuiltinAction::Emoji => true,
        BuiltinAction::Timer => c.timer,
        BuiltinAction::Clipboard => c.clipboard,
        BuiltinAction::Lock => c.lock,
        BuiltinAction::Power => c.session,
        BuiltinAction::Reload => c.reload,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// An index into `BUILTINS`.
    Builtin(usize),
    /// An index into the `[[tools.entry]]` list the catalog was built from.
    User(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choices {
    None,
    /// The configured `choices`, in order.
    Fixed(Vec<String>),
    /// `choices_cmd`: read at drill-in (never listed in the flat search, never recorded).
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub name: String,
    /// An index into `Catalog::categories`; `None`: the root.
    pub category: Option<usize>,
    pub icon: Option<char>,
    /// A root user entry sorts among the categories by it (default 50); a built-in's is unused.
    pub order: u16,
    pub choices: Choices,
    pub confirm: bool,
    /// Whether a state is known for it: a built-in with a daemon-side state, a user entry with `state`.
    pub has_state: bool,
    /// The history key (spec § 5); a fixed choice appends `/<choice>` (`Catalog::choice_key`).
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub name: String,
    pub icon: char,
    pub order: u16,
}

/// A row of the root: a category or an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Category(usize),
    Entry(usize),
}

/// One row of the flat search: an entry, or one of its fixed choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flat {
    pub entry: usize,
    /// An index into the entry's `Choices::Fixed`.
    pub choice: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub categories: Vec<Category>,
    /// The built-ins that show (table order), then the user entries (file order).
    pub entries: Vec<Entry>,
    /// The entries `[tools] root` lists at the root, first, in that order.
    pub root: Vec<usize>,
}

fn icon_of(s: &Option<String>) -> Option<char> {
    s.as_deref().and_then(|s| s.chars().next())
}

impl Catalog {
    /// The catalog for `t` under `c`. An unknown id in `hide` or `root` is noted once per list,
    /// however many it holds (never quoted), and ignored, so a file written for a newer alpheus stays valid.
    pub fn build(t: &ToolsConfig, c: &Context, note: &mut dyn FnMut(&str)) -> Catalog {
        let known = |id: &String| BUILTINS.iter().any(|b| b.id == id);
        if !t.hide.iter().all(known) {
            note("tools: unknown built-in id in [tools] hide");
        }
        if !t.root.iter().all(known) {
            note("tools: unknown built-in id in [tools] root");
        }
        let mut categories: Vec<Category> = BUILTIN_CATEGORIES
            .iter()
            .zip(BUILTIN_GLYPHS)
            .zip(BUILTIN_ORDERS)
            .map(|((name, icon), order)| Category { name: (*name).to_string(), icon, order })
            .collect();
        for u in &t.categories {
            match categories.iter_mut().find(|k| k.name == u.name) {
                // A user category of a built-in's name restyles it.
                Some(k) => {
                    k.icon = icon_of(&u.icon).unwrap_or(k.icon);
                    k.order = u.order.unwrap_or(k.order);
                }
                None => categories.push(Category {
                    name: u.name.clone(),
                    icon: icon_of(&u.icon).unwrap_or(FOLDER),
                    order: u.order.unwrap_or(DEFAULT_ORDER),
                }),
            }
        }
        // An entry of a category cut past the bound never gets here (`config::bounded` drops it).
        let category_of = |name: &str| categories.iter().position(|k| k.name == name);
        let mut entries = Vec::new();
        if t.builtins {
            for (i, b) in BUILTINS.iter().enumerate() {
                if shown(b, c) && !t.hide.iter().any(|h| h == b.id) {
                    entries.push(Entry {
                        kind: EntryKind::Builtin(i),
                        name: b.label.to_string(),
                        category: category_of(b.category),
                        icon: Some(b.glyph),
                        order: DEFAULT_ORDER,
                        choices: Choices::None,
                        confirm: false,
                        has_state: matches!(b.action, BuiltinAction::Dnd | BuiltinAction::Toggle(_)),
                        key: format!("tool:builtin:{}", b.id),
                    });
                }
            }
        }
        for (i, e) in t.entries.iter().enumerate() {
            let choices = if !e.choices.is_empty() {
                Choices::Fixed(e.choices.clone())
            } else if !e.choices_cmd.is_empty() {
                Choices::Command
            } else {
                Choices::None
            };
            entries.push(Entry {
                kind: EntryKind::User(i),
                name: e.name.clone(),
                category: e.category.as_deref().and_then(category_of),
                icon: icon_of(&e.icon),
                order: e.order.unwrap_or(DEFAULT_ORDER),
                choices,
                confirm: e.confirm,
                has_state: !e.state.is_empty(),
                key: format!("tool:user:{}/{}", e.category.as_deref().unwrap_or(""), e.name),
            });
        }
        let mut root = Vec::new();
        for id in &t.root {
            let at = entries.iter().position(|e| matches!(e.kind, EntryKind::Builtin(b) if BUILTINS.get(b).is_some_and(|b| b.id == id)));
            if let Some(at) = at
                && !root.contains(&at)
            {
                root.push(at);
            }
        }
        Catalog { categories, entries, root }
    }

    /// The root's rows: the built-ins `[tools] root` lists, then the categories that hold an
    /// entry and the root user entries sorted together by `order`, then name.
    pub fn root_rows(&self) -> Vec<Item> {
        let mut rows: Vec<Item> = self.root.iter().map(|&e| Item::Entry(e)).collect();
        let mut sorted: Vec<(u16, String, u8, Item)> = Vec::new();
        for (i, k) in self.categories.iter().enumerate() {
            if self.entries.iter().any(|e| e.category == Some(i)) {
                sorted.push((k.order, k.name.to_lowercase(), 0, Item::Category(i)));
            }
        }
        for (i, e) in self.entries.iter().enumerate() {
            if e.category.is_none() {
                sorted.push((e.order, e.name.to_lowercase(), 1, Item::Entry(i)));
            }
        }
        sorted.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));
        rows.extend(sorted.into_iter().map(|s| s.3));
        rows
    }

    /// The entries of category `category`: its built-ins in the table's order, then its user
    /// entries in the file's order.
    pub fn category_rows(&self, category: usize) -> Vec<usize> {
        self.entries.iter().enumerate().filter(|(_, e)| e.category == Some(category)).map(|(i, _)| i).collect()
    }

    /// Every entry, then every fixed choice as (entry, choice); never a dynamic choice.
    pub fn flat(&self) -> Vec<Flat> {
        let mut rows: Vec<Flat> = (0..self.entries.len()).map(|entry| Flat { entry, choice: None }).collect();
        for (entry, e) in self.entries.iter().enumerate() {
            if let Choices::Fixed(list) = &e.choices {
                rows.extend((0..list.len()).map(|choice| Flat { entry, choice: Some(choice) }));
            }
        }
        rows
    }

    /// The history key of a fixed choice of `entry` (spec § 5).
    pub fn choice_key(&self, entry: usize, choice: &str) -> String {
        self.entries.get(entry).map(|e| format!("{}/{choice}", e.key)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
