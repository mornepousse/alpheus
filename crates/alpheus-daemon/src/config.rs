//! `bar.toml`: every setting of the daemon, read once at start.
//!
//! The file is optional. Absent, the defaults apply with no message; invalid, the defaults apply
//! with one warning in the log, and never an abort. A setting costs nothing while the module that
//! reads it is not in `modules`. The example below is the whole file, with each key's default and
//! what it does; docs/configuration.md is the same thing as a page, with the bounds.
//!
//! ```toml
//! modules = ["launcher", "workspaces", "window", "timer", "clock"]   # order and presence, left to right; absent: every module in `ModuleId::ALL`'s order ("timer" just before "clock")
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
//! open_tab = ["alpheus-toggle", "{tab}"]                    # {tab}: home, sound, network, bluetooth, diagnostics
//! resources = ["foot", "-e", "btop"]                        # right click on the resources, and the popup's Monitor…
//! [resources]
//! interval = 2                                              # seconds between readings, 1 to 60
//! graph = 20                                                # accepted, 0 to 60, and ignored (the sparkline is gone)
//! sensor = ""                                               # hwmon name or thermal zone type; "": automatic
//! show = ["cpu", "memory", "temperature"]   # in this order; also "net", "disk" ($HOME's filesystem), "gpu"
//! gpu_sensor = ""   # GPU hwmon name (amdgpu, nouveau, i915, xe, nvidia); "": automatic
//! [battery]
//! peripherals = "always"                                    # "always" | "low" | "never": peripherals with a real percentage
//! coarse = "low"                                            # "always" | "low" | "never": peripherals with only a coarse level
//! warn = [20, 10, 5]  # notify when the laptop battery, discharging, reaches each %; the lowest is critical (a single one too); [] turns it off; read at start, no hot reload (needs "notifications")
//! [tray]
//! passive = "dim"                                           # "dim" | "show" | "hide": items that say they are idle
//! tooltips = true                                           # the item's name after ½ s of hovering
//! [tray.icons]                                              # glyph overrides, keyed like the built-in table
//! keepassxc = "󰌋"
//! [media]
//! title_max = 30                                            # cells of the title in the bar, 8 to 80
//! ignore = ["kdeconnect"]                                   # players never shown: substrings of their bus name (playerctld always is)
//! [privacy]                                                 # the microphone, camera and screen indicators (with "privacy" in modules)
//! ignore = []                                               # apps never shown: names compared whole, case-insensitively
//! dev_dir = "/dev"                                          # where video devices are listed (watched, never opened)
//! proc_dir = "/proc"                                        # where processes are listed (a camera opened outside PipeWire)
//! [failed]                                                  # the failed-units count (systemd's)
//! user_units = true                                         # also count the user manager's failed units (the session bus)
//! subscribe = false                                         # true: Subscribe() to systemd so that a change arrives at once (costs systemd and dbus-daemon a signal per unit change); false: read on triggers only
//! notify = false                                            # a notification when the count rises
//! [notifications]                                           # the bubbles and the bell (with "notifications" in modules)
//! timeout = 5                                               # seconds a bubble stays when the app leaves it to us
//! timeout_min = 2                                           # an app's own timeout is clamped to timeout_min..timeout_max
//! timeout_max = 20                                          # 1 ≤ timeout_min ≤ timeout ≤ timeout_max ≤ 600
//! max_visible = 4                                           # bubbles at once, 1 to 8 (newest on top)
//! width = 40                                                # bubble width in cells, 24 to 80
//! history = 50                                              # entries kept for the panel, 0 to 200 (0: none)
//! dnd_while_sharing = false  # do not disturb while the screen is shared (needs "privacy" and "notifications" in modules); a change made meanwhile wins
//! [[notifications.rules]]  # per app, first match wins, at most 32
//! app = "Chatty"  # the whole app name, any case
//! action = "mute"  # "mute": no bubble, kept and counted; "drop": ignored
//! urgency = "low"  # "low" | "normal" | "critical": replaces the app's (not with "drop")
//! [osd]                                                     # the volume, microphone and brightness pill (with "osd" in modules)
//! timeout_ms = 1300                                         # how long it stays after the last change, 600 to 5000
//! width = 32                                                # cells of the pill's row, 24 to 60
//! margin = 48                                               # logical px above the bottom edge, 0 to 400
//! backlight = ""                                            # a device in backlight_dir; "": the first one, in sorted order
//! backlight_dir = "/sys/class/backlight"                    # where backlight devices are listed; read on `alpheus-launch --osd brightness` and for the quick panel's brightness row
//! [quick]                                                   # the quick settings panel (with "quick" in modules)
//! step = 5                                                  # % per ←/→ or wheel notch on a slider, 1 to 25
//! keep_awake = { toggle = [], state = "" }                  # toggle: argv, run as given; [] hides the row
//! night_light = { toggle = [], state = "" }                 # state: "" (unknown), "file:<path>" (on while it exists), "process:<name>" (on while it runs)
//! lock = ["loginctl", "lock-session"]                       # session buttons and the ⏻ menu's entries, run as given; [] hides one
//! logout = []
//! reboot = ["systemctl", "reboot"]                          # reboot and poweroff ask once more first
//! poweroff = ["systemctl", "poweroff"]
//! wifi_more = ["foot", "-e", "nmtui"]                       # More… in the Wi-Fi detail; absent: launcher.toml's terminal then nmtui; [] hides it
//! vpn_more = ["foot", "-e", "nmtui-edit"]                   # More… in the VPN detail; absent: launcher.toml's terminal, then nmtui-edit; [] hides it
//! mixer = ["foot", "-e", "wiremix"]                        # Mixer… in the panel; absent: launcher.toml's terminal then wiremix; [] hides it
//! dnd_until = "08:00"                                       # the do-not-disturb detail's "Until" entry, HH:MM
//! [timer]                                                   # the focus timer (with "timer" in modules); read at start
//! countdown = 10                                            # minutes, the popup's Timer row at start, 1 to 180
//! work = 25                                                 # Pomodoro, minutes, 1 to 180
//! short_break = 5                                           # minutes, 1 to 180
//! long_break = 15                                           # minutes, 1 to 180
//! rounds = 4                                                # work sessions before the long break, 1 to 12
//! sound = []                                                # argv run at each end (not under do-not-disturb); [] none
//! idle = "glyph"                                            # "glyph" | "hidden": the chip when nothing runs
//! [displays]                                                # the screens through the output protocol (with "displays" in modules)
//! auto = true                                               # apply the displays.toml profile matching the connected screens
//! drm_dir = "/sys/class/drm"                                # where DRM connectors are listed (DisplayLink detection; read, never watched)
//! [clipboard]                                               # the text clipboard history (read at start)
//! enabled = false                                           # true: follow the clipboard (data-control), 50 entries in memory, never on disk
//! [shield]                                                  # Shield mode for public networks (none set: no Shield anywhere); read at start
//! flag = "/var/lib/shield/on"                               # absolute; the mode is on while this file exists (not in alpheus's config directory)
//! on = ["pkexec", "/usr/local/bin/shield-root", "on"]    # argv, run as given; their program asks for authorisation itself
//! off = ["pkexec", "/usr/local/bin/shield-root", "off"]
//! allow = ["pkexec", "/usr/local/bin/shield-root", "allow", "{id}"]   # {id}: one whole element, once; absent: the blocked USB devices are listed only
//! # blocked = ["usbguard", "list-devices", "--blocked"]     # the default; [] turns the device list off
//! # on_text = "Protections for public networks are on."     # the notices' bodies, no control or format character, at most 256 bytes
//! # off_text = "Back to the usual settings."
//! [hooks]                                                   # a command run on an event, its data in ALPHEUS_* variables (none set: nothing runs)
//! started = ["notify-send", "alpheus up"]                   # argv, run as given (no ~ or $VAR expansion), detached; [] or absent: no hook
//! theme_mode = []                                           # also dnd, battery_charge, battery_level, power_profile, network, vpn, bluetooth, privacy, shield, failed_units, displays
//! ```
//!
//! Never:
//! - abort on a bad file: an unknown key or a value out of bounds makes the whole file fall back to
//!   the defaults, with one line in the log naming the key; the daemon always starts;
//! - read the file again: `bar.toml` is read once at start, so a change needs a restart (the theme
//!   files and `launcher.toml` are the two that reload, each in its own place);
//! - cost anything for a module that is not in `modules`: no source of its own is started
//!   (`startup::wanted`), and the memory suite measures each one.
//!
//! See: docs/configuration.md (every key, its default and its bounds, written from this file),
//! docs/features/*.md ("Configuration" of each feature), docs/cli.md (when each file is read).

