//! ~/.config/alpheus/bar.toml, optional. Absent: defaults, no message.
//! Invalid: defaults and a warning (daemon log). Never an abort. Each
//! setting is read once at startup and costs nothing if unused.
//!
//! ```toml
//! modules = ["workspaces", "window", "clock"]   # order and presence, left to right
//! height = 22                                   # logical px
//! [font]
//! family = "JetBrainsMono Nerd Font Mono"       # absent: a monospace font from the system
//! size = 14.0
//! fallback = []                                 # e.g. ["Noto Color Emoji"]: loaded in addition, on demand
//! [clock]
//! format = "%H:%M"
//! date = "%a %e %b"                             # "": no date
//! [workspaces]
//! command = ["wm-set", "workspace", "{workspace}", "{output}"]
//! [actions]                                     # clicks on the system modules
//! open_tab = ["alpheus-toggle", "{tab}"]        # {tab}: home, sound, network, bluetooth
//! resources = ["alacritty", "-e", "btop"]
//! [resources]
//! interval = 2                                  # seconds between readings, 1 to 60
//! graph = 20                                    # cells of CPU history, 0 to 60 (0: no graph)
//! sensor = ""                                   # hwmon name or thermal zone type; "": automatic
//! ```

use crate::clock;
use serde::Deserialize;
use std::path::Path;

/// The bar's modules (spec, § 2), left to right by default. All are
/// accepted from the start: a bar.toml written today stays valid once
/// deliveries 3 to 5 arrive. Those not yet delivered simply do not show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
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
    pub const ALL: [ModuleId; 11] = [
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
    /// Fallback families for missing characters (emoji, CJK…). Empty by
    /// default: only the regular and bold faces of `family` are loaded
    /// (≈ 2 MB); each added family costs its own memory (Q11).
    pub fallback: Vec<String>,
}

impl Default for FontConfig {
    fn default() -> Self {
        FontConfig { family: "JetBrainsMono Nerd Font Mono".into(), size: 14.0, fallback: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClockConfig {
    /// The time, in bold accent.
    pub format: String,
    /// The date, muted, before the time; empty = no date.
    pub date: String,
}

impl Default for ClockConfig {
    fn default() -> Self {
        ClockConfig { format: "%H:%M".into(), date: "%a %e %b".into() }
    }
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
    /// alacritty (Mae's terminal; portability step, as above).
    pub resources: Vec<String>,
}

impl Default for ActionsConfig {
    fn default() -> Self {
        ActionsConfig {
            open_tab: ["alpheus-toggle", "{tab}"].map(String::from).to_vec(),
            resources: ["alacritty", "-e", "btop"].map(String::from).to_vec(),
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
        Ok(BarConfig {
            modules,
            height: b.height,
            font: b.font,
            clock: b.clock,
            workspaces: b.workspaces,
            actions: b.actions,
            resources: b.resources,
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
        assert_eq!((d.modules.first(), d.modules.last()), (Some(&ModuleId::Workspaces), Some(&ModuleId::Clock)));
        assert_eq!((d.height, d.font.size, d.font.fallback.len()), (22, 14.0, 0));
        assert_eq!(d.workspaces.command, ["wm-set", "workspace", "{workspace}", "{output}"]);
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
        assert_eq!(d.actions.resources, ["alacritty", "-e", "btop"]);
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
}
