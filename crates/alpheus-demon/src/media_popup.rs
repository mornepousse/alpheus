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
        let mut popup = MediaPopup { selected: usize::MAX, button: 1 };
        popup.fit(players);
        popup
    }

    /// After the players changed under the popup (a row gone, a capability lost): an unselectable
    /// row goes back to the buttons (or the first selectable row), an incapable button to play/pause,
    /// then next, then previous.
    pub fn fit(&mut self, players: &Players) {
        let Some(p) = shown_active(players) else { return };
        let lines = lines(players);
        let ok = |i: usize| selectable(&lines, p, i);
        if !ok(self.selected) {
            let buttons = lines.iter().position(|l| *l == Line::Buttons).filter(|&i| ok(i));
            self.selected = buttons.or_else(|| list::first_selectable(lines.len(), ok)).unwrap_or(0);
        }
        if !capable(p, self.button) {
            self.button = [1, 2, 0].into_iter().find(|&b| capable(p, b)).unwrap_or(1);
        }
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
                    g.write(0, width, &media::truncate_title(&p.track.artists.join(", "), width), Style::NORMAL);
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
    if *popup == before { MediaAction::Nothing } else { MediaAction::Redraw }
}

/// A left press at `cell` (Boxed coordinates: column 0 is the pad): a capable button runs, a player
/// is pinned; anything else does nothing. The press selects what it ran.
pub fn press(popup: &mut MediaPopup, players: &Players, cell: (u16, u16)) -> MediaAction {
    let Some(p) = shown_active(players) else { return MediaAction::Nothing };
    let lines = lines(players);
    let row = usize::from(cell.1);
    match lines.get(row) {
        Some(Line::Buttons) => {
            let Some(b) = cell.0.checked_sub(1).and_then(|x| button_at(x, WIDTH)) else { return MediaAction::Nothing };
            if !capable(p, b) {
                return MediaAction::Nothing;
            }
            popup.selected = row;
            popup.button = b;
            activate(popup, players, &lines, p)
        }
        Some(Line::Player(_)) => {
            popup.selected = row;
            activate(popup, players, &lines, p)
        }
        _ => MediaAction::Nothing,
    }
}

