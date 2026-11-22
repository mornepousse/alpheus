//! ratatui rendering. `render` draws everything and returns the map of
//! clickable regions: each component knows its own region for click testing.

mod appearance;
mod bluetooth;
mod controls;
mod diagnostics;
mod digits;
mod displays;
pub mod draw;
pub mod hit;
pub mod home;
mod journal;
mod network;
mod overlays;
mod power;
pub mod rail;
mod report_views;
pub mod shape;
mod sound;
mod tabs;

pub use hit::{HitMap, Target};

use crate::app::App;
use crate::app::action::{Action, Tab};
use draw::put;
use home::LocalTime;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Below this, only a message is shown (never a panic).
pub const MIN_W: u16 = 30;
pub const MIN_H: u16 = 8;
/// "alpheus" and the two spaces after it, in the tab bar.
const BRAND_W: usize = 9;

/// Which chrome a terminal of this size gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chrome {
    /// The tab bar on top, the status line and the shortcut bar at the foot.
    Classic,
    /// The left rail (see `rail`).
    Rail,
}

pub fn chrome(area: Rect) -> Chrome {
    if area.width >= rail::RAIL_MIN_W && area.height >= rail::RAIL_MIN_H { Chrome::Rail } else { Chrome::Classic }
}

/// Draws everything at the local time now (see `render_at`).
pub fn render(f: &mut Frame, app: &App) -> HitMap {
    render_at(f, app, LocalTime::now())
}

/// Draws everything; `now` is the time Home's dashboard shows (none: no clock).
pub fn render_at(f: &mut Frame, app: &App, now: Option<LocalTime>) -> HitMap {
    let area = f.area();
    let buf = f.buffer_mut();
    let mut hits = HitMap::default();
    let th = &app.theme;
    draw::clear(buf, area, Style::default().fg(th.fg).bg(th.bg));
    if area.width < MIN_W || area.height < MIN_H {
        put(buf, area, area.x, area.y, "alpheus: window too small", Style::default().fg(th.error));
        return hits;
    }
    if chrome(area) == Chrome::Rail {
        let body = rail::render(buf, area, app, &mut hits);
        tabs::render(buf, body, app, &mut hits, Chrome::Rail, now);
    } else {
        let bar = |y| Rect::new(area.x, y, area.width, 1);
        let body = Rect::new(area.x + 1, area.y + 2, area.width - 2, area.height - 4);
        tab_bar(buf, bar(area.y), app, &mut hits);
        tabs::render(buf, body, app, &mut hits, Chrome::Classic, now);
        status_line(buf, bar(area.bottom() - 2), app);
        shortcut_bar(buf, bar(area.bottom() - 1), app);
    }
    overlays::render(buf, area, app, &mut hits);
    hits
}

fn tab_bar(buf: &mut Buffer, r: Rect, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let mut x = r.x + 1;
    let labels: Vec<String> = Tab::ALL.iter().enumerate().map(|(i, t)| format!(" {} {} ", i + 1, t.title())).collect();
    // The brand ("alpheus" and two spaces) goes first when the eight tabs would not fit beside it.
    let tabs_w: usize = labels.iter().map(|l| l.chars().count()).sum();
    if tabs_w + BRAND_W < usize::from(r.width) {
        x += put(buf, r, x, r.y, "alpheus", Style::default().fg(th.accent).add_modifier(Modifier::BOLD)) + 2;
    }
    for (t, label) in Tab::ALL.iter().zip(&labels) {
        let style = if *t == app.tab { th.selected() } else { Style::default().fg(th.fg) };
        let w = put(buf, r, x, r.y, label, style);
        hits.push(Rect::new(x, r.y, w, 1), Target::Tab(*t));
        // Glued together: each label already carries its own spaces, so
        // the eight tabs fit in 80 columns this way (the brand dropped).
        x = x.saturating_add(w);
    }
}

