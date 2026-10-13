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
//! [notifications]                                           # the bubbles and the bell (with "notifications" in modules)
//! timeout = 5                                               # seconds a bubble stays when the app leaves it to us
//! timeout_min = 2                                           # an app's own timeout is clamped to timeout_min..timeout_max
//! timeout_max = 20                                          # 1 ≤ timeout_min ≤ timeout ≤ timeout_max ≤ 600
//! max_visible = 4                                           # bubbles at once, 1 to 8 (newest on top)
//! width = 40                                                # bubble width in cells, 24 to 80
//! history = 50                                              # entries kept for the panel, 0 to 200 (0: none)
//! [osd]                                                     # the volume, microphone and brightness pill (with "osd" in modules)
//! timeout_ms = 1300                                         # how long it stays after the last change, 600 to 5000
//! width = 32                                                # cells of the pill's row, 24 to 60
//! margin = 48                                               # logical px above the bottom edge, 0 to 400
//! backlight = ""                                            # a device in backlight_dir; "": the first one, in sorted order
//! backlight_dir = "/sys/class/backlight"                    # where backlight devices are listed; read only on `alpheus-launch --osd brightness`
//! [quick]                                                   # the quick settings panel (with "quick" in modules)
//! step = 5                                                  # % per ←/→ or wheel notch on a slider, 1 to 25
//! keep_awake = { toggle = [], state = "" }                  # toggle: argv, run as given; [] hides the row
//! night_light = { toggle = [], state = "" }                 # state: "" (unknown), "file:<path>" (on while it exists), "process:<name>" (on while it runs)
//! lock = ["loginctl", "lock-session"]                       # session buttons and the ⏻ menu's entries, run as given; [] hides one
//! logout = []
//! reboot = ["systemctl", "reboot"]                          # reboot and poweroff ask once more first
//! poweroff = ["systemctl", "poweroff"]
//! wifi_more = ["foot", "-e", "nmtui"]                       # More… in the Wi-Fi detail; absent: launcher.toml's terminal then nmtui; [] hides it
//! ```

use crate::clock;
use crate::grid::text_width;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// The on-screen display (OSD spec): volume, microphone and brightness in a pill at the bottom
    /// of the focused output. No segment in the bar.
    Osd,
    /// The quick settings panel (quick spec): opened by a left click on the volume, network or
    /// Bluetooth module, or `alpheus-launch --quick`. No segment in the bar.
    Quick,
    Clock,
    /// The power button (power spec § 1): ⏻ at the bar's end, a left click opens its menu with
    /// `[quick]`'s lock, logout, reboot and poweroff. No segment when all four are empty.
    Power,
}

impl ModuleId {
    pub const ALL: [ModuleId; 15] = [
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
        ModuleId::Osd,
        ModuleId::Quick,
        ModuleId::Clock,
        ModuleId::Power,
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
            ModuleId::Osd => "osd",
            ModuleId::Quick => "quick",
            ModuleId::Clock => "clock",
            ModuleId::Power => "power",
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

/// The bounds of `[notifications]`.
pub const NOTIFY_TIMEOUT_LIMIT: u32 = 600;
pub const NOTIFY_VISIBLE_MAX: u8 = 8;
pub const NOTIFY_WIDTH_MIN: u16 = 24;
pub const NOTIFY_WIDTH_MAX: u16 = 80;
pub const NOTIFY_HISTORY_MAX: u16 = 200;

/// `[notifications]` (notifications spec § 6): the bubbles' timeouts, count and width, and the
/// history's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// Seconds a bubble stays when the app sends -1.
    pub timeout: u32,
    /// An app's own timeout is clamped to `timeout_min..=timeout_max` seconds; 0 means `timeout_max`.
    pub timeout_min: u32,
    pub timeout_max: u32,
    /// Bubbles shown at once (1 to 8); the others wait.
    pub max_visible: u8,
    /// A bubble's width in cells (24 to 80).
    pub width: u16,
    /// Entries the panel keeps (0 to 200; 0: none).
    pub history: u16,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        NotificationsConfig { timeout: 5, timeout_min: 2, timeout_max: 20, max_visible: 4, width: 40, history: 50 }
    }
}

