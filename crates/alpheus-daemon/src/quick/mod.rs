//! The quick settings panel's model: which rows it shows, what a key, a click or the wheel does to
//! them, and the commands it asks for.
//!
//! This is the pure half of the panel. `Quick` is the open panel's own state, dropped with its
//! popup; `Live` borrows what the rest of the daemon knows (the sound state, do-not-disturb, the
//! power profile, the configuration) each time it is used; `QuickAction` is what a key or a click
//! asks the wiring to do (run a command, close the panel, repaint). `ROWS` says which rows exist
//! and when each shows, and the selection is kept by the row it is on, not by its position.
//! The other files of this folder: `app.rs` is the wiring (the popup, the reads, spawning the
//! commands), `state.rs` reads a configured toggle's state, `draw.rs` turns the state into cells,
//! and `wifi.rs`, `bt.rs`, `vpn.rs`, `dnd.rs` and `shield.rs` are the lists with a detail: each
//! is its own struct with its own state, rows, keys and safety rules. This file only hands such a
//! list its turn (`Row`, `ROWS`, `Line`, `lines`, `selected_line`, `key`, `press`, `fit`) and
//! gives it the helpers they share (`nav`, `moved`, `run`, `run_and_close`, `argv`, `FAILED_SHOWN`,
//! the tags, and `text_row` and `note_row` from `draw.rs`, re-exported here).
//!
//! Never:
//! - write or create a file, or make a raw system call (guardrail
//!   `the_quick_model_and_its_state_reader_only_read`);
//! - run a command or start a thread (guardrail `the_tray_spawns_no_thread`, through its
//!   `NO_THREAD_NOR_COMMAND` list), or arm a timer (the table of timer and thread tokens in
//!   `nothing_resident_while_the_launcher_is_closed`): the wiring passes `now` and `Live`, and gets
//!   the commands back as `QuickAction`;
//! - read a clock: the wiring passes `now` (the one exception is the time zone: `live` turns the
//!   do-not-disturb end into local time, which reads the zone);
//! - put a configured command's argv in a log line: a command carries a short `what` word to log
//!   instead;
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`).
//!
//! See: docs/features/quick-settings.md and docs/decisions.md ("A slider sends one level per
//! 150 ms window and keeps showing it", "The quick panel never sets the backlight below 1 %",
//! "A click on a slider's bar sets the level of the cell", "Only the arrow keys repeat in the quick
//! panel", "The quick panel's outputs detail stays open after a choice", "A session button closes the
//! quick panel", "Do not disturb shows in the quick panel only with the notifications module",
//! "Reboot and power off ask first, and a second activation confirms from 0.4 s to 3 s").

use crate::actions::Button;
use crate::backlight::Reread;
use crate::clock::LocalTime;
use crate::config::{ModuleId, QuickConfig, ToggleConfig};
use crate::grid::{Grid, Style, Target, text_width};
use crate::keys::Key;
use crate::list;
use crate::notify::dnd::Dnd;
use crate::osd::PERCENT_CELLS;
use crate::quick::bt::{Bt, BtOrder, Code};
use crate::quick::dnd::{DndChoice, DndDetail};
use crate::quick::shield::{ShieldAsk, ShieldPanel};
use crate::quick::vpn::Vpn;
use crate::quick::wifi::Wifi;
use crate::segments::BELL;
use crate::session::{self, Activation};
pub use crate::session::{CONFIRM, CONFIRM_AFTER, LOCK, LOGOUT, POWER, REBOOT, Session};
use crate::sources::dbus::bt_act::{ActCall, ActError, AgentId, AgentRequest};
use crate::state::{Bar, Feed};
use crate::system::{
    BtScope, BtState, BtView, Headset, HeadsetMode, ProfileState, Sink, SoundState, VpnScope, VpnState, WifiScope, WifiState,
};
pub use draw::rows;
// `pub(crate)`: visible inside the daemon, not outside it.
pub(crate) use draw::{dnd_switch, note_row, session_cells, switch, text_row};
use draw::{
    draw_bluetooth, draw_brightness, draw_headset, draw_mark, draw_mic, draw_profile, draw_shield, draw_volume, draw_vpn, draw_wifi,
    session_row, write_cell,
};
use std::time::{Duration, Instant};

/// Content width in cells; a row is two more (a blank pad cell each side, as the other popups').
pub const WIDTH: u16 = 32;
/// A slider's glyph (Boxed coordinates: column 0 is the pad), where `osd::level` starts.
pub const GLYPH_AT: u16 = 1;
/// A slider's bar: its first column (after the glyph and a space) and its cells.
pub const BAR_START: u16 = GLYPH_AT + 2;
pub const BAR: u16 = 22;
/// The percentage's first cell ("{:>3}%", `osd::PERCENT_CELLS`).
pub const PERCENT_AT: u16 = 26;
// `osd::level(g, GLYPH_AT, PERCENT_AT + PERCENT_CELLS, …)` draws `end − at − 3 − PERCENT_CELLS`
// bar cells: the click mapping (`bar_value`) counts on BAR being that width. The `const _` line
// is checked when the program is built, so changing one number without the others does not compile.
const _: () = assert!(PERCENT_AT + PERCENT_CELLS - GLYPH_AT - 3 - PERCENT_CELLS == BAR);
/// The volume's `›` (the outputs detail), on the column where the text rows end.
pub const MORE_AT: u16 = 32;
/// wpctl's targets: the default output, as the bar's own clicks name it, and the default input.
pub const SINK: &str = crate::actions::SINK;
pub const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";
// The row glyphs: Nerd Font Material Design icons; the session row's are
// session.rs's (shared with the power menu).
pub const PROFILE: char = '\u{f0f85}';
pub const KEEP_AWAKE: char = '\u{f0176}';
pub const NIGHT_LIGHT: char = '\u{f0594}';
pub const MIXER: char = '\u{f066a}'; // md-tune_vertical
/// Re-reads after a command: the backlight, a configured toggle's state.
pub const BRIGHTNESS_REREAD: Duration = Duration::from_millis(150);
pub const TOGGLE_REREAD: Duration = Duration::from_millis(300);
/// A slider sends at most one command per window this long: a held key, the wheel or clicks in a
/// row coalesce into the latest level, sent when the window ends. See docs/decisions.md, "A slider
/// sends one level per 150 ms window and keeps showing it". The window is the length of the
/// backlight's re-read delay.
pub const COALESCE: Duration = BRIGHTNESS_REREAD;
/// The backlight's floor from the panel: never a black screen. See docs/decisions.md, "The quick
/// panel never sets the backlight below 1 %".
pub const BRIGHTNESS_MIN: u8 = 1;
/// How long a slider keeps showing (and stepping from) the level it sent while the live source
/// (pw-dump, sysfs) still reports another one: wpctl or brightnessctl may take longer than a window
/// to apply it.
pub const SETTLE: Duration = Duration::from_secs(1);
/// How long a list's entry shows `failed` after what ran on it failed.
pub const FAILED_SHOWN: Duration = Duration::from_secs(4);
/// The tag a list's entry shows while what was asked of it runs.
pub(crate) const RUNNING_TAG: &str = "…";
/// The tag a list's entry shows for `FAILED_SHOWN` after what ran on it failed.
pub(crate) const FAILED_TAG: &str = "failed";

/// Where the panel opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The first row: the bare verb.
    Top,
    /// The volume row: a click on the volume module, or `--quick sound`.
    Sound,
    /// The Wi-Fi row, its detail opened once the Wi-Fi state arrives: a
    /// click on the network module, or `--quick wifi`. Without a Wi-Fi row, the first row.
    Wifi,
    /// The Bluetooth row, its detail opened: a click on the Bluetooth
    /// module, or `--quick bluetooth`. Without a Bluetooth row, the first row.
    Bluetooth,
    /// The VPN row, its detail opened once the VPN state arrives: `--quick vpn`.
    /// Without a VPN row, the first row.
    Vpn,
    /// The Shield row: a click on the `shield` glyph. Nothing opens.
    Shield,
}

impl Focus {
    /// The section a left click on `target` opens.
    pub fn of(target: &Target) -> Focus {
        match target {
            Target::Module(ModuleId::Volume) => Focus::Sound,
            Target::Module(ModuleId::Network) => Focus::Wifi,
            Target::Module(ModuleId::Bluetooth) => Focus::Bluetooth,
            Target::Module(ModuleId::Shield) => Focus::Shield,
            _ => Focus::Top,
        }
    }
}

/// One row of the panel. The declaration order is the panel's top-to-bottom order, and `ROWS` is
/// indexed by the variant (`Row::spec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Volume,
    /// The default sink's Bluetooth headset: Hi-Fi or call mode.
    Headset,
    Brightness,
    /// The Wi-Fi: first under the sliders' separator.
    Wifi,
    /// The Bluetooth adapter: right under the Wi-Fi row.
    Bluetooth,
    /// The VPN: right under the Bluetooth row.
    Vpn,
    /// Shield mode: right under the VPN row.
    Shield,
    Mic,
    /// `Mixer…` (`[quick] mixer`): opens a terminal mixer, right under the microphone.
    Mixer,
    Dnd,
    Profile,
    KeepAwake,
    NightLight,
    Session,
}

