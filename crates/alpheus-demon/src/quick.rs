//! The quick settings panel (quick spec), pure: which rows show for the hardware and the
//! configuration, the selection kept by identity, the keys, the clicks and the wheel, the outputs
//! detail, the reboot and power-off confirmation, the re-reads it waits for, and the exact commands
//! to run. The popup (`PopupBody::Quick`), the reads (sysfs, `/proc`: `quick_state.rs`) and the
//! commands themselves are the wiring's (`quick_app.rs`): nothing here reads a file, starts a
//! thread or runs a command.

use crate::config::{ModuleId, QuickConfig, ToggleConfig};
use crate::grid::{Grid, Style, Target, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed};
use crate::osd::{self, BRIGHTNESS, MIC_OFF, MIC_ON, PERCENT_CELLS, volume_glyph};
use crate::segments::BELL;
use crate::state::{Bar, Feed};
use crate::system::{ProfileState, Sink, SoundState, Volume};
use std::time::Instant;

/// Content width in cells; a row is two more (a blank pad cell each side, as the other popups').
pub const WIDTH: u16 = 32;
/// A slider's bar: its first column (Boxed coordinates: column 0 is the pad) and its cells.
pub const BAR_START: u16 = 3;
pub const BAR: u16 = 22;
/// The percentage's first cell ("{:>3}%", `osd::PERCENT_CELLS`).
pub const PERCENT_AT: u16 = 26;
/// The volume's `›` (the outputs detail), on the column where the text rows end.
pub const MORE_AT: u16 = 32;
/// The session buttons' first glyph column, and the gap from one to the next.
pub const SESSION_AT: u16 = 3;
pub const SESSION_GAP: u16 = 5;
/// wpctl's targets: the default output, as the bar's own clicks name it, and the default input.
pub const SINK: &str = crate::actions::SINK;
pub const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";
// The mockup's glyphs (spec § 2): Nerd Font Material Design icons, and the power symbol.
pub const PROFILE: char = '\u{f0f85}';
pub const KEEP_AWAKE: char = '\u{f0176}';
pub const NIGHT_LIGHT: char = '\u{f0594}';
pub const LOCK: char = '\u{f033e}';
pub const LOGOUT: char = '\u{f0343}';
pub const REBOOT: char = '\u{f0709}';
pub const POWER: char = '\u{23fb}';

/// Where the panel opens (spec § 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The first row: a click on the network or Bluetooth module (their own sections come in
    /// deliveries 2 and 3), or the bare verb.
    Top,
    /// The volume row: a click on the volume module, or `--quick sound`.
    Sound,
}

impl Focus {
    /// The section a left click on `target` opens.
    pub fn of(target: &Target) -> Focus {
        if *target == Target::Module(ModuleId::Volume) { Focus::Sound } else { Focus::Top }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Volume,
    Brightness,
    Mic,
    Dnd,
    Profile,
    KeepAwake,
    NightLight,
    Session,
}

impl Row {
    /// Top to bottom (spec § 2): also the order a selection falls back along when its row goes.
    pub const ORDER: [Row; 8] =
        [Row::Volume, Row::Brightness, Row::Mic, Row::Dnd, Row::Profile, Row::KeepAwake, Row::NightLight, Row::Session];

    /// For the log line "quick shown (rows: …)".
    pub fn name(self) -> &'static str {
        match self {
            Row::Volume => "volume",
            Row::Brightness => "brightness",
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
}

/// A session button (spec § 2), left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Lock,
    Logout,
    Reboot,
    Poweroff,
}

impl Session {
    pub const ALL: [Session; 4] = [Session::Lock, Session::Logout, Session::Reboot, Session::Poweroff];

    /// Its configured command; empty: the button is not drawn.
    pub fn command(self, c: &QuickConfig) -> &[String] {
        match self {
            Session::Lock => &c.lock,
            Session::Logout => &c.logout,
            Session::Reboot => &c.reboot,
            Session::Poweroff => &c.poweroff,
        }
    }

    fn glyph(self) -> char {
        match self {
            Session::Lock => LOCK,
            Session::Logout => LOGOUT,
            Session::Reboot => REBOOT,
            Session::Poweroff => POWER,
        }
    }

    /// The question while it waits for its second activation: "Reboot? ⏎".
    fn prompt(self) -> &'static str {
        match self {
            Session::Lock => "Lock",
            Session::Logout => "Log out",
            Session::Reboot => "Reboot",
            Session::Poweroff => "Power off",
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
    /// When to re-read the backlight, and each toggle's state (Task 5 arms them).
    pub reread_backlight: Option<Instant>,
    pub reread_toggles: [Option<Instant>; 2],
    /// The first line shown, and the rows the panel may take under its bar.
    pub top: usize,
    pub room: u16,
    /// Programs that failed to start during this open: each logged once (spec § 6).
    pub failed: Vec<String>,
}

fn argv(parts: &[&str]) -> Vec<String> {
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
        Row::Mic => live.sound.is_some_and(|s| s.source.is_some()),
        Row::Dnd => live.dnd.is_some(),
        Row::Profile => live.profile.is_some_and(|p| p.active.is_some() && !p.offered.is_empty()),
        Row::KeepAwake => !live.config.keep_awake.toggle.is_empty(),
        Row::NightLight => !live.config.night_light.toggle.is_empty(),
        Row::Session => Session::ALL.iter().any(|s| !s.command(live.config).is_empty()),
    }
}

/// The rows shown, top to bottom.
pub fn shown(live: &Live, q: &Quick) -> Vec<Row> {
    Row::ORDER.into_iter().filter(|r| present(*r, live, q)).collect()
}

/// The lines drawn: the rows with a separator between two blocks; in the detail, its header, a
/// separator and the sinks.
pub fn lines(live: &Live, q: &Quick) -> Vec<Line> {
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
            reread_backlight: None,
            reread_toggles: [None; 2],
            top: 0,
            room,
            failed: Vec::new(),
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
        Session::ALL.into_iter().filter(|s| !s.command(live.config).is_empty()).collect()
    }

