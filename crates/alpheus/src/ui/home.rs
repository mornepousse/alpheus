//! The Home tab as a dashboard (UI refresh spec): a block-digit clock with the date, a greeting and
//! a one-line summary; a card per setting (volume and brightness with big digits and a gauge, the
//! energy profiles); four tiles filled with their hue when on; the connections and the devices.
//! Each domain keeps its hue (sound blue, brightness yellow, energy green, Wi-Fi cyan, Bluetooth
//! purple, do-not-disturb red, keep-awake orange). The seven items and their order are
//! `App::items()`'s, so keys, the palette and the mouse work as on the rows.
//!
//! Laid out for Mae's window first (a 67×20 body beside the rail: one-cell digits, the date,
//! greeting and summary stacked, compact cards and tiles), it grows into larger bodies; below
//! `MIN_W` × `MIN_H` `tabs::render` draws the rows instead.

use super::digits::big_glyph;
use super::draw::{clear, put, truncate};
use super::hit::{HitMap, Target};
use super::rail::plural;
use super::shape;
use crate::app::action::{Action, profile_label};
use crate::app::{App, Loadable, SliderId, ToggleId};
use crate::theme::{Shape, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Widget};

/// The smallest body that gets the dashboard.
pub const MIN_W: u16 = 64;
pub const MIN_H: u16 = 20;

// Item indices, as `App::items()` orders Home (checked by a test).
const VOLUME: usize = 0;
const BRIGHTNESS: usize = 1;
const WIFI: usize = 2;
const BLUETOOTH: usize = 3;
const DND: usize = 4;
const KEEP_AWAKE: usize = 5;
const PROFILE: usize = 6;

/// A tile at least this wide puts its state beside the label and a subtitle under it; a narrower
/// one stacks the label over the glyph and the state.
const WIDE_TILE: u16 = 24;

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// The local wall-clock time the dashboard shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
    /// 0 = Sunday.
    pub weekday: u8,
    pub day: u8,
    /// 0 = January.
    pub month: u8,
}

impl LocalTime {
    /// Now, in the local time zone; `None` when the C library cannot tell.
    pub fn now() -> Option<LocalTime> {
        // SAFETY: time(NULL) reads the clock; localtime_r writes only into the zeroed `tm` owned here.
        let tm = unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&t, &mut tm).is_null() {
                return None;
            }
            tm
        };
        let field = |v: libc::c_int| u8::try_from(v).ok();
        Some(LocalTime {
            hour: field(tm.tm_hour)?,
            minute: field(tm.tm_min)?,
            weekday: field(tm.tm_wday)?,
            day: field(tm.tm_mday)?,
            month: field(tm.tm_mon)?,
        })
    }
}

pub fn fits(body: Rect) -> bool {
    body.width >= MIN_W && body.height >= MIN_H
}

pub fn greeting(hour: u8) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// `"2 connections · 1 device · Balanced"`; the profile left out while the profiles are not ready.
pub fn summary(app: &App) -> String {
    let links = app.data.network.ready().map_or(0, |n| n.active.len());
    let devices = app.data.bluetooth.ready().map_or(0, |b| b.connected.len());
    let mut s = format!("{} · {}", plural(links, "connection", "connections"), plural(devices, "device", "devices"));
    if let Some(p) = app.data.profiles.ready() {
        s.push_str(" · ");
        s.push_str(&profile_label(&p.active));
    }
    s
}

/// Heights of the dashboard's bands; the lists take what is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bands {
    hero: u16,
    cards: u16,
    /// Blank rows after the cards, then after the tiles.
    gap1: u16,
    tiles: u16,
    gap2: u16,
}

#[derive(Clone, Copy)]
enum Grow {
    Hero,
    Cards,
    Gap1,
    Tiles,
    Gap2,
}