impl Row {
    /// Its table row (`ROWS` is indexed by the variant).
    fn spec(self) -> &'static RowSpec {
        &ROWS[self as usize]
    }

    /// For the log line "quick shown (rows: …)".
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// Its block: the sliders, the toggles, the session buttons (a 1 px line between two).
    fn group(self) -> u8 {
        self.spec().group
    }
}

/// A configured toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    KeepAwake,
    NightLight,
}

impl Toggle {
    pub const ALL: [Toggle; 2] = [Toggle::KeepAwake, Toggle::NightLight];

    /// Its place in `Quick::toggles` and `Quick::reread_toggles`.
    pub fn index(self) -> usize {
        match self {
            Toggle::KeepAwake => 0,
            Toggle::NightLight => 1,
        }
    }

    pub fn config(self, c: &QuickConfig) -> &ToggleConfig {
        match self {
            Toggle::KeepAwake => &c.keep_awake,
            Toggle::NightLight => &c.night_light,
        }
    }

    /// In the log: "quick: keep awake".
    fn word(self) -> &'static str {
        match self {
            Toggle::KeepAwake => "keep awake",
            Toggle::NightLight => "night light",
        }
    }
}

/// One line of the panel as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Row(Row),
    /// A blank row the painter draws as a 1 px line.
    Separator,
    /// The outputs detail's "‹ Outputs".
    Header,
    /// The n-th of `Quick::sinks`.
    Sink(usize),
    /// The Wi-Fi detail's "‹ Wi-Fi".
    WifiHeader,
    /// The n-th of the Wi-Fi detail's networks (`Wifi::networks`).
    Network(usize),
    /// "Wi-Fi is off", "Searching…", "No networks".
    WifiNote(&'static str),
    /// "More…" (`[quick] wifi_more`).
    WifiMore,
    /// The Wi-Fi detail's password field, under its network.
    Password,
    /// The Bluetooth detail's "‹ Bluetooth" (selectable: Space powers the adapter).
    BtHeader,
    /// The n-th of the Bluetooth detail's devices (`Bt::devices`).
    BtDevice(usize),
    /// The "Nearby" heading over the devices found while the detail is open.
    BtNearby,
    /// "Bluetooth is off".
    BtNote(&'static str),
    /// The pairing code under its device.
    BtCode,
    /// The VPN detail's "‹ VPN".
    VpnHeader,
    /// The n-th of the VPN detail's profiles (`Vpn::listed`).
    VpnProfile(usize),
    /// "More…" (`[quick] vpn_more`).
    VpnMore,
    /// The Shield detail's "‹ Shield".
    ShieldHeader,
    /// The n-th of the Shield detail's blocked devices.
    ShieldDevice(usize),
    /// "Reading…", "No blocked device", "Cannot list the devices", "Shield is off".
    ShieldNote(&'static str),
    /// The do-not-disturb detail's "‹ Do not disturb".
    DndHeader,
    /// The n-th of the do-not-disturb detail's choices (`dnd::CHOICES`).
    DndChoice(usize),
}

/// The backlight as the panel shows it: its device (`brightnessctl --device=`) and level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    pub device: String,
    pub percent: u8,
}

/// What the panel reads from the rest of the daemon, borrowed at each use (`live`).
#[derive(Debug, Clone, Copy)]
pub struct Live<'a> {
    /// pw-dump's state; None while it starts or is down (no volume, no microphone).
    pub sound: Option<&'a SoundState>,
    /// Do-not-disturb; None without the notifications module (no row).
    pub dnd: Option<bool>,
    /// The local `(hour, minute)` a timed do-not-disturb ends at; None when untimed or off.
    pub dnd_end: Option<(u8, u8)>,
    /// power-profiles-daemon's state; None while the system bus is down.
    pub profile: Option<&'a ProfileState>,
    pub config: &'a QuickConfig,
    /// The bar's Bluetooth state; None while it is not read (no row).
    pub bluetooth: Option<&'a BtState>,
    /// Shield mode's view; None without `[shield]` (no row).
    pub shield: Option<crate::shield::ShieldView>,
}

/// The bar's state as the panel reads it.
pub fn live<'a>(bar: &'a Bar, dnd: &Dnd) -> Live<'a> {
    // Do-not-disturb has a row only when the notifications module runs: without it there is no
    // server to mute. See docs/decisions.md, "Do not disturb shows in the quick panel only with the
    // notifications module".
    let dnd_shown = if bar.config.modules.contains(&ModuleId::Notifications) { Some(dnd.on) } else { None };
    Live {
        sound: match &bar.sound {
            Feed::Ready(s) => Some(s),
            _ => None,
        },
        dnd: dnd_shown,
        // A capture's do-not-disturb ends with the capture, not at a time, so it shows no `until`.
        dnd_end: match dnd.until {
            Some(until) if dnd.on && !dnd.by_sharing => {
                let t = LocalTime::from_epoch(until as i64);
                Some((t.hour, t.minute))
            }
            _ => None,
        },
        profile: match &bar.profile {
            Feed::Ready(p) => Some(p),
            _ => None,
        },
        config: &bar.config.quick,
        bluetooth: match &bar.bluetooth {
            Feed::Ready(s) => Some(s),
            _ => None,
        },
        shield: bar.shield,
    }
}

/// What a key, a press or the wheel asks of the wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickAction {
    Nothing,
    Redraw,
    Close,
    /// Spawn `argv` (never waited for), then repaint. `what` names it in the log: a configured
    /// command's argv never reaches the log.
    Run {
        argv: Vec<String>,
        what: String,
    },
    /// A session button: spawn, then close the panel.
    RunAndClose {
        argv: Vec<String>,
        what: String,
    },
    /// Do-not-disturb flips through `App::set_dnd`, the bell's and the verb's one path.
    ToggleDnd,
    /// Spawn `argv` with the open password field's bytes and a newline on its standard input, then
    /// close the field (its buffer wiped): `nmcli --ask`. The password is never
    /// in `argv`, nor anywhere in this action.
    RunSecret {
        argv: Vec<String>,
        what: String,
    },
    /// Calls on BlueZ's acting connection, or the agent's answers (`bt.rs`), in order.
    Bluetooth(Vec<BtOrder>),
    /// A choice of the do-not-disturb detail: the wiring applies it (`app.rs`, `dnd_request`).
    Dnd(DndChoice),
    /// Shield mode's row or detail asks for a switch or an allow (`App::shield_switch`, `shield_allow`).
    Shield(ShieldAsk),
}

/// The question the panel waits on for a second activation: "Reboot? ⏎", "Power off? ⏎", or a
/// Bluetooth device's "Forget? ⏎" (by object path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    Session(Session),
    Forget(String),
}

/// What a tick finds due (`Quick::due`): the re-reads to do, whether to repaint (the question
/// went, a slider back on the level read), and the slider levels that waited for their window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Due {
    pub backlight: bool,
    pub toggles: [bool; Toggle::ALL.len()],
    pub redraw: bool,
    pub run: Vec<QuickAction>,
    /// A Wi-Fi connection failed at this tick (the 20 s timeout): why, for the log.
    pub wifi_failed: Option<&'static str>,
    /// Bluetooth's log lines at this tick (an action's timeout), and its orders (a pairing
    /// cancelled, a call forgotten).
    pub bt_logs: Vec<String>,
    pub bt_orders: Vec<BtOrder>,
    /// A VPN connection failed at this tick (the 20 s timeout): why, for the log.
    pub vpn_failed: Option<&'static str>,
    /// The Shield row's `failed` ended at this tick: the wiring clears the model's and republishes.
    pub shield_failed: bool,
}

/// What a Wi-Fi state brought (`Quick::set_wifi`): the action to take (a rescan, a cleanup), and why
/// a connection failed, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNews {
    pub action: QuickAction,
    pub failed: Option<&'static str>,
}