use crate::clock;
use crate::grid::text_width;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The bar's modules (spec § 2), left to right by default: `[bar] modules` both orders the bar and
/// says which daemon features run at all, so the OSD, the quick panel and the screens are named
/// here although they draw no segment. An unknown name is an error when the file is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    /// The launcher's button (opens the launcher on a left click).
    Launcher,
    Workspaces,
    Window,
    Media,
    /// The privacy indicators (privacy spec § 1): who holds the microphone, the camera or the screen.
    Privacy,
    /// Shield mode (shield spec § 4): a green glyph drawn only while the mode is on; its row and
    /// detail live in the quick panel.
    Shield,
    /// The failed-units count (diagnostics spec § 6): `󰀦 N` when a unit of the system or the user
    /// manager is in the failed state; nothing at 0. A left click opens the control center's Diag tab.
    Failed,
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
    /// The screens (displays spec § 1): read and changed through the output management protocol, the
    /// matching profile applied on hotplug. No segment in the bar.
    Displays,
    /// The focus timer (timer spec): a chip with a countdown, a Pomodoro or a stopwatch, a popup
    /// under it and `alpheus-launch --timer`. One dim glyph at rest unless `[timer] idle = "hidden"`.
    Timer,
    Clock,
    /// The power button (power spec § 1): ⏻ at the bar's end, a left click opens its menu with
    /// `[quick]`'s lock, logout, reboot and poweroff. No segment when all four are empty.
    Power,
}