/// At `MIN_H`: a 4-row clock, 7-row cards, 4-row tiles and 5 rows of lists. Each row more goes, in
/// this order, to the cards (their ticks, then the hint), a gap, the tiles, the clock's top margin…
/// up to the spike's 5 / 11 / 7 at a 30-row body; any further row grows the lists.
fn bands(height: u16) -> Bands {
    use Grow::*;
    const ORDER: [Grow; 10] = [Cards, Gap1, Cards, Tiles, Gap2, Hero, Cards, Tiles, Cards, Tiles];
    let mut b = Bands { hero: 4, cards: 7, gap1: 0, tiles: 4, gap2: 0 };
    for g in ORDER.iter().take(usize::from(height.saturating_sub(MIN_H))) {
        match g {
            Hero => b.hero += 1,
            Cards => b.cards += 1,
            Gap1 => b.gap1 += 1,
            Tiles => b.tiles += 1,
            Gap2 => b.gap2 += 1,
        }
    }
    b
}

pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, now: Option<LocalTime>) {
    let th = &app.theme;
    let b = bands(body.height);
    let hero = Rect::new(body.x, body.y, body.width, b.hero);
    clock(buf, hero, app, now);
    let y1 = hero.bottom();
    let cards = split(Rect::new(body.x, y1, body.width, b.cards), 3);
    slider_card(buf, cards[0], app, VOLUME, SliderId::Volume, " 󰕾 Volume ", th.blue, hits);
    slider_card(buf, cards[1], app, BRIGHTNESS, SliderId::Brightness, " 󰃠 Brightness ", th.yellow, hits);
    profile_card(buf, cards[2], app, hits);
    let y2 = y1 + b.cards + b.gap1;
    let tiles = split(Rect::new(body.x, y2, body.width, b.tiles), 4);
    let ssid = app.data.network.ready().and_then(|n| n.active.iter().find(|c| c.kind == "Wi-Fi")).map(|c| c.name.as_str());
    let device = app.data.bluetooth.ready().and_then(|b| b.connected.first()).map(|d| d.name.as_str());
    let tile_of = |i, id, glyph, hue, detail| TileSpec { i, id, glyph, hue, detail };
    tile(buf, tiles[0], app, tile_of(WIFI, ToggleId::Wifi, "󰖩", th.cyan, ssid), hits);
    tile(buf, tiles[1], app, tile_of(BLUETOOTH, ToggleId::Bluetooth, "󰂯", th.purple, device), hits);
    tile(buf, tiles[2], app, tile_of(DND, ToggleId::Dnd, "󰂛", th.red, None), hits);
    tile(buf, tiles[3], app, tile_of(KEEP_AWAKE, ToggleId::KeepAwake, "󰅶", th.orange, None), hits);
    let y3 = y2 + b.tiles + b.gap2;
    if y3 + 4 <= body.bottom() {
        let lists = split(Rect::new(body.x, y3, body.width, body.bottom() - y3), 2);
        connections_card(buf, lists[0], app);
        devices_card(buf, lists[1], app);
    }
}

/// `n` columns of equal width in `r`, one cell apart; the last takes the remainder.
fn split(r: Rect, n: u16) -> Vec<Rect> {
    let w = r.width.saturating_sub(n - 1) / n;
    (0..n)
        .map(|k| {
            let x = r.x + k * (w + 1);
            let w = if k == n - 1 { r.right().saturating_sub(x) } else { w };
            Rect::new(x, r.y, w, r.height)
        })
        .collect()
}

fn width(s: &str) -> u16 {
    u16::try_from(s.chars().count()).unwrap_or(u16::MAX)
}