/// A slider's move: by a step, or to a level.
#[derive(Debug, Clone, Copy)]
enum Level {
    Up(u8),
    Down(u8),
    Set(u8),
}

impl Level {
    /// From `from`, clamped to `min..=100`.
    fn from(self, from: u8, min: u8) -> u8 {
        match self {
            Level::Up(s) => from.saturating_add(s),
            Level::Down(s) => from.saturating_sub(s),
            Level::Set(n) => n,
        }
        .clamp(min, 100)
    }
}

/// A slider's pace: the window opened by the last command it sent, and whether a newer level
/// waits for that window's end. Only the latest level goes, one command per window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pace {
    pub until: Option<Instant>,
    pub waiting: bool,
}

impl Pace {
    /// A new level at `now`: true when it goes at once (a window opens), false when it waits.
    fn ask(&mut self, now: Instant) -> bool {
        if self.until.is_some_and(|u| now < u) {
            self.waiting = true;
            return false;
        }
        self.until = Some(now + COALESCE);
        self.waiting = false;
        true
    }

    /// At `now`: None while the window runs (or none is open); Some(true) when the waiting level
    /// goes now (a new window opens); Some(false) when the window ended with nothing waiting.
    fn due(&mut self, now: Instant) -> Option<bool> {
        if !self.until.is_some_and(|u| u <= now) {
            return None;
        }
        let send = std::mem::take(&mut self.waiting);
        self.until = send.then_some(now + COALESCE);
        Some(send)
    }
}

/// ↑ ↓ Home End (and Ctrl+P, Ctrl+N) over a list of `len` entries, never wrapping: where they go
/// from `at` (nothing selected yet: to the first). None: another key.
pub(crate) fn nav(k: Key, at: Option<usize>, len: usize) -> Option<usize> {
    let last = len.saturating_sub(1);
    match k {
        Key::Up | Key::CtrlP => Some(at.map_or(0, |a| a.saturating_sub(1))),
        Key::Down | Key::CtrlN => Some(at.map_or(0, |a| (a + 1).min(last))),
        Key::Home => Some(0),
        Key::End => Some(last),
        _ => None,
    }
}

/// A selection put on `to`: Redraw when it moved.
pub(crate) fn moved<T: PartialEq>(selected: &mut T, to: T) -> QuickAction {
    if *selected == to {
        return QuickAction::Nothing;
    }
    *selected = to;
    QuickAction::Redraw
}

pub(crate) fn run(parts: &[&str], what: &str) -> QuickAction {
    QuickAction::Run { argv: argv(parts), what: what.to_string() }
}

/// `wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ N%`: never beyond 100 % from the panel.
fn volume_command(n: u8) -> QuickAction {
    QuickAction::Run { argv: argv(&["wpctl", "set-volume", "-l", "1.0", SINK, &format!("{n}%")]), what: format!("volume {n}%") }
}

/// `brightnessctl --class=backlight --device=<name> set N%`.
fn brightness_command(b: &Backlight) -> QuickAction {
    let argv = vec![
        "brightnessctl".to_string(),
        "--class=backlight".to_string(),
        format!("--device={}", b.device),
        "set".to_string(),
        format!("{}%", b.percent),
    ];
    QuickAction::Run { argv, what: format!("brightness {}%", b.percent) }
}

/// The percent a press on a slider's bar sets; None off the bar. See docs/decisions.md, "A click
/// on a slider's bar sets the level of the cell".
fn bar_value(col: u16) -> Option<u8> {
    if !(BAR_START..BAR_START + BAR).contains(&col) {
        return None;
    }
    // Cell 0 of the bar is 1/22 of the way: 4 %, the last cell 100 %.
    Some(((u32::from(col - BAR_START) + 1) * 100 / u32::from(BAR)) as u8)
}

/// A configured command (`More…`, `Mixer…`) run as given, then the panel closes; nothing when it
/// is empty. Its argv never reaches the log, only `what`.
pub(crate) fn run_and_close(argv: &[String], what: &str) -> QuickAction {
    if argv.is_empty() { QuickAction::Nothing } else { QuickAction::RunAndClose { argv: argv.to_vec(), what: what.into() } }
}

/// Enter or a click on the profile: the next one the daemon offers, wrapping.
fn cycle(live: &Live) -> QuickAction {
    let Some(p) = live.profile else { return QuickAction::Nothing };
    let Some(active) = p.active else { return QuickAction::Nothing };
    let next = match p.offered.iter().position(|o| *o == active) {
        Some(i) => p.offered.get((i + 1) % p.offered.len()).copied(),
        None => p.offered.first().copied(),
    };
    match next {
        Some(n) if n != active => run(&["powerprofilesctl", "set", n.name()], &format!("profile {}", n.name())),
        _ => QuickAction::Nothing,
    }
}

/// The open panel's own state: dropped whole with its popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quick {
    /// The selected row, by identity: a row appearing or going above it never moves it.
    pub selected: Row,
    /// The session button chosen on its row.
    pub button: Session,
    /// The outputs detail shows in place of the rows.
    pub detail: bool,
    /// The sink chosen in the detail, by node id.
    pub sink: Option<u32>,
    pub sinks: Vec<Sink>,
    /// The default sink's headset, while the panel is open (`set_headset`).
    pub headset: Option<Headset>,
    pub backlight: Option<Backlight>,
    /// Keep awake, night light: on, off, or unknown (`·`).
    pub toggles: [Option<bool>; Toggle::ALL.len()],
    /// "Reboot? ⏎" (or "Power off? ⏎") until that instant.
    pub confirm: Option<(Question, Instant)>,
    /// The volume shown and sent while its slider moves, and the default sink's node it
    /// was set for; None: pw-dump's own level. Kept until pw-dump reports it, or SETTLE passes.
    pub volume_level: Option<u8>,
    pub volume_sink: Option<String>,
    /// The sliders' paces; the backlight is re-read once its window ends with nothing waiting.
    pub volume: Pace,
    pub brightness: Pace,
    /// After a quiet window, until when the level sent stands against a live one that differs.
    pub volume_settle: Option<Instant>,
    pub brightness_settle: Option<Instant>,
    /// When to re-read each toggle's state.
    pub reread_toggles: [Option<Instant>; Toggle::ALL.len()],
    /// The first line shown, and the rows the panel may take under its bar.
    pub top: usize,
    pub room: u16,
    /// The selected line's index when the window last followed it: a Wi-Fi state that leaves it
    /// there leaves the window (a wheel scroll) alone.
    pub kept_at: Option<usize>,
    /// Programs that failed to start during this open: each logged once.
    pub failed: Vec<String>,
    /// The Wi-Fi row and its detail (`wifi.rs`).
    pub wifi: Wifi,
    /// The Bluetooth row and its detail (`bt.rs`).
    pub bt: Bt,
    /// The VPN row and its detail (`vpn.rs`).
    pub vpn: Vpn,
    /// The do-not-disturb detail (`dnd.rs`).
    pub dnd: DndDetail,
    /// The Shield detail (`shield.rs`).
    pub shield: ShieldPanel,
}

pub(crate) fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The sink the outputs detail chooses when it has none of its own: the default, else the first.
fn default_sink(sinks: &[Sink]) -> Option<u32> {
    match sinks.iter().find(|s| s.default) {
        Some(s) => Some(s.id),
        None => sinks.first().map(|s| s.id),
    }
}

/// One row of the panel's table.
struct RowSpec {
    /// The variant this row is for; also its index (a test pins it).
    row: Row,
    /// For the log line "quick shown (rows: …)".
    name: &'static str,
    /// Its block: the sliders, the toggles, the session buttons (a 1 px line between two).
    group: u8,
    /// Whether it shows: a row whose tool, hardware or command is missing does not.
    present: fn(&Live, &Quick) -> bool,
    /// Draws it, `selected` for the band.
    draw: fn(&mut Grid, &Live, &Quick, bool),
}