    /// Before each use: the detail closed when fewer than two sinks are left, its sink found again
    /// by id (else the default); the selection back on a shown row (the next one down the order when
    /// its own went, else the one above); the session button on a drawn one.
    pub fn fit(&mut self, live: &Live) {
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

    /// After a key: the window over the lines keeps the selection in view.
    fn keep_in_view(&mut self, live: &Live) {
        let all = lines(live, self);
        let at = all.iter().position(|l| match *l {
            Line::Row(r) => r == self.selected,
            Line::Sink(i) => self.sinks.get(i).is_some_and(|s| Some(s.id) == self.sink),
            _ => false,
        });
        if let Some(at) = at {
            self.top = list::keep_visible(at, self.top, self.visible(), all.len());
        }
    }

    /// A key (spec § 3). `repeat`: the keyboard's auto-repeat, which only ↑ ↓ ← → (and Ctrl+P,
    /// Ctrl+N) follow: a held Space or Enter acts once (Review Focus 5).
    pub fn key(&mut self, live: &Live, k: Key, repeat: bool, now: Instant) -> QuickAction {
        if repeat && !matches!(k, Key::Up | Key::Down | Key::Left | Key::Right | Key::CtrlP | Key::CtrlN) {
            return QuickAction::Nothing;
        }
        self.fit(live);
        let action = if self.detail { self.detail_key(k) } else { self.rows_key(live, k, now) };
        self.keep_in_view(live);
        action
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

    /// Space, Enter, ← → on a row: Task 5.
    fn act(&mut self, _live: &Live, _k: Key, _now: Instant) -> QuickAction {
        QuickAction::Nothing
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

    /// A re-read of the backlight: its level now; None: the device went, and its row with it.
    pub fn set_backlight(&mut self, percent: Option<u8>) {
        match (percent, self.backlight.as_mut()) {
            (Some(p), Some(b)) => b.percent = p,
            (None, _) => self.backlight = None,
            (Some(_), None) => {}
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
                slider(g, volume_glyph(v), v, q.sinks.len() >= 2, selected);
            }
        }
        Row::Brightness => {
            if let Some(b) = &q.backlight {
                slider(g, BRIGHTNESS, Volume { percent: b.percent, muted: false }, false, selected);
            }
        }
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
/// whole bar in warn. Selected: the band behind the glyph, the percentage and the arrow; the bar
/// keeps its colours (decision 6).
fn slider(g: &mut Grid, glyph: char, v: Volume, more: bool, selected: bool) {
    let end = g.width();
    if selected {
        g.write(0, end, &" ".repeat(usize::from(end)), Style::SELECTED);
    }
    osd::level(g, BAR_START - 2, PERCENT_AT + PERCENT_CELLS, glyph, v, selected);
    if more {
        g.write(MORE_AT, MORE_AT + 1, "›", if selected { Style::SELECTED } else { Style::MUTED });
    }
}

/// A text row as a menu draws it (`list::row`: the icon in the accent, the label), then `right`
/// right-aligned one cell from the end in its own style.
fn text_row(g: &mut Grid, icon: Option<char>, label: &str, right: &str, right_style: Style, selected: bool) {
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
        g.write(1, end.saturating_sub(1), &format!("{} {}? ⏎", s.glyph(), s.prompt()), Style::WARN);
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
    use crate::segments::VOLUME_MEDIUM;
    use crate::system::Profile;

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
        assert_eq!(
            b.rows[0].styles(),
            format!("SSS{}{}{}", "a".repeat(14), "m".repeat(8), "S".repeat(9)),
            "selected: the band around the bar, the bar in its colours (decision 6)"
        );
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
        assert!(b.rows[0].styles()[3..25].chars().all(|c| c == 'w'), "above 100 %: the whole bar in warn: {}", b.rows[0].styles());
        let mut muted = World::full();
        muted.sound.sink = vol(64, true);
        let b = rows(&muted.live(), &quick(&muted, Focus::Top));
        assert!(b.rows[0].styles()[3..25].chars().all(|c| c == 'm'), "muted: the bar dimmed");
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
        assert_eq!(Focus::of(&Target::Module(ModuleId::Network)), Focus::Top);
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
        q.set_backlight(Some(40));
        assert_eq!(q.backlight, backlight(40));
        q.set_backlight(None);
        assert!(!shown(&w.live(), &q).contains(&Row::Brightness), "the device went: its row with it");
        q.set_toggle(Toggle::NightLight, Some(true));
        assert_eq!(q.toggles, [None, Some(true)]);
    }
}