/// A card's frame, as the theme's shape cuts it: quiet at rest, in `hue` with a filled title when
/// focused. Returns the inside.
fn card(buf: &mut Buffer, r: Rect, title: &str, hue: Color, focused: bool, th: &Theme) -> Rect {
    clear(buf, r, Style::default().bg(th.bg).fg(th.fg));
    let (border, sides) = shape::frame(th.shape, focused);
    let (bstyle, tstyle) = match (focused, th.shape == Shape::Line) {
        (_, true) => (
            Style::default().fg(if focused { hue } else { th.raised }),
            Style::default().fg(hue).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        (true, false) => (Style::default().fg(hue), Style::default().fg(th.on_accent).bg(hue).add_modifier(Modifier::BOLD)),
        (false, false) => (Style::default().fg(th.raised), Style::default().fg(hue).add_modifier(Modifier::BOLD)),
    };
    let block = Block::default()
        .borders(sides)
        .border_type(border)
        .border_style(bstyle)
        .title(ratatui::text::Span::styled(title.to_string(), tstyle));
    let inner = block.inner(r);
    block.render(r, buf);
    inner
}

/// `"✗ <error>"` on the inside's second row, cut to fit.
fn failed(buf: &mut Buffer, inner: Rect, y: u16, e: &str, th: &Theme) {
    let text = truncate(&format!("✗ {e}"), usize::from(inner.width.saturating_sub(2)));
    put(buf, inner, inner.x + 1, y, &text, Style::default().fg(th.error));
}

#[allow(clippy::too_many_arguments)]
fn slider_card(buf: &mut Buffer, r: Rect, app: &App, i: usize, id: SliderId, title: &str, hue: Color, hits: &mut HitMap) {
    let th = &app.theme;
    let focused = app.focus == i;
    let inner = card(buf, r, title, hue, focused, th);
    hits.push(r, Target::Item(i));
    if inner.height < 5 || inner.width < 12 {
        return;
    }
    match app.slider_loadable(id) {
        Loadable::Loading => {
            put(buf, inner, inner.x + 1, inner.y + 1, "…", Style::default().fg(th.muted));
        }
        Loadable::Failed(e) => failed(buf, inner, inner.y + 1, &e, th),
        Loadable::Ready(p) => {
            let muted = app.slider_muted(id);
            // The value in big digits, then "%".
            let mut x = inner.x + 1;
            for ch in p.to_string().chars() {
                x += big_glyph(buf, inner, x, inner.y, ch, 1, Style::default().fg(th.fg)) + 1;
            }
            put(buf, inner, x, inner.y + 2, "%", Style::default().fg(th.muted));
            if muted {
                // The word when it fits, else the glyph.
                let tag = if x + 2 + 7 <= inner.right() { " muted " } else { " 󰝟 " };
                put(buf, inner, x + 2, inner.y + 2, tag, Style::default().fg(th.on_accent).bg(th.warn).add_modifier(Modifier::BOLD));
            }
            // A two-row gauge (full blocks, then a dim track); a blank row above it, its ticks and the
            // focused card's hint below it, as the height allows.
            let gw = inner.width - 2;
            let gy = if inner.height >= 8 { inner.y + 4 } else { inner.y + 3 };
            let ink = if muted { th.muted } else { hue };
            let filled = (u32::from(p.min(100)) * u32::from(gw) + 50) / 100;
            for k in 0..gw {
                let on = u32::from(k) < filled;
                let style = Style::default().fg(if on { ink } else { th.raised });
                put(buf, inner, inner.x + 1 + k, gy, if on { "█" } else { " " }, style);
                put(buf, inner, inner.x + 1 + k, gy + 1, if on { "█" } else { "▔" }, style);
            }
            hits.push(Rect::new(inner.x + 1, gy, gw, 2), Target::Slider { item: i });
            let below = inner.bottom().saturating_sub(gy + 2);
            if below >= 1 {
                // Ticks at 0, 25, 50, 75 and 100 %.
                for q in 0..=4u16 {
                    put(buf, inner, inner.x + 1 + (gw - 1) * q / 4, gy + 2, "╵", Style::default().fg(th.muted));
                }
            }
            if below >= 2 && focused {
                let km = &app.keymap;
                let hint = format!("{}/{} adjust · click the gauge", km.label(Action::Decrease), km.label(Action::Increase));
                put(buf, inner, inner.x + 1, inner.bottom() - 1, &truncate(&hint, usize::from(gw)), Style::default().fg(th.muted));
            }
        }
    }
}

fn profile_card(buf: &mut Buffer, r: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let focused = app.focus == PROFILE;
    let inner = card(buf, r, " 󰓅 Energy ", th.green, focused, th);
    hits.push(r, Target::Item(PROFILE));
    match &app.data.profiles {
        Loadable::Loading => {
            put(buf, inner, inner.x + 1, inner.y + 1, "…", Style::default().fg(th.muted));
        }
        Loadable::Failed(e) => failed(buf, inner, inner.y + 1, e, th),
        Loadable::Ready(p) => {
            // One row in two when they fit so, with a margin above when there is room.
            let n = u16::try_from(p.available.len()).unwrap_or(u16::MAX);
            let step = if inner.height >= (2 * n).saturating_sub(1) { 2 } else { 1 };
            let top = if inner.height > 2 * n { 1 } else { 0 };
            for (j, name) in p.available.iter().enumerate() {
                let y = inner.y + top + step * j as u16;
                if y >= inner.bottom() {
                    break;
                }
                let glyph = match name.as_str() {
                    "performance" => "󰓅",
                    "power-saver" => "󰌪",
                    _ => "󰾅",
                };
                let row = Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1);
                let active = *name == p.active;
                let style = if active {
                    Style::default().fg(th.on_accent).bg(th.green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(th.muted).bg(th.pill)
                };
                clear(buf, row, style);
                let text = truncate(&format!("{glyph}  {}", profile_label(name)), usize::from(row.width.saturating_sub(1)));
                let end = row.x + 1 + width(&text);
                put(buf, row, row.x + 1, y, &text, style);
                // The dot only beside a whole label.
                if active && end < row.right().saturating_sub(2) {
                    put(buf, row, row.right() - 2, y, "●", style);
                }
                hits.push(row, Target::ProfileOption(j));
            }
        }
    }
}

/// What a tile shows.
struct TileSpec<'a> {
    i: usize,
    id: ToggleId,
    glyph: &'a str,
    hue: Color,
    /// What is on (the network, the device), shown while on.
    detail: Option<&'a str>,
}

