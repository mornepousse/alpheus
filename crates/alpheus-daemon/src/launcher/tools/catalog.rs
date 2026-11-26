//! The Tools menu's catalog: the built-in table, the `Context` of booleans that
//! decides which rows show, and the merged categories and entries (built-ins first, then the
//! user's). Built at each open, dropped at close: nothing is cached. Pure: no thread, no
//! command, no log (the notes go through a callback the glue logs).

use super::config::{BUILTIN_CATEGORIES, DEFAULT_ORDER, ToolsConfig};
use super::diag::{PORTS_PROMPT, Scan, TARGET_PROMPT};
use crate::grid::Hue;
use crate::quick::Toggle;
use alpheus_common::diagnostics::query::Preset;
use alpheus_common::launcher_ipc::{CaptureVerb, QuickVerb};
use std::borrow::Cow;

/// A category with no glyph of its own.
pub const FOLDER: char = '\u{F024B}';

/// The built-in categories' orders, in `BUILTIN_CATEGORIES`' order.
const BUILTIN_ORDERS: [u16; 7] = [10, 20, 30, 40, 80, 90, 70];
/// Their hues, in the same order.
const BUILTIN_HUES: [Hue; 7] = [Hue::Blue, Hue::Purple, Hue::Cyan, Hue::Orange, Hue::Yellow, Hue::Red, Hue::Green];
/// The hues user categories take in turn, by their place among the user categories.
const USER_HUES: [Hue; 7] = [Hue::Blue, Hue::Cyan, Hue::Green, Hue::Yellow, Hue::Orange, Hue::Red, Hue::Purple];
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
    /// What it does, in one plain line from the user's side: at most 40 characters, no trailing
    /// period (the command panel's second line and detail card).
    pub description: &'static str,
    pub glyph: char,
    pub action: BuiltinAction,
    /// The default letter that runs it from the Tools menu's root (`[tools] keys` overrides it).
    pub key: Option<char>,
}

