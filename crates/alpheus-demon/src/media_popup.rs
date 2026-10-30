//! The media popup (spec § 2), pure: its rows as cells (title, artist, elapsed time with a progress
//! bar and the length, the ⏮ ⏯ ⏭ buttons, then the players with ● on the active one when there are
//! two or more), its keys and its mouse hit map, and the rule for its 1 s redraw tick. The popup
//! itself (`PopupBody::Media`) and the D-Bus calls are the wiring's: nothing here talks to a bus,
//! starts a thread or runs a command.

use crate::grid::{Grid, Style, text_width};
use crate::keys::Key;
use crate::list::{self, Boxed};
use crate::media::{self, Control, Player, Players, Status};
use crate::segments::{PAUSE, PLAY, SKIP_NEXT, SKIP_PREVIOUS};
use std::time::{Duration, Instant};

/// Content width in cells (the spec's mockup); `list::padded` adds one blank cell each side.
pub const WIDTH: u16 = 30;
/// Added to every tick so it lands just past the second it waits for, never a hair before it (a
/// tick that early would redraw the same rows and ask for another tick a microsecond later).
pub const SLACK: Duration = Duration::from_millis(1);
/// The buttons, left to right.
const CONTROLS: [Control; 3] = [Control::Previous, Control::PlayPause, Control::Next];
/// A button takes this many cells either side of its glyph for a click.
const REACH: u16 = 2;

/// One row of the popup, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Title,
    Artist,
    Time,
    Buttons,
    /// A blank row the painter draws as a 1 px line.
    Separator,
    /// The n-th of `Players::shown()`.
    Player(usize),
}

/// What is selected: a row of `lines()`, and the button when that row is the buttons'.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPopup {
    pub selected: usize,
    /// 0 ⏮, 1 ⏯, 2 ⏭.
    pub button: usize,
    /// What `selected` points at, kept across changes: `fit` finds its row again when rows or
    /// players come or go above it.
    pub chosen: Option<Chosen>,
}