fn tile(buf: &mut Buffer, r: Rect, app: &App, t: TileSpec, hits: &mut HitMap) {
    let th = &app.theme;
    if r.height < 3 || r.width < 4 {
        return;
    }
    let focused = app.focus == t.i;
    let state = app.toggle_loadable(t.id);
    let on = matches!(state, Loadable::Ready(true));
    // On: the whole tile in its hue. Off: a quiet tile; under the line style, no fill at all.
    let fill = match (on, th.shape == Shape::Line) {
        (true, _) => t.hue,
        (false, true) => th.raised,
        (false, false) => th.pill,
    };
    let bg = shape::tile_fill(buf, r, fill, th.bg, th.shape);
    let (fg, dim) = match (on, bg != th.bg) {
        (true, true) => (th.on_accent, th.on_accent),
        (true, false) => (t.hue, th.muted),
        (false, _) => (th.fg, th.muted),
    };
    let r = if th.shape == Shape::Float { Rect::new(r.x, r.y, r.width - 1, r.height - 1) } else { r };
    // The focus: a frame in the hue around the tile's edge, on the page's background so it shows on
    // a filled tile too.
    if focused {
        let bt = if th.shape == Shape::Round { BorderType::Rounded } else { shape::frame(th.shape, true).0 };
        Block::bordered().border_type(bt).border_style(Style::default().fg(t.hue).bg(th.bg)).render(r, buf);
    }
    let label = crate::app::toggle_label(t.id);
    let (word, wstyle) = match &state {
        Loadable::Loading => ("…".to_string(), Style::default().fg(dim).bg(bg)),
        Loadable::Failed(e) => (format!("✗ {e}"), Style::default().fg(th.error).bg(bg)),
        Loadable::Ready(true) => ("ON".to_string(), Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)),
        Loadable::Ready(false) => ("off".to_string(), Style::default().fg(dim).bg(bg)),
    };
    let glyph_style = Style::default().fg(if on { fg } else { t.hue }).bg(bg);
    let label_style = Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(dim).bg(bg);
    // The last row text may use: the frame's row is kept when focused.
    let last = r.bottom().saturating_sub(1 + u16::from(focused));
    let y = r.y + 1;
    if r.width >= WIDE_TILE {
        // Glyph, label and state on one row; a subtitle under them; a switch at the foot.
        let x = r.x + 2;
        let word = if matches!(state, Loadable::Failed(_)) { "✗".to_string() } else { word };
        let wx = r.right().saturating_sub(width(&word) + 2);
        put(buf, r, x, y, t.glyph, glyph_style);
        put(buf, r, x + 2, y, &truncate(label, usize::from(wx.saturating_sub(x + 3))), label_style);
        put(buf, r, wx, y, &word, wstyle);
        let toggle_key = app.keymap.label(Action::Toggle);
        let sub = match (&state, t.detail) {
            (Loadable::Ready(true), Some(d)) => Some(d.to_string()),
            (Loadable::Ready(true), None) => Some("active".to_string()),
            (Loadable::Failed(e), _) => Some(e.clone()),
            (Loadable::Ready(false), _) if !toggle_key.is_empty() => Some(format!("{toggle_key} to turn on")),
            _ => None,
        };
        let sy = if r.height >= 5 { r.y + 3 } else { r.y + 2 };
        if let Some(sub) = sub.filter(|_| sy <= last) {
            put(buf, r, x + 2, sy, &truncate(&sub, usize::from(r.width.saturating_sub(6))), dim_style);
        }
        if r.height >= 6 {
            let (sw, sstyle) = if on { ("━━━━━●", label_style) } else { ("○┄┄┄┄┄", dim_style) };
            put(buf, r, r.right().saturating_sub(9), r.bottom() - 2, sw, sstyle);
        }
    } else {
        // Narrow: the label alone on its row (so "Do not disturb" stays whole in Mae's window), then
        // the glyph and what is on, or the state.
        let x = r.x + 1;
        put(buf, r, x, y, &truncate(label, usize::from(r.width.saturating_sub(2))), label_style);
        if y < last {
            put(buf, r, x, y + 1, t.glyph, glyph_style);
            let room = usize::from(r.width.saturating_sub(4));
            match (on, t.detail) {
                (true, Some(d)) => put(buf, r, x + 2, y + 1, &truncate(d, room), label_style.remove_modifier(Modifier::BOLD)),
                _ => put(buf, r, x + 2, y + 1, &truncate(&word, room), wstyle),
            };
        }
    }
    hits.push(r, Target::Item(t.i));
}

