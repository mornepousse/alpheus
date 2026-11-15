//! The quick settings panel (quick spec), pure: which rows show for the hardware and the
//! configuration, the selection kept by identity, the keys, the clicks and the wheel, the outputs
//! detail, the reboot and power-off confirmation, the re-reads it waits for, and the exact commands
//! to run. The popup (`PopupBody::Quick`), the reads (sysfs, `/proc`: `quick_state.rs`) and the
//! commands themselves are the wiring's (`quick_app.rs`): nothing here reads a file, starts a
//! thread or runs a command.
//!
//! A list with a detail (`quick_wifi.rs`, `quick_bt.rs`) is its own struct and file: its state, rows,
//! keys and safety rules. This file only hands it its turn (`Row`, `ROWS`, `Line`, `lines`,
//! `selected_line`, `key`, `press`, `rows`, `fit`), and gives it `nav`, `moved`, `text_row`, `note_row`,
//! `FAILED_SHOWN` and the tags.

use crate::actions::Button;
use crate::backlight::Reread;
use crate::config::{ModuleId, QuickConfig, ToggleConfig};
use crate::grid::{Grid, Style, Target, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed};
use crate::osd::{self, BRIGHTNESS, MIC_OFF, MIC_ON, PERCENT_CELLS, volume_glyph};
use crate::quick_bt::{Bt, BtOrder, Code};
use crate::quick_wifi::Wifi;
use crate::segments::BELL;
use crate::session::{self, Activation};
pub use crate::session::{CONFIRM, CONFIRM_AFTER, LOCK, LOGOUT, POWER, REBOOT, Session};
use crate::sources::dbus::bt_act::{ActCall, ActError, AgentId, AgentRequest};
use crate::state::{Bar, Feed};
use crate::system::{BtScope, BtState, BtView, Headset, HeadsetMode, ProfileState, Sink, SoundState, Volume, WifiScope, WifiState};
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
// bar cells: the click mapping (`bar_value`) counts on BAR being that width.
const _: () = assert!(PERCENT_AT + PERCENT_CELLS - GLYPH_AT - 3 - PERCENT_CELLS == BAR);
/// The volume's `›` (the outputs detail), on the column where the text rows end.
pub const MORE_AT: u16 = 32;
/// wpctl's targets: the default output, as the bar's own clicks name it, and the default input.
pub const SINK: &str = crate::actions::SINK;
pub const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";
// The mockup's glyphs (spec § 2): Nerd Font Material Design icons; the session row's are
// session.rs's (shared with the power menu, power spec § 4).
pub const PROFILE: char = '\u{f0f85}';
pub const KEEP_AWAKE: char = '\u{f0176}';
pub const NIGHT_LIGHT: char = '\u{f0594}';
pub const MIXER: char = '\u{f066a}'; // md-tune_vertical
/// Re-reads after a command (spec § 5): the backlight, a configured toggle's state.
pub const BRIGHTNESS_REREAD: Duration = Duration::from_millis(150);
pub const TOGGLE_REREAD: Duration = Duration::from_millis(300);
/// A slider sends at most one command per window this long (fix S1): a held key, the wheel or
/// clicks in a row coalesce into the latest level, sent when the window ends.
pub const COALESCE: Duration = BRIGHTNESS_REREAD;
/// The backlight's floor from the panel (Mae, 2026-10-11): never a black screen.
pub const BRIGHTNESS_MIN: u8 = 1;
/// How long a slider keeps showing (and stepping from) the level it sent while the live source
/// (pw-dump, sysfs) still reports another one: wpctl or brightnessctl may lag a window under load.
pub const SETTLE: Duration = Duration::from_secs(1);
/// How long a list's entry shows `failed` after what ran on it failed; the tags of both.
pub const FAILED_SHOWN: Duration = Duration::from_secs(4);
pub(crate) const RUNNING_TAG: &str = "…";
pub(crate) const FAILED_TAG: &str = "failed";

/// Where the panel opens (spec § 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The first row: the bare verb.
    Top,
    /// The volume row: a click on the volume module, or `--quick sound`.
    Sound,
    /// The Wi-Fi row, its detail opened once the Wi-Fi state arrives (quick Wi-Fi spec § 1): a
    /// click on the network module, or `--quick wifi`. Without a Wi-Fi row, the first row.
    Wifi,
    /// The Bluetooth row, its detail opened (quick Bluetooth spec § 1): a click on the Bluetooth
    /// module, or `--quick bluetooth`. Without a Bluetooth row, the first row.
    Bluetooth,
}