/// Every row, top to bottom: also the order a selection falls back along when its row
/// goes. A test pins each entry's `row` to its index. Adding a row:
/// - a toggle with no configuration (like the microphone or do-not-disturb): a `Row` variant at its
///   place, one entry here (its `draw_*` in `draw.rs`, `draw_mark` for a switch), its Space/Enter arm
///   in `Quick::act`;
/// - a configured toggle (like keep awake): also a `Toggle` variant with its `index`, `config` and
///   `word` arms, its `QuickConfig` field in config.rs (default, argv and state checks, the bar.toml
///   header) and its state read in `app.rs`. The per-toggle arrays follow `Toggle::ALL`.
const ROWS: [RowSpec; 14] = [
    RowSpec {
        row: Row::Volume,
        name: "volume",
        group: 0,
        present: |live, _| live.sound.is_some_and(|s| s.sink.is_some()),
        draw: draw_volume,
    },
    RowSpec { row: Row::Headset, name: "headset", group: 0, present: |_, q| q.headset.is_some(), draw: draw_headset },
    RowSpec { row: Row::Brightness, name: "brightness", group: 0, present: |_, q| q.backlight.is_some(), draw: draw_brightness },
    RowSpec { row: Row::Wifi, name: "wifi", group: 1, present: |_, q| q.wifi.shows(), draw: draw_wifi },
    RowSpec { row: Row::Bluetooth, name: "bluetooth", group: 1, present: |live, _| Bt::shows(live.bluetooth), draw: draw_bluetooth },
    RowSpec { row: Row::Vpn, name: "vpn", group: 1, present: |_, q| q.vpn.shows(), draw: draw_vpn },
    RowSpec { row: Row::Shield, name: "shield", group: 1, present: |live, _| live.shield.is_some(), draw: draw_shield },
    RowSpec { row: Row::Mic, name: "mic", group: 1, present: |live, _| live.sound.is_some_and(|s| s.source.is_some()), draw: draw_mic },
    RowSpec {
        row: Row::Mixer,
        name: "mixer",
        group: 1,
        present: |live, _| live.config.mixer.as_deref().is_some_and(|m| !m.is_empty()),
        draw: |g, _, _, selected| text_row(g, Some(MIXER), "Mixer…", "", Style::NORMAL, selected),
    },
    RowSpec {
        row: Row::Dnd,
        name: "dnd",
        group: 1,
        present: |live, _| live.dnd.is_some(),
        draw: |g, live, _, selected| {
            text_row(g, Some(BELL), "Do not disturb", "", Style::NORMAL, selected);
            let (text, style) = dnd_switch(live);
            write_cell(g, &text, style);
        },
    },
    RowSpec {
        row: Row::Profile,
        name: "profile",
        group: 1,
        present: |live, _| live.profile.is_some_and(|p| p.active.is_some() && !p.offered.is_empty()),
        draw: draw_profile,
    },
    RowSpec {
        row: Row::KeepAwake,
        name: "keep-awake",
        group: 1,
        present: |live, _| !live.config.keep_awake.toggle.is_empty(),
        draw: |g, _, q, selected| draw_mark(g, KEEP_AWAKE, "Keep awake", q.toggles[Toggle::KeepAwake.index()], selected),
    },
    RowSpec {
        row: Row::NightLight,
        name: "night-light",
        group: 1,
        present: |live, _| !live.config.night_light.toggle.is_empty(),
        draw: |g, _, q, selected| draw_mark(g, NIGHT_LIGHT, "Night light", q.toggles[Toggle::NightLight.index()], selected),
    },
    RowSpec { row: Row::Session, name: "session", group: 2, present: |live, _| session::any_configured(live.config), draw: session_row },
];

/// Whether `r` shows: a row whose tool, hardware or command is missing does not.
fn present(r: Row, live: &Live, q: &Quick) -> bool {
    (r.spec().present)(live, q)
}

/// The rows shown, top to bottom.
pub fn shown(live: &Live, q: &Quick) -> Vec<Row> {
    ROWS.iter().map(|s| s.row).filter(|r| present(*r, live, q)).collect()
}

/// The lines drawn: the rows with a separator between two blocks; in the detail, its header, a
/// separator and the sinks.
pub fn lines(live: &Live, q: &Quick) -> Vec<Line> {
    if q.bt.open {
        return q.bt.lines();
    }
    if q.vpn.open {
        return q.vpn.lines(live.config);
    }
    if q.dnd.open {
        return q.dnd.lines();
    }
    if q.shield.open {
        return q.shield.lines();
    }
    if q.wifi.open {
        return q.wifi.lines(live.config);
    }
    if q.detail {
        let mut v = vec![Line::Header, Line::Separator];
        v.extend((0..q.sinks.len()).map(Line::Sink));
        return v;
    }
    let mut v = Vec::new();
    let mut block = None;
    for r in shown(live, q) {
        if block.is_some_and(|b| b != r.group()) {
            v.push(Line::Separator);
        }
        block = Some(r.group());
        v.push(Line::Row(r));
    }
    v
}

impl Quick {
    /// At open: the row `focus` asks for, else the first one shown.
    pub fn new(
        focus: Focus,
        live: &Live,
        sinks: Vec<Sink>,
        backlight: Option<Backlight>,
        toggles: [Option<bool>; Toggle::ALL.len()],
        room: u16,
    ) -> Quick {
        let mut q = Quick {
            selected: Row::Volume,
            button: Session::Lock,
            detail: false,
            sink: None,
            sinks,
            headset: None,
            backlight,
            toggles,
            confirm: None,
            volume_level: None,
            volume_sink: None,
            volume: Pace::default(),
            brightness: Pace::default(),
            volume_settle: None,
            brightness_settle: None,
            reread_toggles: [None; Toggle::ALL.len()],
            top: 0,
            kept_at: None,
            room,
            failed: Vec::new(),
            wifi: Wifi { focus_pending: focus == Focus::Wifi, ..Wifi::default() },
            bt: Bt::default(),
            vpn: Vpn { focus_pending: focus == Focus::Vpn, ..Vpn::default() },
            dnd: DndDetail::default(),
            shield: ShieldPanel::default(),
        };
        let rows = shown(live, &q);
        q.selected = match focus {
            Focus::Sound if rows.contains(&Row::Volume) => Row::Volume,
            Focus::Bluetooth if rows.contains(&Row::Bluetooth) => Row::Bluetooth,
            Focus::Shield if rows.contains(&Row::Shield) => Row::Shield,
            _ => rows.first().copied().unwrap_or(Row::Volume),
        };
        if focus == Focus::Bluetooth && q.selected == Row::Bluetooth {
            // Asked for (the module, `--quick bluetooth`), never a first row by chance. No view read
            // yet: nothing to discover, a repaint the open paints anyway.
            q.open_bt(live);
        }
        q.fit(live);
        q
    }

    /// Nothing to show at all: the wiring opens nothing, or closes the panel.
    pub fn is_empty(&self, live: &Live) -> bool {
        shown(live, self).is_empty()
    }

    /// The session buttons drawn, left to right.
    fn buttons(&self, live: &Live) -> Vec<Session> {
        session::configured(live.config)
    }

    /// Before each use: the detail closed when fewer than two sinks are left, its sink found again
    /// by id (else the default); the selection back on a shown row (the next one down the order when
    /// its own went, else the one above); the session button on a drawn one.
    pub fn fit(&mut self, live: &Live) {
        if self.bt.open && !present(Row::Bluetooth, live, self) {
            // The row went (BlueZ or its adapter gone): back on the rows, its orders dropped (the
            // acting connection goes with BlueZ).
            let _dropped = self.bt.close_detail();
            self.back_on(Row::Bluetooth);
        }
        self.bt.fit();
        if self.vpn.open && !present(Row::Vpn, live, self) {
            // The row went (the last profile, or NetworkManager): back on the rows.
            self.vpn.close_detail();
            self.back_on(Row::Vpn);
        }
        self.vpn.fit(live.config);
        if self.dnd.open && !present(Row::Dnd, live, self) {
            // The row went (the notifications module is not read): back on the rows.
            self.dnd.open = false;
            self.back_on(Row::Dnd);
        }
        if self.shield.open && !present(Row::Shield, live, self) {
            // The row went (`[shield]` is gone): back on the rows.
            self.shield.close_detail();
            self.back_on(Row::Shield);
        }
        self.shield.fit(live.shield.as_ref());
        let was_open = self.wifi.open;
        self.wifi.fit(live.config);
        if self.wifi.open {
            self.detail = false;
        } else if was_open {
            // The Wi-Fi detail closed under it (its device gone): back on the rows, from the top.
            self.back_on(Row::Wifi);
        }
        if self.volume_level.is_some() {
            let sound = live.sound;
            let sink = sound.and_then(|s| s.sink_name.as_ref()).map(|d| d.node.as_str());
            if sink != self.volume_sink.as_deref() {
                // The default sink switched: the level was the old one's, never sent to the new.
                self.volume_level = None;
                self.volume.waiting = false;
                self.volume_settle = None;
            } else if self.volume.until.is_none() && sound.and_then(|s| s.sink).map(|v| v.percent) == self.volume_level {
                // pw-dump reports the level sent: its own from now on.
                self.volume_level = None;
                self.volume_settle = None;
            }
        }
        if self.detail && self.sinks.len() < 2 {
            self.detail = false;
            self.top = 0;
        }
        if self.detail && !self.sinks.iter().any(|s| Some(s.id) == self.sink) {
            self.sink = default_sink(&self.sinks);
        }
        let rows = shown(live, self);
        if !rows.contains(&self.selected) {
            // The selected row went. `self.selected as usize` is its index in `ROWS` (the variant's
            // number); the slices are the table from that row down, and up to it.
            let at = self.selected as usize;
            let below = ROWS[at..].iter().find(|s| rows.contains(&s.row));
            let above = ROWS[..at].iter().rev().find(|s| rows.contains(&s.row));
            if let Some(s) = below.or(above) {
                self.selected = s.row;
            }
        }
        let drawn = self.buttons(live);
        if !drawn.contains(&self.button)
            && let Some(b) = drawn.first()
        {
            self.button = *b;
        }
    }