/// One list row: the glyph, the name, and the tag right-aligned when the whole name still fits
/// beside it (else the name alone, cut only when even that does not fit).
#[allow(clippy::too_many_arguments)]
fn list_row(buf: &mut Buffer, inner: Rect, y: u16, glyph: &str, hue: Color, name: &str, tag: &str, tag_style: Style, th: &Theme) {
    put(buf, inner, inner.x + 1, y, glyph, Style::default().fg(hue));
    let nx = inner.x + 3;
    let end = inner.right().saturating_sub(1);
    let tw = width(tag);
    let fits_beside = nx + width(name) + 1 + tw <= end;
    let room = usize::from(end.saturating_sub(nx));
    put(buf, inner, nx, y, &truncate(name, room), Style::default().fg(th.fg));
    if fits_beside {
        put(buf, inner, end - tw, y, tag, tag_style);
    }
}

fn connections_card(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let inner = card(buf, r, " 󰛳 Connections ", th.cyan, false, th);
    match &app.data.network {
        Loadable::Ready(n) if !n.active.is_empty() => {
            for (k, c) in n.active.iter().enumerate() {
                let y = inner.y + k as u16;
                if y >= inner.bottom() {
                    break;
                }
                let (glyph, hue) = match c.kind.as_str() {
                    "Wi-Fi" => ("󰖩", th.cyan),
                    "Ethernet" => ("󰈀", th.green),
                    "WireGuard" | "VPN" => ("󰦝", th.purple),
                    _ => ("󰛳", th.muted),
                };
                list_row(buf, inner, y, glyph, hue, &c.name, &format!(" {} ", c.kind), Style::default().fg(hue).bg(th.pill), th);
            }
        }
        Loadable::Failed(e) => failed(buf, inner, inner.y, e, th),
        Loadable::Loading => {
            put(buf, inner, inner.x + 1, inner.y, "…", Style::default().fg(th.muted));
        }
        Loadable::Ready(_) => {
            put(buf, inner, inner.x + 1, inner.y, "no connection", Style::default().fg(th.muted));
        }
    }
}

