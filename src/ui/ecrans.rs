//! Onglet Écrans : barre des profils, carte à l'échelle, ligne d'une sortie ;
//! et la boîte « Garder ces réglages ? ».

use super::draw::{centered, put, truncate};
use super::hit::{HitMap, Target};
use super::overlays::frame;
use crate::app::ecrans::placement_labels;
use crate::app::{App, Item, Keep};
use crate::displays::geometry::logical_size;
use crate::displays::{active_count, short_names, Output};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// Hauteur maximale de la carte, en lignes.
pub const MAP_ROWS: u16 = 8;

/// Case de la carte : caractère, et appartient-il à l'écran focalisé ?
pub type MapCell = Option<(char, bool)>;

/// Carte à l'échelle des écrans activés, `width` colonnes au plus. Une
/// ligne de terminal vaut à peu près deux colonnes. Chaque écran porte son
/// numéro (rang dans la liste) et son nom court ; celui qui a le focus dans
/// la liste porte « ● ».
pub fn map_canvas(outs: &[Output], focused: Option<&str>, width: u16) -> Vec<Vec<MapCell>> {
    let labels = short_names(outs);
    let boxes: Vec<(usize, i32, i32, i32, i32)> = outs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| {
            o.active.as_ref().map(|a| {
                let (w, h) = logical_size(a.mode, a.scale, a.transform);
                (i, a.x, a.y, w, h)
            })
        })
        .collect();
    if boxes.is_empty() || width < 4 {
        return Vec::new();
    }
    let min_x = boxes.iter().map(|b| b.1).min().unwrap_or(0);
    let min_y = boxes.iter().map(|b| b.2).min().unwrap_or(0);
    let max_x = boxes.iter().map(|b| b.1 + b.3).max().unwrap_or(1);
    let max_y = boxes.iter().map(|b| b.2 + b.4).max().unwrap_or(1);
    let (bw, bh) = (f64::from((max_x - min_x).max(1)), f64::from((max_y - min_y).max(1)));
    let ky = (f64::from(MAP_ROWS) / bh).min(f64::from(width) / bw / 2.0);
    let kx = ky * 2.0;
    let cols = ((bw * kx).round() as usize).clamp(1, usize::from(width));
    let rows = ((bh * ky).round() as usize).clamp(1, usize::from(MAP_ROWS));
    let mut grid: Vec<Vec<MapCell>> = vec![vec![None; cols]; rows];
    for (i, x, y, w, h) in boxes {
        let cx = |v: i32| ((f64::from(v - min_x) * kx).round() as usize).min(cols);
        let cy = |v: i32| ((f64::from(v - min_y) * ky).round() as usize).min(rows);
        let (x0, y0) = (cx(x).min(cols - 1), cy(y).min(rows - 1));
        let x1 = cx(x + w).max(x0 + 1);
        let y1 = cy(y + h).max(y0 + 1);
        let foc = focused == Some(outs[i].name.as_str());
        let mut set = |c: usize, r: usize, ch: char| {
            if r < rows && c < cols {
                grid[r][c] = Some((ch, foc));
            }
        };
        let number = (i + 1).to_string();
        let (wc, hc) = (x1 - x0, y1 - y0);
        if hc == 1 || wc < 3 {
            // Trop petit pour un cadre : « [Alpha 3] » sur une ligne.
            let text = truncate(&format!("[{} {number}]", labels[i]), wc);
            for (k, ch) in text.chars().enumerate() {
                set(x0 + k, y0, ch);
            }
            continue;
        }
        let (r0, r1) = (y0, y1 - 1);
        for c in x0..x1 {
            set(c, r0, '─');
            set(c, r1, '─');
        }
        for r in r0..=r1 {
            set(x0, r, '│');
            set(x1 - 1, r, '│');
        }
        set(x0, r0, '┌');
        set(x1 - 1, r0, '┐');
        set(x0, r1, '└');
        set(x1 - 1, r1, '┘');
        let name = truncate(&labels[i], wc.saturating_sub(2));
        let start = x0 + 1 + (wc - 2 - name.chars().count()) / 2;
        for (k, ch) in name.chars().enumerate() {
            set(start + k, r0, ch);
        }
        if hc >= 3 {
            let mid = r0 + (r1 - r0) / 2;
            let start = x0 + (wc - number.len()) / 2;
            for (k, ch) in number.chars().enumerate() {
                set(start + k, mid, ch);
            }
            if foc && wc >= 6 {
                set(x1 - 3, mid, '●');
            }
        }
    }
    grid
}

/// Ligne de la barre des profils : « Profils  [ Bureau ]  [ + Enregistrer l'actuel ] ».
pub fn draw_profiles(buf: &mut Buffer, body: Rect, y: u16, focused: bool, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let lx = body.x + 2;
    let label = if focused { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
    put(buf, body, lx, y, "Profils", label);
    let names = app.ecrans.profiles.iter().map(|p| p.name.clone()).chain(std::iter::once("+ Enregistrer l'actuel".to_string()));
    let mut x = lx + 11;
    for (j, name) in names.enumerate() {
        let style = if focused && j == app.ecrans.profile_sel {
            Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.accent)
        };
        let w = put(buf, body, x, y, &format!("[ {name} ]"), style);
        hits.push(Rect::new(x, y, w, 1), Target::DisplayProfile(j));
        x = x.saturating_add(w + 2);
    }
}

