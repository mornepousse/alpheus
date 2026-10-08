//! ~/.config/alpheus/bar.toml, optional. Absent: defaults, no message.
//! Invalid: defaults and a warning (daemon log). Never an abort. Each
//! setting is read once at startup and costs nothing if unused.
//!
//! ```toml
//! modules = ["launcher", "workspaces", "window", "clock"]   # order and presence, left to right
//! height = 22                                               # logical px
//! [font]
//! family = "JetBrainsMono Nerd Font Mono"                   # absent: a monospace font from the system
//! size = 14.0
//! fallback = ["DejaVu Sans Mono"]                           # used only for characters `family` lacks; setting it replaces this default (list DejaVu to keep it), [] disables it
//! [clock]
//! format = "%H:%M"
//! date = "%a %e %b"                                         # "": no date
//! calendar = true                                           # a left click on the clock opens the month
//! week_numbers = false                                      # ISO week numbers in the calendar
//! [workspaces]
//! command = ["wm-set", "workspace", "{workspace}", "{output}"]
//! [actions]                                                 # clicks on the system modules
//! open_tab = ["alpheus-toggle", "{tab}"]                    # {tab}: home, sound, network, bluetooth
//! resources = ["foot", "-e", "btop"]
//! [resources]
//! interval = 2                                              # seconds between readings, 1 to 60
//! graph = 20                                                # cells of CPU history, 0 to 60 (0: no graph)
//! sensor = ""                                               # hwmon name or thermal zone type; "": automatic
//! [battery]
//! peripherals = "always"                                    # "always" | "low" | "never": peripherals with a real percentage
//! coarse = "low"                                            # "always" | "low" | "never": peripherals with only a coarse level
//! [tray]
//! passive = "dim"                                           # "dim" | "show" | "hide": items that say they are idle
//! tooltips = true                                           # the item's name after ½ s of hovering
//! [tray.icons]                                              # glyph overrides, keyed like the built-in table
//! keepassxc = "󰌋"
//! [media]
//! title_max = 30                                            # cells of the title in the bar, 8 to 80
//! ignore = ["kdeconnect"]                                   # players never shown: substrings of their bus name (playerctld always is)
//! ```

use crate::clock;
use crate::grid::text_width;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The bar's modules (spec, § 2), left to right by default. All are
/// accepted from the start: a bar.toml written today stays valid once
/// deliveries 3 to 5 arrive. Those not yet delivered simply do not show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    /// The launcher's button (opens the launcher on a left click).
    Launcher,
    Workspaces,
    Window,
    Media,
    Volume,
    Network,
    Bluetooth,
    Battery,
    Resources,
    Tray,
    Notifications,
    Clock,
}

impl ModuleId {
    pub const ALL: [ModuleId; 12] = [
        ModuleId::Launcher,
        ModuleId::Workspaces,
        ModuleId::Window,
        ModuleId::Media,
        ModuleId::Volume,
        ModuleId::Network,
        ModuleId::Bluetooth,
        ModuleId::Battery,
        ModuleId::Resources,
        ModuleId::Tray,
        ModuleId::Notifications,
        ModuleId::Clock,
    ];

