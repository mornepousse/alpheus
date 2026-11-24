//! The Tools menu's catalog: the built-in table, the `Context` of booleans that
//! decides which rows show, and the merged categories and entries (built-ins first, then the
//! user's). Built at each open, dropped at close: nothing is cached. Pure: no thread, no
//! command, no log (the notes go through a callback the glue logs).

use super::config::{BUILTIN_CATEGORIES, DEFAULT_ORDER, ToolsConfig};
use super::diag::{PORTS_PROMPT, Scan, TARGET_PROMPT};
use crate::quick::Toggle;
use alpheus_common::diagnostics::query::Preset;
use alpheus_common::launcher_ipc::{CaptureVerb, QuickVerb};

/// A category with no glyph of its own.
pub const FOLDER: char = '\u{F024B}';

/// The built-in categories' orders, in `BUILTIN_CATEGORIES`' order.
const BUILTIN_ORDERS: [u16; 7] = [10, 20, 30, 40, 80, 90, 70];
/// Their glyphs (Nerd Font Material Design), in the same order.
const BUILTIN_GLYPHS: [char; 7] = ['\u{F0493}', '\u{F056E}', '\u{F05B7}', '\u{F0100}', '\u{F08D6}', '\u{F0343}', '\u{F00E4}'];

/// What a built-in does when chosen. Run by `app.rs`, after the box closed.
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
    /// The scheduled shutdown: its choices are the durations (and a cancel), see `timer::shutdown`.
    Shutdown,
    Clipboard,
    Emoji,
    Lock,
    Power,
    Reload,
    Shield,
    /// The network scans (`diag.rs`): a fixed choice, then the typed target.
    Scan,
    /// `udevadm monitor` and the kernel journal, in the held terminal.
    UsbEvents,
    /// `[tools.builtin] usb_reset`, in the held terminal.
    UsbReset,
    /// The control center's Diag tab opened on this preset.
    Preset(Preset),
    /// The kill list (`--kill`).
    Kill,
    /// A capture verb of the daemon (`--capture <word>`), replayed after the box closed.
    Capture(CaptureVerb),
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
const CAPTURES: &str = "Captures";
const SYSTEM: &str = "System";
const SESSION: &str = "Session";
const DIAGNOSTICS: &str = "Diagnostics";

/// The built-in table, in its order.
pub const BUILTINS: [Builtin; 45] = [
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
    b("capture.full", "Screenshot", CAPTURES, '\u{F0E51}', BuiltinAction::Capture(CaptureVerb::Full)),
    b("capture.area", "Area", CAPTURES, '\u{F0489}', BuiltinAction::Capture(CaptureVerb::Area)),
    b("capture.area_save", "Area (save)", CAPTURES, '\u{F0193}', BuiltinAction::Capture(CaptureVerb::AreaSave)),
    b("capture.annotate", "Annotate", CAPTURES, '\u{F03EB}', BuiltinAction::Capture(CaptureVerb::Annotate)),
    b("capture.record", "Record screen", CAPTURES, '\u{F044A}', BuiltinAction::Capture(CaptureVerb::Record)),
    b("capture.record_area", "Record area", CAPTURES, '\u{F0567}', BuiltinAction::Capture(CaptureVerb::RecordArea)),
    b("capture.stop", "Stop recording", CAPTURES, '\u{F04DB}', BuiltinAction::Capture(CaptureVerb::Stop)),
    b("capture.pick", "Colour picker", CAPTURES, '\u{F020A}', BuiltinAction::Capture(CaptureVerb::Pick)),
    b("capture.ocr", "Text from area (OCR)", CAPTURES, '\u{F113D}', BuiltinAction::Capture(CaptureVerb::Ocr)),
    b("shutdown", "Schedule shutdown", SYSTEM, '\u{F0902}', BuiltinAction::Shutdown),
    b("process.kill", "Kill a process", SYSTEM, '\u{F068C}', BuiltinAction::Kill),
    b("lock", "Lock", SESSION, '\u{F033E}', BuiltinAction::Lock),
    b("power", "Power menu", SESSION, '\u{F0425}', BuiltinAction::Power),
    b("reload", "Reload the bar", SESSION, '\u{F0450}', BuiltinAction::Reload),
    b("diag.scan", "Network scan", DIAGNOSTICS, '\u{F0437}', BuiltinAction::Scan),
    b("diag.usb_events", "USB events", DIAGNOSTICS, '\u{F0553}', BuiltinAction::UsbEvents),
    b("diag.usb_reset", "Reset USB (xHCI)", DIAGNOSTICS, '\u{F0709}', BuiltinAction::UsbReset),
    b("diag.preset.errors", "Errors this boot", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::ErrorsThisBoot)),
    b("diag.preset.usb", "USB", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::Usb)),
    b("diag.preset.suspend", "Suspend and resume", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::SuspendResume)),
    b("diag.preset.oom", "Out of memory", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::OutOfMemory)),
    b("diag.preset.thermal", "Thermal and throttling", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::Thermal)),
    b("diag.preset.network", "NetworkManager", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::NetworkManager)),
    b("diag.preset.live", "Live journal", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::Live)),
    b("diag.preset.service", "Logs of a service…", DIAGNOSTICS, '\u{F0219}', BuiltinAction::Preset(Preset::ServiceLogs)),
];