    /// A list's detail closed: back on its row, from the top.
    fn back_on(&mut self, row: Row) {
        self.selected = row;
        self.top = 0;
    }

    /// How many lines the window shows: the room the popup has under the bar, at least 3.
    fn visible(&self) -> usize {
        usize::from(self.room).max(3)
    }

    /// The selected line's index among `all` (the lines shown), if any.
    fn selected_line(&self, all: &[Line]) -> Option<usize> {
        let nets = if self.wifi.open { self.wifi.networks() } else { Vec::new() };
        let devices = if self.bt.open { self.bt.devices() } else { Vec::new() };
        let profiles = if self.vpn.open { self.vpn.listed() } else { Vec::new() };
        all.iter().position(|l| match *l {
            Line::Row(r) => r == self.selected,
            Line::Sink(i) => self.sinks.get(i).is_some_and(|s| Some(s.id) == self.sink),
            l @ (Line::Network(_) | Line::WifiMore | Line::Password) => self.wifi.is_selected(l, &nets),
            l @ (Line::BtHeader | Line::BtDevice(_)) => self.bt.is_selected(l, &devices),
            l @ (Line::VpnProfile(_) | Line::VpnMore) => self.vpn.is_selected(l, &profiles),
            Line::DndChoice(i) => i == self.dnd.selected,
            l @ Line::ShieldDevice(_) => self.shield.is_selected(l, self.shield.can_allow),
            _ => false,
        })
    }

    /// After a key: the window over the lines keeps the selection in view.
    fn keep_in_view(&mut self, live: &Live) {
        let all = lines(live, self);
        let at = self.selected_line(&all);
        self.keep_visible(at, all.len());
    }

    /// The window over `len` lines keeps the selected line `at` in view; `at` noted (`kept_at`).
    fn keep_visible(&mut self, at: Option<usize>, len: usize) {
        if let Some(at) = at {
            self.top = list::keep_visible(at, self.top, self.visible(), len);
        }
        self.kept_at = at;
    }

    /// A key. `repeat`: the keyboard's auto-repeat, which only ↑ ↓ ← → (and Ctrl+P,
    /// Ctrl+N) follow, and Backspace in the password field: a held Space or Enter acts once. See
    /// docs/decisions.md, "Only the arrow keys repeat in the quick panel".
    pub fn key(&mut self, live: &Live, k: Key, repeat: bool, now: Instant) -> QuickAction {
        let erasing = k == Key::Backspace && self.wifi.field.is_some();
        if repeat && !erasing && !matches!(k, Key::Up | Key::Down | Key::Left | Key::Right | Key::CtrlP | Key::CtrlN) {
            return QuickAction::Nothing;
        }
        // A held key never answers a code question: pressed before it arrived, it would reject it.
        if repeat && matches!(self.bt.code, Some(Code::Question { .. })) {
            return QuickAction::Nothing;
        }
        self.wifi.focus_pending = false;
        self.vpn.focus_pending = false;
        self.fit(live);
        // Moving away cancels "Reboot? ⏎": any key but Enter, its second activation.
        let cancelled = k != Key::Enter && self.confirm.take().is_some();
        let action = if self.bt.open {
            let a = self.bt.key(k, &mut self.confirm, now);
            if !self.bt.open {
                self.back_on(Row::Bluetooth);
            }
            a
        } else if self.vpn.open {
            let a = self.vpn.key(live.config, k, now);
            if !self.vpn.open {
                self.back_on(Row::Vpn);
            }
            a
        } else if self.dnd.open {
            let a = self.dnd.key(live, k);
            if !self.dnd.open {
                self.back_on(Row::Dnd);
            }
            a
        } else if self.shield.open {
            let a = match live.shield {
                Some(v) => self.shield.key(&v, k, now),
                None => QuickAction::Nothing,
            };
            if !self.shield.open {
                self.back_on(Row::Shield);
            }
            a
        } else if self.wifi.open {
            let a = self.wifi.key(live.config, k, now);
            if !self.wifi.open {
                self.back_on(Row::Wifi);
            }
            a
        } else if self.detail {
            self.detail_key(k)
        } else if repeat && self.selected == Row::Headset && matches!(k, Key::Left | Key::Right) {
            // A held arrow would send `wpctl set-profile` again before pw-dump reports the switch.
            QuickAction::Nothing
        } else {
            self.rows_key(live, k, now)
        };
        self.keep_in_view(live);
        if cancelled && action == QuickAction::Nothing { QuickAction::Redraw } else { action }
    }

    fn rows_key(&mut self, live: &Live, k: Key, now: Instant) -> QuickAction {
        let rows = shown(live, self);
        let at = rows.iter().position(|r| *r == self.selected).unwrap_or(0);
        match k {
            Key::Escape => return QuickAction::Close,
            Key::Enter if self.selected == Row::Volume => return self.open_outputs(),
            _ => {}
        }
        let Some(to) = nav(k, Some(at), rows.len()) else { return self.act(live, k, now) };
        match rows.get(to) {
            Some(row) => moved(&mut self.selected, *row),
            None => QuickAction::Nothing,
        }
    }

    /// Space, Enter and ← → on a row.
    fn act(&mut self, live: &Live, k: Key, now: Instant) -> QuickAction {
        let step = live.config.step;
        match (self.selected, k) {
            (Row::Headset, Key::Char(' ') | Key::Enter | Key::Left | Key::Right) => self.switch_headset(),
            (Row::Volume, Key::Right) => self.volume(live, Level::Up(step), now),
            (Row::Volume, Key::Left) => self.volume(live, Level::Down(step), now),
            (Row::Volume, Key::Char(' ')) => run(&["wpctl", "set-mute", SINK, "toggle"], "mute"),
            (Row::Brightness, Key::Right) => self.brightness(Level::Up(step), now),
            (Row::Brightness, Key::Left) => self.brightness(Level::Down(step), now),
            (Row::Wifi, Key::Char(' ')) => self.wifi.radio(),
            (Row::Wifi, Key::Enter | Key::Right) => self.open_wifi(),
            (Row::Bluetooth, Key::Char(' ')) => {
                let a = live.bluetooth.map_or(QuickAction::Nothing, |s| self.bt.power(s));
                self.bt.arm(now);
                a
            }
            (Row::Bluetooth, Key::Enter | Key::Right) => self.open_bt(live),
            (Row::Vpn, Key::Char(' ')) => self.vpn.toggle(now),
            (Row::Vpn, Key::Enter | Key::Right) => self.open_vpn(live),
            (Row::Mic, Key::Char(' ') | Key::Enter) => run(&["wpctl", "set-mute", SOURCE, "toggle"], "microphone mute"),
            (Row::Mixer, Key::Char(' ') | Key::Enter) => run_and_close(live.config.mixer.as_deref().unwrap_or(&[]), "mixer"),
            (Row::Shield, Key::Char(' ')) => match live.shield {
                Some(v) if v.state.is_some() && !v.switching => QuickAction::Shield(ShieldAsk::Toggle),
                _ => QuickAction::Nothing,
            },
            (Row::Shield, Key::Enter | Key::Right) => self.open_shield(live),
            (Row::Dnd, Key::Char(' ')) => QuickAction::ToggleDnd,
            (Row::Dnd, Key::Enter | Key::Right) => self.open_dnd(live),
            (Row::Profile, Key::Enter) => cycle(live),
            (Row::KeepAwake, Key::Char(' ') | Key::Enter) => self.toggle(live, Toggle::KeepAwake, now),
            (Row::NightLight, Key::Char(' ') | Key::Enter) => self.toggle(live, Toggle::NightLight, now),
            (Row::Session, Key::Left) => self.move_button(live, -1),
            (Row::Session, Key::Right) => self.move_button(live, 1),
            (Row::Session, Key::Enter) => self.activate(live, self.button, now),
            _ => QuickAction::Nothing,
        }
    }

