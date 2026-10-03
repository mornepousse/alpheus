//! Contenu des onglets, en lignes : un élément focalisable par ligne, titres
//! et listes autour ; défilement pour garder le focus visible.

use super::draw::{put, truncate};
use super::hit::{HitMap, Target};
use crate::app::action::{profile_label, Tab};
use crate::app::{toggle_label, App, Item, Loadable};
use crate::theme::Theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Largeur maximale d'une barre de curseur.
const BAR_W: u16 = 24;

enum Row {
    Blank,
    Heading(&'static str),
    Text(String),
    Error(String),
    Item(usize),
}

pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap) {
    let rows = rows(app);
    let label_w: u16 = if app.tab == Tab::Son { 34 } else { 20 };
    let focus_row = rows.iter().position(|r| matches!(r, Row::Item(i) if *i == app.focus)).unwrap_or(0);
    let h = usize::from(body.height);
    let offset = if focus_row >= h { focus_row + 1 - h } else { 0 };
    let items = app.items();
    for (n, row) in rows.iter().skip(offset).take(h).enumerate() {
        let y = body.y + n as u16;
        let th = &app.theme;
        match row {
            Row::Blank => {}
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
                    draw_item(buf, body, y, *i, item, app, label_w, hits);
                }
            }
        }
    }
}

fn rows(app: &App) -> Vec<Row> {
    let mut rows = Vec::new();
    if app.search.editing || !app.search.text.is_empty() {
        let cursor = if app.search.editing { "▏" } else { "" };
        rows.push(Row::Text(format!("Recherche : {}{cursor}", app.search.text)));
        rows.push(Row::Blank);
    }
    let items = app.items();
    match app.tab {
        Tab::Accueil | Tab::Alimentation => {
            for i in 0..items.len() {
                if i > 0 {
                    rows.push(Row::Blank);
                }
                rows.push(Row::Item(i));
            }
        }
        Tab::Son => {
            let (sinks, sources) = app.visible_nodes();
            let mut i = 0;
            for (title, count) in [("Sorties", sinks.len()), ("Entrées", sources.len())] {
                rows.push(Row::Heading(title));
                match &app.data.audio {
                    Loadable::Loading => rows.push(Row::Text("…".into())),
                    Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                    Loadable::Ready(_) if count == 0 => rows.push(Row::Text("aucune".into())),
                    Loadable::Ready(_) => {
                        for _ in 0..count {
                            rows.push(Row::Item(i));
                            i += 1;
                        }
                    }
                }
                rows.push(Row::Blank);
            }
            rows.push(Row::Item(items.len() - 1)); // « Mixage par appli »
        }
        Tab::Reseau => {
            rows.push(Row::Item(0));
            rows.push(Row::Blank);
            rows.push(Row::Heading("Connexions actives"));
            match &app.data.network {
                Loadable::Loading => rows.push(Row::Text("…".into())),
                Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                Loadable::Ready(n) => {
                    let list: Vec<_> = n.active.iter().filter(|c| app.matches_search(&c.name)).collect();
                    if list.is_empty() {
                        rows.push(Row::Text("aucune".into()));
                    }
                    for c in list {
                        rows.push(Row::Text(format!("{:<10} {}  ({})", c.kind, c.name, c.device)));
                    }
                }
            }
            rows.push(Row::Blank);
            rows.push(Row::Item(1));
        }
        Tab::Bluetooth => {
            rows.push(Row::Item(0));
            rows.push(Row::Blank);
            rows.push(Row::Heading("Appareils connectés"));
            match &app.data.bluetooth {
                Loadable::Loading => rows.push(Row::Text("…".into())),
                Loadable::Failed(e) => rows.push(Row::Error(e.clone())),
                Loadable::Ready(b) if !b.powered => rows.push(Row::Text("Bluetooth coupé".into())),
                Loadable::Ready(b) => {
                    let list: Vec<_> = b.connected.iter().filter(|d| app.matches_search(&d.name)).collect();
                    if list.is_empty() {
                        rows.push(Row::Text("aucun".into()));
                    }
                    for d in list {
                        rows.push(Row::Text(format!("{}  ({})", d.name, d.mac)));
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
fn draw_item(buf: &mut Buffer, body: Rect, y: u16, i: usize, item: &Item, app: &App, label_w: u16, hits: &mut HitMap) {
    let th = &app.theme;
    let focused = i == app.focus;
    let row = Rect::new(body.x, y, body.width, 1);
    put(buf, body, body.x, y, if focused { "▶" } else { " " }, Style::default().fg(th.accent));
    let label_style = if focused {
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(th.fg)
    };
    let lx = body.x + 2;
    let vx = lx + label_w;
    let label_max = usize::from(label_w - 1);
    match item {
        Item::Slider(id) => {
            put(buf, body, lx, y, &truncate(&app.slider_label(*id), label_max), label_style);
            hits.push(row, Target::Item(i));
            if let Some(bar) = draw_slider(buf, body, vx, y, &app.slider_loadable(*id), app.slider_muted(*id), th) {
                hits.push(bar, Target::Slider { item: i });
            }
        }
        Item::Toggle(id) => {
            put(buf, body, lx, y, toggle_label(*id), label_style);
            hits.push(row, Target::Item(i));
            draw_toggle(buf, body, vx, y, &app.toggle_loadable(*id), th);
        }
        Item::Profile => {
            put(buf, body, lx, y, "Profil d'énergie", label_style);
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
                        let style = if *name == p.active {
                            Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(th.muted)
                        };
                        let w = put(buf, body, x, y, &format!(" {} ", profile_label(name)), style);
                        hits.push(Rect::new(x, y, w, 1), Target::ProfileOption(j));
                        x = x.saturating_add(w + 1);
                    }
                }
            }
        }
        Item::Button(c) => {
            let style = if focused {
                Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.accent)
            };
            let w = put(buf, body, lx, y, &format!("[ {} ]", c.label()), style);
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
            // 12 colonnes réservées à « 100 % muet ».
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
                tail.push_str(" muet");
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
        Loadable::Ready(true) => ("[■] activé".to_string(), Style::default().fg(th.accent)),
        Loadable::Ready(false) => ("[ ] désactivé".to_string(), Style::default().fg(th.muted)),
    };
    put(buf, clip, x, y, &text, style);
}