/// The bounds of `[osd]`.
pub const OSD_TIMEOUT_MIN: u32 = 600;
pub const OSD_TIMEOUT_MAX: u32 = 5000;
pub const OSD_WIDTH_MIN: u16 = 24;
pub const OSD_WIDTH_MAX: u16 = 60;
pub const OSD_MARGIN_MAX: u32 = 400;

/// `[osd]` (OSD spec § 4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OsdConfig {
    /// Milliseconds the pill stays after the last change.
    pub timeout_ms: u32,
    /// Cells of the pill's row.
    pub width: u16,
    /// Logical px between the pill and the output's bottom edge.
    pub margin: u32,
    /// A device name in `backlight_dir`; empty: the first one in sorted order.
    pub backlight: String,
    /// Where the backlight devices are listed. Read only, and only on `alpheus-launch --osd brightness`.
    pub backlight_dir: PathBuf,
}

impl Default for OsdConfig {
    fn default() -> Self {
        OsdConfig {
            timeout_ms: 1300,
            width: 32,
            margin: 48,
            backlight: String::new(),
            backlight_dir: PathBuf::from("/sys/class/backlight"),
        }
    }
}

/// A device name as sysfs lists it: no path, nothing hidden, short.
fn plain_device(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
        && !name.chars().any(char::is_control)
}

/// The largest `[quick] step`, in percent.
pub const QUICK_STEP_MAX: u8 = 25;
/// The longest process name `/proc/<pid>/comm` holds (TASK_COMM_LEN − 1).
pub const COMM_MAX: usize = 15;

/// One configured toggle of the panel (quick spec § 4): the command its row runs, and where its
/// state is read from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToggleConfig {
    /// Run as given, never through a shell; empty: the row is hidden.
    pub toggle: Vec<String>,
    /// "", "file:<path>" or "process:<name>" (`state_form`).
    pub state: String,
}

/// `[quick]` (quick spec § 4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuickConfig {
    /// Percent per ←/→ or wheel notch on a slider, 1 to 25.
    pub step: u8,
    pub keep_awake: ToggleConfig,
    pub night_light: ToggleConfig,
    /// The session buttons' commands, run as given; an empty one hides its button. Reboot and power
    /// off ask once more first. Nothing here handles sleep or screen-off.
    pub lock: Vec<String>,
    pub logout: Vec<String>,
    pub reboot: Vec<String>,
    pub poweroff: Vec<String>,
    /// The Wi-Fi detail's `More…` (quick Wi-Fi spec § 4), run as given: enterprise Wi-Fi, hidden
    /// networks, VPN, hotspot. None: absent from bar.toml, resolved at load time
    /// (`resolve_wifi_more`); an empty list hides the row.
    pub wifi_more: Option<Vec<String>>,
}

impl Default for QuickConfig {
    /// Neutral defaults (spec § 4): the session's lock through logind (hypridle or swayidle run the
    /// lock command), reboot and power off through systemd, no logout and no toggle.
    fn default() -> Self {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        QuickConfig {
            step: 5,
            keep_awake: ToggleConfig::default(),
            night_light: ToggleConfig::default(),
            lock: argv(&["loginctl", "lock-session"]),
            logout: Vec::new(),
            reboot: argv(&["systemctl", "reboot"]),
            poweroff: argv(&["systemctl", "poweroff"]),
            wifi_more: None,
        }
    }
}

/// `[quick] wifi_more` absent from bar.toml: the launcher's terminal (launcher.toml's `terminal`),
/// then "nmtui" (quick Wi-Fi spec § 4). Given (even empty): kept as is.
pub fn resolve_wifi_more(q: &mut QuickConfig, terminal: &[String]) {
    if q.wifi_more.is_none() {
        q.wifi_more = Some(terminal.iter().cloned().chain(std::iter::once("nmtui".to_string())).collect());
    }
}

/// A toggle's `state`, read (quick spec § 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateForm {
    /// "": the row shows `·`.
    Unknown,
    /// "file:<path>": on while the file exists. The path starts with `/`, `~/` or
    /// `$XDG_RUNTIME_DIR/`, the only two expansions (`quick_state::expand`).
    File(String),
    /// "process:<name>": on while a process with that `comm` runs.
    Process(String),
}