/// The selected row by what it shows, not by its number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    Buttons,
    /// A player, by bus name.
    Player(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaAction {
    Nothing,
    Redraw,
    Close,
    /// To the active player (`Mpris::control`, which checks the capability again).
    Control(Control),
    /// This player becomes the active one (spec § 3, the pin), by bus name.
    Pin(String),
}

/// The active player if it is shown (never a stopped one).
fn shown_active(players: &Players) -> Option<&Player> {
    players.active().filter(|p| p.status != Status::Stopped)
}

/// The length when it is known: missing, 0 or a stream (no length) all hide the time row.
fn length(p: &Player) -> Option<u64> {
    p.track.length_us.filter(|&l| l > 0)
}

/// Whether button `b` can do anything for `p`: `Player::can` on its control (⏯ asks CanPause while
/// playing, CanPlay otherwise).
fn capable(p: &Player, b: usize) -> bool {
    CONTROLS.get(b).is_some_and(|c| p.can(*c))
}

/// The glyph columns of the three buttons in a row `width` cells wide: centred, five cells apart.
fn button_columns(width: u16) -> [u16; 3] {
    let c = width / 2;
    [c.saturating_sub(5), c, c.saturating_add(5)]
}

/// The button whose cells hold content column `x`.
fn button_at(x: u16, width: u16) -> Option<usize> {
    button_columns(width).iter().position(|&c| x.abs_diff(c) <= REACH)
}

/// The popup's rows for the active player; empty when no player is shown.
pub fn lines(players: &Players) -> Vec<Line> {
    let Some(p) = shown_active(players) else { return Vec::new() };
    let mut out = vec![Line::Title];
    if !p.track.artists.is_empty() {
        out.push(Line::Artist);
    }
    if length(p).is_some() {
        out.push(Line::Time);
    }
    out.push(Line::Buttons);
    let n = players.shown().count();
    if n >= 2 {
        out.push(Line::Separator);
        out.extend((0..n).map(Line::Player));
    }
    out
}

/// Whether row `i` can be selected: the buttons when one of them is capable, every player.
fn selectable(lines: &[Line], p: &Player, i: usize) -> bool {
    match lines.get(i) {
        Some(Line::Buttons) => (0..3).any(|b| capable(p, b)),
        Some(Line::Player(_)) => true,
        _ => false,
    }
}

/// A player's name in the list (`media::player_name`): its Identity, else the first label after the
/// MPRIS prefix.
pub fn label(p: &Player) -> String {
    media::player_name(p.identity.as_deref(), &p.bus_name)
}

impl MediaPopup {
    /// At open: the buttons row on play/pause (or the nearest capable button), else the first player.
    pub fn new(players: &Players) -> MediaPopup {
        let mut popup = MediaPopup { selected: usize::MAX, button: 1, chosen: None };
        popup.fit(players);
        popup
    }

    /// After the players changed under the popup (a row added or gone, a capability lost): the
    /// chosen row is found again where it now is; gone or unselectable, the selection goes back
    /// to the buttons (or the first selectable row), an incapable button to play/pause, then
    /// next, then previous.
    pub fn fit(&mut self, players: &Players) {
        let Some(p) = shown_active(players) else { return };
        let lines = lines(players);
        if let Some(chosen) = &self.chosen {
            let line = match chosen {
                Chosen::Buttons => Some(Line::Buttons),
                Chosen::Player(name) => players.shown().position(|q| q.bus_name == *name).map(Line::Player),
            };
            self.selected = line.and_then(|l| lines.iter().position(|x| *x == l)).unwrap_or(usize::MAX);
        }
        let ok = |i: usize| selectable(&lines, p, i);
        if !ok(self.selected) {
            let buttons = lines.iter().position(|l| *l == Line::Buttons).filter(|&i| ok(i));
            self.selected = buttons.or_else(|| list::first_selectable(lines.len(), ok)).unwrap_or(0);
            // Moved: remembered anew (a fit that found its row again allocates nothing).
            self.remember(players, &lines);
        } else if self.chosen.is_none() {
            self.remember(players, &lines);
        }
        if !capable(p, self.button) {
            self.button = [1, 2, 0].into_iter().find(|&b| capable(p, b)).unwrap_or(1);
        }
    }

    /// `chosen` from `selected` (after a fit, a key, a press).
    fn remember(&mut self, players: &Players, lines: &[Line]) {
        self.chosen = match lines.get(self.selected) {
            Some(Line::Buttons) => Some(Chosen::Buttons),
            Some(Line::Player(i)) => players.shown().nth(*i).map(|q| Chosen::Player(q.bus_name.clone())),
            _ => None,
        };
    }
}

/// The popup's cells: `width` wide plus the pad cells, the separator row recorded for the painter.
pub fn rows(players: &Players, popup: &MediaPopup, now: Instant, width: u16) -> Boxed {
    let Some(p) = shown_active(players) else { return Boxed::default() };
    let lines = lines(players);
    let listed: Vec<&Player> = players.shown().collect();
    let selected = lines.get(popup.selected).copied().filter(|_| selectable(&lines, p, popup.selected));
    let content: Vec<Grid> = lines
        .iter()
        .map(|line| {
            let mut g = Grid::empty(width);
            match *line {
                Line::Title => {
                    let title = media::display_title(&p.track, p.identity.as_deref(), &p.bus_name);
                    g.write(0, width, &media::truncate_title(&title, width), Style::BOLD_ACCENT);
                }
                Line::Artist => {
                    g.write(0, width, &media::truncate_title(&p.track.artist_line(), width), Style::NORMAL);
                }
                Line::Time => time_row(&mut g, p, now, width),
                Line::Buttons => buttons_row(&mut g, p, (selected == Some(Line::Buttons)).then_some(popup.button), width),
                Line::Separator => {}
                Line::Player(i) => {
                    if let Some(q) = listed.get(i) {
                        player_row(&mut g, q, q.bus_name == p.bus_name, selected == Some(Line::Player(i)), width);
                    }
                }
            }
            g
        })
        .collect();
    let mut boxed = list::padded(&content);
    boxed.separators = lines.iter().position(|l| *l == Line::Separator).into_iter().collect();
    boxed
}

/// "1:42 ━━━━━━━━──────────── 4:05": elapsed and length muted, the elapsed part of the bar in the accent.
fn time_row(g: &mut Grid, p: &Player, now: Instant, width: u16) {
    let Some(len) = length(p) else { return };
    let pos = media::position_now(p, now).unwrap_or(0).min(len);
    let (left, right) = (media::clock(pos), media::clock(len));
    let bar = width.saturating_sub(text_width(&left).saturating_add(text_width(&right)).saturating_add(2));
    // pos ≤ len, so filled ≤ bar; u128: no overflow whatever a player reports.
    let filled = (u128::from(pos) * u128::from(bar) / u128::from(len)) as u16;
    let mut x = g.write(0, width, &left, Style::MUTED);
    x = g.write(x, width, " ", Style::NORMAL);
    x = g.write(x, width, &"━".repeat(usize::from(filled)), Style::ACCENT);
    x = g.write(x, width, &"─".repeat(usize::from(bar - filled)), Style::MUTED);
    x = g.write(x, width, " ", Style::NORMAL);
    g.write(x, width, &right, Style::MUTED);
}

/// ⏮ ⏯ ⏭ centred; an incapable one dim; the selected one (if capable) on a 3-cell selection band.
fn buttons_row(g: &mut Grid, p: &Player, selected: Option<usize>, width: u16) {
    let glyphs = [SKIP_PREVIOUS, if p.status == Status::Playing { PAUSE } else { PLAY }, SKIP_NEXT];
    for (b, (x, glyph)) in button_columns(width).into_iter().zip(glyphs).enumerate() {
        let style = if !capable(p, b) {
            Style::MUTED
        } else if selected == Some(b) {
            g.write(x.saturating_sub(1), x.saturating_add(2), "   ", Style::SELECTED);
            Style::SELECTED
        } else {
            Style::NORMAL
        };
        g.write(x, x.saturating_add(1), &glyph.to_string(), style);
    }
}

/// "● Lumen Player" for the active player, "  beta" for another; the selection band across the row.
fn player_row(g: &mut Grid, q: &Player, active: bool, selected: bool, width: u16) {
    let pick = |s: Style| if selected { Style::SELECTED } else { s };
    if selected {
        g.write(0, width, &" ".repeat(usize::from(width)), Style::SELECTED);
    }
    let x = g.write(0, width, if active { "●" } else { " " }, pick(Style::ACCENT));
    let x = g.write(x, width, " ", pick(Style::NORMAL));
    g.write(x, width, &media::truncate_title(&label(q), width.saturating_sub(x)), pick(Style::NORMAL));
}

/// What the popup shows on top, as drawn in `b` (cut to its width): "Title — Artist", or the title
/// alone without an artist row; empty when nothing is shown. For the log line "media popup shown".
pub fn heading(players: &Players, b: &Boxed) -> String {
    let drawn = |i: usize| b.rows.get(i).map(|g| g.text().trim().to_string()).unwrap_or_default();
    match lines(players).get(1) {
        Some(Line::Artist) => format!("{} — {}", drawn(0), drawn(1)),
        _ => drawn(0),
    }
}

/// What Enter (or a press) does on the selected row.
fn activate(popup: &MediaPopup, players: &Players, lines: &[Line], p: &Player) -> MediaAction {
    match lines.get(popup.selected) {
        Some(Line::Buttons) if capable(p, popup.button) => {
            CONTROLS.get(popup.button).map_or(MediaAction::Nothing, |c| MediaAction::Control(*c))
        }
        Some(Line::Player(i)) => players.shown().nth(*i).map_or(MediaAction::Nothing, |q| MediaAction::Pin(q.bus_name.clone())),
        _ => MediaAction::Nothing,
    }
}

/// ↑ ↓ (and Ctrl+P / Ctrl+N, as in the menus) move over the selectable rows, ← → over the capable
/// buttons, Enter activates, Escape closes. Clamped, never wrapping.
pub fn key(popup: &mut MediaPopup, players: &Players, k: Key) -> MediaAction {
    if k == Key::Escape {
        return MediaAction::Close;
    }
    let Some(p) = shown_active(players) else { return MediaAction::Nothing };
    popup.fit(players);
    let lines = lines(players);
    let before = popup.clone();
    let ok = |i: usize| selectable(&lines, p, i);
    match k {
        Key::Up | Key::CtrlP if ok(popup.selected) => popup.selected = list::step(popup.selected, -1, lines.len(), ok),
        Key::Down | Key::CtrlN if ok(popup.selected) => popup.selected = list::step(popup.selected, 1, lines.len(), ok),
        Key::Left | Key::Right if lines.get(popup.selected) == Some(&Line::Buttons) && ok(popup.selected) => {
            let delta = if k == Key::Left { -1 } else { 1 };
            popup.button = list::step(popup.button, delta, CONTROLS.len(), |b| capable(p, b));
        }
        Key::Enter if ok(popup.selected) => return activate(popup, players, &lines, p),
        _ => {}
    }
    if *popup == before {
        return MediaAction::Nothing;
    }
    popup.remember(players, &lines);
    MediaAction::Redraw
}

/// A left press at `cell` (Boxed coordinates: column 0 is the pad): a capable button runs, a player
/// is pinned; anything else does nothing. The press selects what it ran.
/// `width`: the one `rows` drew them at.
pub fn press(popup: &mut MediaPopup, players: &Players, cell: (u16, u16), width: u16) -> MediaAction {
    let Some(p) = shown_active(players) else { return MediaAction::Nothing };
    let lines = lines(players);
    let row = usize::from(cell.1);
    match lines.get(row) {
        Some(Line::Buttons) => {
            let Some(b) = cell.0.checked_sub(1).and_then(|x| button_at(x, width)) else { return MediaAction::Nothing };
            if !capable(p, b) {
                return MediaAction::Nothing;
            }
            popup.selected = row;
            popup.button = b;
            popup.remember(players, &lines);
            activate(popup, players, &lines, p)
        }
        Some(Line::Player(_)) => {
            popup.selected = row;
            popup.remember(players, &lines);
            activate(popup, players, &lines, p)
        }
        _ => MediaAction::Nothing,
    }
}

/// When the open popup must redraw next: at the track's next whole second (real time, at `rate`),
/// or at its end if that comes first. Faster than 1×, at the first whole second at least
/// 1 − 1/`rate` real seconds away: about one redraw a real second, never `rate` of them. None when
/// closed, not playing, at a standstill, with no known length, or at the end: then no timer exists
/// at all.
pub fn tick_due(open: bool, playing: bool, rate: f64, position_us: u64, length_us: Option<u64>) -> Option<Duration> {
    let length = length_us.filter(|&l| l > 0)?;
    if !open || !playing || !rate.is_finite() || rate <= 0.0 || position_us >= length {
        return None;
    }
    let to_second = 1_000_000 - position_us % 1_000_000;
    // In track µs: the 1 − 1/rate real seconds of the floor (0 up to 1×). Saturating casts.
    let floor = ((rate - 1.0).max(0.0) * 1e6) as u64;
    let skipped = floor.saturating_sub(to_second).div_ceil(1_000_000);
    let media_us = to_second.saturating_add(skipped.saturating_mul(1_000_000)).min(length - position_us);
    let real_us = (media_us as f64 / rate).ceil() as u64;
    Some(Duration::from_micros(real_us) + SLACK)
}

/// The open popup's next tick for `p` at `now` (`tick_due`); none while its position is unknown:
/// the time row stands at 0:00 until the read answers, nothing to redraw.
pub fn player_tick(open: bool, p: &Player, now: Instant) -> Option<Duration> {
    let position = media::position_now(p, now)?;
    tick_due(open, p.status == Status::Playing, p.rate, position, p.track.length_us)
}

/// The tick's deadline: one `armed` and still ahead is kept (a refresh never moves it), else `due`
/// from now; none when no tick is due.
pub fn next_deadline(due: Option<Duration>, armed: Option<Instant>, now: Instant) -> Option<Instant> {
    let due = due?;
    match armed {
        Some(at) if at > now => Some(at),
        _ => Some(now + due),
    }
}

#[cfg(test)]
mod tests;