    /// Name in bar.toml.
    pub fn name(self) -> &'static str {
        match self {
            ModuleId::Launcher => "launcher",
            ModuleId::Workspaces => "workspaces",
            ModuleId::Window => "window",
            ModuleId::Media => "media",
            ModuleId::Volume => "volume",
            ModuleId::Network => "network",
            ModuleId::Bluetooth => "bluetooth",
            ModuleId::Battery => "battery",
            ModuleId::Resources => "resources",
            ModuleId::Tray => "tray",
            ModuleId::Notifications => "notifications",
            ModuleId::Clock => "clock",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    /// Monospace family; absent from the system: another monospace one.
    pub family: String,
    /// Size in logical px.
    pub size: f32,
    /// Fallback families for characters `family` lacks (emoji, CJK, odd
    /// window-title symbols…), tried in order. Defaults to DejaVu Sans
    /// Mono, a small font (4 faces, only its regular and bold ones ever
    /// loaded) shipped alongside alpheus specifically to cover the symbols
    /// a Nerd Font window manager or terminal likes to put in a title (✳,
    /// ◐◓◑◒…) that JetBrainsMono Nerd Font Mono itself does not have; each
    /// added or removed family changes memory by its own faces (Q11),
    /// `[]` disables fallback entirely. A character that still has no
    /// glyph anywhere (checked family then every fallback) is masked
    /// rather than drawn as a `.notdef` box (`painter.rs`).
    pub fallback: Vec<String>,
}

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig { family: "JetBrainsMono Nerd Font Mono".into(), size: 14.0, fallback: vec!["DejaVu Sans Mono".into()] }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClockConfig {
    /// The time, in bold accent.
    pub format: String,
    /// The date, muted, before the time; empty = no date.
    pub date: String,
    /// A left click on the clock opens the calendar (default true —
    /// question 10 of the tray plan).
    pub calendar: bool,
    /// ISO week numbers in the calendar (default false).
    pub week_numbers: bool,
}

impl Default for ClockConfig {
    fn default() -> Self {
        ClockConfig { format: "%H:%M".into(), date: "%a %e %b".into(), calendar: true, week_numbers: false }
    }
}

/// How a tray item whose `Status` is `Passive` (idle, nothing to report)
/// is drawn: dimmed (default), like an active one, or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Passive {
    #[default]
    Dim,
    Show,
    Hide,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrayConfig {
    pub passive: Passive,
    /// Tooltips after ½ s of hovering; false: no hover timer at all.
    pub tooltips: bool,
    /// Overrides of the built-in glyph table, keyed like it (lower-cased at
    /// load): a key matched as a substring of the item's Id, IconName,
    /// tooltip title or process name, a glyph of one or two cells. Two keys
    /// differing only by case are an error (never one picked silently).
    pub icons: BTreeMap<String, String>,
}

impl Default for TrayConfig {
    fn default() -> Self {
        TrayConfig { passive: Passive::Dim, tooltips: true, icons: BTreeMap::new() }
    }
}

/// Most `[tray.icons]` entries accepted.
pub const TRAY_ICONS_MAX: usize = 128;

/// `[tray.icons]` checked and its keys lower-cased: at most
/// `TRAY_ICONS_MAX` entries, no blank key, no blanks around a key, no two
/// keys differing only by case, each glyph one or two cells with no
/// control character.
fn tray_icons(raw: BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    if raw.len() > TRAY_ICONS_MAX {
        return Err(format!("tray.icons: {} entries, at most {TRAY_ICONS_MAX}", raw.len()));
    }
    let mut icons = BTreeMap::new();
    for (key, glyph) in raw {
        if key.trim().is_empty() {
            return Err("tray.icons: empty key".into());
        }
        if key.trim() != key {
            return Err(format!("tray.icons.{key:?}: blanks around the key (the needle would never match)"));
        }
        if key.chars().any(char::is_control) {
            return Err(format!("tray.icons.{key:?}: a control character in the key (the needle would never match)"));
        }
        if glyph.chars().any(char::is_control) || !(1..=2).contains(&text_width(&glyph)) {
            return Err(format!("tray.icons.{key:?}: expected a glyph of one or two cells, got {glyph:?}"));
        }
        let lower = key.to_lowercase();
        if icons.insert(lower, glyph).is_some() {
            return Err(format!("tray.icons.{key:?}: another key differs from it only by case"));
        }
    }
    Ok(icons)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspacesConfig {
    /// Command run to go to a workspace (click, wheel); `{workspace}` and
    /// `{output}` are replaced in it. Default: `wm-set` (Mae's
    /// nixos-config), the only path to the compositor allowed by the
    /// framing document.
    pub command: Vec<String>,
}

impl Default for WorkspacesConfig {
    fn default() -> Self {
        WorkspacesConfig { command: ["wm-set", "workspace", "{workspace}", "{output}"].map(String::from).to_vec() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActionsConfig {
    /// Opens alpheus on a tab (left click on volume, network, Bluetooth,
    /// battery); `{tab}` becomes home, sound, network or bluetooth (the
    /// TUI's `--tab`). Default: `alpheus-toggle` (Mae's nixos-config), which
    /// opens the TUI in a floating terminal, or closes it if already open —
    /// not a neutral default yet: see the portability step (CLAUDE.md).
    pub open_tab: Vec<String>,
    /// Left click on the resources: a system monitor. Default: btop in
    /// foot (Mae's decision, 2026-10-05: lightness — foot measures ≈ 9.9 MB
    /// PSS against ≈ 92.6 MB for alacritty, for the same TUI).
    pub resources: Vec<String>,
}

impl Default for ActionsConfig {
    fn default() -> Self {
        ActionsConfig {
            open_tab: ["alpheus-toggle", "{tab}"].map(String::from).to_vec(),
            resources: ["foot", "-e", "btop"].map(String::from).to_vec(),
        }
    }
}

/// Most points of CPU history (spec § 3: graph histories capped at 60).
pub const GRAPH_MAX: u16 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcesConfig {
    /// Seconds between two readings of /proc (1 to 60).
    pub interval: u64,
    /// Cells of CPU history shown, one point per cell (0 to 60; 0 = no graph).
    pub graph: u16,
    /// Temperature sensor: an hwmon `name` (coretemp, k10temp…) or a thermal
    /// zone `type` (x86_pkg_temp…); empty = automatic (Q3).
    pub sensor: String,
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        ResourcesConfig { interval: 2, graph: 20, sensor: String::new() }
    }
}

/// How a battery segment shows up (spec Q, Mae 2026-10-05): every peripheral
/// with a real percentage defaults to "always" (today's behaviour); one that
/// only reports a coarse UPower `BatteryLevel` defaults to "low" (today's
/// behaviour: hidden unless Low, Critical or Action).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BatteryVisibility {
    Always,
    Low,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatteryConfig {
    /// Peripherals with a real percentage (spec Q7's default: always shown,
    /// error tint below 20 %).
    pub peripherals: BatteryVisibility,
    /// Peripherals that only report a coarse level: "always" shows the bare
    /// icon (error tint when low, normal otherwise), "low" (default) shows
    /// it only when Low, Critical or Action, "never" hides it.
    pub coarse: BatteryVisibility,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        BatteryConfig { peripherals: BatteryVisibility::Always, coarse: BatteryVisibility::Low }
    }
}

/// The narrowest and widest `[media] title_max`, in cells.
pub const TITLE_MAX_MIN: u16 = 8;
pub const TITLE_MAX_MAX: u16 = 80;

/// `[media]` (media spec § 4): the title's width in the bar, and the players never shown.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Cells of the title in the bar (8 to 80), "…" included.
    pub title_max: u16,
    /// Substrings of a player's bus name: that player is never queried. playerctld is always
    /// ignored, whatever this holds.
    pub ignore: Vec<String>,
}

impl Default for MediaConfig {
    fn default() -> Self {
        MediaConfig { title_max: 30, ignore: vec!["kdeconnect".into()] }
    }
}

pub const DEFAULT_HEIGHT: u32 = 22;

#[derive(Debug, Clone, PartialEq)]
pub struct BarConfig {
    pub modules: Vec<ModuleId>,
    /// Height of the bar, in logical px.
    pub height: u32,
    pub font: FontConfig,
    pub clock: ClockConfig,
    pub workspaces: WorkspacesConfig,
    pub actions: ActionsConfig,
    pub resources: ResourcesConfig,
    pub battery: BatteryConfig,
    pub tray: TrayConfig,
    pub media: MediaConfig,
}

impl Default for BarConfig {
    fn default() -> Self {
        BarConfig {
            modules: ModuleId::ALL.to_vec(),
            height: DEFAULT_HEIGHT,
            font: FontConfig::default(),
            clock: ClockConfig::default(),
            workspaces: WorkspacesConfig::default(),
            actions: ActionsConfig::default(),
            resources: ResourcesConfig::default(),
            battery: BatteryConfig::default(),
            tray: TrayConfig::default(),
            media: MediaConfig::default(),
        }
    }
}

fn default_height() -> u32 {
    DEFAULT_HEIGHT
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    modules: Option<Vec<ModuleId>>,
    #[serde(default = "default_height")]
    height: u32,
    #[serde(default)]
    font: FontConfig,
    #[serde(default)]
    clock: ClockConfig,
    #[serde(default)]
    workspaces: WorkspacesConfig,
    #[serde(default)]
    actions: ActionsConfig,
    #[serde(default)]
    resources: ResourcesConfig,
    #[serde(default)]
    battery: BatteryConfig,
    #[serde(default)]
    tray: TrayConfig,
    #[serde(default)]
    media: MediaConfig,
}

impl BarConfig {
    /// Absent → defaults with no message; unreadable or invalid → defaults + message.
    pub fn load(path: &Path) -> (BarConfig, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BarConfig::default(), None),
            Err(e) => (BarConfig::default(), Some(format!("bar.toml unreadable: {e}"))),
            Ok(s) => match Self::from_text(&s) {
                Ok(c) => (c, None),
                Err(m) => (BarConfig::default(), Some(format!("bar.toml invalid: {m}"))),
            },
        }
    }