impl ModuleId {
    /// Every module, in `modules::MODULES`' order (the default bar, left to right).
    pub const ALL: [ModuleId; crate::modules::COUNT] = {
        let mut all = [ModuleId::Launcher; crate::modules::COUNT];
        let mut i = 0;
        while i < all.len() {
            all[i] = crate::modules::MODULES[i].id;
            i += 1;
        }
        all
    };

    /// Name in bar.toml.
    pub fn name(self) -> &'static str {
        self.spec().name
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
    /// added or removed family changes memory by its own faces,
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
    /// A left click on the clock opens the calendar (default true:
    /// docs/features/calendar.md).
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
    /// Command run to go to a workspace (click, wheel); `{workspace}` and `{output}` are replaced
    /// in it. Default: `wm-set`, a script of the maintainer's own session (docs/compositors.md,
    /// "External commands"). This command is the one write to the compositor the daemon makes:
    /// everything else it does with the compositor is a read (CONTRIBUTING.md, "The rules").
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
    /// Opens alpheus on a tab (left click on volume, network, Bluetooth, battery, `failed`);
    /// `{tab}` becomes home, sound, network, bluetooth or diagnostics (the TUI's `--tab`).
    /// Default: `alpheus-toggle`, a script of the maintainer's own session (docs/compositors.md,
    /// "External commands") that opens the TUI in a floating terminal, or closes it if already
    /// open — not a neutral default yet, which CONTRIBUTING.md's "What is accepted" asks for.
    pub open_tab: Vec<String>,
    /// Right click on the resources, and the popup's `Monitor…` row (the left click opens the
    /// popup): a system monitor. Default: btop in
    /// foot (2026-10-05: lightness — foot measures ≈ 9.9 MB
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

/// Largest `[resources] graph` still accepted (the setting is ignored since the UI refresh).
pub const GRAPH_MAX: u16 = 60;

/// One figure of the resources module, in the order `[resources] show` lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceItem {
    Cpu,
    Memory,
    Temperature,
    Net,
    Disk,
    Gpu,
}

impl ResourceItem {
    /// The word `bar.toml` uses.
    pub fn name(self) -> &'static str {
        match self {
            ResourceItem::Cpu => "cpu",
            ResourceItem::Memory => "memory",
            ResourceItem::Temperature => "temperature",
            ResourceItem::Net => "net",
            ResourceItem::Disk => "disk",
            ResourceItem::Gpu => "gpu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcesConfig {
    /// Seconds between two readings of /proc (1 to 60).
    pub interval: u64,
    /// Accepted (0 to 60) and ignored: the CPU sparkline is gone; kept so an existing `bar.toml` still loads.
    pub graph: u16,
    /// Temperature sensor: an hwmon `name` (coretemp, k10temp…) or a thermal
    /// zone `type` (x86_pkg_temp…); empty = automatic.
    pub sensor: String,
    /// The figures shown, in this order (never empty, no repeat).
    pub show: Vec<ResourceItem>,
    /// GPU hwmon `name` (amdgpu, nouveau, i915, xe, nvidia); empty = automatic.
    pub gpu_sensor: String,
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        ResourcesConfig {
            interval: 2,
            graph: 20,
            sensor: String::new(),
            show: vec![ResourceItem::Cpu, ResourceItem::Memory, ResourceItem::Temperature],
            gpu_sensor: String::new(),
        }
    }
}