fn devices_card(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let inner = card(buf, r, " 󰂯 Devices ", th.purple, false, th);
    match &app.data.bluetooth {
        Loadable::Ready(b) if !b.powered => {
            put(buf, inner, inner.x + 1, inner.y, "Bluetooth is off", Style::default().fg(th.muted));
        }
        Loadable::Ready(b) if !b.connected.is_empty() => {
            for (k, d) in b.connected.iter().enumerate() {
                let y = inner.y + k as u16;
                if y >= inner.bottom() {
                    break;
                }
                list_row(buf, inner, y, "󰂱", th.purple, &d.name, "● connected", Style::default().fg(th.ok), th);
            }
        }
        Loadable::Failed(e) => failed(buf, inner, inner.y, e, th),
        Loadable::Loading => {
            put(buf, inner, inner.x + 1, inner.y, "…", Style::default().fg(th.muted));
        }
        Loadable::Ready(_) => {
            put(buf, inner, inner.x + 1, inner.y, "nothing connected", Style::default().fg(th.muted));
        }
    }
}

/// Columns the time takes in big digits at `scale`, with the cell after each glyph.
fn clock_width(scale: u16) -> u16 {
    4 * (3 * scale + 1) + scale + 1
}

/// The clock strip: the time in big digits, the date, the greeting and the summary beside it, a rule
/// under it. With no time, the summary alone.
fn clock(buf: &mut Buffer, r: Rect, app: &App, now: Option<LocalTime>) {
    let th = &app.theme;
    // The digits' first row: a margin above them when the strip has 5 rows.
    let top = r.y + r.height.saturating_sub(4);
    let sum = summary(app);
    let bold = Style::default().fg(th.fg).add_modifier(Modifier::BOLD);
    let quiet = Style::default().fg(th.muted);
    // A thin rule under the strip, accent on its left third.
    let ry = r.bottom().saturating_sub(1);
    for k in 0..r.width {
        let c = if k < r.width / 3 { th.accent } else { th.raised };
        put(buf, r, r.x + k, ry, "─", Style::default().fg(c));
    }
    let Some(t) = now else {
        put(buf, r, r.x + 1, top + 1, &truncate(&sum, usize::from(r.width.saturating_sub(2))), quiet);
        return;
    };
    let day = DAYS[usize::from(t.weekday % 7)];
    let date = format!("{} {}", t.day, MONTHS[usize::from(t.month % 12)]);
    let hello = greeting(t.hour);
    // The first arrangement that fits: the date beside the digits and the greeting right-aligned, or
    // all three stacked beside them; two-cell pixels before one-cell ones.
    let (dw, hw, sw) = (width(day).max(width(&date)), width(hello), width(&sum));
    let (scale, side) = [(2, true), (1, true), (2, false)]
        .into_iter()
        .find(|&(scale, side)| {
            let dx = r.x + 1 + clock_width(scale) + 2;
            let need = if side { dw + 2 + hw.max(sw) + 1 } else { (width(day) + 1 + width(&date)).max(hw).max(sw) + 1 };
            dx + need <= r.right()
        })
        .unwrap_or((1, false));
    let mut x = r.x + 1;
    for ch in format!("{:02}:{:02}", t.hour, t.minute).chars() {
        x += big_glyph(buf, r, x, top, ch, scale, Style::default().fg(th.accent)) + 1;
    }
    let dx = x + 2;
    if side {
        put(buf, r, dx, top, day, bold);
        put(buf, r, dx, top + 1, &date, quiet);
        let right = |w: u16| r.right().saturating_sub(w + 1).max(dx + dw + 2);
        put(buf, r, right(hw), top, hello, bold);
        put(buf, r, right(sw), top + 1, &sum, quiet);
    } else {
        let w = put(buf, r, dx, top, day, bold);
        put(buf, r, dx + w + 1, top, &date, quiet);
        put(buf, r, dx, top + 1, hello, bold);
        put(buf, r, dx, top + 2, &truncate(&sum, usize::from(r.right().saturating_sub(dx + 1))), quiet);
    }
}

#[cfg(test)]
mod tests;