    pub fn from_text(s: &str) -> Result<BarConfig, String> {
        let b: Raw = toml::from_str(s).map_err(|e| e.message().to_string())?;
        let modules = b.modules.unwrap_or_else(|| ModuleId::ALL.to_vec());
        for (i, m) in modules.iter().enumerate() {
            if modules[..i].contains(m) {
                return Err(format!("duplicate module \"{}\"", m.name()));
            }
        }
        if !(12..=64).contains(&b.height) {
            return Err(format!("height {} out of range 12 to 64 px", b.height));
        }
        if !(6.0..=48.0).contains(&b.font.size) {
            return Err(format!("font.size {} out of range 6 to 48 px", b.font.size));
        }
        if b.font.family.trim().is_empty() {
            return Err("font.family empty".into());
        }
        if b.clock.format.trim().is_empty() {
            return Err("clock.format empty".into());
        }
        clock::validate(&b.clock.format).map_err(|m| format!("clock.format: {m}"))?;
        clock::validate(&b.clock.date).map_err(|m| format!("clock.date: {m}"))?;
        if b.workspaces.command.first().is_none_or(|p| p.trim().is_empty()) {
            return Err("workspaces.command empty (expected: [\"program\", \"argument\", …])".into());
        }
        if !(1..=60).contains(&b.resources.interval) {
            return Err(format!("resources.interval {} out of range 1 to 60 s", b.resources.interval));
        }
        if b.resources.graph > GRAPH_MAX {
            return Err(format!("resources.graph {} out of range 0 to {GRAPH_MAX} cells", b.resources.graph));
        }
        for (key, command) in [("actions.open_tab", &b.actions.open_tab), ("actions.resources", &b.actions.resources)] {
            if command.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(format!("{key} empty (expected: [\"program\", \"argument\", …])"));
            }
        }
        if !(TITLE_MAX_MIN..=TITLE_MAX_MAX).contains(&b.media.title_max) {
            return Err(format!("media.title_max {} out of range {TITLE_MAX_MIN} to {TITLE_MAX_MAX} cells", b.media.title_max));
        }
        let tray = TrayConfig { icons: tray_icons(b.tray.icons)?, ..b.tray };
        Ok(BarConfig {
            modules,
            height: b.height,
            font: b.font,
            clock: b.clock,
            workspaces: b.workspaces,
            actions: b.actions,
            resources: b.resources,
            battery: b.battery,
            tray,
            media: b.media,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file(name: &str, content: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("alpheus-{}-bar-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("bar.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn absent_defaults_without_message() {
        let (c, w) = BarConfig::load(Path::new("/nonexistent/bar.toml"));
        assert_eq!((c, w), (BarConfig::default(), None));
        let d = BarConfig::default();
        assert_eq!((d.modules.first(), d.modules.last()), (Some(&ModuleId::Launcher), Some(&ModuleId::Clock)));
        assert_eq!((d.height, d.font.size, d.font.fallback.as_slice()), (22, 14.0, ["DejaVu Sans Mono".to_string()].as_slice()));
        assert_eq!(d.workspaces.command, ["wm-set", "workspace", "{workspace}", "{output}"]);
    }

    #[test]
    fn launcher_module_first_by_default_and_removable() {
        assert_eq!(ModuleId::ALL[0], ModuleId::Launcher);
        assert_eq!(ModuleId::Launcher.name(), "launcher");
        let c = BarConfig::from_text("modules = [\"clock\", \"launcher\"]").unwrap();
        assert_eq!(c.modules, vec![ModuleId::Clock, ModuleId::Launcher], "named \"launcher\" in bar.toml, placed anywhere");
        let c = BarConfig::from_text("modules = [\"workspaces\", \"clock\"]").unwrap();
        assert!(!c.modules.contains(&ModuleId::Launcher), "removable");
        for m in ModuleId::ALL {
            let parsed = BarConfig::from_text(&format!("modules = [\"{}\"]", m.name())).unwrap();
            assert_eq!(parsed.modules, vec![m], "name() is the bar.toml name of {m:?}");
        }
    }

    #[test]
    fn settings_read() {
        let text = "modules = [\"clock\", \"workspaces\"]\nheight = 28\n[font]\nfamily = \"Fira Code\"\nfallback = [\"Noto Color Emoji\"]\n[clock]\nformat = \"%H:%M:%S\"\n[workspaces]\ncommand = [\"my-tool\", \"{workspace}\"]\n";
        let (c, w) = BarConfig::load(&file("ok", text));
        assert_eq!(w, None);
        assert_eq!(c.modules, vec![ModuleId::Clock, ModuleId::Workspaces]);
        assert_eq!((c.height, c.font.family.as_str(), c.font.size), (28, "Fira Code", 14.0));
        assert_eq!((c.clock.format.as_str(), c.clock.date.as_str()), ("%H:%M:%S", "%a %e %b"));
        assert_eq!(c.font.fallback, ["Noto Color Emoji"]);
        assert_eq!(c.workspaces.command, ["my-tool", "{workspace}"]);
    }

    #[test]
    fn unknown_module_defaults_and_message() {
        let (c, w) = BarConfig::load(&file("unknown", "modules = [\"workspaces\", \"weather\"]\n"));
        assert_eq!(c, BarConfig::default());
        let w = w.unwrap();
        assert!(w.contains("bar.toml invalid") && w.contains("weather"), "{w}");
    }

    #[test]
    fn refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        assert!(err("modules = [\"clock\", \"clock\"]").contains("duplicate"));
        assert!(err("[clock]\nformat = \"%Q\"").contains("clock.format"));
        assert!(err("[clock]\ndate = \"%\"").contains("clock.date"));
        assert!(err("[clock]\nformat = \"\"").contains("empty"));
        assert!(err("height = 4").contains("height"));
        assert!(err("[font]\nsize = 200.0").contains("font.size"));
        assert!(err("[font]\nfamily = \" \"").contains("font.family"));
        assert!(err("[workspaces]\ncommand = []").contains("workspaces.command"));
        assert!(BarConfig::from_text("size = 3").is_err(), "unknown key refused");
        assert_eq!(BarConfig::from_text("modules = []").unwrap().modules, vec![]);
    }

    #[test]
    fn actions_and_resources_defaults_and_settings() {
        let d = BarConfig::default();
        assert_eq!(d.actions.open_tab, ["alpheus-toggle", "{tab}"]);
        assert_eq!(d.actions.resources, ["foot", "-e", "btop"]);
        assert_eq!((d.resources.interval, d.resources.graph, d.resources.sensor.as_str()), (2, 20, ""));
        let text = "[actions]\nopen_tab = [\"my-panel\", \"--tab\", \"{tab}\"]\nresources = [\"foot\", \"htop\"]\n[resources]\ninterval = 5\ngraph = 0\nsensor = \"k10temp\"\n";
        let c = BarConfig::from_text(text).unwrap();
        assert_eq!(c.actions.open_tab, ["my-panel", "--tab", "{tab}"]);
        assert_eq!(c.actions.resources, ["foot", "htop"]);
        assert_eq!((c.resources.interval, c.resources.graph, c.resources.sensor.as_str()), (5, 0, "k10temp"));
    }

    #[test]
    fn actions_and_resources_refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        assert!(err("[resources]\ninterval = 0").contains("resources.interval"));
        assert!(err("[resources]\ninterval = 61").contains("resources.interval"));
        assert!(err("[resources]\ngraph = 61").contains("resources.graph"));
        assert!(err("[actions]\nopen_tab = []").contains("actions.open_tab"));
        assert!(err("[actions]\nresources = [\" \"]").contains("actions.resources"));
        assert!(err("[resources]\nperiod = 2").contains("period"), "unknown key refused");
        let c = BarConfig::from_text("[resources]\ninterval = 60\ngraph = 60").unwrap();
        assert_eq!((c.resources.interval, c.resources.graph), (60, GRAPH_MAX), "bounds included");
    }

    #[test]
    fn battery_defaults_and_settings() {
        let d = BarConfig::default();
        assert_eq!((d.battery.peripherals, d.battery.coarse), (BatteryVisibility::Always, BatteryVisibility::Low));
        let text = "[battery]\nperipherals = \"low\"\ncoarse = \"never\"\n";
        let c = BarConfig::from_text(text).unwrap();
        assert_eq!((c.battery.peripherals, c.battery.coarse), (BatteryVisibility::Low, BatteryVisibility::Never));
        let text = "[battery]\nperipherals = \"never\"\ncoarse = \"always\"\n";
        let c = BarConfig::from_text(text).unwrap();
        assert_eq!((c.battery.peripherals, c.battery.coarse), (BatteryVisibility::Never, BatteryVisibility::Always));
    }

    #[test]
    fn battery_refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        assert!(err("[battery]\nperipherals = \"sometimes\"").contains("sometimes"));
        assert!(err("[battery]\ncoarse = \"soon\"").contains("soon"));
        assert!(err("[battery]\nshown = true").contains("shown"), "unknown key refused");
    }

    #[test]
    fn tray_and_clock_keys_parse_with_defaults() {
        let d = BarConfig::default();
        assert_eq!(d.tray, TrayConfig { passive: Passive::Dim, tooltips: true, icons: BTreeMap::new() });
        assert_eq!((d.clock.calendar, d.clock.week_numbers), (true, false), "the calendar on, week numbers off");
        assert_eq!(BarConfig::from_text("height = 22").unwrap().tray, TrayConfig::default(), "absent: defaults");
        let text = "[tray]\npassive = \"hide\"\ntooltips = false\n[tray.icons]\nkeepassxc = \"\u{f030b}\"\n\
                    [clock]\ncalendar = false\nweek_numbers = true\n";
        let c = BarConfig::from_text(text).unwrap();
        assert_eq!((c.tray.passive, c.tray.tooltips), (Passive::Hide, false));
        assert_eq!(c.tray.icons.get("keepassxc").map(String::as_str), Some("\u{f030b}"));
        assert_eq!((c.clock.calendar, c.clock.week_numbers), (false, true));
        assert_eq!((c.clock.format.as_str(), c.clock.date.as_str()), ("%H:%M", "%a %e %b"), "the other clock keys keep their defaults");
        assert_eq!(BarConfig::from_text("[tray]\npassive = \"show\"").unwrap().tray.passive, Passive::Show);
        assert_eq!(BarConfig::from_text("[tray]\npassive = \"dim\"").unwrap().tray.passive, Passive::Dim);
    }

    #[test]
    fn tray_icons_keys_are_lowercased() {
        let c = BarConfig::from_text("[tray.icons]\nDiscord = \"\u{f066f}\"\n\"KeePassXC\" = \"\u{f030b}\"\n").unwrap();
        let keys: Vec<&str> = c.tray.icons.keys().map(String::as_str).collect();
        assert_eq!(keys, ["discord", "keepassxc"]);
    }

    #[test]
    fn invalid_passive_mode_is_an_error() {
        let e = BarConfig::from_text("[tray]\npassive = \"blink\"").unwrap_err();
        assert!(e.contains("blink"), "{e}");
        let (c, w) = BarConfig::load(&file("passive", "[tray]\npassive = \"blink\"\n"));
        assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
        assert!(w.is_some_and(|w| w.contains("bar.toml invalid")));
    }

    #[test]
    fn an_icon_wider_than_two_cells_or_empty_is_an_error() {
        let err = |t: &str| BarConfig::from_text(&format!("[tray.icons]\n{t}\n")).unwrap_err();
        // The key is escaped ({key:?}, not {key}): "tray.icons.\"foo\"", not "tray.icons.foo".
        assert!(err("foo = \"abc\"").contains("tray.icons.\"foo\""), "three cells");
        assert!(err("foo = \"日a\"").contains("tray.icons.\"foo\""), "a wide character and one more: three cells");
        assert!(err("foo = \"\"").contains("tray.icons.\"foo\""), "empty");
        assert!(err("foo = \"\u{301}\"").contains("tray.icons.\"foo\""), "a lone combining mark: no cell");
        assert!(err("foo = \"a\\u0007\"").contains("tray.icons.\"foo\""), "a control character");
        assert!(err("\"\" = \"x\"").contains("empty key"), "an empty key");
        assert!(err("\" \" = \"x\"").contains("empty key"), "a blank key");
        assert!(err("\" discord\" = \"x\"").contains("blanks around the key"), "a key with blanks around it");
        assert!(err("\"discord \" = \"x\"").contains("blanks around the key"), "trailing blanks too");
        for ok in ["foo = \"x\"", "foo = \"ab\"", "foo = \"日\"", "foo = \"\u{f066f}\""] {
            assert!(BarConfig::from_text(&format!("[tray.icons]\n{ok}\n")).is_ok(), "{ok}: one or two cells");
        }
    }

    #[test]
    fn unknown_tray_key_is_an_error() {
        let e = BarConfig::from_text("[tray]\ncolour = \"red\"").unwrap_err();
        assert!(e.contains("colour"), "{e}");
        assert!(BarConfig::from_text("[tray.icons]\nanything = \"x\"").is_ok(), "[tray.icons] takes any key");
    }

    #[test]
    fn too_many_icons_is_an_error() {
        let icons = |n: usize| format!("[tray.icons]\n{}", (0..n).map(|i| format!("app{i} = \"x\"\n")).collect::<String>());
        assert_eq!(BarConfig::from_text(&icons(TRAY_ICONS_MAX)).unwrap().tray.icons.len(), TRAY_ICONS_MAX);
        let e = BarConfig::from_text(&icons(TRAY_ICONS_MAX + 1)).unwrap_err();
        assert!(e.contains("tray.icons") && e.contains("128"), "{e}");
    }

    #[test]
    fn tray_icon_keys_differing_only_by_case_are_an_error() {
        let e = BarConfig::from_text("[tray.icons]\nDiscord = \"x\"\ndiscord = \"y\"\n").unwrap_err();
        assert!(e.contains("tray.icons.") && e.contains("another key differs from it only by case"), "{e}");
        let (c, w) = BarConfig::load(&file("icon-case", "[tray.icons]\nSteam = \"x\"\nSTEAM = \"y\"\n"));
        assert_eq!(c, BarConfig::default(), "defaults, never one of the two glyphs picked silently");
        assert!(w.is_some_and(|w| w.contains("bar.toml invalid")));
        assert!(BarConfig::from_text("[tray.icons]\ndiscord = \"x\"\nsteam = \"y\"\n").is_ok(), "distinct keys");
    }

    #[test]
    fn tray_icons_error_messages_escape_the_key() {
        // A literal quote stands in for a TOML-escaped control character
        // (ESC, a newline…): `{key:?}` must escape it, `{key}` would print
        // it raw and could inject the character straight into the log line.
        let glyph_err = BarConfig::from_text("[tray.icons]\n'fo\"o' = \"abc\"\n").unwrap_err();
        assert!(glyph_err.contains("fo\\\"o"), "{glyph_err}");
        let case_err = BarConfig::from_text("[tray.icons]\n'Fo\"o' = \"x\"\n'fo\"o' = \"y\"\n").unwrap_err();
        assert!(case_err.contains("fo\\\"o"), "{case_err}");
    }

    #[test]
    fn media_defaults_and_settings() {
        let d = BarConfig::default();
        assert_eq!(d.media, MediaConfig { title_max: 30, ignore: vec!["kdeconnect".to_string()] });
        assert!(d.modules.contains(&ModuleId::Media), "on by default");
        assert_eq!(BarConfig::from_text("height = 22").unwrap().media, MediaConfig::default(), "absent: defaults");
        let c = BarConfig::from_text("[media]\ntitle_max = 50\nignore = [\"firefox\", \"chromium\"]\n").unwrap();
        assert_eq!((c.media.title_max, c.media.ignore), (50, vec!["firefox".to_string(), "chromium".to_string()]));
        let c = BarConfig::from_text("[media]\nignore = []\n").unwrap();
        assert_eq!((c.media.title_max, c.media.ignore), (30, Vec::<String>::new()), "[] ignores only playerctld; title_max kept");
        for bound in [TITLE_MAX_MIN, TITLE_MAX_MAX] {
            let c = BarConfig::from_text(&format!("[media]\ntitle_max = {bound}\n")).unwrap();
            assert_eq!(c.media.title_max, bound, "bounds included");
        }
    }

    #[test]
    fn media_refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        assert!(err("[media]\ntitle_max = 7").contains("media.title_max 7 out of range 8 to 80"), "{}", err("[media]\ntitle_max = 7"));
        assert!(err("[media]\ntitle_max = 81").contains("media.title_max 81"));
        assert!(BarConfig::from_text("[media]\ntitle_max = -1").is_err(), "not a u16");
        assert!(BarConfig::from_text("[media]\ntitle_max = 70000").is_err(), "not a u16");
        assert!(BarConfig::from_text("[media]\nignore = \"kdeconnect\"").is_err(), "a list, not a string");
        assert!(err("[media]\nscroll = true").contains("scroll"), "unknown key refused");
        let (c, w) = BarConfig::load(&file("media", "[media]\ntitle_max = 5\n"));
        assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
        assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("media.title_max")));
    }

    #[test]
    fn tray_icon_key_with_a_control_character_is_an_error() {
        let e = BarConfig::from_text("[tray.icons]\n\"a\\u0007b\" = \"x\"\n").unwrap_err();
        assert!(e.contains("tray.icons.") && e.contains("control character"), "{e}");
        assert!(BarConfig::from_text("[tray.icons]\ndiscord = \"x\"\n").is_ok(), "an ordinary key still passes");
    }
}