/// How a battery segment shows up (spec Q, 2026-10-05): every peripheral
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatteryConfig {
    /// Peripherals with a real percentage (always shown,
    /// error tint below 20 %).
    pub peripherals: BatteryVisibility,
    /// Peripherals that only report a coarse level: "always" shows the bare
    /// icon (error tint when low, normal otherwise), "low" (default) shows
    /// it only when Low, Critical or Action, "never" hides it.
    pub coarse: BatteryVisibility,
    /// Percentages at which the discharging laptop battery notifies (sorted descending and
    /// de-duplicated at load, 1 to 99); the lowest is critical; empty turns it off.
    pub warn: Vec<u8>,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        BatteryConfig { peripherals: BatteryVisibility::Always, coarse: BatteryVisibility::Low, warn: vec![20, 10, 5] }
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

/// `[privacy]` (privacy spec § 7): the apps never shown (`ignore`) and, for the camera scan, where
/// devices and processes are listed (`dev_dir`, `proc_dir`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacyConfig {
    /// App names left out of the indicators, compared whole and case-insensitively.
    pub ignore: Vec<String>,
    /// Where video devices are listed (watched, never opened).
    pub dev_dir: PathBuf,
    /// Where processes are listed (their `comm` and open files are read).
    pub proc_dir: PathBuf,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        PrivacyConfig { ignore: Vec::new(), dev_dir: PathBuf::from("/dev"), proc_dir: PathBuf::from("/proc") }
    }
}

/// `[failed]` (diagnostics spec § 7): how the failed-units count is followed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FailedConfig {
    /// Also count the user manager's failed units (the session bus).
    pub user_units: bool,
    /// `Subscribe()` to systemd so that a change arrives at once (the icon is instant, and systemd
    /// and dbus-daemon send a signal per unit change to every subscriber); false (the default): the
    /// count is read on triggers only (a resume, `failed-refresh`, another domain's wake-up, a
    /// piggyback). See docs/decisions.md, "The failed-units count is read on triggers, and
    /// `Subscribe` is off by default".
    pub subscribe: bool,
    /// A notification when the count rises.
    pub notify: bool,
}

impl Default for FailedConfig {
    fn default() -> Self {
        FailedConfig { user_units: true, subscribe: false, notify: false }
    }
}

/// The longest `[shield] on_text` or `off_text`, in bytes.
pub const SHIELD_TEXT_MAX: usize = 256;

/// `[shield]` (shield spec § 2): empty by default, no Shield anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShieldConfig {
    /// Absolute path; the mode is on while this file exists.
    pub flag: String,
    pub on: Vec<String>,
    pub off: Vec<String>,
    /// `{id}` as one whole element; empty: the blocked devices cannot be allowed from alpheus.
    pub allow: Vec<String>,
    /// None: USBGuard's own command; `[]`: no device list.
    pub blocked: Option<Vec<String>>,
    pub on_text: Option<String>,
    pub off_text: Option<String>,
}

impl ShieldConfig {
    /// `flag`, `on` and `off` all set.
    pub fn configured(&self) -> bool {
        !self.flag.is_empty() && !self.on.is_empty() && !self.off.is_empty()
    }

    /// The command printing the blocked devices; empty: none.
    pub fn blocked_argv(&self) -> &[String] {
        const DEFAULT: &[&str] = &["usbguard", "list-devices", "--blocked"];
        match &self.blocked {
            Some(v) => v,
            None => {
                static OWNED: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
                OWNED.get_or_init(|| DEFAULT.iter().map(|s| s.to_string()).collect())
            }
        }
    }

    pub fn on_body(&self) -> &str {
        self.on_text.as_deref().unwrap_or("Protections for public networks are on.")
    }

    pub fn off_body(&self) -> &str {
        self.off_text.as_deref().unwrap_or("Back to the usual settings.")
    }
}