    /// The volume by a step or to a level, from the level shown: shown at once, sent as an
    /// absolute level now or when its window ends.
    fn volume(&mut self, live: &Live, to: Level, now: Instant) -> QuickAction {
        let Some(from) = self.volume_level.or(live.sound.and_then(|s| s.sink).map(|v| v.percent)) else {
            return QuickAction::Nothing;
        };
        let n = to.from(from, 0);
        if self.volume_level.is_none() {
            self.volume_sink = live.sound.and_then(|s| s.sink_name.as_ref()).map(|d| d.node.clone());
        }
        self.volume_level = Some(n);
        self.volume_settle = None;
        if self.volume.ask(now) { volume_command(n) } else { QuickAction::Redraw }
    }

    /// The backlight by a step or to a level, never below BRIGHTNESS_MIN: shown at once, sent as
    /// the volume is; re-read BRIGHTNESS_REREAD after the last command.
    fn brightness(&mut self, to: Level, now: Instant) -> QuickAction {
        let Some(b) = self.backlight.as_mut() else { return QuickAction::Nothing };
        b.percent = to.from(b.percent, BRIGHTNESS_MIN);
        self.brightness_settle = None;
        if self.brightness.ask(now) { brightness_command(b) } else { QuickAction::Redraw }
    }

    /// A configured toggle: its command as given; its state re-read TOGGLE_REREAD later.
    fn toggle(&mut self, live: &Live, t: Toggle, now: Instant) -> QuickAction {
        let argv = t.config(live.config).toggle.clone();
        if argv.is_empty() {
            return QuickAction::Nothing;
        }
        self.reread_toggles[t.index()] = Some(now + TOGGLE_REREAD);
        QuickAction::Run { argv, what: t.word().to_string() }
    }

    /// ← → between the session buttons, clamped.
    fn move_button(&mut self, live: &Live, delta: i32) -> QuickAction {
        let drawn = self.buttons(live);
        let at = drawn.iter().position(|b| *b == self.button).unwrap_or(0);
        match drawn.get(list::step(at, delta, drawn.len(), |_| true)) {
            Some(button) => moved(&mut self.button, *button),
            None => QuickAction::Nothing,
        }
    }

    /// A session button: lock and log out at once; reboot and power off on a second
    /// activation within CONFIRM, the first one turning the row into the question.
    fn activate(&mut self, live: &Live, s: Session, now: Instant) -> QuickAction {
        let argv = s.command(live.config).to_vec();
        if argv.is_empty() {
            return QuickAction::Nothing;
        }
        let activation = if s.asks() {
            session::ask(&mut self.confirm, Question::Session(s), now)
        } else {
            self.confirm = None;
            Activation::Run
        };
        match activation {
            // Too soon after the question: a double activation, which never confirms; the question
            // stays. See docs/decisions.md, "Reboot and power off ask first, and a second activation
            // confirms from 0.4 s to 3 s".
            Activation::Wait => QuickAction::Nothing,
            Activation::Ask => QuickAction::Redraw,
            Activation::Run => QuickAction::RunAndClose { argv, what: s.word().to_string() },
        }
    }

    /// A press at `cell` (Boxed coordinates: column 0 the pad, row 0 the first line shown).
    pub fn press(&mut self, live: &Live, cell: (u16, u16), b: Button, now: Instant) -> QuickAction {
        self.wifi.focus_pending = false;
        self.vpn.focus_pending = false;
        self.fit(live);
        let all = lines(live, self);
        // The window may start below where the lines now end (a list shrank): it is cut to the last
        // window that fits.
        let top = self.top.min(all.len().saturating_sub(self.visible()));
        let Some(line) = all.get(top + usize::from(cell.1)).copied() else { return QuickAction::Nothing };
        // A press anywhere but the question's own line cancels it: the session row for a session
        // question, the asking device for a forget one.
        let keep = match (&self.confirm, line) {
            (Some((Question::Session(_), _)), Line::Row(Row::Session)) => true,
            (Some((Question::Forget(p), _)), Line::BtDevice(i)) => self.bt.devices().get(i).is_some_and(|d| &d.path == p),
            _ => false,
        };
        let cancelled = !keep && self.confirm.take().is_some();
        let before = self.selected;
        let action = match (line, b) {
            (Line::Header, Button::Left) => {
                self.detail = false;
                self.top = 0;
                QuickAction::Redraw
            }
            (Line::Sink(i), Button::Left) => {
                self.sink = self.sinks.get(i).map(|s| s.id);
                self.choose_sink(i)
            }
            (l @ (Line::WifiHeader | Line::Network(_) | Line::WifiNote(_) | Line::WifiMore | Line::Password), _) => {
                let a = self.wifi.press(live.config, l, b, now);
                if !self.wifi.open {
                    self.back_on(Row::Wifi);
                }
                a
            }
            (l @ (Line::BtHeader | Line::BtDevice(_) | Line::BtNearby | Line::BtNote(_) | Line::BtCode), _) => {
                let a = self.bt.press(l, b, &mut self.confirm, now);
                if !self.bt.open {
                    self.back_on(Row::Bluetooth);
                }
                a
            }
            (l @ (Line::VpnHeader | Line::VpnProfile(_) | Line::VpnMore), _) => {
                let a = self.vpn.press(live.config, l, b, now);
                if !self.vpn.open {
                    self.back_on(Row::Vpn);
                }
                a
            }
            (l @ (Line::ShieldHeader | Line::ShieldDevice(_) | Line::ShieldNote(_)), _) => {
                let a = match live.shield {
                    Some(v) => self.shield.press(&v, l, b, now),
                    None => QuickAction::Nothing,
                };
                if !self.shield.open {
                    self.back_on(Row::Shield);
                }
                a
            }
            (l @ (Line::DndHeader | Line::DndChoice(_)), _) => {
                let a = self.dnd.press(live, l, b);
                if !self.dnd.open {
                    self.back_on(Row::Dnd);
                }
                a
            }
            (Line::Row(Row::Dnd), Button::Left) => {
                self.selected = Row::Dnd;
                // The switch's drawn cells open the detail; the rest of the row toggles.
                let width = text_width(&dnd_switch(live).0);
                if cell.0 >= WIDTH + 2 - 1 - width { self.open_dnd(live) } else { QuickAction::ToggleDnd }
            }
            (Line::Row(Row::Volume), Button::Right) => {
                self.selected = Row::Volume;
                run(&["wpctl", "set-mute", SINK, "toggle"], "mute")
            }
            (Line::Row(Row::Volume), Button::Left) => {
                self.selected = Row::Volume;
                match bar_value(cell.0) {
                    Some(n) => self.volume(live, Level::Set(n), now),
                    None => self.open_outputs(),
                }
            }
            (Line::Row(Row::Brightness), Button::Left) => {
                self.selected = Row::Brightness;
                match bar_value(cell.0) {
                    Some(n) => self.brightness(Level::Set(n), now),
                    None => QuickAction::Nothing,
                }
            }
            (Line::Row(Row::Session), Button::Left) => self.press_session(live, cell.0, now),
            // A left click on any other row is Enter.
            (Line::Row(r), Button::Left) => {
                self.selected = r;
                self.act(live, Key::Enter, now)
            }
            _ => QuickAction::Nothing,
        };
        if action == QuickAction::Nothing && (cancelled || self.selected != before) { QuickAction::Redraw } else { action }
    }

    /// A left press on the session row: the button under it; while the question shows, anywhere on
    /// the row is the second activation.
    fn press_session(&mut self, live: &Live, col: u16, now: Instant) -> QuickAction {
        self.selected = Row::Session;
        if let Some((Question::Session(s), _)) = self.confirm {
            return self.activate(live, s, now);
        }
        let drawn = self.buttons(live);
        let hit = session_cells(WIDTH + 2, drawn.len()).iter().position(|(at, w)| col >= *at && col < at + w).map(|i| drawn[i]);
        let Some(s) = hit else { return QuickAction::Nothing };
        self.button = s;
        self.activate(live, s, now)
    }

