//! The quick settings panel (quick spec), pure: which rows show for the hardware and the
//! configuration, the selection kept by identity, the keys, the clicks and the wheel, the outputs
//! detail, the reboot and power-off confirmation, the re-reads it waits for, and the exact commands
//! to run. The popup (`PopupBody::Quick`), the reads (sysfs, `/proc`: `quick_state.rs`) and the
//! commands themselves are the wiring's (`quick_app.rs`): nothing here reads a file, starts a
//! thread or runs a command.

use crate::actions::Button;
use crate::backlight::Reread;
use crate::config::{ModuleId, QuickConfig, ToggleConfig};
use crate::grid::{Grid, Style, Target, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed};
use crate::osd::{self, BRIGHTNESS, MIC_OFF, MIC_ON, PERCENT_CELLS, volume_glyph};
use crate::quick_wifi::Wifi;
use crate::segments::BELL;
use crate::session::{self, Activation};
pub use crate::session::{CONFIRM, CONFIRM_AFTER, LOCK, LOGOUT, POWER, REBOOT, Session};
use crate::state::{Bar, Feed};
use crate::system::{ProfileState, Sink, SoundState, Volume, WifiScope, WifiState};
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
/// The session buttons' first glyph column, and the gap from one to the next.
pub const SESSION_AT: u16 = 3;
pub const SESSION_GAP: u16 = 5;
/// wpctl's targets: the default output, as the bar's own clicks name it, and the default input.
pub const SINK: &str = crate::actions::SINK;
pub const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";
// The mockup's glyphs (spec § 2): Nerd Font Material Design icons; the session row's are
// session.rs's (shared with the power menu, power spec § 4).
pub const PROFILE: char = '\u{f0f85}';
pub const KEEP_AWAKE: char = '\u{f0176}';
pub const NIGHT_LIGHT: char = '\u{f0594}';
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
/// A session button takes this many cells either side of its glyph for a click.
const REACH: u16 = 2;

/// Where the panel opens (spec § 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The first row: a click on the Bluetooth module (its section comes in delivery 3), or the
    /// bare verb.
    Top,
    /// The volume row: a click on the volume module, or `--quick sound`.
    Sound,
    /// The Wi-Fi row, its detail opened once the Wi-Fi state arrives (quick Wi-Fi spec § 1): a
    /// click on the network module, or `--quick wifi`. Without a Wi-Fi row, the first row.
    Wifi,
}

impl Focus {
    /// The section a left click on `target` opens.
    pub fn of(target: &Target) -> Focus {
        match target {
            Target::Module(ModuleId::Volume) => Focus::Sound,
            Target::Module(ModuleId::Network) => Focus::Wifi,
            _ => Focus::Top,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Volume,
    Brightness,
    /// The Wi-Fi (quick Wi-Fi spec § 1): first under the sliders' separator.
    Wifi,
    Mic,
    Dnd,
    Profile,
    KeepAwake,
    NightLight,
    Session,
}

impl Row {
    /// Top to bottom (spec § 2): also the order a selection falls back along when its row goes.
    pub const ORDER: [Row; 9] =
        [Row::Volume, Row::Brightness, Row::Wifi, Row::Mic, Row::Dnd, Row::Profile, Row::KeepAwake, Row::NightLight, Row::Session];

    /// For the log line "quick shown (rows: …)".
    pub fn name(self) -> &'static str {
        match self {
            Row::Volume => "volume",
            Row::Brightness => "brightness",
            Row::Wifi => "wifi",
            Row::Mic => "mic",
            Row::Dnd => "dnd",
            Row::Profile => "profile",
            Row::KeepAwake => "keep-awake",
            Row::NightLight => "night-light",
            Row::Session => "session",
        }
    }

    /// Its block: the sliders, the toggles, the session buttons (a 1 px line between two).
    fn group(self) -> u8 {
        match self {
            Row::Volume | Row::Brightness => 0,
            Row::Session => 2,
            _ => 1,
        }
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
    /// Do-not-disturb; None without the notifications module (no row, decision 4).
    pub dnd: Option<bool>,
    /// power-profiles-daemon's state; None while the system bus is down.
    pub profile: Option<&'a ProfileState>,
    pub config: &'a QuickConfig,
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
    /// A session button: spawn, then close the panel (decision 7).
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
}

/// What a tick finds due (`Quick::due`): the re-reads to do, whether to repaint (the question
/// went, a slider back on the level read), and the slider levels that waited for their window.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Due {
    pub backlight: bool,
    pub toggles: [bool; 2],
    pub redraw: bool,
    pub run: Vec<QuickAction>,
    /// A Wi-Fi connection failed at this tick (the 20 s timeout): why, for the log.
    pub wifi_failed: Option<&'static str>,
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

/// The percent a press on a slider's bar sets (decision 10); None off the bar.
fn bar_value(col: u16) -> Option<u8> {
    (BAR_START..BAR_START + BAR).contains(&col).then(|| ((u32::from(col - BAR_START) + 1) * 100 / u32::from(BAR)) as u8)
}

/// Enter or a click on the profile: the next one the daemon offers, wrapping (decision 8).
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
    pub backlight: Option<Backlight>,
    /// Keep awake, night light: on, off, or unknown (`·`).
    pub toggles: [Option<bool>; 2],
    /// "Reboot? ⏎" (or "Power off? ⏎") until that instant.
    pub confirm: Option<(Session, Instant)>,
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
    pub reread_toggles: [Option<Instant>; 2],
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
}

pub(crate) fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The sink the outputs detail chooses when it has none of its own: the default, else the first.
fn default_sink(sinks: &[Sink]) -> Option<u32> {
    sinks.iter().find(|s| s.default).or(sinks.first()).map(|s| s.id)
}

/// Whether `r` shows (spec § 2): a row whose tool, hardware or command is missing does not.
fn present(r: Row, live: &Live, q: &Quick) -> bool {
    match r {
        Row::Volume => live.sound.is_some_and(|s| s.sink.is_some()),
        Row::Brightness => q.backlight.is_some(),
        Row::Wifi => q.wifi.shows(),
        Row::Mic => live.sound.is_some_and(|s| s.source.is_some()),
        Row::Dnd => live.dnd.is_some(),
        Row::Profile => live.profile.is_some_and(|p| p.active.is_some() && !p.offered.is_empty()),
        Row::KeepAwake => !live.config.keep_awake.toggle.is_empty(),
        Row::NightLight => !live.config.night_light.toggle.is_empty(),
        Row::Session => session::any_configured(live.config),
    }
}

/// The rows shown, top to bottom.
pub fn shown(live: &Live, q: &Quick) -> Vec<Row> {
    Row::ORDER.into_iter().filter(|r| present(*r, live, q)).collect()
}

/// The lines drawn: the rows with a separator between two blocks; in the detail, its header, a
/// separator and the sinks.
pub fn lines(live: &Live, q: &Quick) -> Vec<Line> {
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
    pub fn new(focus: Focus, live: &Live, sinks: Vec<Sink>, backlight: Option<Backlight>, toggles: [Option<bool>; 2], room: u16) -> Quick {
        let mut q = Quick {
            selected: Row::Volume,
            button: Session::Lock,
            detail: false,
            sink: None,
            sinks,
            backlight,
            toggles,
            confirm: None,
            volume_level: None,
            volume_sink: None,
            volume: Pace::default(),
            brightness: Pace::default(),
            volume_settle: None,
            brightness_settle: None,
            reread_toggles: [None; 2],
            top: 0,
            kept_at: None,
            room,
            failed: Vec::new(),
            wifi: Wifi { focus_pending: focus == Focus::Wifi, ..Wifi::default() },
        };
        let rows = shown(live, &q);
        q.selected = match focus {
            Focus::Sound if rows.contains(&Row::Volume) => Row::Volume,
            _ => rows.first().copied().unwrap_or(Row::Volume),
        };
        q.fit(live);
        q
    }

    /// Nothing to show at all (Review Focus 2): the wiring opens nothing, or closes the panel.
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
        let was_open = self.wifi.open;
        self.wifi.fit(live.config);
        if self.wifi.open {
            self.detail = false;
        } else if was_open {
            // The Wi-Fi detail closed under it (its device gone): back on the rows, from the top.
            self.selected = Row::Wifi;
            self.top = 0;
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
            let at = Row::ORDER.iter().position(|r| *r == self.selected).unwrap_or(0);
            let below = Row::ORDER[at..].iter().find(|r| rows.contains(r));
            let above = Row::ORDER[..at].iter().rev().find(|r| rows.contains(r));
            if let Some(r) = below.or(above) {
                self.selected = *r;
            }
        }
        let drawn = self.buttons(live);
        if !drawn.contains(&self.button)
            && let Some(b) = drawn.first()
        {
            self.button = *b;
        }
    }