/// `[shield]` checked: the hard errors of the file.
fn check_shield(sh: &ShieldConfig) -> Result<(), String> {
    if !sh.flag.is_empty() {
        let p = Path::new(&sh.flag);
        if !p.is_absolute() || p.file_name().is_none() || sh.flag.ends_with('/') || sh.flag.contains('\0') {
            return Err("shield.flag: must be an absolute path to a file".into());
        }
    }
    for (key, command) in [
        ("shield.on", sh.on.as_slice()),
        ("shield.off", sh.off.as_slice()),
        ("shield.allow", sh.allow.as_slice()),
        ("shield.blocked", sh.blocked.as_deref().unwrap_or(&[])),
    ] {
        if command.first().is_some_and(|p| p.trim().is_empty()) {
            return Err(format!("{key}: the program is blank"));
        }
        if command.iter().any(|a| a.contains('\0')) {
            return Err(format!("{key}: an argument holds a NUL"));
        }
    }
    if !sh.allow.is_empty() {
        let whole = sh.allow.iter().filter(|a| a.as_str() == "{id}").count();
        let any = sh.allow.iter().filter(|a| a.contains("{id}")).count();
        if whole != 1 || any != 1 {
            return Err("shield.allow: {id} must be a whole element, once".into());
        }
    }
    for (key, t) in [("shield.on_text", &sh.on_text), ("shield.off_text", &sh.off_text)] {
        if t.as_ref().is_some_and(|t| t.len() > SHIELD_TEXT_MAX || t.chars().any(|c| c.is_control() || format_char(c))) {
            return Err(format!("{key}: at most {SHIELD_TEXT_MAX} bytes, no control or format character"));
        }
    }
    Ok(())
}

/// A Unicode format character (Cf) that hides or reorders text: bidi controls, zero width, soft
/// hyphen, word joiner and invisible operators, BOM. (ZWNJ and ZWJ are shaping, but a notice body
/// has no use for them either.)
fn format_char(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{61c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

/// `[displays]` (displays spec § 1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplaysConfig {
    /// Apply the profile whose screens are exactly the connected ones when they change.
    pub auto: bool,
    /// Where the DRM connectors are listed: a screen whose card is driven by `evdi` is DisplayLink.
    pub drm_dir: PathBuf,
}

impl Default for DisplaysConfig {
    fn default() -> Self {
        DisplaysConfig { auto: true, drm_dir: PathBuf::from("/sys/class/drm") }
    }
}

/// `[hooks]` (hooks spec § 1): one argv per event, empty for none.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HooksConfig {
    pub started: Vec<String>,
    pub theme_mode: Vec<String>,
    pub dnd: Vec<String>,
    pub battery_charge: Vec<String>,
    pub battery_level: Vec<String>,
    pub power_profile: Vec<String>,
    pub network: Vec<String>,
    pub vpn: Vec<String>,
    pub bluetooth: Vec<String>,
    pub privacy: Vec<String>,
    pub displays: Vec<String>,
    pub shield: Vec<String>,
    pub failed_units: Vec<String>,
}

impl HooksConfig {
    /// Every hook with its TOML key, in field order.
    pub fn commands(&self) -> [(&'static str, &[String]); 13] {
        [
            ("started", &self.started),
            ("theme_mode", &self.theme_mode),
            ("dnd", &self.dnd),
            ("battery_charge", &self.battery_charge),
            ("battery_level", &self.battery_level),
            ("power_profile", &self.power_profile),
            ("network", &self.network),
            ("vpn", &self.vpn),
            ("bluetooth", &self.bluetooth),
            ("privacy", &self.privacy),
            ("displays", &self.displays),
            ("shield", &self.shield),
            ("failed_units", &self.failed_units),
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.commands().iter().all(|(_, c)| c.is_empty())
    }
}

/// `[hooks]` checked: a non-empty command has a non-blank program and no NUL in an argument.
fn check_hooks(h: &HooksConfig) -> Result<(), String> {
    for (key, command) in h.commands() {
        if command.first().is_some_and(|p| p.trim().is_empty()) {
            return Err(format!("hooks.{key}: the program is blank"));
        }
        if command.iter().any(|a| a.contains('\0')) {
            return Err(format!("hooks.{key}: an argument holds a NUL"));
        }
    }
    Ok(())
}

/// The bounds of `[notifications]`.
pub const NOTIFY_TIMEOUT_LIMIT: u32 = 600;
pub const NOTIFY_VISIBLE_MAX: u8 = 8;
pub const NOTIFY_WIDTH_MIN: u16 = 24;
pub const NOTIFY_WIDTH_MAX: u16 = 80;
pub const NOTIFY_HISTORY_MAX: u16 = 200;

/// `[notifications]` (notifications spec § 6): the bubbles' timeouts, count and width, and the
/// history's length.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
    /// Do not disturb while the privacy graph shows a screen capture (needs `privacy` in `modules`).
    pub dnd_while_sharing: bool,
    /// `[[notifications.rules]]`: per app, first match wins, at most `NOTIFY_RULES_MAX`.
    pub rules: Vec<NotifyRule>,
}

/// At most this many `[[notifications.rules]]`.
pub const NOTIFY_RULES_MAX: usize = 32;
/// An app name longer than this never matches (the codec caps and cleans names).
pub const NOTIFY_RULE_APP_MAX: usize = 64;

/// What a rule does with a matching notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    /// No bubble; kept in the history and counted unread.
    Mute,
    /// Answered to the sender, never stored.
    Drop,
}