fn status_line(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let style = Style::default().fg(th.fg).bg(th.surface);
    draw::clear(buf, r, style);
    let last = app.status.last.as_deref().unwrap_or("ready");
    let mut x = r.x + 1;
    x += put(buf, r, x, r.y, &format!("{} · {last}", app.tab.title()), style);
    // Last action error, then the startup warning (persistent).
    for msg in [&app.status.error, &app.status.warning].into_iter().flatten() {
        x = x.saturating_add(put(buf, r, x, r.y, &format!(" · ✗ {msg}"), Style::default().fg(th.error).bg(th.surface)));
    }
}

pub(crate) fn shortcut_pairs(app: &App) -> Vec<(String, &'static str)> {
    let km = &app.keymap;
    let mut pairs: Vec<(String, &'static str)> = Vec::new();
    if app.search.editing {
        pairs.push(("Enter".into(), "Confirm"));
        pairs.push(("Esc".into(), "Clear"));
    } else {
        let adjust = format!("{}/{}", km.label(Action::Decrease), km.label(Action::Increase));
        match app.tab {
            Tab::Home => home::hints(&mut pairs, km, adjust),
            Tab::Sound => sound::hints(&mut pairs, km, adjust),
            Tab::Network => network::hints(&mut pairs, km),
            Tab::Bluetooth => bluetooth::hints(&mut pairs, km),
            Tab::Power => power::hints(&mut pairs, km),
            Tab::Diagnostics => diagnostics::hints(&mut pairs, app, km),
            Tab::Appearance => appearance::hints(&mut pairs, app, km),
            Tab::Displays => displays::hints(&mut pairs, app, km),
        }
        // Generic keys last: a bar too narrow for every pair drops Back, then Palette, then Help
        // then Quit before any key that acts on the focused item.
        pairs.push((km.label(Action::Quit), "Quit"));
        pairs.push((km.label(Action::Help), "Help"));
        pairs.push((km.label(Action::Palette), "Palette"));
        pairs.push((km.label(Action::Back), "Back"));
    }
    pairs
}

fn shortcut_bar(buf: &mut Buffer, r: Rect, app: &App) {
    let th = &app.theme;
    let mut x = r.x + 1;
    for (key, what) in shortcut_pairs(app) {
        if key.is_empty() {
            continue; // action with no key (unassigned in config.toml)
        }
        // Width of the "key label" pair, without the trailing padding: if it
        // doesn't fit whole before the right edge, stop rather than cut a
        // label in the middle of a word.
        let w = key.chars().count() as u16 + 1 + what.chars().count() as u16;
        if x.saturating_add(w) > r.right() {
            break;
        }
        x = x.saturating_add(put(buf, r, x, r.y, &key, Style::default().fg(th.accent).add_modifier(Modifier::BOLD)));
        x = x.saturating_add(put(buf, r, x, r.y, &format!(" {what}  "), Style::default().fg(th.fg)));
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// The fixed time the tests draw with: Sunday 15 November, 14:05.
    pub const T: LocalTime = LocalTime { hour: 14, minute: 5, weekday: 0, day: 15, month: 10 };

    /// Draws `app` on a w×h terminal at `T`; returns the rows and the regions.
    pub fn draw(app: &App, w: u16, h: u16) -> (Vec<String>, HitMap) {
        draw_at(app, w, h, Some(T))
    }

    /// Draws `app` on a w×h terminal at `now`; returns the rows and the regions.
    pub fn draw_at(app: &App, w: u16, h: u16, now: Option<LocalTime>) -> (Vec<String>, HitMap) {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitMap::default();
        t.draw(|f| hits = render_at(f, app, now)).unwrap();
        let buf = t.backend().buffer();
        let rows = (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect();
        (rows, hits)
    }

    /// The cells `render_at` leaves on a w×h terminal.
    pub fn buffer_at(app: &App, w: u16, h: u16, now: Option<LocalTime>) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            render_at(f, app, now);
        })
        .unwrap();
        t.backend().buffer().clone()
    }
}

#[cfg(test)]
mod contrast_tests;
#[cfg(test)]
mod tests;