impl Focus {
    /// The section a left click on `target` opens.
    pub fn of(target: &Target) -> Focus {
        match target {
            Target::Module(ModuleId::Volume) => Focus::Sound,
            Target::Module(ModuleId::Network) => Focus::Wifi,
            Target::Module(ModuleId::Bluetooth) => Focus::Bluetooth,
            _ => Focus::Top,
        }
    }
}

/// The declaration order is the panel's top-to-bottom order (and `ROWS`' index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Volume,
    /// The default sink's Bluetooth headset: Hi-Fi or call mode (quick Bluetooth spec § 4).
    Headset,
    Brightness,
    /// The Wi-Fi (quick Wi-Fi spec § 1): first under the sliders' separator.
    Wifi,
    /// The Bluetooth adapter (quick Bluetooth spec § 1): right under the Wi-Fi row.
    Bluetooth,
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

/// A configured toggle (spec § 4).
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
    /// power-profiles-daemon's state; None while the system bus is down.
    pub profile: Option<&'a ProfileState>,
    pub config: &'a QuickConfig,
    /// The bar's Bluetooth state; None while it is not read (no row).
    pub bluetooth: Option<&'a BtState>,
}

/// The bar's state as the panel reads it.
pub fn live(bar: &Bar, dnd: bool) -> Live<'_> {
    Live {
        sound: match &bar.sound {
            Feed::Ready(s) => Some(s),
            _ => None,
        },
        dnd: bar.config.modules.contains(&ModuleId::Notifications).then_some(dnd),
        profile: match &bar.profile {
            Feed::Ready(p) => Some(p),
            _ => None,
        },
        config: &bar.config.quick,
        bluetooth: match &bar.bluetooth {
            Feed::Ready(s) => Some(s),
            _ => None,
        },
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
    /// close the field (its buffer wiped): `nmcli --ask` (quick Wi-Fi spec § 3). The password is never
    /// in `argv`, nor anywhere in this action.
    RunSecret {
        argv: Vec<String>,
        what: String,
    },
    /// Calls on BlueZ's acting connection, or the agent's answers (quick_bt.rs), in order.
    Bluetooth(Vec<BtOrder>),
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

/// A slider's pace (fix S1): the window opened by the last command it sent, and whether a newer
/// level waits for that window's end. Only the latest level goes, one command per window.
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

/// The percent a press on a slider's bar sets; None off the bar.
fn bar_value(col: u16) -> Option<u8> {
    (BAR_START..BAR_START + BAR).contains(&col).then(|| ((u32::from(col - BAR_START) + 1) * 100 / u32::from(BAR)) as u8)
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
    /// The volume shown and sent while its slider moves (fix S1), and the default sink's node it
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
    /// Programs that failed to start during this open: each logged once (spec § 6).
    pub failed: Vec<String>,
    /// The Wi-Fi row and its detail (quick_wifi.rs).
    pub wifi: Wifi,
    /// The Bluetooth row and its detail (quick_bt.rs).
    pub bt: Bt,
}

pub(crate) fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The sink the outputs detail chooses when it has none of its own: the default, else the first.
fn default_sink(sinks: &[Sink]) -> Option<u32> {
    sinks.iter().find(|s| s.default).or(sinks.first()).map(|s| s.id)
}

/// One row of the panel's table.
struct RowSpec {
    /// The variant this row is for; also its index (a test pins it).
    row: Row,
    /// For the log line "quick shown (rows: …)".
    name: &'static str,
    /// Its block: the sliders, the toggles, the session buttons (a 1 px line between two).
    group: u8,
    /// Whether it shows (spec § 2): a row whose tool, hardware or command is missing does not.
    present: fn(&Live, &Quick) -> bool,
    /// Draws it, `selected` for the band.
    draw: fn(&mut Grid, &Live, &Quick, bool),
}

/// Every row, top to bottom (spec § 2): also the order a selection falls back along when its row
/// goes. A test pins each entry's `row` to its index. Adding a row:
/// - a toggle with no configuration (like the microphone or do-not-disturb): a `Row` variant at its
///   place, one entry here (its `draw_mark` for a ●/○ state), its Space/Enter arm in `Quick::act`;
/// - a configured toggle (like keep awake): also a `Toggle` variant with its `index`, `config` and
///   `word` arms, its `QuickConfig` field in config.rs (default, argv and state checks, the bar.toml
///   header) and its state read in `quick_app.rs`. The per-toggle arrays follow `Toggle::ALL`.
const ROWS: [RowSpec; 12] = [
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
        draw: |g, live, _, selected| draw_mark(g, BELL, "Do not disturb", live.dnd, selected),
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

/// Whether `r` shows (spec § 2): a row whose tool, hardware or command is missing does not.
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
        };
        let rows = shown(live, &q);
        q.selected = match focus {
            Focus::Sound if rows.contains(&Row::Volume) => Row::Volume,
            Focus::Bluetooth if rows.contains(&Row::Bluetooth) => Row::Bluetooth,
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

    fn visible(&self) -> usize {
        usize::from(self.room).max(3)
    }

    /// The selected line's index among `all` (the lines shown), if any.
    fn selected_line(&self, all: &[Line]) -> Option<usize> {
        let nets = if self.wifi.open { self.wifi.networks() } else { Vec::new() };
        let devices = if self.bt.open { self.bt.devices() } else { Vec::new() };
        all.iter().position(|l| match *l {
            Line::Row(r) => r == self.selected,
            Line::Sink(i) => self.sinks.get(i).is_some_and(|s| Some(s.id) == self.sink),
            l @ (Line::Network(_) | Line::WifiMore | Line::Password) => self.wifi.is_selected(l, &nets),
            l @ (Line::BtHeader | Line::BtDevice(_)) => self.bt.is_selected(l, &devices),
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

    /// A key (spec § 3). `repeat`: the keyboard's auto-repeat, which only ↑ ↓ ← → (and Ctrl+P,
    /// Ctrl+N) follow, and Backspace in the password field: a held Space or Enter acts once (Review
    /// Focus 5).
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
        self.fit(live);
        // Moving away cancels "Reboot? ⏎" (spec § 2): any key but Enter, its second activation.
        let cancelled = k != Key::Enter && self.confirm.take().is_some();
        let action = if self.bt.open {
            let a = self.bt.key(k, &mut self.confirm, now);
            if !self.bt.open {
                self.back_on(Row::Bluetooth);
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
        rows.get(to).map_or(QuickAction::Nothing, |r| moved(&mut self.selected, *r))
    }

    /// Space, Enter and ← → on a row (spec § 3).
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
            (Row::Mic, Key::Char(' ') | Key::Enter) => run(&["wpctl", "set-mute", SOURCE, "toggle"], "microphone mute"),
            (Row::Mixer, Key::Char(' ') | Key::Enter) => run_and_close(live.config.mixer.as_deref().unwrap_or(&[]), "mixer"),
            (Row::Dnd, Key::Char(' ') | Key::Enter) => QuickAction::ToggleDnd,
            (Row::Profile, Key::Enter) => cycle(live),
            (Row::KeepAwake, Key::Char(' ') | Key::Enter) => self.toggle(live, Toggle::KeepAwake, now),
            (Row::NightLight, Key::Char(' ') | Key::Enter) => self.toggle(live, Toggle::NightLight, now),
            (Row::Session, Key::Left) => self.move_button(live, -1),
            (Row::Session, Key::Right) => self.move_button(live, 1),
            (Row::Session, Key::Enter) => self.activate(live, self.button, now),
            _ => QuickAction::Nothing,
        }
    }

    /// The volume by a step or to a level, from the level shown (fix S1): shown at once, sent as an
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
    /// the volume is; re-read BRIGHTNESS_REREAD after the last command (spec § 5).
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
        drawn.get(list::step(at, delta, drawn.len(), |_| true)).map_or(QuickAction::Nothing, |b| moved(&mut self.button, *b))
    }

    /// A session button (spec § 2): lock and log out at once; reboot and power off on a second
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
            // Too soon after the question (Mae): a double activation; the question stays.
            Activation::Wait => QuickAction::Nothing,
            Activation::Ask => QuickAction::Redraw,
            Activation::Run => QuickAction::RunAndClose { argv, what: s.word().to_string() },
        }
    }

    /// A press at `cell` (Boxed coordinates: column 0 the pad, row 0 the first line shown), spec § 3.
    pub fn press(&mut self, live: &Live, cell: (u16, u16), b: Button, now: Instant) -> QuickAction {
        self.wifi.focus_pending = false;
        self.fit(live);
        let all = lines(live, self);
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
            // A left click on any other row is Enter (spec § 3).
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
        ]
        .into_iter()
        .chain(self.reread_toggles)
        .flatten()
        .min()
    }

    /// At `now`: what is due, cleared (the levels that waited, the re-reads to do, the question gone).
    pub fn due(&mut self, now: Instant) -> Due {
        let mut d = Due::default();
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

    /// `program` failed to start: true the first time this open (log it), false after (spec § 6).
    pub fn note_failure(&mut self, program: &str) -> bool {
        if self.failed.iter().any(|p| p == program) {
            return false;
        }
        self.failed.push(program.to_string());
        true
    }

    /// `›`, or Enter on the volume (spec § 2): the outputs in place of the rows, the default chosen.
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

    /// Enter, →, or a click on the Wi-Fi row (quick Wi-Fi spec § 1): its detail in place of the rows.
    fn open_wifi(&mut self) -> QuickAction {
        if !self.wifi.shows() {
            return QuickAction::Nothing;
        }
        self.detail = false;
        self.top = 0;
        self.wifi.open_detail()
    }

    /// The D-Bus thread's Wi-Fi state, while open (`quick_take`). Asked for from the network module
    /// or `--quick wifi`, the first state with a device selects the row and opens the detail.
    pub fn set_wifi(&mut self, state: Option<WifiState>, now: Instant) -> WifiNews {
        let (action, failed) = self.wifi.set_state(state, now);
        WifiNews { action: self.spend_wifi_focus().unwrap_or(action), failed }
    }

    /// The focus asked at open, spent on the first state with a device: the row
    /// selected and the detail opened (its action). A state without one keeps it
    /// (NetworkManager not read yet is no reason to give up); the user's first key, click or
    /// wheel notch drops it (the panel never jumps under them).
    fn spend_wifi_focus(&mut self) -> Option<QuickAction> {
        (self.wifi.shows() && std::mem::take(&mut self.wifi.focus_pending)).then(|| {
            self.selected = Row::Wifi;
            self.open_wifi()
        })
    }

    /// A followed attempt was just taken back with the state it saw (`quick_app::resumed`): the
    /// focus asked at open spent on that state, since none is coming. None: nothing to open.
    pub fn resumed_wifi_focus(&mut self) -> Option<QuickAction> {
        self.wifi.state.as_ref()?;
        self.spend_wifi_focus()
    }

    /// The D-Bus thread's Wi-Fi state for the open panel (`quick_take`): `set_wifi`, then fitted. The
    /// window follows the selection only when the state moved it (a reorder, the detail opened),
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

    /// Enter, →, or a click on the Bluetooth row (quick Bluetooth spec § 1): its detail in place of
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
    /// `Forget?` (spec § 3).
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

    /// `wpctl set-default <id>` for the i-th sink (spec § 5).
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

    /// The sound thread's headset, while open (`quick_take`).
    pub fn set_headset(&mut self, h: Option<Headset>) {
        self.headset = h;
    }

    /// The sound thread's list, while open (`quick_take`).
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

/// The panel's cells: `WIDTH + 2` wide, the lines in the window, the separators for the painter.
pub fn rows(live: &Live, q: &Quick) -> Boxed {
    let all = lines(live, q);
    let visible = q.visible();
    let top = q.top.min(all.len().saturating_sub(visible));
    let mut out = Boxed::default();
    let nets = if q.wifi.open { q.wifi.networks() } else { Vec::new() };
    let devices = if q.bt.open { q.bt.devices() } else { Vec::new() };
    for (r, line) in all.iter().skip(top).take(visible).enumerate() {
        let mut g = Grid::empty(WIDTH + 2);
        match *line {
            Line::Separator => out.separators.push(r),
            Line::Header => text_row(&mut g, None, "‹ Outputs", "", Style::NORMAL, false),
            Line::Sink(i) => {
                if let Some(s) = q.sinks.get(i) {
                    text_row(&mut g, Some(if s.default { '●' } else { ' ' }), &s.label, "", Style::NORMAL, q.sink == Some(s.id));
                }
            }
            Line::Row(row) => draw(&mut g, row, live, q),
            l @ (Line::WifiHeader | Line::Network(_) | Line::WifiNote(_) | Line::WifiMore | Line::Password) => {
                q.wifi.draw(&mut g, l, &nets, q.wifi.is_selected(l, &nets))
            }
            l @ (Line::BtHeader | Line::BtDevice(_) | Line::BtNearby | Line::BtNote(_) | Line::BtCode) => {
                q.bt.draw(&mut g, l, &devices, q.confirm.as_ref(), q.bt.is_selected(l, &devices))
            }
        }
        out.rows.push(g);
    }
    out
}

fn draw(g: &mut Grid, row: Row, live: &Live, q: &Quick) {
    (row.spec().draw)(g, live, q, q.selected == row);
}

fn draw_volume(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    if let Some(v) = live.sound.and_then(|s| s.sink) {
        let v = Volume { percent: q.volume_level.unwrap_or(v.percent), ..v };
        slider(g, volume_glyph(v), v, q.sinks.len() >= 2, selected);
    }
}

fn draw_headset(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    if let Some(h) = &q.headset {
        let mode = if h.active == HeadsetMode::HiFi { "‹ Hi-Fi ›" } else { "‹ Mic ›" };
        text_row(g, None, &h.label, mode, Style::NORMAL, selected);
    }
}

fn draw_brightness(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    if let Some(b) = &q.backlight {
        slider(g, BRIGHTNESS, Volume { percent: b.percent, muted: false }, false, selected);
    }
}

fn draw_wifi(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    text_row(g, Some(q.wifi.glyph()), "Wi-Fi", &format!("{} ›", q.wifi.value()), Style::NORMAL, selected);
}

fn draw_bluetooth(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    if let Some(s) = live.bluetooth {
        text_row(g, Some(q.bt.glyph(s)), "Bluetooth", &format!("{} ›", q.bt.value(s)), Style::NORMAL, selected);
    }
}

fn draw_mic(g: &mut Grid, live: &Live, _: &Quick, selected: bool) {
    if let Some(v) = live.sound.and_then(|s| s.source) {
        text_row(g, Some(if v.muted { MIC_OFF } else { MIC_ON }), "Microphone", "", Style::NORMAL, selected);
        write_switch(g, Some(!v.muted));
    }
}

fn draw_profile(g: &mut Grid, live: &Live, _: &Quick, selected: bool) {
    if let Some(p) = live.profile.and_then(|p| p.active) {
        text_row(g, Some(PROFILE), "Power profile", &format!("{} ›", p.label()), Style::NORMAL, selected);
    }
}

/// A row with a switch on the right.
fn draw_mark(g: &mut Grid, glyph: char, label: &str, on: Option<bool>, selected: bool) {
    text_row(g, Some(glyph), label, "", Style::NORMAL, selected);
    write_switch(g, on);
}

/// A toggle's state, 7 cells (UI refresh, Decision 8): ` ON  ● ` filled in the accent,
/// ` ○ off ` on a pill, `·` dim when unknown.
pub(crate) fn switch(on: Option<bool>) -> (&'static str, Style) {
    match on {
        Some(true) => (" ON  ● ", Style::ACTIVE),
        Some(false) => (" ○ off ", Style::SURFACE),
        None => ("·", Style::MUTED),
    }
}

/// `switch(on)` right-aligned to end one cell before the row's end.
fn write_switch(g: &mut Grid, on: Option<bool>) {
    let end = g.width();
    let (text, style) = switch(on);
    g.write(end.saturating_sub(text_width(text) + 1), end, text, style);
}

/// "G ━━━━━━━━━━━━━━────────  64%  ›" (spec § 2), the OSD's level (`osd::level`) one pad cell in:
/// the glyph dim when muted; filled in the accent, the rest dim; muted, all dim; above 100 %, the
/// whole bar in warn. Selected: the band across the whole row, the track included; the bar
/// keeps its own foreground on it.
fn slider(g: &mut Grid, glyph: char, v: Volume, more: bool, selected: bool) {
    let end = g.width();
    if selected {
        g.write(0, end, &" ".repeat(usize::from(end)), Style::SELECTED);
    }
    osd::level(g, GLYPH_AT, PERCENT_AT + PERCENT_CELLS, glyph, v, selected);
    if more {
        g.write(MORE_AT, MORE_AT + 1, "›", if selected { Style::SELECTED } else { Style::MUTED });
    }
}

/// A text row as a menu draws it (`list::row`: the icon in the accent, the label), then `right`
/// right-aligned one cell from the end in its own style.
pub(crate) fn text_row(g: &mut Grid, icon: Option<char>, label: &str, right: &str, right_style: Style, selected: bool) {
    let end = g.width();
    list::row(g, 0, end, &list::Row { icon, icon_style: Style::ACCENT, text: label, right: "", style: Style::NORMAL, selected });
    let w = text_width(right);
    if w > 0 {
        g.write(end.saturating_sub(w + 1), end, right, if selected { Style::SELECTED } else { right_style });
    }
}

/// A list's note in place of its entries, dim.
pub(crate) fn note_row(g: &mut Grid, note: &str) {
    let end = g.width();
    g.write(3, end.saturating_sub(1), note, Style::MUTED);
}

/// The session buttons' cells (UI refresh, Decision 7): columns `1..end - 1`, one-cell gaps, equal
/// widths, the remainder on the last. (start, width); drawing and clicks both come from here.
pub(crate) fn session_cells(end: u16, n: usize) -> Vec<(u16, u16)> {
    let n = n as u16;
    if n == 0 {
        return Vec::new();
    }
    let room = end.saturating_sub(2).saturating_sub(n - 1);
    let w = room / n;
    let rest = room - w * n;
    (0..n).map(|i| (1 + i * (w + 1), if i == n - 1 { w + rest } else { w })).collect()
}

/// A button's word on the panel.
pub(crate) fn session_word(s: Session) -> &'static str {
    match s {
        Session::Lock => "Lock",
        Session::Logout => "Log out",
        Session::Reboot => "Reboot",
        Session::Poweroff => "Off",
    }
}