    /// The wheel, `steps` notches (negative: up), at most 10: over a slider, one move
    /// of `steps × step` percent (up: louder, brighter); elsewhere the window moves when the lines
    /// do not fit.
    pub fn scroll(&mut self, live: &Live, cell: Option<(u16, u16)>, steps: i32, now: Instant) -> QuickAction {
        let notches = steps.unsigned_abs().min(10) as u8;
        if notches == 0 {
            return QuickAction::Nothing;
        }
        self.wifi.focus_pending = false;
        self.vpn.focus_pending = false;
        self.fit(live);
        let all = lines(live, self);
        let visible = self.visible();
        let top = self.top.min(all.len().saturating_sub(visible));
        let line = cell.and_then(|c| all.get(top + usize::from(c.1))).copied();
        // Saturating, then clamped to a level: no further cap needed.
        let amount = live.config.step.saturating_mul(notches);
        let up = steps < 0;
        let to = if up { Level::Up(amount) } else { Level::Down(amount) };
        match line {
            Some(Line::Row(Row::Volume)) => self.volume(live, to, now),
            Some(Line::Row(Row::Brightness)) => self.brightness(to, now),
            _ if all.len() > visible => {
                let last_top = all.len() - visible;
                let to = if up { top.saturating_sub(usize::from(notches)) } else { (top + usize::from(notches)).min(last_top) };
                if to == top {
                    QuickAction::Nothing
                } else {
                    self.top = to;
                    QuickAction::Redraw
                }
            }
            _ => QuickAction::Nothing,
        }
    }

    /// The earliest slider window's end, pending re-read or the question's end; None: nothing to
    /// wait for (no timer).
    pub fn deadline(&self) -> Option<Instant> {
        [
            self.volume.until,
            self.brightness.until,
            self.volume_settle,
            self.brightness_settle,
            self.confirm.as_ref().map(|(_, at)| *at),
            self.wifi.deadline(),
            self.bt.deadline(),
            self.vpn.deadline(),
            self.shield.deadline(),
        ]
        .into_iter()
        // The per-toggle deadlines join the same sequence; `flatten` drops the `None`s, so `min`
        // is the earliest one set, or `None` when nothing waits.
        .chain(self.reread_toggles)
        .flatten()
        .min()
    }

    /// At `now`: what is due, cleared (the levels that waited, the re-reads to do, the question gone).
    pub fn due(&mut self, now: Instant) -> Due {
        let mut d = Due::default();
        // `extend` on an `Option` adds its one command, or nothing when it is `None`.
        match self.volume.due(now) {
            Some(true) => d.run.extend(self.volume_level.map(volume_command)),
            // Quiet: the level sent stands until pw-dump reports it (`fit`), SETTLE at most.
            Some(false) if self.volume_level.is_some() => self.volume_settle = Some(now + SETTLE),
            _ => {}
        }
        if self.volume_settle.is_some_and(|at| at <= now) {
            self.volume_settle = None;
            self.volume_level = None;
            d.redraw = true;
        }
        match self.brightness.due(now) {
            Some(true) => d.run.extend(self.backlight.as_ref().map(brightness_command)),
            // Quiet: re-read; a reading that differs is kept back until SETTLE (`set_backlight`).
            Some(false) => {
                d.backlight = true;
                self.brightness_settle = Some(now + SETTLE);
            }
            None => {}
        }
        if self.brightness_settle.is_some_and(|at| at <= now) {
            // One last re-read, taken whatever it says.
            self.brightness_settle = None;
            d.backlight = true;
        }
        for t in Toggle::ALL {
            let i = t.index();
            if self.reread_toggles[i].is_some_and(|at| at <= now) {
                self.reread_toggles[i] = None;
                d.toggles[i] = true;
            }
        }
        if session::expire(&mut self.confirm, now) {
            d.redraw = true;
        }
        let (run, failed, redraw) = self.wifi.due(now);
        d.run.extend(run);
        d.wifi_failed = failed;
        d.redraw |= redraw;
        let (orders, log, redraw) = self.bt.due(now);
        d.bt_orders = orders;
        d.bt_logs.extend(log);
        d.redraw |= redraw;
        let (failed, redraw) = self.vpn.due(now);
        d.vpn_failed = failed;
        d.redraw |= redraw;
        d.redraw |= self.shield.due(now);
        d.shield_failed = std::mem::take(&mut self.shield.row_failed_due);
        d
    }

    /// The panel closes: the levels still waiting for their window, sent now (never lost).
    pub fn flush(&mut self) -> Vec<QuickAction> {
        let mut v = Vec::new();
        if std::mem::take(&mut self.volume.waiting) {
            v.extend(self.volume_level.map(volume_command));
        }
        if std::mem::take(&mut self.brightness.waiting) {
            v.extend(self.backlight.as_ref().map(brightness_command));
        }
        v
    }

    /// `program` failed to start: true the first time this open (log it), false after, so a program
    /// that is missing is logged once.
    pub fn note_failure(&mut self, program: &str) -> bool {
        if self.failed.iter().any(|p| p == program) {
            return false;
        }
        self.failed.push(program.to_string());
        true
    }

    /// `›`, or Enter on the volume: the outputs in place of the rows, the default chosen.
    /// With a single sink, nothing.
    fn open_outputs(&mut self) -> QuickAction {
        if self.sinks.len() < 2 {
            return QuickAction::Nothing;
        }
        self.detail = true;
        self.sink = default_sink(&self.sinks);
        self.top = 0;
        QuickAction::Redraw
    }

    /// Enter, →, or a click on the Wi-Fi row: its detail in place of the rows.
    fn open_wifi(&mut self) -> QuickAction {
        if !self.wifi.shows() {
            return QuickAction::Nothing;
        }
        self.detail = false;
        self.top = 0;
        self.wifi.open_detail()
    }

    /// The D-Bus thread's Wi-Fi state, while open (`app.rs`, `quick_take`). Asked for from the
    /// network module or `--quick wifi`, the first state with a device selects the row and opens the
    /// detail.
    pub fn set_wifi(&mut self, state: Option<WifiState>, now: Instant) -> WifiNews {
        let (action, failed) = self.wifi.set_state(state, now);
        WifiNews { action: self.spend_wifi_focus().unwrap_or(action), failed }
    }

    /// The focus asked at open, spent on the first state with a device: the row
    /// selected and the detail opened (its action). A state without one keeps it
    /// (NetworkManager not read yet is no reason to give up); the user's first key, click or
    /// wheel notch drops it (the panel never jumps under them).
    fn spend_wifi_focus(&mut self) -> Option<QuickAction> {
        // `std::mem::take` reads the flag and leaves `false` behind in one step, so the focus is
        // spent at most once. It runs only when the row shows (`&&` stops at the first `false`).
        if !(self.wifi.shows() && std::mem::take(&mut self.wifi.focus_pending)) {
            return None;
        }
        self.selected = Row::Wifi;
        Some(self.open_wifi())
    }

    /// A followed attempt was just taken back with the state it saw (`app.rs`, `resumed`): the
    /// focus asked at open spent on that state, since none is coming. None: nothing to open.
    pub fn resumed_wifi_focus(&mut self) -> Option<QuickAction> {
        self.wifi.state.as_ref()?;
        self.spend_wifi_focus()
    }

    /// The D-Bus thread's Wi-Fi state for the open panel (`app.rs`, `quick_take`): `set_wifi`, then
    /// fitted. The window follows the selection only when the state moved it (a reorder, the detail opened),
    /// never undoing a wheel scroll on a mere strength change.
    pub fn follow_wifi(&mut self, live: &Live, state: Option<WifiState>, now: Instant) -> WifiNews {
        let news = self.set_wifi(state, now);
        self.follow_selection(live);
        news
    }

    /// Fitted after a list's new state; the window moves only when the selected line did.
    fn follow_selection(&mut self, live: &Live) {
        self.fit(live);
        let all = lines(live, self);
        let at = self.selected_line(&all);
        if at != self.kept_at {
            self.keep_visible(at, all.len());
        }
    }

    /// What the D-Bus thread is to read while the panel is open.
    pub fn wifi_scope(&self) -> WifiScope {
        self.wifi.scope()
    }

