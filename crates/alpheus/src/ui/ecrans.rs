//! Onglet Écrans : barre des profils, carte à l'échelle, ligne d'une sortie ;
//! et ses surcouches (panneau de réglage, « Garder ces réglages ? »,
//! enregistrement d'un profil).

use super::draw::{centered, put, truncate};
use super::hit::{HitMap, Target};
use super::overlays::frame;
use crate::app::ecrans::{panel_fields, place_label, placement_labels};
use crate::app::{App, Item, Keep, Panel, PanelField, PlaceChoice, SaveDialog};
use crate::displays::geometry::logical_size;
use crate::displays::{Output, active_count, rotation_label, short_from_description, short_names};
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

fn field_label(f: PanelField) -> &'static str {
    match f {
        PanelField::Mode => "Mode",
        PanelField::Scale => "Échelle",
        PanelField::Rotation => "Rotation",
        PanelField::Place => "Placement",
        PanelField::Reference => "Référence",
        PanelField::Align => "Alignement",
        PanelField::Cancel | PanelField::Apply => "",
    }
}

/// Valeur affichée d'un champ du panneau.
fn field_value(app: &App, p: &Panel, f: PanelField) -> String {
    match f {
        PanelField::Mode => {
            let modes = app.panel_modes(p);
            let k = modes.iter().position(|m| *m == p.mode).map_or(0, |k| k + 1);
            format!("{}  {k}/{}", p.mode.label(), modes.len())
        }
        PanelField::Scale => p.scale.label(),
        PanelField::Rotation => rotation_label(p.transform),
        PanelField::Place => place_label(p.place).to_string(),
        PanelField::Reference => {
            let outs = app.outputs();
            let labels = short_names(outs);
            match &p.reference {
                None => "—".into(),
                Some(d) => outs.iter().position(|o| o.description == *d).map_or_else(|| short_from_description(d), |i| labels[i].clone()),
            }
        }
        PanelField::Align => match p.place {
            PlaceChoice::Rel(r) => p.align.label(r).to_string(),
            _ => String::new(),
        },
        PanelField::Cancel | PanelField::Apply => String::new(),
    }
}

/// Panneau de réglage : un champ par ligne (◀ valeur ▶), la raison d'un
/// refus, puis Annuler et Appliquer (grisé si la géométrie refuse).
pub fn panel(buf: &mut Buffer, area: Rect, app: &App, p: &Panel, hits: &mut HitMap) {
    let th = &app.theme;
    let outs = app.outputs();
    let labels = short_names(outs);
    let title = match outs.iter().position(|o| o.name == p.output) {
        Some(i) => format!(" Réglage — {} ({}) ", labels[i], p.output),
        None => format!(" Réglage — {} ", p.output),
    };
    let fields: Vec<PanelField> = panel_fields(p).into_iter().filter(|f| !matches!(f, PanelField::Cancel | PanelField::Apply)).collect();
    let preview = app.panel_preview(p);
    let extra = u16::from(p.query.is_some()) + 1; // recherche, raison ou ligne vide
    let r = centered(area, 60, fields.len() as u16 + extra + 4);
    let inner = frame(buf, r, &title, app, hits);
    let text = Style::default().fg(th.fg).bg(th.surface);
    for (n, f) in fields.iter().enumerate() {
        let y = inner.y + n as u16;
        if y >= inner.bottom().saturating_sub(1) {
            break;
        }
        let focused = p.field == *f;
        let style = if focused { Style::default().fg(th.accent).bg(th.surface).add_modifier(Modifier::BOLD) } else { text };
        hits.push(Rect::new(inner.x, y, inner.width, 1), Target::PanelField(*f));
        put(buf, inner, inner.x + 1, y, if focused { "▶" } else { " " }, style);
        put(buf, inner, inner.x + 3, y, field_label(*f), style);
        let vx = inner.x + 16;
        put(buf, inner, vx, y, "◀", Style::default().fg(th.accent).bg(th.surface));
        hits.push(Rect::new(vx, y, 1, 1), Target::PanelStep { field: *f, dir: -1 });
        let w = put(buf, inner, vx + 2, y, &field_value(app, p, *f), text);
        put(buf, inner, vx + 3 + w, y, "▶", Style::default().fg(th.accent).bg(th.surface));
        hits.push(Rect::new(vx + 3 + w, y, 1, 1), Target::PanelStep { field: *f, dir: 1 });
    }
    let mut y = inner.y + fields.len() as u16;
    if let Some(q) = &p.query {
        put(buf, inner, inner.x + 3, y, &format!("Recherche : {q}▏"), Style::default().fg(th.accent).bg(th.surface));
        y += 1;
    }
    if let Err(e) = &preview {
        put(
            buf,
            inner,
            inner.x + 3,
            y,
            &truncate(&format!("✗ {e}"), usize::from(inner.width.saturating_sub(4))),
            Style::default().fg(th.error).bg(th.surface),
        );
    }
    let by = inner.y + inner.height.saturating_sub(1);
    let on = Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD);
    let mut x = inner.x + 1;
    let cancel_style = if p.field == PanelField::Cancel { on } else { text };
    let w = put(buf, inner, x, by, "[ Annuler ]", cancel_style);
    hits.push(Rect::new(x, by, w, 1), Target::PanelCancel);
    x = x.saturating_add(w + 2);
    let apply_style = match (preview.is_ok(), p.field == PanelField::Apply) {
        (false, _) => Style::default().fg(th.muted).bg(th.surface),
        (true, true) => on,
        (true, false) => text,
    };
    let w = put(buf, inner, x, by, "[ Appliquer ]", apply_style);
    hits.push(Rect::new(x, by, w, 1), Target::PanelApply);
}

/// « Enregistrer l'actuel » : nom, puis « Remplacer le profil ? » s'il existe.
pub fn save(buf: &mut Buffer, area: Rect, app: &App, d: &SaveDialog, hits: &mut HitMap) {
    let th = &app.theme;
    let r = centered(area, 52, 7);
    let inner = frame(buf, r, " Enregistrer l'actuel ", app, hits);
    let text = Style::default().fg(th.fg).bg(th.surface);
    if d.replace {
        put(buf, inner, inner.x + 1, inner.y, &format!("Remplacer le profil « {} » ?", d.name), text.add_modifier(Modifier::BOLD));
        let y = inner.y + inner.height.saturating_sub(1);
        buttons(buf, inner, y, "Annuler", "Remplacer", d.yes_focused, true, app, hits);
        return;
    }
    put(buf, inner, inner.x + 1, inner.y, "Nom du profil :", text);
    put(buf, inner, inner.x + 1, inner.y + 1, &format!("> {}▏", d.name), Style::default().fg(th.accent).bg(th.surface));
    let y = inner.y + inner.height.saturating_sub(1);
    put(buf, inner, inner.x + 1, y, "Entrée Enregistrer · Échap Annuler", Style::default().fg(th.muted).bg(th.surface));
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