/// How the buttons are labelled, the same for every cell of the row: the richest form that fits all of
/// them (a mix of "󰌾 Lock", "Log out" and "󰐥 Off" reads as a mistake).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LabelForm {
    GlyphAndWord,
    Word,
    Glyph,
}

/// The richest form whose labels all fit their `cells`.
pub(crate) fn session_form(buttons: &[Session], cells: &[(u16, u16)]) -> LabelForm {
    [LabelForm::GlyphAndWord, LabelForm::Word]
        .into_iter()
        .find(|f| buttons.iter().zip(cells).all(|(s, (_, w))| text_width(&session_label(*s, *f)) <= *w))
        .unwrap_or(LabelForm::Glyph)
}

/// A button's label in `form`.
pub(crate) fn session_label(s: Session, form: LabelForm) -> String {
    match form {
        LabelForm::GlyphAndWord => format!("{} {}", s.glyph(), session_word(s)),
        LabelForm::Word => session_word(s).to_string(),
        LabelForm::Glyph => s.glyph().to_string(),
    }
}

/// The session buttons in equal cells on `SURFACE`, the chosen one `ACTIVE` when the row is
/// selected, the label centred. Waiting for a second activation, the row is the question instead,
/// in warn.
fn session_row(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    let end = g.width();
    if let Some((Question::Session(s), _)) = &q.confirm {
        g.write(1, end.saturating_sub(1), &s.question(), Style::WARN);
        return;
    }
    let buttons = q.buttons(live);
    let cells = session_cells(end, buttons.len());
    let form = session_form(&buttons, &cells);
    for (s, (at, w)) in buttons.iter().zip(cells) {
        let style = if selected && *s == q.button { Style::ACTIVE } else { Style::SURFACE };
        g.write(at, at + w, &" ".repeat(usize::from(w)), style);
        let label = session_label(*s, form);
        let x = at + w.saturating_sub(text_width(&label)) / 2;
        g.write(x, at + w, &label, style);
    }
}

/// For the log line "quick shown (…)": the rows by name, and the sinks counted.
pub fn describe(live: &Live, q: &Quick) -> String {
    let names: Vec<&str> = shown(live, q).into_iter().map(Row::name).collect();
    format!("rows: {}; sinks: {}", names.join(" "), q.sinks.len())
}

#[cfg(test)]
mod tests;