    fn visible(&self) -> usize {
        usize::from(self.room).max(3)
    }

    /// The selected line's index among `all` (the lines shown), if any.
    fn selected_line(&self, all: &[Line]) -> Option<usize> {
        let nets = if self.wifi.open { self.wifi.networks() } else { Vec::new() };
        all.iter().position(|l| match *l {
            Line::Row(r) => r == self.selected,
            Line::Sink(i) => self.sinks.get(i).is_some_and(|s| Some(s.id) == self.sink),
            l @ (Line::Network(_) | Line::WifiMore | Line::Password) => self.wifi.is_selected(l, &nets),
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
        self.wifi.focus_pending = false;
        self.fit(live);
        // Moving away cancels "Reboot? ⏎" (spec § 2): any key but Enter, its second activation.
        let cancelled = k != Key::Enter && self.confirm.take().is_some();
        let action = if self.wifi.open {
            let a = self.wifi.key(live.config, k, now);
            if !self.wifi.open {
                self.selected = Row::Wifi;
                self.top = 0;
            }
            a
        } else if self.detail {
            self.detail_key(k)
        } else {
            self.rows_key(live, k, now)
        };
        self.keep_in_view(live);
        if cancelled && action == QuickAction::Nothing { QuickAction::Redraw } else { action }
    }

    fn rows_key(&mut self, live: &Live, k: Key, now: Instant) -> QuickAction {
        let rows = shown(live, self);
        let at = rows.iter().position(|r| *r == self.selected).unwrap_or(0);
        let last = rows.len().saturating_sub(1);
        let to = match k {
            Key::Escape => return QuickAction::Close,
            Key::Up | Key::CtrlP => at.saturating_sub(1),
            Key::Down | Key::CtrlN => (at + 1).min(last),
            Key::Home => 0,
            Key::End => last,
            Key::Enter if self.selected == Row::Volume => return self.open_outputs(),
            _ => return self.act(live, k, now),
        };
        match rows.get(to) {
            Some(r) if *r != self.selected => {
                self.selected = *r;
                QuickAction::Redraw
            }
            _ => QuickAction::Nothing,
        }
    }

    /// Space, Enter and ← → on a row (spec § 3).
    fn act(&mut self, live: &Live, k: Key, now: Instant) -> QuickAction {
        let step = live.config.step;
        match (self.selected, k) {
            (Row::Volume, Key::Right) => self.volume(live, Level::Up(step), now),
            (Row::Volume, Key::Left) => self.volume(live, Level::Down(step), now),
            (Row::Volume, Key::Char(' ')) => run(&["wpctl", "set-mute", SINK, "toggle"], "mute"),
            (Row::Brightness, Key::Right) => self.brightness(Level::Up(step), now),
            (Row::Brightness, Key::Left) => self.brightness(Level::Down(step), now),
            (Row::Wifi, Key::Char(' ')) => self.wifi.radio(),
            (Row::Wifi, Key::Enter | Key::Right) => self.open_wifi(),
            (Row::Mic, Key::Char(' ') | Key::Enter) => run(&["wpctl", "set-mute", SOURCE, "toggle"], "microphone mute"),
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
        match drawn.get(list::step(at, delta, drawn.len(), |_| true)) {
            Some(b) if *b != self.button => {
                self.button = *b;
                QuickAction::Redraw
            }
            _ => QuickAction::Nothing,
        }
    }

    /// A session button (spec § 2): lock and log out at once; reboot and power off on a second
    /// activation within CONFIRM, the first one turning the row into the question.
    fn activate(&mut self, live: &Live, s: Session, now: Instant) -> QuickAction {
        let argv = s.command(live.config).to_vec();
        if argv.is_empty() {
            return QuickAction::Nothing;
        }
        match session::activate(&mut self.confirm, s, now) {
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
        // A press anywhere but the session row cancels the question.
        let cancelled = line != Line::Row(Row::Session) && self.confirm.take().is_some();
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
                    self.selected = Row::Wifi;
                    self.top = 0;
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
        if let Some((s, _)) = self.confirm {
            return self.activate(live, s, now);
        }
        let drawn = self.buttons(live);
        let hit = drawn.iter().enumerate().find(|(i, _)| col.abs_diff(SESSION_AT + SESSION_GAP * *i as u16) <= REACH).map(|(_, s)| *s);
        let Some(s) = hit else { return QuickAction::Nothing };
        self.button = s;
        self.activate(live, s, now)
    }

    /// The wheel, `steps` notches (negative: up), at most 10 (decision 9): over a slider, one move
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
            self.reread_toggles[0],
            self.reread_toggles[1],
            self.confirm.map(|(_, at)| at),
            self.wifi.deadline(),
        ]
        .into_iter()
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

    /// The focus asked at open (decision 12), spent on the first state with a device: the row
    /// selected and the detail opened (its action). A state without one keeps it (final review
    /// B-M1: NetworkManager not read yet is no reason to give up); the user's first key, click or
    /// wheel notch drops it (F3 m2: the panel never jumps under them).
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
        self.fit(live);
        let all = lines(live, self);
        let at = self.selected_line(&all);
        if at != self.kept_at {
            self.keep_visible(at, all.len());
        }
        news
    }

    /// What the D-Bus thread is to read while the panel is open.
    pub fn wifi_scope(&self) -> WifiScope {
        self.wifi.scope()
    }

    /// Keys in the outputs detail: ↑ ↓ Home End move, Enter makes the sink the default, Esc or ←
    /// go back to the rows (the volume still selected).
    fn detail_key(&mut self, k: Key) -> QuickAction {
        let at = self.sinks.iter().position(|s| Some(s.id) == self.sink).unwrap_or(0);
        let last = self.sinks.len().saturating_sub(1);
        let to = match k {
            Key::Escape | Key::Left => {
                self.detail = false;
                self.top = 0;
                return QuickAction::Redraw;
            }
            Key::Up | Key::CtrlP => at.saturating_sub(1),
            Key::Down | Key::CtrlN => (at + 1).min(last),
            Key::Home => 0,
            Key::End => last,
            Key::Enter => return self.choose_sink(at),
            _ => return QuickAction::Nothing,
        };
        let before = self.sink;
        self.sink = self.sinks.get(to).map(|s| s.id);
        if self.sink == before { QuickAction::Nothing } else { QuickAction::Redraw }
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

    /// The sound thread's list, while open (`quick_take`).
    pub fn set_sinks(&mut self, sinks: Vec<Sink>) {
        self.sinks = sinks;
    }

    /// A re-read of the backlight: its level now; Gone: the device went, and its row with it;
    /// Unreadable (a transient sysfs error): the last level stands (final review A-m2). Within
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
        }
        out.rows.push(g);
    }
    out
}

fn draw(g: &mut Grid, row: Row, live: &Live, q: &Quick) {
    let selected = q.selected == row;
    match row {
        Row::Volume => {
            if let Some(v) = live.sound.and_then(|s| s.sink) {
                let v = Volume { percent: q.volume_level.unwrap_or(v.percent), ..v };
                slider(g, volume_glyph(v), v, q.sinks.len() >= 2, selected);
            }
        }
        Row::Brightness => {
            if let Some(b) = &q.backlight {
                slider(g, BRIGHTNESS, Volume { percent: b.percent, muted: false }, false, selected);
            }
        }
        Row::Wifi => text_row(g, Some(q.wifi.glyph()), "Wi-Fi", &format!("{} ›", q.wifi.value()), Style::NORMAL, selected),
        Row::Mic => {
            if let Some(v) = live.sound.and_then(|s| s.source) {
                let (glyph, word, style) = if v.muted { (MIC_OFF, "muted", Style::WARN) } else { (MIC_ON, "live", Style::NORMAL) };
                text_row(g, Some(glyph), "Microphone", word, style, selected);
            }
        }
        Row::Dnd => text_row(g, Some(BELL), "Do not disturb", mark(live.dnd), mark_style(live.dnd), selected),
        Row::Profile => {
            if let Some(p) = live.profile.and_then(|p| p.active) {
                text_row(g, Some(PROFILE), "Power profile", &format!("{} ›", p.label()), Style::NORMAL, selected);
            }
        }
        Row::KeepAwake => text_row(g, Some(KEEP_AWAKE), "Keep awake", mark(q.toggles[0]), mark_style(q.toggles[0]), selected),
        Row::NightLight => text_row(g, Some(NIGHT_LIGHT), "Night light", mark(q.toggles[1]), mark_style(q.toggles[1]), selected),
        Row::Session => session_row(g, live, q, selected),
    }
}

/// A toggle's state: ● on, ○ off, · unknown (spec § 2, § 4).
fn mark(on: Option<bool>) -> &'static str {
    match on {
        Some(true) => "●",
        Some(false) => "○",
        None => "·",
    }
}