    /// Enter, →, or a click on the Bluetooth row: its detail in place of
    /// the rows, discovery started on a powered adapter.
    fn open_bt(&mut self, live: &Live) -> QuickAction {
        if !present(Row::Bluetooth, live, self) {
            return QuickAction::Nothing;
        }
        self.detail = false;
        self.wifi.close_detail();
        self.top = 0;
        self.bt.open_detail()
    }

    /// Enter, →, or a click on the VPN row: its detail in place of the rows.
    fn open_vpn(&mut self, live: &Live) -> QuickAction {
        if !present(Row::Vpn, live, self) {
            return QuickAction::Nothing;
        }
        self.detail = false;
        self.wifi.close_detail();
        self.top = 0;
        self.vpn.open_detail()
    }

    /// The D-Bus thread's VPN state, while open (`app.rs`, `quick_take`). Asked for by `--quick vpn`,
    /// the first state with a profile selects the row and opens the detail. Returns why a connection failed,
    /// for the log (`WifiNews`' shape, no action: nothing is cleaned up).
    pub fn set_vpn(&mut self, state: Option<VpnState>, now: Instant) -> WifiNews {
        let failed = self.vpn.set_state(state, now);
        let action = if self.vpn.shows() && std::mem::take(&mut self.vpn.focus_pending) {
            self.selected = Row::Vpn;
            self.detail = false;
            self.wifi.close_detail();
            self.top = 0;
            self.vpn.open_detail()
        } else {
            QuickAction::Redraw
        };
        WifiNews { action, failed }
    }

    /// `set_vpn`, then fitted; the window follows the selection only when the state moved it (as
    /// `follow_wifi`).
    pub fn follow_vpn(&mut self, live: &Live, state: Option<VpnState>, now: Instant) -> WifiNews {
        let news = self.set_vpn(state, now);
        self.follow_selection(live);
        news
    }

    /// What the D-Bus thread is to read for the VPNs while the panel is open.
    pub fn vpn_scope(&self) -> VpnScope {
        self.vpn.scope()
    }

    /// Enter, →, or a press on the switch of the do-not-disturb row: its detail in place of the rows.
    fn open_dnd(&mut self, live: &Live) -> QuickAction {
        if !present(Row::Dnd, live, self) {
            return QuickAction::Nothing;
        }
        self.detail = false;
        self.top = 0;
        self.dnd.open(live);
        QuickAction::Redraw
    }

    /// Enter, →, or a click on the Shield row: its detail in place of the rows, a
    /// read of the blocked devices asked for while the mode is on. Nothing when `blocked` is `[]`.
    pub fn open_shield(&mut self, live: &Live) -> QuickAction {
        let (Some(view), true) = (live.shield, present(Row::Shield, live, self)) else { return QuickAction::Nothing };
        self.detail = false;
        self.wifi.close_detail();
        self.vpn.close_detail();
        self.dnd.open = false;
        self.top = 0;
        self.shield.open_detail(&view)
    }

    /// A detail the Shield's cannot replace in place is open: the Bluetooth one carries orders for the
    /// acting connection and the Wi-Fi one a password field, so the wiring closes the whole panel through
    /// its proper path before opening the Shield's (the VPN and do-not-disturb details close in place).
    pub fn other_detail_open(&self) -> bool {
        self.bt.open || self.wifi.open
    }

    /// What the Bluetooth reading covers while the panel is open: the detail's lists, else the row.
    pub fn bt_scope(&self) -> BtScope {
        self.bt.scope(true)
    }

    /// The acting connection's answer to `call` (`Bt::answered`), then fitted; the log line, if any.
    pub fn bt_answered(&mut self, call: &ActCall, result: Result<(), ActError>, now: Instant) -> (QuickAction, Option<String>) {
        let r = self.bt.answered(call, result, now);
        self.bt.fit();
        r
    }

    /// One of BlueZ's agent requests (`Bt::agent`), then fitted; a code question cancels a pending
    /// `Forget?`.
    pub fn bt_agent(&mut self, id: AgentId, request: AgentRequest, now: Instant) -> QuickAction {
        let a = self.bt.agent(id, request, now);
        if matches!(self.bt.code, Some(Code::Question { .. })) && matches!(self.confirm, Some((Question::Forget(_), _))) {
            self.confirm = None;
        }
        self.bt.fit();
        a
    }

    /// The panel closing: a pending `Forget?` dropped, then `Bt::leaving`'s orders.
    pub fn bt_leaving(&mut self) -> Vec<BtOrder> {
        if matches!(self.confirm, Some((Question::Forget(_), _))) {
            self.confirm = None;
        }
        self.bt.leaving()
    }

    /// The D-Bus thread's Bluetooth view for the open panel (`Bt::set_view`), then fitted: the
    /// selection back on the row when the detail closed under it; the window follows the selection
    /// only when it moved (as `follow_wifi`).
    pub fn follow_bt(&mut self, live: &Live, view: Option<BtView>, now: Instant) -> QuickAction {
        let was_open = self.bt.open;
        let action = self.bt.set_view(view, now);
        if was_open && !self.bt.open {
            self.back_on(Row::Bluetooth);
        }
        self.follow_selection(live);
        action
    }

    /// Keys in the outputs detail: ↑ ↓ Home End move, Enter makes the sink the default, Esc or ←
    /// go back to the rows (the volume still selected).
    fn detail_key(&mut self, k: Key) -> QuickAction {
        let at = self.sinks.iter().position(|s| Some(s.id) == self.sink).unwrap_or(0);
        match k {
            Key::Escape | Key::Left => {
                self.detail = false;
                self.top = 0;
                return QuickAction::Redraw;
            }
            Key::Enter => return self.choose_sink(at),
            _ => {}
        }
        let Some(to) = nav(k, Some(at), self.sinks.len()) else { return QuickAction::Nothing };
        let to = self.sinks.get(to).map(|s| s.id);
        moved(&mut self.sink, to)
    }

    /// `wpctl set-default <id>` for the i-th sink.
    fn choose_sink(&self, i: usize) -> QuickAction {
        match self.sinks.get(i) {
            Some(s) => {
                QuickAction::Run { argv: argv(&["wpctl", "set-default", &s.id.to_string()]), what: format!("default sink {}", s.id) }
            }
            None => QuickAction::Nothing,
        }
    }

    /// The other mode of the headset: `wpctl set-profile` with that mode's profile index.
    fn switch_headset(&self) -> QuickAction {
        let Some(h) = &self.headset else { return QuickAction::Nothing };
        let (index, what) = match h.active {
            HeadsetMode::HiFi => (h.mic, "headset mode mic"),
            HeadsetMode::Mic => (h.hifi, "headset mode hi-fi"),
        };
        run(&["wpctl", "set-profile", &h.device.to_string(), &index.to_string()], what)
    }

    /// The sound thread's headset, while open (`app.rs`, `quick_take`).
    pub fn set_headset(&mut self, h: Option<Headset>) {
        self.headset = h;
    }

    /// The sound thread's list, while open (`app.rs`, `quick_take`).
    pub fn set_sinks(&mut self, sinks: Vec<Sink>) {
        self.sinks = sinks;
    }

    /// A re-read of the backlight: its level now; Gone: the device went, and its row with it;
    /// Unreadable (a transient sysfs error): the last level stands. Within
    /// SETTLE of a quiet window, a level that differs from the one sent is not taken yet
    /// (brightnessctl may not have written it): the last re-read, at SETTLE, is.
    pub fn set_backlight(&mut self, read: Reread) {
        match (read, self.backlight.as_mut()) {
            (Reread::Level(p), Some(b)) if self.brightness_settle.is_some() && p != b.percent => {}
            (Reread::Level(p), Some(b)) => {
                b.percent = p;
                self.brightness_settle = None;
            }
            (Reread::Gone, _) => self.backlight = None,
            (Reread::Level(_), None) | (Reread::Unreadable, _) => {}
        }
    }

    /// A re-read of a toggle's state.
    pub fn set_toggle(&mut self, t: Toggle, on: Option<bool>) {
        self.toggles[t.index()] = on;
    }
}

/// For the log line "quick shown (…)": the rows by name, and the sinks counted.
pub fn describe(live: &Live, q: &Quick) -> String {
    let names: Vec<&str> = shown(live, q).into_iter().map(Row::name).collect();
    format!("rows: {}; sinks: {}", names.join(" "), q.sinks.len())
}

pub mod app;
pub mod bt;
pub mod bt_app;
pub mod dnd;
mod draw;
pub mod secret;
pub mod shield;
pub mod state;
pub mod vpn;
pub mod wifi;

#[cfg(test)]
mod tests;