/// `state` as written in bar.toml; Err: why it is refused (without the key, which the caller adds).
pub fn state_form(s: &str) -> Result<StateForm, String> {
    if s.is_empty() {
        return Ok(StateForm::Unknown);
    }
    if let Some(path) = s.strip_prefix("file:") {
        let anchored = path.starts_with('/') || path.starts_with("~/") || path.starts_with("$XDG_RUNTIME_DIR/");
        if !anchored {
            return Err(format!("the file must be an absolute path (\"/…\", \"~/…\" or \"$XDG_RUNTIME_DIR/…\"), not {path:?}"));
        }
        if path.chars().any(char::is_control) {
            return Err(format!("the file path holds a control character: {path:?}"));
        }
        return Ok(StateForm::File(path.to_string()));
    }
    if let Some(name) = s.strip_prefix("process:") {
        if name.is_empty() || name.len() > COMM_MAX {
            return Err(format!("a process name is 1 to {COMM_MAX} bytes (as /proc/<pid>/comm keeps it), not {name:?}"));
        }
        if name.contains('/') || name.chars().any(char::is_control) {
            return Err(format!("a process name has no '/' nor control character: {name:?}"));
        }
        return Ok(StateForm::Process(name.to_string()));
    }
    Err(format!("expected \"\", \"file:<path>\" or \"process:<name>\", not {s:?}"))
}