impl Builtin {
    /// The table row `self` with a default letter.
    const fn with_key(self, key: char) -> Builtin {
        Builtin { key: Some(key), ..self }
    }

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

const fn b(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    description: &'static str,
    glyph: char,
    action: BuiltinAction,
) -> Builtin {
    Builtin { id, label, category, description, glyph, action, key: None }
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

/// The built-in table, in its order: one row per line, read as a table.
#[rustfmt::skip]
pub const BUILTINS: [Builtin; 45] = [
    b("quick.sound", "Sound", QUICK, "Set volume and output in quick settings", '\u{F057E}', BuiltinAction::Quick(QuickVerb::Sound)),
    b("quick.wifi", "Wi-Fi", QUICK, "Pick a Wi-Fi network in quick settings", '\u{F05A9}', BuiltinAction::Quick(QuickVerb::Wifi)).with_key('w'),
    b("quick.bluetooth", "Bluetooth", QUICK, "Connect a device in quick settings", '\u{F00AF}', BuiltinAction::Quick(QuickVerb::Bluetooth)).with_key('b'),
    b("quick.vpn", "VPN", QUICK, "Connect a VPN in quick settings", '\u{F0582}', BuiltinAction::Quick(QuickVerb::Vpn)).with_key('v'),
    b("quick.shield", "Shield", QUICK, "Open Shield in quick settings", '\u{F0498}', BuiltinAction::Shield).with_key('h'),
    b("dnd", "Do not disturb", QUICK, "Turn do not disturb on or off", '\u{F009B}', BuiltinAction::Dnd).with_key('d'),
    b("keep_awake", "Keep awake", QUICK, "Keep the screen on, or let it sleep", '\u{F0176}', BuiltinAction::Toggle(Toggle::KeepAwake)).with_key('z'),
    b("night_light", "Night light", QUICK, "Turn warmer screen colours on or off", '\u{F0594}', BuiltinAction::Toggle(Toggle::NightLight)).with_key('i'),
    b("tab.home", "Home", CONTROL, "Open the control center's Home tab", '\u{F02DC}', BuiltinAction::Tab),
    b("tab.sound", "Sound", CONTROL, "Open the control center's Sound tab", '\u{F057E}', BuiltinAction::Tab),
    b("tab.network", "Network", CONTROL, "Open the control center's Network tab", '\u{F06F3}', BuiltinAction::Tab),
    b("tab.bluetooth", "Bluetooth", CONTROL, "Open the control center's Bluetooth tab", '\u{F00AF}', BuiltinAction::Tab),
    b("tab.power", "Power", CONTROL, "Open the control center's Power tab", '\u{F0079}', BuiltinAction::Tab),
    b("tab.displays", "Displays", CONTROL, "Open the control center's Displays tab", '\u{F0379}', BuiltinAction::Tab),
    b("tab.appearance", "Theme", CONTROL, "Open the control center's Theme tab", '\u{F03D8}', BuiltinAction::Tab),
    b("tab.diagnostics", "Diagnostics", CONTROL, "Open the control center's Diag tab", '\u{F04D9}', BuiltinAction::Tab),
    b("notes", "Notes", UTILITIES, "Jot down a note", '\u{F039E}', BuiltinAction::Notes).with_key('n'),
    b("timer", "Timer", UTILITIES, "Set a focus timer", '\u{F051B}', BuiltinAction::Timer).with_key('t'),
    b("clipboard", "Clipboard", UTILITIES, "Pick from the clipboard history", '\u{F014C}', BuiltinAction::Clipboard).with_key('c'),
    b("emoji", "Emoji", UTILITIES, "Find an emoji and copy it", '\u{F0C68}', BuiltinAction::Emoji).with_key('m'),
    b("capture.full", "Screenshot", CAPTURES, "Copy a screenshot of this screen", '\u{F0E51}', BuiltinAction::Capture(CaptureVerb::Full)).with_key('f'),
    b("capture.area", "Area", CAPTURES, "Select an area, copy it", '\u{F0489}', BuiltinAction::Capture(CaptureVerb::Area)).with_key('a'),
    b("capture.area_save", "Area (save)", CAPTURES, "Select an area, save it to Pictures", '\u{F0193}', BuiltinAction::Capture(CaptureVerb::AreaSave)),
    b("capture.annotate", "Annotate", CAPTURES, "Select an area, draw on it", '\u{F03EB}', BuiltinAction::Capture(CaptureVerb::Annotate)).with_key('e'),
    b("capture.record", "Record screen", CAPTURES, "Record this screen to Videos", '\u{F044A}', BuiltinAction::Capture(CaptureVerb::Record)).with_key('r'),
    b("capture.record_area", "Record area", CAPTURES, "Select an area, record it", '\u{F0567}', BuiltinAction::Capture(CaptureVerb::RecordArea)),
    b("capture.stop", "Stop recording", CAPTURES, "End the recording and save it", '\u{F04DB}', BuiltinAction::Capture(CaptureVerb::Stop)).with_key('r'),
    b("capture.pick", "Colour picker", CAPTURES, "Copy the colour of a pixel", '\u{F020A}', BuiltinAction::Capture(CaptureVerb::Pick)).with_key('p'),
    b("capture.ocr", "Text from area (OCR)", CAPTURES, "Select an area, copy its text", '\u{F113D}', BuiltinAction::Capture(CaptureVerb::Ocr)).with_key('o'),
    b("shutdown", "Schedule shutdown", SYSTEM, "Power off after a delay", '\u{F0902}', BuiltinAction::Shutdown).with_key('s'),
    b("process.kill", "Kill a process", SYSTEM, "Pick a process to end", '\u{F068C}', BuiltinAction::Kill).with_key('k'),
    b("lock", "Lock", SESSION, "Lock the screen", '\u{F033E}', BuiltinAction::Lock).with_key('l'),
    b("power", "Power menu", SESSION, "Lock, log out, reboot or power off", '\u{F0425}', BuiltinAction::Power).with_key('q'),
    b("reload", "Reload the bar", SESSION, "Run the reload command you set", '\u{F0450}', BuiltinAction::Reload),
    b("diag.scan", "Network scan", DIAGNOSTICS, "Scan hosts and ports with nmap", '\u{F0437}', BuiltinAction::Scan),
    b("diag.usb_events", "USB events", DIAGNOSTICS, "Watch USB devices come and go", '\u{F0553}', BuiltinAction::UsbEvents).with_key('u'),
    b("diag.usb_reset", "Reset USB (xHCI)", DIAGNOSTICS, "Restart the USB controller", '\u{F0709}', BuiltinAction::UsbReset).with_key('x'),
    b("diag.preset.errors", "Errors this boot", DIAGNOSTICS, "Read journal errors since this boot", '\u{F0219}', BuiltinAction::Preset(Preset::ErrorsThisBoot)).with_key('j'),
    b("diag.preset.usb", "USB", DIAGNOSTICS, "Read journal lines about USB", '\u{F0219}', BuiltinAction::Preset(Preset::Usb)),
    b("diag.preset.suspend", "Suspend and resume", DIAGNOSTICS, "Read journal lines on suspend and resume", '\u{F0219}', BuiltinAction::Preset(Preset::SuspendResume)),
    b("diag.preset.oom", "Out of memory", DIAGNOSTICS, "Read out-of-memory kills in the journal", '\u{F0219}', BuiltinAction::Preset(Preset::OutOfMemory)),
    b("diag.preset.thermal", "Thermal and throttling", DIAGNOSTICS, "Read the journal on heat and throttling", '\u{F0219}', BuiltinAction::Preset(Preset::Thermal)),
    b("diag.preset.network", "NetworkManager", DIAGNOSTICS, "Read NetworkManager's journal", '\u{F0219}', BuiltinAction::Preset(Preset::NetworkManager)),
    b("diag.preset.live", "Live journal", DIAGNOSTICS, "Follow the journal as it is written", '\u{F0219}', BuiltinAction::Preset(Preset::Live)),
    b("diag.preset.service", "Logs of a service…", DIAGNOSTICS, "Read one service's journal", '\u{F0219}', BuiltinAction::Preset(Preset::ServiceLogs)),
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
    /// A recording runs: "Stop recording" shows, and the two "Record" rows hide.
    pub recording: bool,
    /// `[captures] annotate` is set and its program is on the `$PATH`.
    pub annotate: bool,
    /// `[captures] ocr` is set and its program is on the `$PATH`.
    pub ocr: bool,
    /// `[captures] record` is set and its program is on the `$PATH`.
    pub record: bool,
}

/// Whether the row of capture `verb` shows, the compositor having screencopy: a row whose external
/// tool is off or missing hides, the two "Record" rows hide while a recording runs (one at a time),
/// and "Stop recording" shows only then.
const fn capture_shown(verb: CaptureVerb, c: &Context) -> bool {
    match verb {
        CaptureVerb::Full | CaptureVerb::Area | CaptureVerb::AreaSave | CaptureVerb::Pick => true,
        CaptureVerb::Annotate => c.annotate,
        CaptureVerb::Ocr => c.ocr,
        CaptureVerb::Record | CaptureVerb::RecordArea => c.record && !c.recording,
        CaptureVerb::Stop => c.recording,
        // The debug selector has no row.
        CaptureVerb::Select => false,
    }
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
        BuiltinAction::Capture(verb) => c.captures && capture_shown(verb, c),
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
    /// The letter that runs it from the Tools menu's root: `None` when it wants none or lost it to an earlier claimant.
    pub letter: Option<char>,
    /// One line on what it does: a built-in's table text (borrowed, never copied), a user entry's
    /// `description`, or none.
    pub description: Option<Cow<'static, str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub name: String,
    pub icon: char,
    pub order: u16,
    /// Its cards' tint; `None` stands for the Pinned card's accent.
    pub hue: Option<Hue>,
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

/// The letter `e` wants: a user entry its `key`, a built-in its `[tools] keys` letter, else its
/// default.
fn wanted_letter(e: &Entry, t: &ToolsConfig) -> Option<char> {
    match e.kind {
        EntryKind::Builtin(b) => BUILTINS.get(b).and_then(|b| t.keys.get(b.id).copied().or(b.key)),
        EntryKind::User(u) => t.entries.get(u).and_then(|u| u.key),
    }
}

impl Catalog {
    /// The catalog for `t` under `c`. An unknown id in `hide`, `root` or `keys` is noted once per list,
    /// however many it holds (never quoted), and ignored, so a file written for a newer alpheus stays valid.
    pub fn build(t: &ToolsConfig, c: &Context, note: &mut dyn FnMut(&str)) -> Catalog {
        let known = |id: &String| BUILTINS.iter().any(|b| b.id == id);
        if !t.hide.iter().all(known) {
            note("tools: unknown built-in id in [tools] hide");
        }
        if !t.root.iter().all(known) {
            note("tools: unknown built-in id in [tools] root");
        }
        if !t.keys.keys().all(known) {
            note("tools: unknown built-in id in [tools] keys");
        }
        let mut categories: Vec<Category> = BUILTIN_CATEGORIES
            .iter()
            .zip(BUILTIN_GLYPHS)
            .zip(BUILTIN_ORDERS)
            .zip(BUILTIN_HUES)
            .map(|(((name, icon), order), hue)| Category { name: (*name).to_string(), icon, order, hue: Some(hue) })
            .collect();
        let mut user_categories = 0;
        for u in &t.categories {
            match categories.iter_mut().find(|k| k.name == u.name) {
                // A user category of a built-in's name restyles it.
                Some(k) => {
                    k.icon = icon_of(&u.icon).unwrap_or(k.icon);
                    k.order = u.order.unwrap_or(k.order);
                }
                None => {
                    categories.push(Category {
                        name: u.name.clone(),
                        icon: icon_of(&u.icon).unwrap_or(FOLDER),
                        order: u.order.unwrap_or(DEFAULT_ORDER),
                        hue: USER_HUES.get(user_categories % USER_HUES.len()).copied(),
                    });
                    user_categories += 1;
                }
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
                        letter: None,
                        description: Some(Cow::Borrowed(b.description)),
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
                letter: None,
                description: e.description.clone().map(Cow::Owned),
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
        let mut catalog = Catalog { categories, entries, root };
        catalog.claim_letters(t);
        catalog
    }

    /// Gives each entry the letter it wants unless an earlier one took it, in display order: the
    /// Pinned card's entries, then each category's in category order. A user entry wants its `key`,
    /// a built-in its `[tools] keys` letter, else its default.
    fn claim_letters(&mut self, t: &ToolsConfig) {
        let rows = self.root_rows();
        let mut order: Vec<usize> = rows.iter().filter_map(|r| if let Item::Entry(e) = *r { Some(e) } else { None }).collect();
        for r in &rows {
            if let Item::Category(k) = *r {
                order.extend(self.category_rows(k));
            }
        }
        let mut taken: Vec<char> = Vec::new();
        for i in order {
            let Some(e) = self.entries.get_mut(i) else { continue };
            if let Some(l) = wanted_letter(e, t)
                && !taken.contains(&l)
            {
                taken.push(l);
                e.letter = Some(l);
            }
        }
    }

    /// How many entries want a letter an earlier claimant took (the glue logs the count, never
    /// which).
    pub fn lost_letters(&self, t: &ToolsConfig) -> usize {
        self.entries.iter().filter(|e| e.letter.is_none() && wanted_letter(e, t).is_some()).count()
    }

    /// Whether entry `e` is one of the root's (`[tools] root`, or a user entry with no category):
    /// what the panel's Pinned section lists.
    pub fn is_pinned(&self, e: usize) -> bool {
        self.root.contains(&e) || self.entries.get(e).is_some_and(|x| x.category.is_none())
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