/// ● in the accent, the others in fg_dim.
fn mark_style(on: Option<bool>) -> Style {
    if on == Some(true) { Style::ACCENT } else { Style::MUTED }
}

/// "G ━━━━━━━━━━━━━━────────  64%  ›" (spec § 2), the OSD's level (`osd::level`) one pad cell in:
/// the glyph dim when muted; filled in the accent, the rest dim; muted, all dim; above 100 %, the
/// whole bar in warn. Selected: the band across the whole row, the track included; the bar
/// keeps its own foreground on it (decision 6).
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

/// The session buttons, SESSION_GAP cells apart; the chosen one on a 3-cell band when the row is
/// selected. Waiting for a second activation, the row is the question instead, in warn.
fn session_row(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    let end = g.width();
    if let Some((s, _)) = q.confirm {
        g.write(1, end.saturating_sub(1), &s.question(), Style::WARN);
        return;
    }
    for (i, s) in q.buttons(live).into_iter().enumerate() {
        let x = SESSION_AT + SESSION_GAP * i as u16;
        let style = if selected && s == q.button {
            g.write(x - 1, x + 2, "   ", Style::SELECTED);
            Style::SELECTED
        } else {
            Style::NORMAL
        };
        g.write(x, x + 1, &s.glyph().to_string(), style);
    }
}