/// What decides which built-ins show: plain booleans, so the
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
    /// `nmap` is on the `$PATH`.
    pub nmap: bool,
    /// `[tools.builtin] usb_reset` is set.
    pub usb_reset: bool,
    /// `timer` in `modules` and `[quick] poweroff` set: the shutdown can be scheduled.
    pub shutdown: bool,
    /// The time left of the shutdown pending, in milliseconds: its first choice cancels it.
    pub shutdown_left: Option<u64>,
    /// The compositor offers screencopy: the Captures entries show.
    pub captures: bool,
    /// A recording runs: "Stop recording" shows.
    pub recording: bool,
}

/// Whether a capture verb is built: the others answer "not available yet", and their rows stay
/// hidden (in the table, with their ids) until their tasks land.
const fn built(verb: CaptureVerb) -> bool {
    matches!(verb, CaptureVerb::Full | CaptureVerb::Area | CaptureVerb::AreaSave | CaptureVerb::Pick)
}

/// Whether `b` shows under `c`.
pub fn shown(b: &Builtin, c: &Context) -> bool {
    match b.action {
        BuiltinAction::Quick(_) => c.quick,
        BuiltinAction::Shield => c.shield,
        BuiltinAction::Dnd => c.notifications,
        BuiltinAction::Toggle(Toggle::KeepAwake) => c.keep_awake,
        BuiltinAction::Toggle(Toggle::NightLight) => c.night_light,
        // Only the diagnostics tab is conditional, and it is the one with a hook.
        BuiltinAction::Tab => b.id != "tab.diagnostics" || c.diagnostics,
        BuiltinAction::Notes | BuiltinAction::Emoji | BuiltinAction::UsbEvents | BuiltinAction::Kill => true,
        BuiltinAction::Timer => c.timer,
        BuiltinAction::Shutdown => c.shutdown,
        BuiltinAction::Clipboard => c.clipboard,
        BuiltinAction::Lock => c.lock,
        BuiltinAction::Power => c.session,
        BuiltinAction::Reload => c.reload,
        BuiltinAction::Scan => c.nmap,
        BuiltinAction::UsbReset => c.usb_reset,
        BuiltinAction::Preset(_) => c.diagnostics,
        BuiltinAction::Capture(verb) => c.captures && built(verb) && (verb != CaptureVerb::Stop || c.recording),
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
    /// The label of the typed text the entry asks for, after its choice if it has one.
    pub ask: Option<String>,
    /// Fixed choices that ask differently: another label, or `None` for no prompt at all (the scan's
    /// sweep has no target, "Specific ports" asks for ports too). Empty for a user entry.
    pub ask_overrides: Vec<(String, Option<String>)>,
    /// Whether a state is known for it: a built-in with a daemon-side state, a user entry with `state`.
    pub has_state: bool,
    /// The history key; a fixed choice appends `/<choice>` (`Catalog::choice_key`).
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

impl Entry {
    /// The prompt label for `choice` (None for an entry with no choices): none when it asks nothing.
    pub fn ask_for(&self, choice: Option<&str>) -> Option<&str> {
        if let Some(c) = choice
            && let Some((_, label)) = self.ask_overrides.iter().find(|(k, _)| k == c)
        {
            return label.as_deref();
        }
        self.ask.as_deref()
    }
}

/// The choices, confirmation and prompts a built-in entry carries beyond its row: only the
/// diagnostics ones and the shutdown have any.
fn builtin_shape(action: BuiltinAction, c: &Context) -> Shape {
    let plain = Shape { choices: Choices::None, confirm: false, ask: None, ask_overrides: Vec::new() };
    match action {
        BuiltinAction::Scan => Shape {
            choices: Choices::Fixed(Scan::ALL.iter().map(|s| s.label().to_string()).collect()),
            ask: Some(TARGET_PROMPT.to_string()),
            ask_overrides: vec![
                (Scan::HostDiscovery.label().to_string(), None),
                (Scan::Ports.label().to_string(), Some(PORTS_PROMPT.to_string())),
            ],
            ..plain
        },
        BuiltinAction::UsbReset => Shape { confirm: true, ..plain },
        BuiltinAction::Shutdown => Shape { choices: Choices::Fixed(crate::timer::shutdown::labels(c.shutdown_left)), ..plain },
        _ => plain,
    }
}

/// What `builtin_shape` returns: the fields of an `Entry` a built-in sets beyond its row.
struct Shape {
    choices: Choices,
    confirm: bool,
    ask: Option<String>,
    ask_overrides: Vec<(String, Option<String>)>,
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
                    let Shape { choices, confirm, ask, ask_overrides } = builtin_shape(b.action, c);
                    entries.push(Entry {
                        kind: EntryKind::Builtin(i),
                        name: b.label.to_string(),
                        category: category_of(b.category),
                        icon: Some(b.glyph),
                        order: DEFAULT_ORDER,
                        choices,
                        confirm,
                        ask,
                        ask_overrides,
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
                ask: e.ask.clone(),
                ask_overrides: Vec::new(),
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

    /// The history key of a fixed choice of `entry`.
    pub fn choice_key(&self, entry: usize, choice: &str) -> String {
        self.entries.get(entry).map(|e| format!("{}/{choice}", e.key)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