/// When the open popup must redraw next: at the track's next whole second (real time, at `rate`),
/// or at its end if that comes first. None when closed, not playing, at a standstill, with no
/// known length, or at the end: then no timer exists at all.
pub fn tick_due(open: bool, playing: bool, rate: f64, position_us: u64, length_us: Option<u64>) -> Option<Duration> {
    let length = length_us.filter(|&l| l > 0)?;
    if !open || !playing || !rate.is_finite() || rate <= 0.0 || position_us >= length {
        return None;
    }
    let to_second = 1_000_000 - position_us % 1_000_000;
    let media_us = to_second.min(length - position_us);
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
mod tests {
    use super::*;
    use crate::grid::text_width;
    use crate::media::{PositionSync, Value};
    use crate::segments::{PAUSE, PLAY, SKIP_NEXT, SKIP_PREVIOUS};

    const PREFIX: &str = "org.mpris.MediaPlayer2.";
    const LEN: i64 = 245_000_000; // 4:05

    /// What GetAll returns for a fictional track (fixtures are fictional: no real artist or title).
    fn props(status: &str, title: &str, artists: &[&str], length_us: Option<i64>) -> Vec<(String, Value)> {
        let mut meta = vec![
            ("mpris:trackid".to_string(), Value::Str(format!("/fake/track/{}", title.len()))),
            ("xesam:title".to_string(), Value::Str(title.to_string())),
            ("xesam:artist".to_string(), Value::StrList(artists.iter().map(|a| a.to_string()).collect())),
        ];
        if let Some(l) = length_us {
            meta.push(("mpris:length".to_string(), Value::Int(l)));
        }
        let can = |k: &str| (k.to_string(), Value::Bool(true));
        vec![
            ("PlaybackStatus".to_string(), Value::Str(status.to_string())),
            ("Metadata".to_string(), Value::Map(meta)),
            ("Rate".to_string(), Value::Float(1.0)),
            can("CanPlay"),
            can("CanPause"),
            can("CanGoNext"),
            can("CanGoPrevious"),
        ]
    }

    /// `p` with `key` set to `v` (the last entry of a key wins, so the old one is dropped first).
    fn set(mut p: Vec<(String, Value)>, key: &str, v: bool) -> Vec<(String, Value)> {
        p.retain(|(k, _)| k != key);
        p.push((key.to_string(), Value::Bool(v)));
        p
    }

    /// One more player, `:1.n` owning `org.mpris.MediaPlayer2.name`, filled as GetAll would.
    fn with(players: &mut Players, name: &str, n: u32, p: Vec<(String, Value)>) -> String {
        let unique = format!(":1.{n}");
        assert!(players.add(format!("{PREFIX}{name}"), unique.clone()));
        let _ = players.apply(&unique, &p, &[]);
        unique
    }

    /// One player playing "Paper Lanterns" by "The Quiet Hours", 4:05 long, at 1:42 at `t0`.
    fn one(t0: Instant) -> Players {
        let mut players = Players::default();
        let u = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)));
        players.set_identity(&u, "Lumen Player");
        players.set_position(&u, PositionSync { at: t0, us: 102_000_000 });
        players
    }

    fn texts(b: &Boxed) -> Vec<String> {
        b.rows.iter().map(|g| g.text().trim_end().to_string()).collect()
    }

    #[test]
    fn one_player_title_artist_time_and_buttons() {
        let t0 = Instant::now();
        let players = one(t0);
        let popup = MediaPopup::new(&players);
        let b = rows(&players, &popup, t0, WIDTH);
        assert_eq!(b.cells(), (WIDTH + 2, 4), "padded: one blank cell each side; no separator nor players with one player");
        assert!(b.separators.is_empty());
        let t = texts(&b);
        assert_eq!(t[0], " Paper Lanterns");
        assert_eq!(t[1], " The Quiet Hours");
        // 20 cells of bar between "1:42 " and " 4:05": 102 / 245 of them filled, rounded down.
        assert_eq!(t[2], format!(" 1:42 {}{} 4:05", "━".repeat(8), "─".repeat(12)));
        assert_eq!(&b.rows[2].styles()[1..12], "mmmm.aaaaaa", "the clocks muted, the elapsed part in the accent");
        let cells = b.rows[3].cells();
        assert_eq!([cells[11].ch, cells[16].ch, cells[21].ch], [SKIP_PREVIOUS, PAUSE, SKIP_NEXT], "centred, ⏸ while playing");
        assert_eq!(&b.rows[3].styles()[10..23], ".....SSS.....", "the play/pause button selected at open");
        assert_eq!(&b.rows[0].styles()[1..2], "G", "the title in bold accent");
    }

    #[test]
    fn three_players_add_a_separator_and_the_list_with_the_active_marked() {
        let t0 = Instant::now();
        let mut players = one(t0);
        with(&mut players, "beta", 8, props("Paused", "Quiet Song", &["Nobody"], Some(LEN)));
        with(&mut players, "gamma", 9, props("Paused", "Other Song", &[], None));
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(b.cells().1, 8, "title, artist, time, buttons, separator, three players");
        assert_eq!(b.separators, vec![4]);
        assert_eq!(b.rows[4].text().trim(), "", "the separator row stays blank: the painter draws its line");
        let t = texts(&b);
        let mut listed: Vec<&str> = t[5..].iter().map(|r| r.trim_start_matches(' ').trim_start_matches('●').trim()).collect();
        listed.sort_unstable();
        assert_eq!(listed, ["Lumen Player", "beta", "gamma"], "Identity, else the bus name suffix");
        let marked: Vec<&String> = t[5..].iter().filter(|r| r.contains('●')).collect();
        assert_eq!(marked, [&" ● Lumen Player".to_string()], "only the active player carries ●");
    }

    #[test]
    fn an_unknown_length_hides_the_time_row() {
        let t0 = Instant::now();
        for length in [None, Some(0)] {
            let mut players = Players::default();
            with(&mut players, "radio", 3, props("Playing", "Live Stream", &["Somewhere FM"], length));
            assert_eq!(lines(&players), [Line::Title, Line::Artist, Line::Buttons], "length {length:?}");
            let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
            assert_eq!(b.cells().1, 3);
            assert_eq!(tick_due(true, true, 1.0, 0, length.map(|l| l as u64)), None, "nothing to redraw: no tick");
        }
        let mut players = Players::default();
        with(&mut players, "radio", 3, props("Playing", "Live Stream", &[], None));
        assert_eq!(lines(&players), [Line::Title, Line::Buttons], "no artist: no artist row");
    }

    #[test]
    fn an_incapable_button_is_dim_skipped_and_ignored() {
        let t0 = Instant::now();
        let mut players = Players::default();
        let u =
            with(&mut players, "lumen", 7, set(props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)), "CanGoNext", false));
        players.set_position(&u, PositionSync { at: t0, us: 0 });
        let mut popup = MediaPopup::new(&players);
        let b = rows(&players, &popup, t0, WIDTH);
        assert_eq!(&b.rows[3].styles()[21..22], "m", "⏭ drawn dim");
        assert_eq!(key(&mut popup, &players, Key::Right), MediaAction::Nothing, "→ has no capable button to go to");
        assert_eq!(popup.button, 1);
        assert_eq!(press(&mut popup, &players, (21, 3)), MediaAction::Nothing, "a click on ⏭ is ignored");
        assert_eq!(popup.button, 1, "and moves nothing");
        assert_eq!(key(&mut popup, &players, Key::Enter), MediaAction::Control(Control::PlayPause));
        // Nothing capable at all: the button row cannot be selected, and with one player nothing can.
        let mut players = Players::default();
        let mut p = props("Playing", "Paper Lanterns", &[], Some(LEN));
        for k in ["CanPlay", "CanPause", "CanGoNext", "CanGoPrevious"] {
            p = set(p, k, false);
        }
        with(&mut players, "lumen", 7, p);
        let mut popup = MediaPopup::new(&players);
        let b = rows(&players, &popup, t0, WIDTH);
        assert!(!b.rows.iter().any(|g| g.styles().contains('S')), "no selection band anywhere");
        // No artist: title, time, buttons — the buttons are row 2, ⏯ at boxed column 16.
        assert_eq!(&b.rows[2].styles()[16..17], "m", "⏯ dim too (Paused would ask CanPlay, Playing CanPause)");
        for k in [Key::Up, Key::Down, Key::Left, Key::Right, Key::Enter] {
            assert_eq!(key(&mut popup, &players, k), MediaAction::Nothing, "{k:?}");
        }
    }

    #[test]
    fn long_and_wide_titles_are_cut_to_the_popup_width() {
        let t0 = Instant::now();
        for title in ["a".repeat(40), "日本語のとても長い曲のタイトルです日本語".to_string()] {
            let mut players = Players::default();
            with(&mut players, "lumen", 7, props("Playing", &title, &["The Quiet Hours"], Some(LEN)));
            let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
            assert_eq!(b.rows[0].width(), WIDTH + 2, "{title}");
            let shown = b.rows[0].text();
            let shown = shown.trim();
            assert!(shown.ends_with('…'), "cut: {shown}");
            assert!(text_width(shown) <= WIDTH, "{shown}: {} cells", text_width(shown));
            assert_eq!(b.rows[0].cells()[WIDTH as usize + 1].ch, ' ', "the right pad cell is never written over");
        }
    }

    #[test]
    fn keys_move_between_the_buttons_and_the_players() {
        let t0 = Instant::now();
        let mut players = one(t0);
        with(&mut players, "beta", 8, props("Paused", "Quiet Song", &[], Some(LEN)));
        with(&mut players, "gamma", 9, props("Paused", "Other Song", &[], Some(LEN)));
        let mut p = MediaPopup::new(&players);
        assert_eq!((p.selected, p.button), (3, 1), "the buttons row, play/pause");
        assert_eq!(key(&mut p, &players, Key::Up), MediaAction::Nothing, "nothing selectable above the buttons");
        assert_eq!(key(&mut p, &players, Key::Right), MediaAction::Redraw);
        assert_eq!(p.button, 2);
        assert_eq!(key(&mut p, &players, Key::Right), MediaAction::Nothing, "clamped, never wrapping");
        assert_eq!(key(&mut p, &players, Key::Enter), MediaAction::Control(Control::Next));
        assert_eq!(key(&mut p, &players, Key::Left), MediaAction::Redraw);
        assert_eq!(key(&mut p, &players, Key::Left), MediaAction::Redraw);
        assert_eq!(key(&mut p, &players, Key::Enter), MediaAction::Control(Control::Previous));
        assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Redraw);
        assert_eq!(p.selected, 5, "over the separator, onto the first player");
        assert_eq!(key(&mut p, &players, Key::Left), MediaAction::Nothing, "← → only on the buttons row");
        let first = players.shown().next().map(|q| q.bus_name.clone()).unwrap();
        assert_eq!(key(&mut p, &players, Key::Enter), MediaAction::Pin(first));
        assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Redraw);
        assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Redraw);
        assert_eq!(key(&mut p, &players, Key::Down), MediaAction::Nothing, "the last player");
        assert_eq!(key(&mut p, &players, Key::Up), MediaAction::Redraw);
        assert_eq!(key(&mut p, &players, Key::Escape), MediaAction::Close);
    }

    #[test]
    fn a_press_maps_cells_to_buttons_and_players() {
        let t0 = Instant::now();
        let mut players = one(t0);
        with(&mut players, "beta", 8, props("Paused", "Quiet Song", &[], Some(LEN)));
        let mut p = MediaPopup::new(&players);
        // Boxed columns: the pad is column 0, so content column c is cell c + 1. Buttons at content
        // 10, 15, 20, each taking the 2 cells either side of it.
        assert_eq!(press(&mut p, &players, (21, 3)), MediaAction::Control(Control::Next));
        assert_eq!((p.selected, p.button), (3, 2), "the press selects what it ran");
        assert_eq!(press(&mut p, &players, (9, 3)), MediaAction::Control(Control::Previous), "content 8: ⏮'s left edge");
        assert_eq!(press(&mut p, &players, (13, 3)), MediaAction::Control(Control::Previous), "content 12: ⏮'s right edge");
        assert_eq!(press(&mut p, &players, (14, 3)), MediaAction::Control(Control::PlayPause), "content 13: ⏯'s left edge");
        assert_eq!(press(&mut p, &players, (23, 3)), MediaAction::Control(Control::Next), "content 22: ⏭'s right edge");
        assert_eq!(press(&mut p, &players, (24, 3)), MediaAction::Nothing, "past ⏭");
        assert_eq!(press(&mut p, &players, (7, 3)), MediaAction::Nothing, "before ⏮");
        assert_eq!(press(&mut p, &players, (0, 3)), MediaAction::Nothing, "the pad cell");
        assert_eq!(press(&mut p, &players, (5, 0)), MediaAction::Nothing, "the title");
        assert_eq!(press(&mut p, &players, (5, 4)), MediaAction::Nothing, "the separator");
        let second = players.shown().nth(1).map(|q| q.bus_name.clone()).unwrap();
        assert_eq!(press(&mut p, &players, (3, 6)), MediaAction::Pin(second));
        assert_eq!(p.selected, 6);
        assert_eq!(press(&mut p, &players, (3, 40)), MediaAction::Nothing, "below the rows");
    }

    #[test]
    fn the_tick_runs_only_while_open_playing_and_before_the_end() {
        let len = Some(LEN as u64);
        assert_eq!(tick_due(false, true, 1.0, 102_250_000, len), None, "closed");
        assert_eq!(tick_due(true, false, 1.0, 102_250_000, len), None, "paused or stopped");
        assert_eq!(tick_due(true, true, 0.0, 102_250_000, len), None, "rate 0: the position stands still");
        assert_eq!(tick_due(true, true, f64::NAN, 102_250_000, len), None);
        assert_eq!(tick_due(true, true, 1.0, 102_250_000, None), None, "unknown length: no time row");
        assert_eq!(tick_due(true, true, 1.0, LEN as u64, len), None, "at the end: the timer stops");
        assert_eq!(tick_due(true, true, 1.0, 102_250_000, len), Some(Duration::from_millis(750) + SLACK), "the next whole second");
        assert_eq!(tick_due(true, true, 1.0, 102_000_000, len), Some(Duration::from_secs(1) + SLACK));
        assert_eq!(tick_due(true, true, 2.0, 102_250_000, len), Some(Duration::from_micros(375_000) + SLACK), "twice as fast");
        assert_eq!(tick_due(true, true, 1.0, 244_900_000, len), Some(Duration::from_millis(100) + SLACK), "the last tick lands on the end");
    }

    /// Review Focus: the position beyond the length (a player reporting 6:40 of a 4:05 track).
    #[test]
    fn a_position_beyond_the_length_is_clamped_and_stops_the_tick() {
        let t0 = Instant::now();
        let mut players = Players::default();
        let u = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &["The Quiet Hours"], Some(LEN)));
        players.set_position(&u, PositionSync { at: t0, us: 400_000_000 });
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(texts(&b)[2], format!(" 4:05 {} 4:05", "━".repeat(20)), "full, never past the bar");
        let p = players.get(&u).unwrap();
        let pos = media::position_now(p, t0).unwrap_or(0);
        assert_eq!(tick_due(true, true, p.rate, pos, p.track.length_us), None, "the end is reached: no tick");
        assert_eq!(tick_due(true, true, 1.0, 400_000_000, Some(LEN as u64)), None, "even unclamped");
    }

    /// Review Focus: a track change while the popup is open. The rows follow, and once the
    /// position is read again (Mpris asks on a track change while open) the tick restarts from 0:00.
    #[test]
    fn a_track_change_while_open_updates_the_rows_and_the_tick() {
        let t0 = Instant::now();
        let mut players = one(t0);
        let mut popup = MediaPopup::new(&players);
        let before = rows(&players, &popup, t0, WIDTH);
        let _ = players.apply(":1.7", &props("Playing", "Second Song", &["Other Band"], Some(200_000_000)), &[]);
        players.set_position(":1.7", PositionSync { at: t0, us: 0 });
        popup.fit(&players);
        let after = rows(&players, &popup, t0, WIDTH);
        assert_ne!(before, after);
        let t = texts(&after);
        assert_eq!((t[0].as_str(), t[1].as_str()), (" Second Song", " Other Band"));
        assert_eq!(t[2], format!(" 0:00 {} 3:20", "─".repeat(20)));
        let p = players.get(":1.7").unwrap();
        assert_eq!(
            tick_due(true, true, p.rate, media::position_now(p, t0).unwrap_or(0), p.track.length_us),
            Some(Duration::from_secs(1) + SLACK)
        );
    }

    /// Review Focus: a pause while open. The timer is disarmed, and ⏯ shows play.
    #[test]
    fn a_pause_while_open_stops_the_tick_and_shows_play() {
        let t0 = Instant::now();
        let mut players = one(t0);
        let _ = players.apply(":1.7", &[("PlaybackStatus".to_string(), Value::Str("Paused".into()))], &[]);
        let p = players.get(":1.7").unwrap();
        assert_eq!(tick_due(true, p.status == Status::Playing, p.rate, 102_000_000, p.track.length_us), None);
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(b.rows[3].cells()[16].ch, PLAY);
    }

    /// Fix round 1: until the position is read (the popup just opened, or the read failed), the
    /// time row stands at 0:00 — nothing to redraw, so no tick.
    #[test]
    fn an_unknown_position_never_ticks() {
        let t0 = Instant::now();
        let mut players = Players::default();
        let u = with(&mut players, "lumen", 7, props("Playing", "Paper Lanterns", &[], Some(LEN)));
        let p = players.get(&u).unwrap();
        assert_eq!(player_tick(true, p, t0), None, "position unknown");
        players.set_position(&u, PositionSync { at: t0, us: 102_250_000 });
        let p = players.get(&u).unwrap();
        assert_eq!(player_tick(true, p, t0), Some(Duration::from_millis(750) + SLACK), "read: the next whole second");
        assert_eq!(player_tick(false, p, t0), None, "closed");
    }

    /// Fix round 1: a refresh (a key, a host output) never moves a tick already armed ahead.
    #[test]
    fn a_refresh_keeps_the_armed_tick() {
        let now = Instant::now();
        let second = Some(Duration::from_secs(1));
        assert_eq!(next_deadline(second, Some(now + Duration::from_millis(300)), now), Some(now + Duration::from_millis(300)), "kept");
        assert_eq!(next_deadline(second, Some(now), now), Some(now + Duration::from_secs(1)), "due now or past: a new one");
        assert_eq!(next_deadline(second, None, now), Some(now + Duration::from_secs(1)), "none armed");
        assert_eq!(next_deadline(None, Some(now + Duration::from_millis(300)), now), None, "no tick due: removed");
    }

    /// Controller reconciliation 10: the "media popup shown" line carries what the popup shows on
    /// top, as drawn (cut to its width), so a headless test can check the fake track reached it.
    #[test]
    fn the_heading_is_the_drawn_title_and_artist() {
        let t0 = Instant::now();
        let players = one(t0);
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(heading(&players, &b), "Paper Lanterns — The Quiet Hours");
        let mut players = Players::default();
        with(&mut players, "radio", 3, props("Playing", &"a".repeat(40), &[], None));
        let b = rows(&players, &MediaPopup::new(&players), t0, WIDTH);
        assert_eq!(heading(&players, &b), format!("{}…", "a".repeat(29)), "no artist row: the title alone, as drawn");
        assert_eq!(heading(&Players::default(), &Boxed::default()), "", "nothing shown");
    }

    /// A tick whose rows did not change commits nothing (`popup_draw` compares with `last`): two
    /// instants in the same second of the track render the very same rows.
    #[test]
    fn two_instants_in_the_same_second_render_the_same_rows() {
        let t0 = Instant::now();
        let players = one(t0);
        let popup = MediaPopup::new(&players);
        assert_eq!(rows(&players, &popup, t0, WIDTH), rows(&players, &popup, t0 + Duration::from_millis(400), WIDTH));
        assert_ne!(rows(&players, &popup, t0, WIDTH), rows(&players, &popup, t0 + Duration::from_millis(1_000), WIDTH));
    }
}