/// Ligne d'une sortie : numéro et nom court, connecteur (⚠ DisplayLink),
/// interrupteur, mode, échelle, placement.
#[allow(clippy::too_many_arguments)]
pub fn draw_output(buf: &mut Buffer, body: Rect, y: u16, i: usize, name: &str, focused: bool, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let outs = app.outputs();
    let Some(k) = outs.iter().position(|o| o.name == name) else { return };
    let o = &outs[k];
    let labels = short_names(outs);
    let places = placement_labels(outs);
    let lx = body.x + 2;
    let label = if focused { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
    hits.push(Rect::new(body.x, y, body.width, 1), Target::Item(i));
    put(buf, body, lx, y, &truncate(&format!("{} {}", k + 1, labels[k]), 12), label);
    let conn = if o.displaylink { format!("{} ⚠", o.name) } else { o.name.clone() };
    put(buf, body, lx + 13, y, &truncate(&conn, 10), Style::default().fg(th.muted));
    let last = o.active.is_some() && active_count(outs) <= 1;
    let (text, style) = match (&o.active, last) {
        (Some(_), true) => ("[■] seul actif", Style::default().fg(th.muted)),
        (Some(_), false) => ("[■] activé", Style::default().fg(th.accent)),
        (None, _) => ("[ ] désactivé", Style::default().fg(th.muted)),
    };
    let w = put(buf, body, lx + 23, y, text, style);
    hits.push(Rect::new(lx + 23, y, w, 1), Target::OutputToggle(i));
    let Some(a) = &o.active else { return };
    put(buf, body, lx + 37, y, &a.mode.label(), Style::default().fg(th.fg));
    put(buf, body, lx + 51, y, &format!("échelle {}", a.scale.label()), Style::default().fg(th.fg));
    let px = lx + 64;
    let avail = usize::from(body.right().saturating_sub(px));
    put(buf, body, px, y, &truncate(&places[k], avail), Style::default().fg(th.muted));
}

// ── Surcouches ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn buttons(buf: &mut Buffer, inner: Rect, y: u16, no: &str, yes: &str, yes_focused: bool, yes_enabled: bool, app: &App, hits: &mut HitMap) {
    let th = &app.theme;
    let on = Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD);
    let off = Style::default().fg(th.fg).bg(th.surface);
    let disabled = Style::default().fg(th.muted).bg(th.surface);
    let mut x = inner.x + 1;
    let w = put(buf, inner, x, y, &format!("[ {no} ]"), if yes_focused { off } else { on });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmNo);
    x = x.saturating_add(w + 2);
    let style = match (yes_enabled, yes_focused) {
        (false, _) => disabled,
        (true, true) => on,
        (true, false) => off,
    };
    let w = put(buf, inner, x, y, &format!("[ {yes} ]"), style);
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmYes);
}

/// « Garder ces réglages ? » : résumé, absents, avertissement DisplayLink,
/// compte à rebours ; le focus démarre sur Annuler.
pub fn keep(buf: &mut Buffer, area: Rect, app: &App, k: &Keep, hits: &mut HitMap) {
    let th = &app.theme;
    let Some(t) = &app.ecrans.trial else { return };
    let text = Style::default().fg(th.fg).bg(th.surface);
    // Tête : titre, résumé, absents — coupée par le bas si la fenêtre est
    // basse. Pied : avertissement DisplayLink, ligne vide (sacrifiée la
    // première) et compte à rebours, toujours visibles.
    let mut head: Vec<(String, Style)> = vec![(t.title.clone(), text.add_modifier(Modifier::BOLD))];
    head.extend(t.summary.iter().map(|s| (s.clone(), text)));
    head.extend(t.notes.iter().map(|s| (s.clone(), Style::default().fg(th.muted).bg(th.surface))));
    let mut foot: Vec<(String, Style)> = Vec::new();
    if t.displaylink {
        foot.push(("⚠ Sortie DisplayLink : risque de gel connu sur ce poste".into(), Style::default().fg(th.error).bg(th.surface)));
    }
    let wait = match t.deadline {
        None => "Application en cours…".to_string(),
        Some(_) => format!("Retour automatique dans {} s", t.remaining),
    };
    foot.push((String::new(), text));
    foot.push((wait, Style::default().fg(th.accent).bg(th.surface)));
    let r = centered(area, 66, (head.len() + foot.len()) as u16 + 3);
    let inner = frame(buf, r, " Garder ces réglages ? ", app, hits);
    let room = usize::from(inner.height.saturating_sub(1));
    if head.len() + foot.len() > room {
        foot.remove(foot.len() - 2); // la ligne vide
    }
    head.truncate(room.saturating_sub(foot.len()));
    let skip = foot.len().saturating_sub(room); // extrême : le compte à rebours d'abord
    let lines: Vec<_> = head.iter().chain(foot.iter().skip(skip)).collect();
    for (n, (l, style)) in lines.iter().enumerate() {
        put(buf, inner, inner.x + 1, inner.y + n as u16, &truncate(l, usize::from(inner.width.saturating_sub(2))), *style);
    }
    let y = inner.y + inner.height.saturating_sub(1);
    buttons(buf, inner, y, "Annuler", "Garder", k.keep_focused, t.deadline.is_some(), app, hits);
}

/// La ligne `n` de la carte, dessinée à partir de `x`.
pub fn draw_map_row(buf: &mut Buffer, body: Rect, x: u16, y: u16, row: &[MapCell], app: &App) {
    let th = &app.theme;
    for (c, cell) in row.iter().enumerate() {
        if let Some((ch, foc)) = cell {
            let style = if *foc { Style::default().fg(th.accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(th.fg) };
            put(buf, body, x + c as u16, y, &ch.to_string(), style);
        }
    }
}

/// Nom de la sortie focalisée dans la liste, s'il y en a une.
pub fn focused_output(app: &App) -> Option<String> {
    match app.focused() {
        Some(Item::Output(name)) => Some(name),
        _ => None,
    }
}