/// `[quick]` checked: `step` in range; every non-empty command with a non-blank program and no NUL; a toggle's
/// `state` only with its `toggle`, and in one of the three forms.
fn check_quick(q: &QuickConfig) -> Result<(), String> {
    if !(1..=QUICK_STEP_MAX).contains(&q.step) {
        return Err(format!("quick.step {} out of range 1 to {QUICK_STEP_MAX}", q.step));
    }
    let no_more: &[String] = &[];
    for (key, command) in [
        ("quick.lock", q.lock.as_slice()),
        ("quick.logout", q.logout.as_slice()),
        ("quick.reboot", q.reboot.as_slice()),
        ("quick.poweroff", q.poweroff.as_slice()),
        ("quick.keep_awake.toggle", q.keep_awake.toggle.as_slice()),
        ("quick.night_light.toggle", q.night_light.toggle.as_slice()),
        ("quick.wifi_more", q.wifi_more.as_deref().unwrap_or(no_more)),
    ] {
        if command.first().is_some_and(|p| p.trim().is_empty()) {
            return Err(format!("{key}: the program is blank"));
        }
        // spawn refuses an argument with a NUL in it: refused here, with the reason, never at a click.
        if command.iter().any(|a| a.contains('\0')) {
            return Err(format!("{key}: an argument holds a NUL"));
        }
    }
    for (key, t) in [("quick.keep_awake", &q.keep_awake), ("quick.night_light", &q.night_light)] {
        if t.toggle.is_empty() && !t.state.is_empty() {
            return Err(format!("{key}.state without a toggle (the row shows only with one)"));
        }
        state_form(&t.state).map_err(|m| format!("{key}.state: {m}"))?;
    }
    Ok(())
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
    pub notifications: NotificationsConfig,
    pub osd: OsdConfig,
    pub quick: QuickConfig,
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
            notifications: NotificationsConfig::default(),
            osd: OsdConfig::default(),
            quick: QuickConfig::default(),
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
    #[serde(default)]
    notifications: NotificationsConfig,
    #[serde(default)]
    osd: OsdConfig,
    #[serde(default)]
    quick: QuickConfig,
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
        let n = b.notifications;
        if !((1..=n.timeout).contains(&n.timeout_min) && (n.timeout..=NOTIFY_TIMEOUT_LIMIT).contains(&n.timeout_max)) {
            return Err(format!(
                "notifications: need 1 ≤ timeout_min ≤ timeout ≤ timeout_max ≤ {NOTIFY_TIMEOUT_LIMIT} s (got {}, {}, {})",
                n.timeout_min, n.timeout, n.timeout_max
            ));
        }
        if !(1..=NOTIFY_VISIBLE_MAX).contains(&n.max_visible) {
            return Err(format!("notifications.max_visible {} out of range 1 to {NOTIFY_VISIBLE_MAX}", n.max_visible));
        }
        if !(NOTIFY_WIDTH_MIN..=NOTIFY_WIDTH_MAX).contains(&n.width) {
            return Err(format!("notifications.width {} out of range {NOTIFY_WIDTH_MIN} to {NOTIFY_WIDTH_MAX} cells", n.width));
        }
        if n.history > NOTIFY_HISTORY_MAX {
            return Err(format!("notifications.history {} out of range 0 to {NOTIFY_HISTORY_MAX}", n.history));
        }
        let o = &b.osd;
        if !(OSD_TIMEOUT_MIN..=OSD_TIMEOUT_MAX).contains(&o.timeout_ms) {
            return Err(format!("osd.timeout_ms {} out of range {OSD_TIMEOUT_MIN} to {OSD_TIMEOUT_MAX} ms", o.timeout_ms));
        }
        if !(OSD_WIDTH_MIN..=OSD_WIDTH_MAX).contains(&o.width) {
            return Err(format!("osd.width {} out of range {OSD_WIDTH_MIN} to {OSD_WIDTH_MAX} cells", o.width));
        }
        if o.margin > OSD_MARGIN_MAX {
            return Err(format!("osd.margin {} out of range 0 to {OSD_MARGIN_MAX} px", o.margin));
        }
        if !o.backlight.is_empty() && !plain_device(&o.backlight) {
            return Err(format!("osd.backlight must be a device name (as listed in backlight_dir), not {:?}", o.backlight));
        }
        if !o.backlight_dir.is_absolute() {
            return Err("osd.backlight_dir must be an absolute path".into());
        }
        check_quick(&b.quick)?;
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
            notifications: b.notifications,
            osd: b.osd,
            quick: b.quick,
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
        assert_eq!((d.modules.first(), d.modules.last()), (Some(&ModuleId::Launcher), Some(&ModuleId::Power)));
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

    /// Power spec § 1: `power` parses, is the last of `ALL`, and the default order ends with the
    /// clock then the power button.
    #[test]
    fn the_power_module_parses_and_ends_the_default_order() {
        assert_eq!(ModuleId::Power.name(), "power");
        assert_eq!(ModuleId::ALL.last(), Some(&ModuleId::Power));
        let c = BarConfig::from_text("modules = [\"clock\", \"power\"]").unwrap();
        assert_eq!(c.modules, vec![ModuleId::Clock, ModuleId::Power]);
        let d = BarConfig::default();
        assert_eq!(d.modules[d.modules.len() - 2..], [ModuleId::Clock, ModuleId::Power]);
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
    fn notifications_defaults_and_settings() {
        let d = BarConfig::default();
        assert_eq!(
            d.notifications,
            NotificationsConfig { timeout: 5, timeout_min: 2, timeout_max: 20, max_visible: 4, width: 40, history: 50 }
        );
        assert!(d.modules.contains(&ModuleId::Notifications), "on by default");
        assert_eq!(BarConfig::from_text("height = 22").unwrap().notifications, NotificationsConfig::default(), "absent: defaults");
        let c = BarConfig::from_text("[notifications]\ntimeout = 8\ntimeout_max = 30\nmax_visible = 2\nwidth = 60\nhistory = 0\n").unwrap();
        assert_eq!(
            c.notifications,
            NotificationsConfig { timeout: 8, timeout_min: 2, timeout_max: 30, max_visible: 2, width: 60, history: 0 }
        );
        for t in [
            "[notifications]\ntimeout_min = 1\ntimeout = 1\ntimeout_max = 1",
            "[notifications]\ntimeout_max = 600",
            "[notifications]\nmax_visible = 1",
            "[notifications]\nmax_visible = 8",
            "[notifications]\nwidth = 24",
            "[notifications]\nwidth = 80",
            "[notifications]\nhistory = 200",
        ] {
            assert!(BarConfig::from_text(t).is_ok(), "{t}: bounds included");
        }
    }

    #[test]
    fn notifications_refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        let order = "notifications: need 1 ≤ timeout_min ≤ timeout ≤ timeout_max ≤ 600 s";
        for (t, got) in [
            ("[notifications]\ntimeout_min = 0", "(got 0, 5, 20)"),
            ("[notifications]\ntimeout = 1", "(got 2, 1, 20)"),
            ("[notifications]\ntimeout = 30", "(got 2, 30, 20)"),
            ("[notifications]\ntimeout_max = 601", "(got 2, 5, 601)"),
        ] {
            let e = err(t);
            assert!(e.contains(order) && e.contains(got), "{t}: {e}");
        }
        for (t, want) in [
            ("[notifications]\nmax_visible = 0", "notifications.max_visible 0 out of range 1 to 8"),
            ("[notifications]\nmax_visible = 9", "notifications.max_visible 9"),
            ("[notifications]\nwidth = 23", "notifications.width 23 out of range 24 to 80 cells"),
            ("[notifications]\nwidth = 81", "notifications.width 81"),
            ("[notifications]\nhistory = 201", "notifications.history 201 out of range 0 to 200"),
        ] {
            let e = err(t);
            assert!(e.contains(want), "{t}: {e}");
        }
        assert!(BarConfig::from_text("[notifications]\nhistory = -1").is_err(), "not a u16");
        assert!(BarConfig::from_text("[notifications]\nmax_visible = 256").is_err(), "not a u8");
        assert!(err("[notifications]\nsound = true").contains("sound"), "unknown key refused");
        let (c, w) = BarConfig::load(&file("notifications", "[notifications]\nwidth = 5\n"));
        assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
        assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("notifications.width")));
    }

    #[test]
    fn tray_icon_key_with_a_control_character_is_an_error() {
        let e = BarConfig::from_text("[tray.icons]\n\"a\\u0007b\" = \"x\"\n").unwrap_err();
        assert!(e.contains("tray.icons.") && e.contains("control character"), "{e}");
        assert!(BarConfig::from_text("[tray.icons]\ndiscord = \"x\"\n").is_ok(), "an ordinary key still passes");
    }

    #[test]
    fn osd_defaults_and_settings() {
        let d = BarConfig::default();
        assert_eq!(
            d.osd,
            OsdConfig {
                timeout_ms: 1300,
                width: 32,
                margin: 48,
                backlight: String::new(),
                backlight_dir: PathBuf::from("/sys/class/backlight")
            }
        );
        assert!(d.modules.contains(&ModuleId::Osd), "on by default");
        assert_eq!(d.modules[d.modules.len() - 2..], [ModuleId::Clock, ModuleId::Power], "the clock, then the power button, last");
        assert_eq!(ModuleId::Osd.name(), "osd");
        let c = BarConfig::from_text(
            "[osd]\ntimeout_ms = 2000\nwidth = 40\nmargin = 0\nbacklight = \"intel_backlight\"\nbacklight_dir = \"/tmp/fake\"\n",
        )
        .unwrap();
        assert_eq!(
            c.osd,
            OsdConfig {
                timeout_ms: 2000,
                width: 40,
                margin: 0,
                backlight: "intel_backlight".into(),
                backlight_dir: PathBuf::from("/tmp/fake")
            }
        );
        for t in ["[osd]\ntimeout_ms = 600", "[osd]\ntimeout_ms = 5000", "[osd]\nwidth = 24", "[osd]\nwidth = 60", "[osd]\nmargin = 400"] {
            assert!(BarConfig::from_text(t).is_ok(), "{t}: bounds included");
        }
    }

    #[test]
    fn osd_refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        for (t, want) in [
            ("[osd]\ntimeout_ms = 599", "osd.timeout_ms 599 out of range 600 to 5000 ms"),
            ("[osd]\ntimeout_ms = 5001", "osd.timeout_ms 5001"),
            ("[osd]\nwidth = 23", "osd.width 23 out of range 24 to 60 cells"),
            ("[osd]\nwidth = 61", "osd.width 61"),
            ("[osd]\nmargin = 401", "osd.margin 401 out of range 0 to 400 px"),
            ("[osd]\nbacklight = \"../intel_backlight\"", "osd.backlight"),
            ("[osd]\nbacklight = \"..\"", "osd.backlight"),
            ("[osd]\nbacklight = \"a\\u0007b\"", "osd.backlight"),
            ("[osd]\nbacklight_dir = \"sys/class/backlight\"", "osd.backlight_dir must be an absolute path"),
        ] {
            let e = err(t);
            assert!(e.contains(want), "{t}: {e}");
        }
        assert!(err("[osd]\nfade = true").contains("fade"), "unknown key refused");
        let (c, w) = BarConfig::load(&file("osd", "[osd]\nwidth = 5\n"));
        assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
        assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("osd.width")));
    }

    /// Quick Wi-Fi spec § 4: `wifi_more` as given; absent, resolved at load time from the launcher's
    /// terminal; an explicit empty list hides `More…` and is never replaced.
    #[test]
    fn wifi_more_is_read_checked_and_resolved() {
        assert_eq!(QuickConfig::default().wifi_more, None, "absent: resolved at load time");
        let c = BarConfig::from_text("[quick]\nwifi_more = [\"foot\", \"-e\", \"nmtui\"]\n").unwrap();
        assert_eq!(c.quick.wifi_more, Some(vec!["foot".to_string(), "-e".into(), "nmtui".into()]));
        let hidden = BarConfig::from_text("[quick]\nwifi_more = []\n").unwrap();
        assert_eq!(hidden.quick.wifi_more, Some(vec![]), "an explicit empty list");
        let mut q = QuickConfig::default();
        resolve_wifi_more(&mut q, &["alacritty".to_string(), "-e".into()]);
        assert_eq!(q.wifi_more, Some(vec!["alacritty".to_string(), "-e".into(), "nmtui".into()]));
        let mut kept = hidden.quick.clone();
        resolve_wifi_more(&mut kept, &["foot".to_string()]);
        assert_eq!(kept.wifi_more, Some(vec![]), "given: never replaced");
        for (t, want) in [
            ("[quick]\nwifi_more = [\" \"]", "quick.wifi_more: the program is blank"),
            ("[quick]\nwifi_more = [\"foot\", \"a\\u0000b\"]", "quick.wifi_more: an argument holds a NUL"),
        ] {
            let e = BarConfig::from_text(t).unwrap_err();
            assert!(e.contains(want), "{t}: {e}");
        }
        assert!(BarConfig::from_text("[quick]\nwifi_mor = []").unwrap_err().contains("wifi_mor"), "unknown key refused");
    }

    #[test]
    fn quick_defaults_and_settings() {
        let d = BarConfig::default();
        assert!(d.modules.contains(&ModuleId::Quick), "on by default");
        assert_eq!(d.modules[d.modules.len() - 2..], [ModuleId::Clock, ModuleId::Power], "the clock, then the power button, last");
        assert_eq!(ModuleId::Quick.name(), "quick");
        let q = &d.quick;
        assert_eq!(q.step, 5);
        assert_eq!((&q.keep_awake, &q.night_light), (&ToggleConfig::default(), &ToggleConfig::default()), "no toggle: rows hidden");
        assert_eq!(q.lock, ["loginctl", "lock-session"]);
        assert!(q.logout.is_empty(), "no neutral logout: the button is hidden");
        assert_eq!(q.reboot, ["systemctl", "reboot"]);
        assert_eq!(q.poweroff, ["systemctl", "poweroff"]);
        assert_eq!(BarConfig::from_text("height = 22").unwrap().quick, QuickConfig::default(), "absent: defaults");
        let c = BarConfig::from_text(
            "[quick]\nstep = 10\n\
             keep_awake = { toggle = [\"tools-menu\", \"Système · Garder éveillé\"], state = \"file:$XDG_RUNTIME_DIR/garder-eveille\" }\n\
             night_light = { toggle = [\"fake-night\"], state = \"process:hyprsunset\" }\n\
             logout = [\"wm-set\", \"exit\"]\nlock = []\n",
        )
        .unwrap();
        assert_eq!(c.quick.step, 10);
        assert_eq!(c.quick.keep_awake.toggle, ["tools-menu", "Système · Garder éveillé"]);
        assert_eq!(c.quick.keep_awake.state, "file:$XDG_RUNTIME_DIR/garder-eveille");
        assert_eq!(
            (c.quick.night_light.toggle.as_slice(), c.quick.night_light.state.as_str()),
            (["fake-night".to_string()].as_slice(), "process:hyprsunset")
        );
        assert_eq!(c.quick.logout, ["wm-set", "exit"]);
        assert!(c.quick.lock.is_empty(), "an empty command hides its button");
        assert_eq!(c.quick.reboot, ["systemctl", "reboot"], "the others keep their defaults");
        for t in ["[quick]\nstep = 1", "[quick]\nstep = 25"] {
            assert!(BarConfig::from_text(t).is_ok(), "{t}: bounds included");
        }
    }

    #[test]
    fn quick_refused_with_reason() {
        let err = |t: &str| BarConfig::from_text(t).unwrap_err();
        for (t, want) in [
            ("[quick]\nstep = 0", "quick.step 0 out of range 1 to 25"),
            ("[quick]\nstep = 26", "quick.step 26"),
            ("[quick]\nlock = [\" \"]", "quick.lock: the program is blank"),
            ("[quick]\nkeep_awake = { toggle = [\"\"] }", "quick.keep_awake.toggle: the program is blank"),
            ("[quick]\nkeep_awake = { state = \"file:/run/x\" }", "quick.keep_awake.state without a toggle"),
            (
                "[quick]\nnight_light = { toggle = [\"x\"], state = \"pid:12\" }",
                "quick.night_light.state: expected \"\", \"file:<path>\" or \"process:<name>\"",
            ),
            (
                "[quick]\nnight_light = { toggle = [\"x\"], state = \"file:relative/x\" }",
                "quick.night_light.state: the file must be an absolute path",
            ),
            ("[quick]\nnight_light = { toggle = [\"x\"], state = \"file:\" }", "the file must be an absolute path"),
            (
                "[quick]\nnight_light = { toggle = [\"x\"], state = \"process:\" }",
                "quick.night_light.state: a process name is 1 to 15 bytes",
            ),
            ("[quick]\nnight_light = { toggle = [\"x\"], state = \"process:a/b\" }", "quick.night_light.state: a process name has no '/'"),
            (
                "[quick]\nnight_light = { toggle = [\"x\"], state = \"file:/run/a\\nb\" }",
                "quick.night_light.state: the file path holds a control character: \"/run/a\\nb\"",
            ),
            ("[quick]\nlogout = [\"fake-logout\", \"a\\u0000b\"]", "quick.logout: an argument holds a NUL"),
            ("[quick]\nkeep_awake = { toggle = [\"x\\u0000\"] }", "quick.keep_awake.toggle: an argument holds a NUL"),
        ] {
            let e = err(t);
            assert!(e.contains(want), "{t}: {e}");
        }
        assert!(BarConfig::from_text("[quick]\nstep = 300").is_err(), "not a u8");
        assert!(err("[quick]\nsuspend = []").contains("suspend"), "unknown key refused: nothing sleeps from here");
        assert!(err("[quick]\nkeep_awake = { toggle = [\"x\"], every = 2 }").contains("every"), "unknown key refused");
        let (c, w) = BarConfig::load(&file("quick", "[quick]\nstep = 0\n"));
        assert_eq!(c, BarConfig::default(), "defaults, as for any invalid bar.toml");
        assert!(w.is_some_and(|w| w.contains("bar.toml invalid") && w.contains("quick.step")));
    }

    /// Review Focus 4: the kernel keeps 15 bytes of a process name (`/proc/<pid>/comm`):
    /// "gammastep-indicator" reads "gammastep-indic" there, so a longer name could never match.
    #[test]
    fn a_process_state_longer_than_comm_is_refused() {
        let state = |name: &str| format!("[quick]\nnight_light = {{ toggle = [\"x\"], state = \"process:{name}\" }}\n");
        let e = BarConfig::from_text(&state("gammastep-indicator")).unwrap_err();
        assert!(e.contains("1 to 15 bytes") && e.contains("gammastep-indicator"), "{e}");
        assert!(BarConfig::from_text(&state("abcdefghijklmnop")).is_err(), "16 bytes");
        assert!(BarConfig::from_text(&state("gammastep-indic")).is_ok(), "15 bytes");
    }

    #[test]
    fn state_forms_are_read() {
        assert_eq!(state_form(""), Ok(StateForm::Unknown));
        assert_eq!(state_form("file:/run/user/1000/flag"), Ok(StateForm::File("/run/user/1000/flag".into())));
        assert_eq!(state_form("file:~/.cache/flag"), Ok(StateForm::File("~/.cache/flag".into())));
        assert_eq!(state_form("file:$XDG_RUNTIME_DIR/garder-eveille"), Ok(StateForm::File("$XDG_RUNTIME_DIR/garder-eveille".into())));
        assert_eq!(state_form("process:hyprsunset"), Ok(StateForm::Process("hyprsunset".into())));
        assert!(state_form("file:$HOME/x").is_err(), "only $XDG_RUNTIME_DIR and ~ are expanded");
        assert!(state_form("File:/x").is_err(), "the prefixes are exact");
        assert!(state_form("process:a\u{7}").is_err(), "no control character");
    }
}