/// For the log line "quick shown (…)": the rows by name, and the sinks counted.
pub fn describe(live: &Live, q: &Quick) -> String {
    let names: Vec<&str> = shown(live, q).into_iter().map(Row::name).collect();
    format!("rows: {}; sinks: {}", names.join(" "), q.sinks.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Tint;
    use crate::quick_wifi::{OFF_NOTE, STRENGTH, WIFI_OFF, WIFI_ON};
    use crate::segments::VOLUME_MEDIUM;
    use crate::system::{AccessPoint, Profile, SavedWifi, Security, WifiDevice, WifiState};

    fn vol(percent: u8, muted: bool) -> Option<Volume> {
        Some(Volume { percent, muted })
    }

    fn sink(id: u32, label: &str, default: bool) -> Sink {
        Sink { id, label: label.into(), default }
    }

    /// Speaker (99) and the default AG06 (122).
    fn two_sinks() -> Vec<Sink> {
        vec![sink(99, "Speaker", false), sink(122, "AG06/AG03", true)]
    }

    fn backlight(percent: u8) -> Option<Backlight> {
        Some(Backlight { device: "intel_backlight".into(), percent })
    }

    /// What the rest of the daemon says, owned, so a test changes one part.
    struct World {
        sound: SoundState,
        dnd: Option<bool>,
        profile: ProfileState,
        config: QuickConfig,
    }

    impl World {
        /// Everything present: the sink at 64 %, the microphone live, do-not-disturb off, the balanced
        /// profile of three, the default config (lock, reboot, power off; no toggle).
        fn full() -> World {
            World {
                sound: SoundState { sink: vol(64, false), source: vol(100, false), ..SoundState::default() },
                dnd: Some(false),
                profile: ProfileState { active: Some(Profile::Balanced), offered: Profile::ALL.to_vec() },
                config: QuickConfig::default(),
            }
        }

        fn live(&self) -> Live<'_> {
            Live { sound: Some(&self.sound), dnd: self.dnd, profile: Some(&self.profile), config: &self.config }
        }
    }

    /// The panel as it opens on `w`: two sinks, the backlight at 75 %, toggles unknown, room for 40 rows.
    fn quick(w: &World, focus: Focus) -> Quick {
        Quick::new(focus, &w.live(), two_sinks(), backlight(75), [None, None], 40)
    }

    fn texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    #[test]
    fn every_row_shows_in_the_spec_order_with_its_separators() {
        let w = World::full();
        let l = w.live();
        let q = quick(&w, Focus::Top);
        assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
        assert_eq!(
            lines(&l, &q),
            [
                Line::Row(Row::Volume),
                Line::Row(Row::Brightness),
                Line::Separator,
                Line::Row(Row::Mic),
                Line::Row(Row::Dnd),
                Line::Row(Row::Profile),
                Line::Separator,
                Line::Row(Row::Session)
            ]
        );
        let b = rows(&l, &q);
        assert_eq!(b.cells(), (WIDTH + 2, 8));
        assert_eq!(b.separators, [2, 6]);
        let t = texts(&b);
        assert_eq!(t[0], format!(" {VOLUME_MEDIUM} {}{}  64%  ›", "━".repeat(14), "─".repeat(8)), "64 % of 22 cells: 14; › with two sinks");
        assert_eq!(t[1], format!(" {BRIGHTNESS} {}{}  75%", "━".repeat(16), "─".repeat(6)));
        assert_eq!(t[3], format!(" {MIC_ON} Microphone{}live", " ".repeat(16)));
        assert_eq!(t[4], format!(" {BELL} Do not disturb{}○", " ".repeat(15)));
        assert_eq!(t[5], format!(" {PROFILE} Power profile{}balanced ›", " ".repeat(7)));
        assert_eq!(t[7], format!("   {LOCK}    {REBOOT}    {POWER}"), "lock, reboot, power off: logout is empty by default");
        // Selected: the band spans the whole row, its track included; the bar cells keep their own
        // foreground on it (decision 6, final review: a band that skipped the track looked broken).
        let selected = b.rows[0].cells();
        assert!(selected.iter().all(|c| c.style.bg == Tint::Selection), "the band spans the row: {}", b.rows[0].styles());
        assert_eq!((&b.rows[0].styles()[..3], &b.rows[0].styles()[25..]), ("SSS", "S".repeat(9).as_str()));
        let track: String = selected[3..25]
            .iter()
            .map(|c| match c.style.fg {
                Tint::Accent => 'a',
                Tint::FgDim => 'm',
                _ => '?',
            })
            .collect();
        assert_eq!(track, format!("{}{}", "a".repeat(14), "m".repeat(8)), "the bar in its colours, on the band");
        assert_eq!(b.rows[1].styles(), format!(".a.{}{}{}", "a".repeat(16), "m".repeat(6), ".".repeat(9)));
        assert_eq!(&b.rows[4].styles()[32..33], "m", "○ in fg_dim");
    }

    #[test]
    fn marks_warn_and_dim() {
        let mut w = World::full();
        w.sound.source = vol(100, true);
        w.sound.sink = vol(120, false);
        w.dnd = Some(true);
        w.config.keep_awake.toggle = vec!["fake-keep-awake".into()];
        w.config.night_light.toggle = vec!["fake-night".into()];
        let q = Quick::new(Focus::Top, &w.live(), two_sinks(), backlight(75), [Some(true), None], 40);
        let b = rows(&w.live(), &q);
        let t = texts(&b);
        // Volume, brightness, separator, mic, DND, profile, keep awake, night light, separator, session.
        assert_eq!(t[3], format!(" {MIC_OFF} Microphone{}muted", " ".repeat(15)));
        assert_eq!(&b.rows[3].styles()[28..33], "wwwww", "muted in warn");
        assert_eq!(t[4], format!(" {BELL} Do not disturb{}●", " ".repeat(15)));
        assert_eq!(&b.rows[4].styles()[32..33], "a", "● in the accent");
        assert_eq!(t[6], format!(" {KEEP_AWAKE} Keep awake{}●", " ".repeat(19)));
        assert_eq!(t[7], format!(" {NIGHT_LIGHT} Night light{}·", " ".repeat(18)), "unknown state");
        // The volume row is selected: its track on the band, in its own foreground.
        let track = |b: &Boxed| b.rows[0].cells()[3..25].iter().map(|c| c.style.fg).collect::<Vec<_>>();
        assert!(track(&b).iter().all(|f| *f == Tint::Warn), "above 100 %: the whole bar in warn: {:?}", track(&b));
        let mut muted = World::full();
        muted.sound.sink = vol(64, true);
        let b = rows(&muted.live(), &quick(&muted, Focus::Top));
        assert!(track(&b).iter().all(|f| *f == Tint::FgDim), "muted: the bar dimmed: {:?}", track(&b));
    }

    #[test]
    fn a_row_whose_tool_hardware_or_command_is_missing_is_not_shown() {
        let w = World::full();
        let l = w.live();
        let rows_of = |l: &Live, b: Option<Backlight>| shown(l, &Quick::new(Focus::Top, l, two_sinks(), b, [None, None], 40));
        assert!(!rows_of(&l, None).contains(&Row::Brightness), "no backlight");
        let no_daemon = ProfileState::default();
        assert!(!rows_of(&Live { profile: Some(&no_daemon), ..l }, backlight(75)).contains(&Row::Profile), "no power-profiles-daemon");
        assert!(!rows_of(&Live { profile: None, ..l }, backlight(75)).contains(&Row::Profile), "the system bus down");
        let no_mic = SoundState { source: None, ..w.sound.clone() };
        assert_eq!(
            rows_of(&Live { sound: Some(&no_mic), ..l }, backlight(75))[..3],
            [Row::Volume, Row::Brightness, Row::Dnd],
            "no default source"
        );
        assert_eq!(rows_of(&Live { sound: None, ..l }, backlight(75))[0], Row::Brightness, "pw-dump down: no volume, no microphone");
        assert!(!rows_of(&Live { dnd: None, ..l }, backlight(75)).contains(&Row::Dnd), "no notifications module (decision 4)");
        let mut c = QuickConfig { lock: vec![], reboot: vec![], poweroff: vec![], ..QuickConfig::default() };
        assert!(!rows_of(&Live { config: &c, ..l }, backlight(75)).contains(&Row::Session), "every session command empty");
        c.logout = vec!["fake-logout".into()];
        let one = Live { config: &c, ..l };
        let b = rows(&one, &Quick::new(Focus::Top, &one, two_sinks(), None, [None, None], 40));
        assert_eq!(texts(&b).last().cloned(), Some(format!("   {LOGOUT}")), "one button: log out");
        let k = QuickConfig {
            keep_awake: ToggleConfig { toggle: vec!["fake-keep-awake".into()], state: String::new() },
            ..QuickConfig::default()
        };
        assert_eq!(
            rows_of(&Live { config: &k, ..l }, None)[4..],
            [Row::KeepAwake, Row::Session],
            "a configured toggle shows; the other stays hidden"
        );
    }

    /// Review Focus 2: nothing at all to show.
    #[test]
    fn with_nothing_to_show_the_panel_is_empty() {
        let c = QuickConfig { lock: vec![], reboot: vec![], poweroff: vec![], ..QuickConfig::default() };
        let l = Live { sound: None, dnd: None, profile: None, config: &c };
        let q = Quick::new(Focus::Sound, &l, Vec::new(), None, [None, None], 40);
        assert!(q.is_empty(&l), "nothing to open: the wiring logs it and opens nothing");
        assert!(lines(&l, &q).is_empty());
        assert_eq!(rows(&l, &q).cells(), (0, 0));
        let w = World::full();
        assert!(!quick(&w, Focus::Top).is_empty(&w.live()));
    }

    #[test]
    fn the_section_asked_for_is_selected_at_open() {
        assert_eq!(Focus::of(&Target::Module(ModuleId::Volume)), Focus::Sound);
        assert_eq!(Focus::of(&Target::Module(ModuleId::Network)), Focus::Wifi, "quick Wi-Fi spec § 1");
        assert_eq!(quick(&World::full(), Focus::Wifi).selected, Row::Volume, "no Wi-Fi state read yet, so no Wi-Fi row: the first row");
        assert_eq!(Focus::of(&Target::Module(ModuleId::Bluetooth)), Focus::Top);
        let w = World::full();
        assert_eq!(quick(&w, Focus::Sound).selected, Row::Volume);
        assert_eq!(quick(&w, Focus::Top).selected, Row::Volume, "decision 3: the volume is the first row");
        let l = Live { sound: None, ..w.live() };
        assert_eq!(
            Quick::new(Focus::Sound, &l, two_sinks(), backlight(75), [None, None], 40).selected,
            Row::Brightness,
            "no volume row: the first"
        );
    }

    #[test]
    fn the_selection_stays_on_its_row_when_rows_come_and_go() {
        let mut w = World::full();
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Mic;
        w.sound.source = None;
        q.fit(&w.live());
        assert_eq!(q.selected, Row::Dnd, "the microphone unplugged: the next row down");
        w.sound.source = vol(100, false);
        q.fit(&w.live());
        assert_eq!(q.selected, Row::Dnd, "it stays where it went: by identity, never by line");
        q.selected = Row::Session;
        w.config.lock = vec![];
        w.config.reboot = vec![];
        w.config.poweroff = vec![];
        q.fit(&w.live());
        assert_eq!(q.selected, Row::Profile, "the last row gone: the one above");
        let mut w = World::full();
        w.sound.source = None;
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Dnd;
        w.sound.source = vol(100, false);
        q.fit(&w.live());
        assert_eq!(q.selected, Row::Dnd, "a row appearing above never moves it");
    }

    #[test]
    fn arrows_home_and_end_move_over_the_rows_never_wrapping() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        let t = Instant::now();
        assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Nothing, "the first row: clamped");
        assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Redraw);
        assert_eq!(q.selected, Row::Brightness);
        assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Redraw);
        assert_eq!(q.selected, Row::Mic, "over the separator");
        assert_eq!(q.key(&l, Key::End, false, t), QuickAction::Redraw);
        assert_eq!(q.selected, Row::Session);
        assert_eq!(q.key(&l, Key::Down, false, t), QuickAction::Nothing, "never wrapping");
        assert_eq!(q.key(&l, Key::CtrlP, false, t), QuickAction::Redraw);
        assert_eq!(q.selected, Row::Profile);
        assert_eq!(q.key(&l, Key::Home, false, t), QuickAction::Redraw);
        assert_eq!(q.selected, Row::Volume);
        assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close);
    }

    /// Review Focus 5: a held Enter opens nothing more; the arrows repeat.
    #[test]
    fn a_held_space_or_enter_acts_once() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        assert_eq!(q.key(&l, Key::Enter, true, t), QuickAction::Nothing, "a repeated Enter: nothing");
        assert!(!q.detail);
        assert_eq!(q.key(&l, Key::Down, true, t), QuickAction::Redraw, "the arrows repeat");
        assert_eq!(q.key(&l, Key::Up, true, t), QuickAction::Redraw);
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "pressed: the outputs open");
        assert!(q.detail);
    }

    #[test]
    fn the_outputs_detail_lists_every_sink_and_sets_the_default() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
        assert!(q.detail);
        assert_eq!(q.sink, Some(122), "the default chosen at open");
        let b = rows(&l, &q);
        assert_eq!(texts(&b), [" ‹ Outputs".to_string(), String::new(), "   Speaker".to_string(), " ● AG06/AG03".to_string()]);
        assert_eq!(b.separators, [1]);
        assert_eq!(&b.rows[3].styles()[..3], "SSS", "the chosen sink on the band");
        assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Redraw);
        assert_eq!(q.sink, Some(99));
        assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Nothing, "clamped");
        assert_eq!(
            q.key(&l, Key::Enter, false, t),
            QuickAction::Run { argv: argv(&["wpctl", "set-default", "99"]), what: "default sink 99".into() }
        );
        assert!(q.detail, "decision 5: the detail stays, the ● follows pw-dump");
        assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
        assert!(!q.detail);
        assert_eq!(q.selected, Row::Volume, "back on the volume row");
        q.key(&l, Key::Enter, false, t);
        assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw, "← leaves it too");
        assert!(!q.detail);
        assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Close, "from the rows, Escape closes");
    }

    #[test]
    fn a_single_sink_draws_no_arrow_and_opens_no_detail() {
        let w = World::full();
        let l = w.live();
        let t = Instant::now();
        let mut q = Quick::new(Focus::Sound, &l, vec![sink(122, "AG06/AG03", true)], backlight(75), [None, None], 40);
        assert!(texts(&rows(&l, &q))[0].ends_with("64%"), "no ›");
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Nothing);
        assert!(!q.detail);
        let mut q = quick(&w, Focus::Sound);
        q.key(&l, Key::Enter, false, t);
        q.set_sinks(vec![sink(122, "AG06/AG03", true)]);
        q.fit(&l);
        assert!(!q.detail, "a sink unplugged under the detail: back to the rows");
    }

    #[test]
    fn the_detail_keeps_its_sink_by_id() {
        let w = World::full();
        let l = w.live();
        let t = Instant::now();
        let mut q = quick(&w, Focus::Sound);
        q.key(&l, Key::Enter, false, t);
        q.key(&l, Key::Up, false, t);
        assert_eq!(q.sink, Some(99));
        q.set_sinks(vec![sink(64, "HDMI", false), sink(99, "Speaker", false), sink(122, "AG06/AG03", true)]);
        q.fit(&l);
        assert_eq!(q.sink, Some(99), "a sink arriving above it moves its line, never the choice");
        assert_eq!(&rows(&l, &q).rows[3].styles()[..3], "SSS", "Speaker is now on line 3");
        q.set_sinks(vec![sink(64, "HDMI", false), sink(122, "AG06/AG03", true)]);
        q.fit(&l);
        assert_eq!(q.sink, Some(122), "its sink gone: the default");
    }

    #[test]
    fn the_window_follows_the_selection_when_the_lines_do_not_fit() {
        let w = World::full();
        let l = w.live();
        let t = Instant::now();
        let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 3);
        assert_eq!(rows(&l, &q).rows.len(), 3, "three rows of room");
        q.key(&l, Key::End, false, t);
        assert_eq!(q.top, 5, "8 lines, 3 shown: the last three");
        assert_eq!(texts(&rows(&l, &q)).last().cloned(), Some(format!("   {LOCK}    {REBOOT}    {POWER}")), "the session row in view");
        q.key(&l, Key::Home, false, t);
        assert_eq!(q.top, 0);
    }

    #[test]
    fn re_reads_replace_the_backlight_and_the_toggles() {
        let w = World::full();
        let mut q = quick(&w, Focus::Top);
        q.set_backlight(Reread::Level(40));
        assert_eq!(q.backlight, backlight(40));
        q.set_backlight(Reread::Unreadable);
        assert_eq!(q.backlight, backlight(40), "one failed read: the row and its last level stay");
        q.set_backlight(Reread::Gone);
        assert!(!shown(&w.live(), &q).contains(&Row::Brightness), "the device went: its row with it");
        q.set_toggle(Toggle::NightLight, Some(true));
        assert_eq!(q.toggles, [None, Some(true)]);
    }

    const DND_LINE: u16 = 4;
    const SESSION_LINE: u16 = 7;

    /// The `i`-th instant a slider may send again: each a full COALESCE after the one before.
    fn later(t: Instant, i: u32) -> Instant {
        t + COALESCE * i
    }

    fn set_volume(n: u8) -> QuickAction {
        run(&["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", &format!("{n}%")], &format!("volume {n}%"))
    }

    fn set_brightness(n: u8) -> QuickAction {
        run(&["brightnessctl", "--class=backlight", "--device=intel_backlight", "set", &format!("{n}%")], &format!("brightness {n}%"))
    }

    /// Fix S1: every volume command an absolute level the model computed from what it shows.
    #[test]
    fn volume_steps_mutes_and_takes_a_click_on_its_bar() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        let mute = run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"], "mute");
        assert_eq!(q.key(&l, Key::Right, false, later(t, 0)), set_volume(69), "64 % + 5");
        assert!(texts(&rows(&l, &q))[0].contains(" 69%"), "the expected level shown at once");
        assert_eq!(q.key(&l, Key::Left, false, later(t, 1)), set_volume(64), "from the level shown, not pw-dump's");
        assert_eq!(q.key(&l, Key::Right, true, later(t, 2)), set_volume(69), "← → repeat");
        assert_eq!(q.key(&l, Key::Char(' '), false, later(t, 2)), mute);
        assert_eq!(q.press(&l, (BAR_START, 0), Button::Left, later(t, 3)), set_volume(4), "the bar's first cell");
        assert_eq!(q.press(&l, (BAR_START + BAR - 1, 0), Button::Left, later(t, 4)), set_volume(100), "its last cell");
        assert_eq!(q.press(&l, (BAR_START + 10, 0), Button::Left, later(t, 5)), set_volume(50), "(10 + 1) × 100 / 22");
        assert_eq!(q.press(&l, (1, 0), Button::Right, later(t, 5)), mute, "a right click on the volume: mute");
        assert_eq!(q.scroll(&l, Some((5, 0)), -1, later(t, 6)), set_volume(55), "the wheel up: louder");
        assert_eq!(q.scroll(&l, Some((5, 0)), 2, later(t, 7)), set_volume(45), "two notches, one command");
        assert_eq!(q.press(&l, (MORE_AT, 0), Button::Left, later(t, 8)), QuickAction::Redraw, "› opens the outputs");
        assert!(q.detail);
        let ten = World { config: QuickConfig { step: 10, ..QuickConfig::default() }, ..World::full() };
        let mut q = quick(&ten, Focus::Sound);
        assert_eq!(q.key(&ten.live(), Key::Right, false, t), set_volume(74), "[quick] step");
        let mut loud = World::full();
        loud.sound.sink = vol(97, false);
        let mut q = quick(&loud, Focus::Sound);
        assert_eq!(q.key(&loud.live(), Key::Right, false, t), set_volume(100), "never beyond 100 %");
    }

    /// Fix S1: a held → sends at most one command per COALESCE, each the latest level; a burst
    /// inside one window is two commands, the second the level the burst ended on.
    #[test]
    fn a_held_slider_key_sends_the_latest_level_once_per_window() {
        let mut w = World::full();
        w.sound.sink = vol(30, false);
        let l = w.live();
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        let burst: Vec<QuickAction> = (0..10).map(|i| q.key(&l, Key::Right, i > 0, t + Duration::from_millis(15 * i))).collect();
        assert_eq!(burst[0], set_volume(35), "the first goes at once");
        assert!(burst[1..].iter().all(|a| *a == QuickAction::Redraw), "the rest wait: {burst:?}");
        assert!(texts(&rows(&l, &q))[0].contains(" 80%"), "the level shown follows every key");
        assert_eq!(q.deadline(), Some(t + COALESCE), "the window's end");
        assert_eq!(q.due(t + COALESCE), Due { run: vec![set_volume(80)], ..Due::default() }, "the latest level, once");
        assert_eq!(q.deadline(), Some(t + COALESCE * 2), "a new window after it");
        assert_eq!(q.due(t + COALESCE * 2), Due::default(), "quiet, pw-dump still at 30 %: the level sent stays");
        assert!(texts(&rows(&l, &q))[0].contains(" 80%"));
        assert_eq!(q.deadline(), Some(t + COALESCE * 2 + SETTLE), "for SETTLE at most");
        assert_eq!(q.due(t + COALESCE * 2 + SETTLE), Due { redraw: true, ..Due::default() }, "then pw-dump's level");
        assert_eq!(q.deadline(), None, "nothing pending: no timer");
        assert!(texts(&rows(&l, &q))[0].contains(" 30%"), "pw-dump's own level again");
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Brightness;
        let burst: Vec<QuickAction> = (0..10).map(|i| q.key(&l, Key::Right, i > 0, t + Duration::from_millis(15 * i))).collect();
        assert_eq!(burst[0], set_brightness(80), "75 % + 5");
        assert!(burst[1..].iter().all(|a| *a == QuickAction::Redraw), "{burst:?}");
        assert_eq!(q.due(t + COALESCE), Due { run: vec![set_brightness(100)], ..Due::default() });
        assert_eq!(q.due(t + COALESCE * 2), Due { backlight: true, ..Due::default() }, "quiet: the re-read");
        assert_eq!(q.key(&l, Key::Left, false, t + COALESCE * 3), set_brightness(95), "a new window: at once");
        assert_eq!(q.key(&l, Key::Left, true, t + COALESCE * 3), QuickAction::Redraw);
        assert_eq!(q.flush(), vec![set_brightness(90)], "closing sends what waits");
        assert_eq!(q.flush(), vec![], "once");
    }

    /// Fix round 2: wpctl and pw-dump (or brightnessctl) slower than a window. The level sent stays
    /// until the live source reports it, or SETTLE passes: a stale reading never makes the next
    /// press step backwards.
    #[test]
    fn a_stale_live_level_never_makes_the_next_press_step_backwards() {
        let mut w = World::full();
        w.sound.sink = vol(30, false);
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        assert_eq!(q.key(&w.live(), Key::Right, false, t), set_volume(35));
        assert_eq!(q.key(&w.live(), Key::Right, true, t + Duration::from_millis(10)), QuickAction::Redraw);
        assert_eq!(q.due(later(t, 1)).run, vec![set_volume(40)]);
        w.sound.sink = vol(35, false); // pw-dump one command behind
        q.fit(&w.live());
        assert_eq!(q.due(later(t, 2)), Due::default(), "quiet, pw-dump still behind");
        q.fit(&w.live());
        assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 2)), set_volume(45), "from 40 % shown, never 35 %");
        q.due(later(t, 3));
        w.sound.sink = vol(45, false);
        q.fit(&w.live());
        assert_eq!(q.volume_level, None, "pw-dump caught up: its level from now on");
        assert_eq!(q.deadline(), None, "and nothing left to wait for");
        q.selected = Row::Brightness;
        assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 4)), set_brightness(80));
        assert_eq!(q.due(later(t, 5)), Due { backlight: true, ..Due::default() });
        q.set_backlight(Reread::Level(75)); // read before brightnessctl wrote
        assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 5)), set_brightness(85), "from 80 % shown, never 75 %");
    }

    /// Fix round 2: the default sink switched while a level is held or waiting: that level was the
    /// old sink's; it is dropped, never sent to the new one.
    #[test]
    fn a_default_sink_switch_drops_the_held_volume() {
        let mut w = World::full();
        w.sound.sink_name = Some(crate::system::DeviceName { node: "alsa_output.ag06".into(), label: "AG06/AG03".into() });
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        assert_eq!(q.key(&w.live(), Key::Right, false, t), set_volume(69));
        assert_eq!(q.key(&w.live(), Key::Right, true, t + Duration::from_millis(10)), QuickAction::Redraw, "74 % waits");
        w.sound.sink_name = Some(crate::system::DeviceName { node: "alsa_output.speaker".into(), label: "Speaker".into() });
        w.sound.sink = vol(40, false);
        q.fit(&w.live());
        assert_eq!(q.due(later(t, 1)).run, vec![], "never sent to the new sink");
        assert_eq!(q.flush(), vec![]);
        assert!(texts(&rows(&w.live(), &q))[0].contains(" 40%"), "the new sink's level");
        assert_eq!(q.key(&w.live(), Key::Right, false, later(t, 2)), set_volume(45), "stepped from it");
    }

    #[test]
    fn brightness_shows_the_expected_level_until_its_re_read() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Brightness;
        let t = Instant::now();
        assert_eq!(q.key(&l, Key::Right, false, t), set_brightness(80));
        assert_eq!(q.backlight, backlight(80), "the expected level at once");
        assert_eq!(q.deadline(), Some(t + BRIGHTNESS_REREAD));
        assert_eq!(q.due(t + Duration::from_millis(149)), Due::default(), "not yet");
        assert_eq!(q.due(t + BRIGHTNESS_REREAD), Due { backlight: true, ..Due::default() });
        q.set_backlight(Reread::Level(80));
        assert_eq!(q.deadline(), None, "the re-read agrees: nothing pending, no timer");
        q.key(&l, Key::Left, false, later(t, 1));
        q.due(later(t, 2));
        q.set_backlight(Reread::Level(78));
        assert_eq!(q.backlight, backlight(75), "a re-read that disagrees is kept back for SETTLE");
        assert_eq!(q.due(later(t, 2) + SETTLE), Due { backlight: true, ..Due::default() }, "one more re-read");
        q.set_backlight(Reread::Level(78));
        assert_eq!(q.backlight, backlight(78), "then the re-read wins");
        assert_eq!(q.deadline(), None);
        q.backlight = backlight(98);
        q.key(&l, Key::Right, false, later(t, 2));
        assert_eq!(q.backlight, backlight(100), "never above 100");
        assert_eq!(q.press(&l, (BAR_START + 10, 1), Button::Left, later(t, 3)), set_brightness(50), "a click on its bar");
        assert_eq!(q.scroll(&l, Some((5, 1)), -2, later(t, 4)), set_brightness(60));
    }

    /// Mae's ruling M2: the panel never takes the backlight below 1 % (a black screen), by a key,
    /// the wheel or a click.
    #[test]
    fn brightness_never_goes_below_one_percent() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Brightness;
        let t = Instant::now();
        q.backlight = backlight(3);
        assert_eq!(q.key(&l, Key::Left, false, later(t, 0)), set_brightness(1), "3 % − 5: 1 %");
        assert_eq!(q.backlight, backlight(1));
        assert_eq!(q.key(&l, Key::Left, false, later(t, 1)), set_brightness(1), "at 1 %: stays");
        q.backlight = backlight(40);
        assert_eq!(q.scroll(&l, Some((5, 1)), 10, later(t, 2)), set_brightness(1), "ten notches down");
        assert_eq!(q.press(&l, (BAR_START, 1), Button::Left, later(t, 3)), set_brightness(4), "the bar's first cell");
        q.backlight = backlight(0);
        assert_eq!(q.key(&l, Key::Right, false, later(t, 4)), set_brightness(5), "from a dark backlight, up");
    }

    /// Fix m4: the wheel counts ten notches at most.
    #[test]
    fn the_wheel_takes_ten_notches_at_most() {
        let mut w = World::full();
        w.sound.sink = vol(30, false);
        let l = w.live();
        let mut q = quick(&w, Focus::Sound);
        let t = Instant::now();
        assert_eq!(q.scroll(&l, Some((5, 0)), -15, t), set_volume(80), "15 notches of 5: 50, not 75");
        q.selected = Row::Brightness;
        q.backlight = backlight(80);
        assert_eq!(q.scroll(&l, Some((5, 1)), 15, t), set_brightness(30), "15 notches down: 50");
    }

    /// Fix m3: `BAR` is the width `osd::level` draws, from `BAR_START` (the click mapping counts on it).
    #[test]
    fn the_drawn_bar_spans_bar_start_and_its_bar_cells() {
        let mut g = Grid::empty(WIDTH + 2);
        slider(&mut g, BRIGHTNESS, Volume { percent: 100, muted: false }, false, false);
        let styles: Vec<char> = g.styles().chars().collect();
        let (first, end) = (usize::from(BAR_START), usize::from(BAR_START + BAR));
        assert!(styles[first..end].iter().all(|c| *c == 'a'), "{}", g.styles());
        assert_ne!(styles[first - 1], 'a', "the cell before the bar: {}", g.styles());
        assert_ne!(styles[end], 'a', "the cell after it: {}", g.styles());
    }

    #[test]
    fn the_toggles_run_their_command_and_re_read_their_state() {
        let mut w = World::full();
        w.config.keep_awake.toggle = vec!["fake-keep-awake".into()];
        w.config.night_light.toggle = vec!["fake-night".into(), "--toggle".into()];
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        let t = Instant::now();
        let mic = run(&["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"], "microphone mute");
        q.selected = Row::Mic;
        assert_eq!(q.key(&l, Key::Char(' '), false, t), mic);
        assert_eq!(q.key(&l, Key::Enter, false, t), mic);
        q.selected = Row::Dnd;
        assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::ToggleDnd, "set_dnd's one path, in the wiring");
        assert_eq!(q.key(&l, Key::Char(' '), true, t), QuickAction::Nothing, "Review Focus 5: a held Space acts once");
        q.selected = Row::KeepAwake;
        assert_eq!(q.key(&l, Key::Enter, false, t), run(&["fake-keep-awake"], "keep awake"), "run as given");
        q.selected = Row::NightLight;
        assert_eq!(q.key(&l, Key::Char(' '), false, t + Duration::from_millis(100)), run(&["fake-night", "--toggle"], "night light"));
        assert_eq!(q.deadline(), Some(t + TOGGLE_REREAD), "keep awake's re-read comes first");
        assert_eq!(q.due(t + TOGGLE_REREAD), Due { toggles: [true, false], ..Due::default() });
        assert_eq!(q.due(t + TOGGLE_REREAD + Duration::from_millis(100)), Due { toggles: [false, true], ..Due::default() });
        q.selected = Row::Volume;
        assert_eq!(q.press(&l, (10, 4), Button::Left, t), QuickAction::ToggleDnd, "a left click on a row is Enter");
        assert_eq!(q.selected, Row::Dnd);
    }

    #[test]
    fn enter_cycles_the_power_profile_and_space_does_not() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Profile;
        let t = Instant::now();
        assert_eq!(q.key(&l, Key::Enter, false, t), run(&["powerprofilesctl", "set", "performance"], "profile performance"));
        assert_eq!(q.key(&l, Key::Char(' '), false, t), QuickAction::Nothing, "decision 8");
        let top = ProfileState { active: Some(Profile::Performance), offered: Profile::ALL.to_vec() };
        assert_eq!(
            q.key(&Live { profile: Some(&top), ..l }, Key::Enter, false, t),
            run(&["powerprofilesctl", "set", "power-saver"], "profile power-saver"),
            "wrapping"
        );
        let two = ProfileState { active: Some(Profile::Balanced), offered: vec![Profile::PowerSaver, Profile::Balanced] };
        assert_eq!(
            q.key(&Live { profile: Some(&two), ..l }, Key::Enter, false, t),
            run(&["powerprofilesctl", "set", "power-saver"], "profile power-saver"),
            "only what the daemon offers"
        );
    }

    #[test]
    fn reboot_and_power_off_ask_first_and_the_question_expires() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Session;
        let t = Instant::now();
        let lock = QuickAction::RunAndClose { argv: argv(&["loginctl", "lock-session"]), what: "lock".into() };
        assert_eq!(q.key(&l, Key::Enter, false, t), lock, "lock: at once");
        q.button = Session::Reboot;
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "the first activation asks");
        assert_eq!(q.confirm, Some((Session::Reboot, t + CONFIRM)));
        let b = rows(&l, &q);
        assert_eq!(texts(&b)[7], format!(" {REBOOT} Reboot? ⏎"));
        assert_eq!(b.rows[7].styles()[1..12], "w".repeat(11), "in warn");
        assert_eq!(q.deadline(), Some(t + CONFIRM), "the timer waits for it");
        let early = t + Duration::from_millis(200);
        assert_eq!(q.key(&l, Key::Enter, false, early), QuickAction::Nothing, "Mae (M1): a double activation never confirms");
        assert_eq!(q.press(&l, (20, SESSION_LINE), Button::Left, early), QuickAction::Nothing, "nor a double click");
        assert_eq!(q.confirm, Some((Session::Reboot, t + CONFIRM)), "the question stays, its 3 s unchanged");
        assert_eq!(
            q.key(&l, Key::Enter, false, t + Duration::from_millis(500)),
            QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
            "0.5 s later: it runs"
        );
        q.button = Session::Reboot;
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
        assert_eq!(
            q.key(&l, Key::Enter, false, t + Duration::from_secs(2)),
            QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
            "the second within 3 s runs it"
        );
        q.button = Session::Poweroff;
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
        assert_eq!(q.due(t + CONFIRM), Due { redraw: true, ..Due::default() }, "3 s later the question goes");
        assert_eq!(q.confirm, None);
        assert_eq!(q.key(&l, Key::Enter, false, t + CONFIRM), QuickAction::Redraw, "the next Enter asks again");
        assert_eq!(texts(&rows(&l, &q))[7], format!(" {POWER} Power off? ⏎"));
    }

    /// Review Focus 3.
    #[test]
    fn moving_away_cancels_the_confirmation_and_coming_back_asks_again() {
        let w = World::full();
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.selected = Row::Session;
        q.button = Session::Reboot;
        let t = Instant::now();
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw);
        assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw, "← moves to lock");
        assert_eq!((q.confirm, q.button), (None, Session::Lock), "and cancels");
        q.key(&l, Key::Right, false, t);
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "back on reboot: it asks again, never runs");
        assert_eq!(q.key(&l, Key::Up, false, t), QuickAction::Redraw);
        assert_eq!(q.confirm, None, "↑ cancels too");
        q.selected = Row::Session;
        q.key(&l, Key::Enter, false, t);
        assert_eq!(q.press(&l, (10, DND_LINE), Button::Left, t), QuickAction::ToggleDnd, "a press on another row");
        assert_eq!(q.confirm, None, "cancels it");
        assert_eq!(q.press(&l, (SESSION_AT + SESSION_GAP, SESSION_LINE), Button::Left, t), QuickAction::Redraw, "a press on reboot asks");
        assert_eq!(
            q.press(&l, (20, SESSION_LINE), Button::Left, t + CONFIRM_AFTER),
            QuickAction::RunAndClose { argv: argv(&["systemctl", "reboot"]), what: "reboot".into() },
            "anywhere on the question: the second activation"
        );
    }

    #[test]
    fn the_wheel_elsewhere_moves_the_window_only_when_the_lines_do_not_fit() {
        let w = World::full();
        let l = w.live();
        let t = Instant::now();
        let mut q = quick(&w, Focus::Top);
        assert_eq!(q.scroll(&l, Some((10, 4)), 1, t), QuickAction::Nothing, "everything fits");
        let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 3);
        assert_eq!(q.scroll(&l, Some((10, 2)), 1, t), QuickAction::Redraw, "on the separator, three rows of room");
        assert_eq!(q.top, 1);
        assert_eq!(q.scroll(&l, Some((10, 2)), 10, t), QuickAction::Redraw);
        assert_eq!(q.top, 5, "clamped: the last three lines");
        assert_eq!(q.scroll(&l, Some((10, 2)), -1, t), QuickAction::Redraw);
        assert_eq!(q.top, 4);
        assert_eq!(q.scroll(&l, None, 0, t), QuickAction::Nothing);
    }

    #[test]
    fn a_program_that_fails_is_noted_once_per_open() {
        let w = World::full();
        let mut q = quick(&w, Focus::Top);
        assert!(q.note_failure("wpctl"));
        assert!(!q.note_failure("wpctl"), "logged once");
        assert!(q.note_failure("brightnessctl"));
    }

    const HOME: &str = "11111111-2222-4333-8444-555555555555";

    fn home() -> AccessPoint {
        AccessPoint { ssid: b"Maison-Test".to_vec(), strength: 80, security: Security::Psk }
    }

    /// Maison-Test (80 %, saved), Bureau-Test (60 %, WPA2); `connected` to the first; `listed`: the
    /// detail's lists read.
    fn wifi_state(enabled: bool, connected: bool, listed: bool) -> WifiState {
        let aps = vec![home(), AccessPoint { ssid: b"Bureau-Test".to_vec(), strength: 60, security: Security::Psk }];
        WifiState {
            enabled,
            device: Some(WifiDevice {
                state: if connected { 100 } else { 30 },
                active: connected.then(home),
                active_uuid: connected.then(|| HOME.to_string()),
                access_points: listed.then_some(aps),
                saved: listed.then(|| vec![SavedWifi { uuid: HOME.into(), ssid: b"Maison-Test".to_vec() }]),
            }),
            stale: false,
        }
    }

    fn run_of(parts: &[&str], what: &str) -> QuickAction {
        QuickAction::Run { argv: argv(parts), what: what.into() }
    }

    #[test]
    fn the_wifi_row_in_every_state() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        assert!(!shown(&l, &q).contains(&Row::Wifi), "nothing read yet: hidden");
        q.set_wifi(Some(WifiState { enabled: true, device: None, stale: false }), t);
        assert!(!shown(&l, &q).contains(&Row::Wifi), "no Wi-Fi device: hidden");
        q.set_wifi(Some(wifi_state(true, true, false)), t);
        assert_eq!(shown(&l, &q), [Row::Volume, Row::Brightness, Row::Wifi, Row::Mic, Row::Dnd, Row::Profile, Row::Session]);
        assert_eq!(lines(&l, &q)[2..4], [Line::Separator, Line::Row(Row::Wifi)], "first under the sliders' separator");
        assert_eq!(texts(&rows(&l, &q))[3], format!(" {} Wi-Fi{}Maison-Test ›", STRENGTH[3], " ".repeat(12)), "80 %: four steps");
        q.set_wifi(Some(wifi_state(true, false, false)), t);
        assert_eq!(texts(&rows(&l, &q))[3], format!(" {WIFI_ON} Wi-Fi{}on ›", " ".repeat(21)));
        q.set_wifi(Some(wifi_state(false, false, false)), t);
        assert_eq!(texts(&rows(&l, &q))[3], format!(" {WIFI_OFF} Wi-Fi{}off ›", " ".repeat(20)));
        assert!(describe(&l, &q).starts_with("rows: volume brightness wifi mic"), "{}", describe(&l, &q));
        q.set_wifi(None, t);
        assert!(!shown(&l, &q).contains(&Row::Wifi), "NetworkManager gone: hidden");
    }

    #[test]
    fn space_toggles_the_radio_and_enter_right_or_a_click_open_the_detail() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        let rescan = run_of(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
        let mut q = quick(&w, Focus::Top);
        q.set_wifi(Some(wifi_state(true, true, true)), t);
        q.selected = Row::Wifi;
        assert_eq!(q.key(&l, Key::Char(' '), false, t), run_of(&["nmcli", "radio", "wifi", "off"], "wifi off"));
        assert_eq!(q.key(&l, Key::Enter, false, t), rescan, "one rescan as it opens");
        assert!(q.wifi.open && lines(&l, &q)[0] == Line::WifiHeader);
        assert_eq!(q.wifi_scope(), WifiScope::Detail);
        assert_eq!(q.key(&l, Key::Escape, false, t), QuickAction::Redraw);
        assert!(!q.wifi.open && q.selected == Row::Wifi, "Escape: back on the Wi-Fi row, the panel open");
        assert_eq!(q.wifi_scope(), WifiScope::Row);
        assert_eq!(q.key(&l, Key::Right, false, t), rescan, "→ opens it too");
        assert_eq!(q.key(&l, Key::Left, false, t), QuickAction::Redraw);
        assert!(!q.wifi.open, "← back");
        q.selected = Row::Volume;
        assert_eq!(q.press(&l, (5, 3), Button::Left, t), rescan, "a click on the Wi-Fi row");
        assert_eq!(q.press(&l, (5, 0), Button::Left, t), QuickAction::Redraw, "a click on the header goes back");
        assert!(!q.wifi.open && q.selected == Row::Wifi);
        q.set_wifi(Some(wifi_state(false, false, false)), t);
        assert_eq!(q.key(&l, Key::Char(' '), false, t), run_of(&["nmcli", "radio", "wifi", "on"], "wifi on"));
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "off: the detail opens, no rescan");
        assert_eq!(texts(&rows(&l, &q))[2].trim(), OFF_NOTE);
    }

    /// Quick Wi-Fi spec § 1, decision 12: from the network module (or `--quick wifi`), the first Wi-Fi
    /// state with a device selects the row and opens the detail, once.
    #[test]
    fn the_network_module_opens_the_detail_once_the_state_arrives() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        let rescan = run_of(&["nmcli", "device", "wifi", "rescan"], "wifi rescan");
        let mut q = quick(&w, Focus::Wifi);
        assert_eq!(q.selected, Row::Volume, "nothing read yet: the first row");
        assert_eq!(q.set_wifi(Some(wifi_state(true, true, false)), t), WifiNews { action: rescan, failed: None });
        assert!(q.wifi.open && q.selected == Row::Wifi);
        q.key(&l, Key::Escape, false, t);
        assert_eq!(q.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "once only");
        assert!(!q.wifi.open);
        let mut none = quick(&w, Focus::Wifi);
        assert_eq!(none.set_wifi(Some(WifiState { enabled: true, device: None, stale: false }), t).action, QuickAction::Redraw);
        assert!(!none.wifi.open && none.selected == Row::Volume, "no device: nothing to open");
        none.set_wifi(Some(wifi_state(true, true, false)), t);
        assert!(none.wifi.open && none.selected == Row::Wifi, "final review B-M1: spent only on a state with a device");
        let mut top = quick(&w, Focus::Top);
        assert_eq!(top.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "the top: no detail");
    }

    /// F3 m2: the focus asked at open waits for the user only until they act. Their first key,
    /// click or wheel notch drops it: a device appearing later moves nothing and opens no detail.
    #[test]
    fn the_users_first_key_or_click_drops_a_pending_wifi_focus() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        let no_device = || Some(WifiState { enabled: true, device: None, stale: false });
        let mut keyed = quick(&w, Focus::Wifi);
        keyed.set_wifi(no_device(), t);
        keyed.key(&l, Key::Down, false, t);
        let chosen = keyed.selected;
        assert_eq!(keyed.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "a key: no rescan");
        assert!(!keyed.wifi.open && keyed.selected == chosen, "a key: the selection stays the user's");
        let mut clicked = quick(&w, Focus::Wifi);
        clicked.set_wifi(no_device(), t);
        clicked.press(&l, (5, 1), Button::Left, t);
        let chosen = clicked.selected;
        assert_eq!(clicked.set_wifi(Some(wifi_state(true, true, false)), t).action, QuickAction::Redraw, "a click: no rescan");
        assert!(!clicked.wifi.open && clicked.selected == chosen, "a click: the selection stays the user's");
        let mut wheeled = quick(&w, Focus::Wifi);
        wheeled.set_wifi(no_device(), t);
        wheeled.scroll(&l, Some((5, 1)), 1, t);
        wheeled.set_wifi(Some(wifi_state(true, true, false)), t);
        assert!(!wheeled.wifi.open, "a wheel notch: no detail");
        let mut untouched = quick(&w, Focus::Wifi);
        untouched.set_wifi(no_device(), t);
        untouched.key(&l, Key::Enter, true, t);
        untouched.set_wifi(Some(wifi_state(true, true, false)), t);
        assert!(untouched.wifi.open, "an auto-repeat ignored by the panel is no act of the user's");
    }

    #[test]
    fn the_window_follows_the_selection_in_the_wifi_detail() {
        let config = QuickConfig { wifi_more: Some(vec!["fake-more".into()]), ..QuickConfig::default() };
        let (w, t) = (World::full(), Instant::now());
        let mut l = w.live();
        l.config = &config;
        let mut s = wifi_state(true, false, true);
        let aps: Vec<AccessPoint> = (0..20)
            .map(|n| AccessPoint { ssid: format!("Reseau-Test-{n:02}").into_bytes(), strength: 50, security: Security::Psk })
            .collect();
        s.device.as_mut().expect("a device").access_points = Some(aps);
        let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 6);
        q.set_wifi(Some(s), t);
        q.selected = Row::Wifi;
        q.key(&l, Key::Enter, false, t);
        q.key(&l, Key::End, false, t);
        let b = rows(&l, &q);
        assert_eq!(b.rows.len(), 6, "the room under the bar");
        assert_eq!(texts(&b).last().map(|s| s.trim().to_string()), Some("More…".to_string()), "More… in view: {:?}", texts(&b));
    }

    /// Task 5 review: a Wi-Fi state while open (`follow_wifi`) goes through `fit`; the window
    /// follows the selection only when the state moved it (a reorder), never undoing a wheel scroll
    /// otherwise; a state that closes the detail (the device gone) brings the window back to the top.
    #[test]
    fn a_wifi_state_moves_the_window_with_the_selection_and_a_closed_detail_goes_back_to_the_top() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        // Twenty networks at 50 %, Labo-Test (a name short enough to show whole) at `first`:
        // first in the list at 90, last at 10.
        let around = |first: u8| {
            let aps = (0..20)
                .map(|n| AccessPoint {
                    ssid: if n == 0 { b"Labo-Test".to_vec() } else { format!("Reseau-Test-{n:02}").into_bytes() },
                    strength: if n == 0 { first } else { 50 },
                    security: Security::Psk,
                })
                .collect();
            let device = WifiDevice { state: 30, active: None, active_uuid: None, access_points: Some(aps), saved: Some(vec![]) };
            WifiState { enabled: true, device: Some(device), stale: false }
        };
        let mut q = Quick::new(Focus::Top, &l, two_sinks(), backlight(75), [None, None], 6);
        q.follow_wifi(&l, Some(around(90)), t);
        q.selected = Row::Wifi;
        q.key(&l, Key::Enter, false, t);
        q.key(&l, Key::Home, false, t);
        assert_eq!(q.top, 0);
        let picked = |q: &Quick| texts(&rows(&l, q)).iter().any(|r| r.contains("Labo-Test"));
        assert!(picked(&q), "first: in view");
        q.follow_wifi(&l, Some(around(10)), t);
        assert!(q.top > 0 && picked(&q), "moved to the end by the state: the window follows it ({})", q.top);
        q.scroll(&l, None, -3, t);
        let scrolled = q.top;
        assert!(!picked(&q), "scrolled away from the selection");
        q.follow_wifi(&l, Some(around(10)), t);
        assert_eq!(q.top, scrolled, "a state that moves nothing keeps the wheel's window");
        q.follow_wifi(&l, Some(WifiState { enabled: true, device: None, stale: false }), t);
        assert!(!q.wifi.open, "the device gone: the detail closed");
        assert_eq!(q.top, 0, "back to the top");
    }

    /// The panel's one timer covers the connect timeout and the `failed` display (quick Wi-Fi § 5).
    #[test]
    fn the_wifi_attempt_rides_the_panel_s_one_timer() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.set_wifi(Some(wifi_state(true, true, true)), t);
        q.selected = Row::Wifi;
        q.key(&l, Key::Enter, false, t);
        q.key(&l, Key::Down, false, t);
        assert_eq!(q.key(&l, Key::Enter, false, t), QuickAction::Redraw, "Bureau-Test: the field");
        for c in "fake-pass".chars() {
            q.key(&l, Key::Char(c), false, t);
        }
        assert!(matches!(q.key(&l, Key::Enter, false, t), QuickAction::RunSecret { .. }));
        assert_eq!(q.deadline(), Some(t + crate::quick_wifi::CONNECT_TIMEOUT));
        let d = q.due(t + crate::quick_wifi::CONNECT_TIMEOUT);
        assert_eq!((d.wifi_failed, d.redraw), (Some("no answer in 20 s"), true));
        assert_eq!(q.deadline(), Some(t + crate::quick_wifi::CONNECT_TIMEOUT + crate::quick_wifi::FAILED_SHOWN));
    }

    /// Fix round 1 (minor 5): a held Backspace keeps erasing in the password field; elsewhere it
    /// does not repeat, nor does a held character.
    #[test]
    fn backspace_repeats_in_the_password_field_only() {
        let (w, t) = (World::full(), Instant::now());
        let l = w.live();
        let mut q = quick(&w, Focus::Top);
        q.set_wifi(Some(wifi_state(true, true, true)), t);
        q.selected = Row::Wifi;
        q.key(&l, Key::Enter, false, t);
        q.key(&l, Key::Down, false, t);
        q.key(&l, Key::Enter, false, t);
        for c in "abc".chars() {
            q.key(&l, Key::Char(c), false, t);
        }
        assert_eq!(q.key(&l, Key::Char('d'), true, t), QuickAction::Nothing, "a held character: once");
        assert_eq!(q.key(&l, Key::Backspace, true, t), QuickAction::Redraw, "a held Backspace erases");
        assert_eq!(q.wifi.password().map(|s| s.bytes()), Some(&b"ab"[..]));
        q.key(&l, Key::Escape, false, t);
        assert_eq!(q.key(&l, Key::Backspace, true, t), QuickAction::Nothing, "no field: no repeat");
    }
}