/// The urgency a rule gives a matching notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleUrgency {
    Low,
    Normal,
    Critical,
}

/// One `[[notifications.rules]]` entry: the app's whole name (any case) and what to do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyRule {
    pub app: String,
    #[serde(default)]
    pub action: Option<RuleAction>,
    #[serde(default)]
    pub urgency: Option<RuleUrgency>,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        NotificationsConfig {
            timeout: 5,
            timeout_min: 2,
            timeout_max: 20,
            max_visible: 4,
            width: 40,
            history: 50,
            dnd_while_sharing: false,
            rules: Vec::new(),
        }
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
    /// The VPN detail's `More…` (quick VPN spec), run as given: editing a profile, a secret. None:
    /// absent from bar.toml, resolved at load time (`resolve_vpn_more`); an empty list hides the row.
    pub vpn_more: Option<Vec<String>>,
    /// The `Mixer…` row, run as given. None: absent from bar.toml, resolved at load time
    /// (`resolve_mixer`; an unresolved config, a test's default, shows no row); an empty list
    /// hides the row.
    pub mixer: Option<Vec<String>>,
    /// The do-not-disturb detail's "Until" entry, `HH:MM` (local time).
    pub dnd_until: String,
}

impl QuickConfig {
    /// `dnd_until` as `(hour, minute)`; 8:00 for a text `check_quick` would have refused.
    pub fn dnd_until_hm(&self) -> (u32, u32) {
        alpheus_common::local_time::parse_clock(&self.dnd_until).unwrap_or((8, 0))
    }
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
            vpn_more: None,
            mixer: None,
            dnd_until: "08:00".into(),
        }
    }
}

/// `[quick] wifi_more` absent from bar.toml: the launcher's terminal (launcher.toml's `terminal`),
/// then "nmtui" (quick Wi-Fi spec § 4). Given (even empty): kept as is.
pub fn resolve_wifi_more(q: &mut QuickConfig, terminal: &[String]) {
    if q.wifi_more.is_none() {
        q.wifi_more = Some(in_terminal(terminal, "nmtui"));
    }
}

/// `[quick] vpn_more` absent from bar.toml: the launcher's terminal, then "nmtui-edit". Given (even
/// empty): kept as is.
pub fn resolve_vpn_more(q: &mut QuickConfig, terminal: &[String]) {
    if q.vpn_more.is_none() {
        q.vpn_more = Some(in_terminal(terminal, "nmtui-edit"));
    }
}

/// `[quick] mixer` absent from bar.toml: the launcher's terminal, then "wiremix". Given (even
/// empty): kept as is. Not looked up on `$PATH`: the panel has no such lookup, and a missing
/// wiremix is a terminal that closes at once.
pub fn resolve_mixer(q: &mut QuickConfig, terminal: &[String]) {
    if q.mixer.is_none() {
        q.mixer = Some(in_terminal(terminal, "wiremix"));
    }
}

fn in_terminal(terminal: &[String], program: &str) -> Vec<String> {
    terminal.iter().cloned().chain(std::iter::once(program.to_string())).collect()
}

