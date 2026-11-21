//! Tab content, as rows: one focusable element per row, headings and lists
//! around it; scrolling keeps the focus visible.

use super::Chrome;
use super::appearance;
use super::controls;
use super::displays::{self, MapCell};
use super::draw::{put, truncate};
use super::hit::{HitMap, Target};
use super::home::{self, LocalTime};
use super::rail::tab_hue;
use super::shape::{self, Ink};
use crate::app::action::{Tab, profile_label};
use crate::app::{App, Item, Loadable, toggle_label};
use crate::theme::{Shape, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Maximum width of a slider bar.
const BAR_W: u16 = 24;

enum Row {
    Blank,
    Heading(&'static str),
    Text(String),
    Error(String),
    Item(usize),
    /// Row of the display map.
    Map(usize),
}

pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, chrome: Chrome, now: Option<LocalTime>) {
    if app.tab == Tab::Appearance {
        return appearance::render(buf, body, app, hits);
    }
    // Home as a dashboard beside the rail when its body has room (the classic chrome keeps the rows).
    if app.tab == Tab::Home && chrome == Chrome::Rail && home::fits(body) {
        return home::render(buf, body, app, hits, now);
    }
    let map: Vec<Vec<MapCell>> = if app.tab == Tab::Displays {
        let focused = displays::focused_output(app);
        displays::map_canvas(app.outputs(), focused.as_deref(), body.width.saturating_sub(4))
    } else {
        Vec::new()
    };
    let rail = chrome == Chrome::Rail;
    let hue = tab_hue(&app.theme, app.tab);
    let rows = rows(app, map.len(), rail);
    let label_w = app.tab.spec().label_width;
    let focus_row = rows.iter().position(|r| matches!(r, Row::Item(i) if *i == app.focus)).unwrap_or(0);
    let h = usize::from(body.height);
    let offset = if focus_row >= h { focus_row + 1 - h } else { 0 };
    let items = app.items();
    for (n, row) in rows.iter().skip(offset).take(h).enumerate() {
        let y = body.y + n as u16;
        let th = &app.theme;
        match row {
            Row::Blank => {}
            Row::Heading(t) if rail => controls::heading(buf, body, body.x, y, t, hue, th),
            Row::Heading(t) => {
                put(buf, body, body.x, y, t, Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
            }
            Row::Text(t) => {
                put(buf, body, body.x + 2, y, t, Style::default().fg(th.fg));
            }
            Row::Error(e) => {
                put(buf, body, body.x + 2, y, &format!("✗ {e}"), Style::default().fg(th.error));
            }
            Row::Item(i) => {
                if let Some(item) = items.get(*i) {
                    draw_item(buf, body, y, *i, item, app, label_w, chrome, hits);
                }
            }
            Row::Map(k) => {
                if let Some(line) = map.get(*k) {
                    displays::draw_map_row(buf, body, body.x + 2, y, line, app);
                }
            }
        }
    }
}

/// Rows of the tab; `map_rows`: height of the display map.
fn rows(app: &App, map_rows: usize, rail: bool) -> Vec<Row> {
    let mut rows = Vec::new();
    if app.search.editing || !app.search.text.is_empty() {
        let cursor = if app.search.editing { "▏" } else { "" };
        rows.push(Row::Text(format!("Search: {}{cursor}", app.search.text)));
        rows.push(Row::Blank);
    }
    let items = app.items();
    match app.tab {
        Tab::Home | Tab::Power => {
            for i in 0..items.len() {
                if i > 0 {
                    rows.push(Row::Blank);
                }
                rows.push(Row::Item(i));
            }
        }
        Tab::Sound => {
            let (sinks, sources) = app.visible_nodes();
            let mut i = 0;
            for (title, count) in [("Outputs", sinks.len()), ("Inputs", sources.len())] {
                rows.push(Row::Heading(title));
                match &app.data.audio {
                    Loadable::Loading => rows.push(Row::Text("…".into())),
                    Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                    Loadable::Ready(_) if count == 0 => rows.push(Row::Text("none".into())),
                    Loadable::Ready(_) => {
                        for _ in 0..count {
                            rows.push(Row::Item(i));
                            i += 1;
                        }
                    }
                }
                rows.push(Row::Blank);
            }
            // The "Per-app mixer" button is always the last item of the
            // Sound tab (App::items()); subtraction is guarded so the
            // render never panics even if that invariant were to change.
            rows.push(Row::Item(items.len().saturating_sub(1)));
        }
        Tab::Network => {
            rows.push(Row::Item(0));
            rows.push(Row::Blank);
            rows.push(Row::Heading("Active connections"));
            match &app.data.network {
                Loadable::Loading => rows.push(Row::Text("…".into())),
                Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                Loadable::Ready(n) => {
                    let list: Vec<_> = n.active.iter().filter(|c| app.matches_search(&c.name)).collect();
                    if list.is_empty() {
                        rows.push(Row::Text("none".into()));
                    }
                    for c in list {
                        rows.push(Row::Text(if rail {
                            let glyph = match c.kind.as_str() {
                                "Wi-Fi" => "󰖩",
                                "Ethernet" => "󰈀",
                                "WireGuard" | "VPN" => "󰦝",
                                _ => "󰛳",
                            };
                            format!("{glyph}  {:<24} {:<10} {}", c.name, c.kind, c.device)
                        } else {
                            format!("{:<10} {}  ({})", c.kind, c.name, c.device)
                        }));
                    }
                }
            }
            rows.push(Row::Blank);
            rows.push(Row::Item(1));
        }
        Tab::Appearance | Tab::Diagnostics => {}
        Tab::Displays => {
            rows.push(Row::Item(0));
            rows.push(Row::Blank);
            match &app.data.displays {
                Loadable::Loading => rows.push(Row::Text("…".into())),
                Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                Loadable::Ready(outs) => {
                    rows.push(Row::Heading("Map"));
                    rows.extend((0..map_rows).map(Row::Map));
                    rows.push(Row::Blank);
                    rows.push(Row::Heading("Displays"));
                    if outs.is_empty() {
                        rows.push(Row::Text("no output".into()));
                    }
                    rows.extend((1..=outs.len()).map(Row::Item));
                }
            }
        }
        Tab::Bluetooth => {
            rows.push(Row::Item(0));
            rows.push(Row::Blank);
            rows.push(Row::Heading("Connected devices"));
            match &app.data.bluetooth {
                Loadable::Loading => rows.push(Row::Text("…".into())),
                Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                Loadable::Ready(b) if !b.powered => rows.push(Row::Text("Bluetooth is off".into())),
                Loadable::Ready(b) => {
                    let list: Vec<_> = b.connected.iter().filter(|d| app.matches_search(&d.name)).collect();
                    if list.is_empty() {
                        rows.push(Row::Text("none".into()));
                    }
                    for d in list {
                        rows.push(Row::Text(if rail {
                            format!("󰂱  {:<24} {}", d.name, d.mac)
                        } else {
                            format!("{}  ({})", d.name, d.mac)
                        }));
                    }
                }
            }
            rows.push(Row::Blank);
            rows.push(Row::Item(1));
        }
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn draw_item(buf: &mut Buffer, body: Rect, y: u16, i: usize, item: &Item, app: &App, label_w: u16, chrome: Chrome, hits: &mut HitMap) {
    let th = &app.theme;
    let focused = i == app.focus;
    let rail = chrome == Chrome::Rail;
    let row = Rect::new(body.x, y, body.width, 1);
    let hue = tab_hue(th, app.tab);
    let line = th.shape == Shape::Line;
    // The band under a focused row (Rail); chips on it sit on its colour.
    let behind = if focused && !line { th.selection } else { th.bg };
    let label_style = match (rail, focused) {
        (false, true) => Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        (true, true) if line => Style::default().fg(hue).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        (true, true) => Style::default().fg(th.fg).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(th.fg),
    };
    if rail {
        if focused {
            shape::band(buf, row, th.selection, hue, th.bg, th.shape);
        }
    } else {
        put(buf, body, body.x, y, if focused { "▶" } else { " " }, Style::default().fg(th.accent));
    }
    let lx = body.x + 2;
    let vx = lx + label_w;
    let label_max = usize::from(label_w - 1);
    match item {
        Item::Slider(id) => {
            put(buf, body, lx, y, &truncate(&app.slider_label(*id), label_max), label_style);
            hits.push(row, Target::Item(i));
            let (v, muted) = (app.slider_loadable(*id), app.slider_muted(*id));
            let bar =
                if rail { controls::gauge(buf, body, vx, y, &v, muted, th, hue) } else { draw_slider(buf, body, vx, y, &v, muted, th) };
            if let Some(bar) = bar {
                hits.push(bar, Target::Slider { item: i });
            }
        }
        Item::Toggle(id) => {
            put(buf, body, lx, y, toggle_label(*id), label_style);
            hits.push(row, Target::Item(i));
            if rail {
                controls::switch(buf, body, vx, y, &app.toggle_loadable(*id), th, hue);
            } else {
                draw_toggle(buf, body, vx, y, &app.toggle_loadable(*id), th);
            }
        }
        Item::Profile => {
            put(buf, body, lx, y, "Energy profile", label_style);
            hits.push(row, Target::Item(i));
            match &app.data.profiles {
                Loadable::Loading => {
                    put(buf, body, vx, y, "…", Style::default().fg(th.muted));
                }
                Loadable::Failed(e) => {
                    put(buf, body, vx, y, &format!("✗ {e}"), Style::default().fg(th.error));
                }
                Loadable::Ready(p) => {
                    let mut x = vx;
                    for (j, name) in p.available.iter().enumerate() {
                        let active = *name == p.active;
                        let text = format!(" {} ", profile_label(name));
                        let w = if rail {
                            let (fg, fill) = if active { (th.on_accent, hue) } else { (th.ink(th.muted, th.raised, 3.0), th.raised) };
                            let fill = if line && !active { th.muted } else { fill };
                            shape::chip(buf, body, x, y, &text, Ink { fg, fill, behind, bold: active }, th.shape)
                        } else {
                            let style = if active { th.filled() } else { Style::default().fg(th.muted) };
                            put(buf, body, x, y, &text, style)
                        };
                        hits.push(Rect::new(x, y, w, 1), Target::ProfileOption(j));
                        x = x.saturating_add(w + 1);
                    }
                }
            }
        }
        Item::DisplayProfiles => displays::draw_profiles(buf, body, y, focused, app, hits, rail),
        Item::Output(name) => displays::draw_output(buf, body, y, i, name, focused, app, hits, rail),
        Item::Button(c) => {
            let w = if rail {
                controls::button(buf, body, lx, y, c, focused, th, hue)
            } else {
                let style = if focused { th.filled() } else { Style::default().fg(th.accent) };
                put(buf, body, lx, y, &format!("[ {} ]", c.label()), style)
            };
            // Clickable area intentionally limited to the drawn button (not the whole row).
            hits.push(Rect::new(body.x, y, w + 2, 1), Target::Item(i));
        }
    }
}

fn draw_slider(buf: &mut Buffer, clip: Rect, x: u16, y: u16, v: &Loadable<u8>, muted: bool, th: &Theme) -> Option<Rect> {
    match v {
        Loadable::Loading => {
            put(buf, clip, x, y, "…", Style::default().fg(th.muted));
            None
        }
        Loadable::Failed(e) => {
            put(buf, clip, x, y, &format!("✗ {e}"), Style::default().fg(th.error));
            None
        }
        Loadable::Ready(p) => {
            // 12 columns reserved for "100 % muted".
            let w = BAR_W.min(clip.right().saturating_sub(x).saturating_sub(12));
            if w < 3 {
                put(buf, clip, x, y, &format!("{p} %"), Style::default().fg(th.fg));
                return None;
            }
            let filled = (u32::from((*p).min(100)) * u32::from(w) + 50) / 100;
            let bar: String = (0..w).map(|k| if u32::from(k) < filled { '█' } else { '░' }).collect();
            put(buf, clip, x, y, &bar, Style::default().fg(th.accent));
            let mut tail = format!(" {p:>3} %");
            if muted {
                tail.push_str(" muted");
            }
            put(buf, clip, x + w, y, &tail, Style::default().fg(th.fg));
            Some(Rect::new(x, y, w, 1))
        }
    }
}

fn draw_toggle(buf: &mut Buffer, clip: Rect, x: u16, y: u16, v: &Loadable<bool>, th: &Theme) {
    let (text, style) = match v {
        Loadable::Loading => ("…".to_string(), Style::default().fg(th.muted)),
        Loadable::Failed(e) => (format!("✗ {e}"), Style::default().fg(th.error)),
        Loadable::Ready(true) => ("[■] on".to_string(), Style::default().fg(th.accent)),
        Loadable::Ready(false) => ("[ ] off".to_string(), Style::default().fg(th.muted)),
    };
    put(buf, clip, x, y, &text, style);
}