/// A toggle's `state`, read (quick spec § 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateForm {
    /// "": the row shows `·`.
    Unknown,
    /// "file:<path>": on while the file exists. The path starts with `/`, `~/` or
    /// `$XDG_RUNTIME_DIR/`, the only two expansions (`quick/state.rs`'s `expand`).
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
        ("quick.vpn_more", q.vpn_more.as_deref().unwrap_or(no_more)),
        ("quick.mixer", q.mixer.as_deref().unwrap_or(no_more)),
    ] {
        if command.first().is_some_and(|p| p.trim().is_empty()) {
            return Err(format!("{key}: the program is blank"));
        }
        // spawn refuses an argument with a NUL in it: refused here, with the reason, never at a click.
        if command.iter().any(|a| a.contains('\0')) {
            return Err(format!("{key}: an argument holds a NUL"));
        }
    }
    if alpheus_common::local_time::parse_clock(&q.dnd_until).is_none() {
        return Err(format!("quick.dnd_until: expected a local time HH:MM (0:00 to 23:59), not {:?}", q.dnd_until));
    }
    for (key, t) in [("quick.keep_awake", &q.keep_awake), ("quick.night_light", &q.night_light)] {
        if t.toggle.is_empty() && !t.state.is_empty() {
            return Err(format!("{key}.state without a toggle (the row shows only with one)"));
        }
        state_form(&t.state).map_err(|m| format!("{key}.state: {m}"))?;
    }
    Ok(())
}

/// The longest `[timer]` length, in minutes.
pub const TIMER_MINUTES_MAX: u32 = 180;
/// The most `[timer] rounds`.
pub const TIMER_ROUNDS_MAX: u8 = 12;

/// The chip when nothing runs (timer spec § 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TimerIdle {
    /// One dim glyph, so the mouse can reach the popup.
    #[default]
    Glyph,
    /// Nothing drawn.
    Hidden,
}

/// `[clipboard]` (clipboard spec): off by default, read at start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    pub enabled: bool,
}

/// `[timer]` (timer spec § 7). Lengths are minutes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TimerConfig {
    /// The popup's Timer row at start.
    pub countdown: u32,
    pub work: u32,
    pub short_break: u32,
    pub long_break: u32,
    /// Work sessions before the long break.
    pub rounds: u8,
    /// Run as given at each end, never through a shell; empty: no sound.
    pub sound: Vec<String>,
    pub idle: TimerIdle,
}

impl Default for TimerConfig {
    fn default() -> Self {
        TimerConfig { countdown: 10, work: 25, short_break: 5, long_break: 15, rounds: 4, sound: Vec::new(), idle: TimerIdle::Glyph }
    }
}

/// `[timer]` checked: every length 1 to 180 minutes, `rounds` 1 to 12, a non-blank sound program
/// with no NUL (as `[quick]`'s commands).
fn check_timer(t: &TimerConfig) -> Result<(), String> {
    for (key, v) in
        [("timer.countdown", t.countdown), ("timer.work", t.work), ("timer.short_break", t.short_break), ("timer.long_break", t.long_break)]
    {
        if !(1..=TIMER_MINUTES_MAX).contains(&v) {
            return Err(format!("{key} {v} out of range 1 to {TIMER_MINUTES_MAX} minutes"));
        }
    }
    if !(1..=TIMER_ROUNDS_MAX).contains(&t.rounds) {
        return Err(format!("timer.rounds {} out of range 1 to {TIMER_ROUNDS_MAX}", t.rounds));
    }
    if t.sound.first().is_some_and(|p| p.trim().is_empty()) {
        return Err("timer.sound: the program is blank".into());
    }
    if t.sound.iter().any(|a| a.contains('\0')) {
        return Err("timer.sound: an argument holds a NUL".into());
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
    pub privacy: PrivacyConfig,
    pub failed: FailedConfig,
    pub notifications: NotificationsConfig,
    pub osd: OsdConfig,
    pub quick: QuickConfig,
    pub timer: TimerConfig,
    pub clipboard: ClipboardConfig,
    pub displays: DisplaysConfig,
    pub hooks: HooksConfig,
    pub shield: ShieldConfig,
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
            privacy: PrivacyConfig::default(),
            failed: FailedConfig::default(),
            notifications: NotificationsConfig::default(),
            osd: OsdConfig::default(),
            quick: QuickConfig::default(),
            timer: TimerConfig::default(),
            clipboard: ClipboardConfig::default(),
            displays: DisplaysConfig::default(),
            hooks: HooksConfig::default(),
            shield: ShieldConfig::default(),
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
    privacy: PrivacyConfig,
    #[serde(default)]
    failed: FailedConfig,
    #[serde(default)]
    notifications: NotificationsConfig,
    #[serde(default)]
    osd: OsdConfig,
    #[serde(default)]
    quick: QuickConfig,
    #[serde(default)]
    timer: TimerConfig,
    #[serde(default)]
    clipboard: ClipboardConfig,
    #[serde(default)]
    displays: DisplaysConfig,
    #[serde(default)]
    hooks: HooksConfig,
    #[serde(default)]
    shield: ShieldConfig,
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
        if b.resources.show.is_empty() {
            return Err("resources.show empty (remove \"resources\" from modules instead; or list at least one of cpu, memory, temperature, net, disk, gpu)".into());
        }
        for (i, item) in b.resources.show.iter().enumerate() {
            if b.resources.show[..i].contains(item) {
                return Err(format!("resources.show: duplicate {:?}", item.name()));
            }
        }
        for (key, command) in [("actions.open_tab", &b.actions.open_tab), ("actions.resources", &b.actions.resources)] {
            if command.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(format!("{key} empty (expected: [\"program\", \"argument\", …])"));
            }
        }
        if !(TITLE_MAX_MIN..=TITLE_MAX_MAX).contains(&b.media.title_max) {
            return Err(format!("media.title_max {} out of range {TITLE_MAX_MIN} to {TITLE_MAX_MAX} cells", b.media.title_max));
        }
        let n = &b.notifications;
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
        if n.rules.len() > NOTIFY_RULES_MAX {
            return Err(format!("notifications.rules: at most {NOTIFY_RULES_MAX} (got {})", n.rules.len()));
        }
        for (i, r) in n.rules.iter().enumerate() {
            let at = i + 1;
            let app = r.app.trim();
            if app.is_empty() {
                return Err(format!("notifications.rules[{at}]: app empty"));
            }
            if app.len() > NOTIFY_RULE_APP_MAX || app.chars().any(|c| c.is_control() || crate::text::invisible(c)) {
                return Err(format!(
                    "notifications.rules[{at}]: app too long (over {NOTIFY_RULE_APP_MAX} bytes) or with control characters"
                ));
            }
            if r.action.is_none() && r.urgency.is_none() {
                return Err(format!("notifications.rules[{at}]: needs action or urgency"));
            }
            if r.action == Some(RuleAction::Drop) && r.urgency.is_some() {
                return Err(format!("notifications.rules[{at}]: drop takes no urgency"));
            }
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
        if !b.privacy.dev_dir.is_absolute() {
            return Err("privacy.dev_dir must be an absolute path".into());
        }
        if !b.privacy.proc_dir.is_absolute() {
            return Err("privacy.proc_dir must be an absolute path".into());
        }
        if !b.displays.drm_dir.is_absolute() {
            return Err("displays.drm_dir must be an absolute path".into());
        }
        check_quick(&b.quick)?;
        check_hooks(&b.hooks)?;
        check_shield(&b.shield)?;
        check_timer(&b.timer)?;
        let mut battery = b.battery;
        if let Some(w) = battery.warn.iter().find(|w| !(1..=99).contains(*w)) {
            return Err(format!("battery.warn: {w} out of range 1 to 99"));
        }
        battery.warn.sort_unstable_by(|a, b| b.cmp(a));
        battery.warn.dedup();
        let tray = TrayConfig { icons: tray_icons(b.tray.icons)?, ..b.tray };
        Ok(BarConfig {
            modules,
            height: b.height,
            font: b.font,
            clock: b.clock,
            workspaces: b.workspaces,
            actions: b.actions,
            resources: b.resources,
            battery,
            tray,
            media: b.media,
            privacy: b.privacy,
            failed: b.failed,
            notifications: {
                let mut n = b.notifications.clone();
                for r in &mut n.rules {
                    r.app = r.app.trim().to_string();
                }
                n
            },
            osd: b.osd,
            quick: b.quick,
            timer: b.timer,
            clipboard: b.clipboard,
            displays: b.displays,
            hooks: b.hooks,
            shield: b.shield,
        })
    }
}

#[cfg(test)]
mod tests;
