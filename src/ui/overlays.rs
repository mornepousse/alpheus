//! Surcouches : aide, palette de commandes, confirmation ; et les messages
//! éphémères en coin. Dessinées en dernier : leurs zones priment au clic.

use super::draw::{centered, clear, put, truncate};
use super::ecrans;
use super::hit::{HitMap, Target};
use crate::app::action::{Action, Tab};
use crate::app::{App, Confirm, Overlay, Palette};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Widget};

/// Lignes de résultats visibles dans la palette.
const PALETTE_ROWS: usize = 10;

pub fn render(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    toasts(buf, area, app);
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => help(buf, area, app, hits),
        Overlay::Palette(p) => palette(buf, area, app, p, hits),
        Overlay::Confirm(c) => confirm(buf, area, app, c, hits),
        Overlay::Keep(k) => ecrans::keep(buf, area, app, k, hits),
        Overlay::Panel(p) => ecrans::panel(buf, area, app, p, hits),
    }
}

/// Cadre de surcouche : fond effacé, bordure, titre ; renvoie l'intérieur.
pub(super) fn frame(buf: &mut Buffer, r: Rect, title: &str, app: &App, hits: &mut HitMap) -> Rect {
    let th = &app.theme;
    let style = Style::default().fg(th.fg).bg(th.surface);
    clear(buf, r, style);
    let block = Block::bordered()
        .title(title.to_string())
        .border_style(Style::default().fg(th.accent).bg(th.surface))
        .style(style);
    let inner = block.inner(r);
    block.render(r, buf);
    hits.push(r, Target::Overlay);
    inner
}

fn toasts(buf: &mut Buffer, area: Rect, app: &App) {
    let th = &app.theme;
    for (n, t) in app.toasts.iter().enumerate() {
        let y = area.y + 1 + n as u16;
        let text = format!(" {} {} ", if t.error { "✗" } else { "✓" }, t.text);
        let w = (text.chars().count() as u16).min(area.width.saturating_sub(2));
        let x = area.right().saturating_sub(w + 1);
        let style = Style::default()
            .fg(if t.error { th.error } else { th.ok })
            .bg(th.surface)
            .add_modifier(Modifier::BOLD);
        put(buf, area, x, y, &truncate(&text, usize::from(w)), style);
    }
}

fn help(buf: &mut Buffer, area: Rect, app: &App, hits: &mut HitMap) {
    let line = |label: &str, keys: &[String]| format!("{label:<24}{}", keys.join(", "));
    let mut lines: Vec<String> = Vec::new();
    // Les six « Onglet X » tiennent sur une ligne, à la place du premier :
    // l'aide entière tient ainsi en 80×26.
    let mut tab_keys: Vec<String> = Vec::new();
    let mut tab_line: Option<usize> = None;
    for (a, keys) in app.keymap.all() {
        let k: Vec<String> = keys.iter().map(|c| c.label()).collect();
        if matches!(a, Action::GoTab(_)) {
            tab_line.get_or_insert(lines.len());
            tab_keys.extend(k);
        } else if !k.is_empty() {
            lines.push(line(&a.label(), &k));
        }
    }
    if let (Some(i), false) = (tab_line, tab_keys.is_empty()) {
        lines.insert(i, line(&format!("Onglets 1 à {}", Tab::ALL.len()), &tab_keys));
    }
    lines.push(line("Souris", &["clic, glisser, molette".to_string()]));
    let r = centered(area, 64, lines.len() as u16 + 2);
    let inner = frame(buf, r, " Aide — Échap pour fermer ", app, hits);
    for (n, l) in lines.iter().enumerate().take(usize::from(inner.height)) {
        put(buf, inner, inner.x + 1, inner.y + n as u16, l, Style::default().fg(app.theme.fg).bg(app.theme.surface));
    }
}

fn palette(buf: &mut Buffer, area: Rect, app: &App, p: &Palette, hits: &mut HitMap) {
    let th = &app.theme;
    let matches = app.palette_matches();
    let shown = matches.len().clamp(1, PALETTE_ROWS) as u16;
    let r = centered(area, 56, shown + 4);
    let inner = frame(buf, r, " Palette de commandes ", app, hits);
    put(buf, inner, inner.x + 1, inner.y, &format!("> {}▏", p.query), Style::default().fg(th.accent).bg(th.surface));
    if matches.is_empty() {
        put(buf, inner, inner.x + 1, inner.y + 2, "aucune commande", Style::default().fg(th.muted).bg(th.surface));
        return;
    }
    let visible = usize::from(inner.height).saturating_sub(2);
    let offset = if p.selected >= visible { p.selected + 1 - visible } else { 0 };
    for (n, (label, _)) in matches.iter().enumerate().skip(offset).take(visible) {
        let y = inner.y + 2 + (n - offset) as u16;
        let style = if n == p.selected {
            Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.fg).bg(th.surface)
        };
        let row = Rect::new(inner.x, y, inner.width, 1);
        clear(buf, row, style);
        put(buf, row, inner.x + 1, y, &truncate(label, usize::from(inner.width.saturating_sub(2))), style);
        hits.push(row, Target::PaletteEntry(n));
    }
}

fn confirm(buf: &mut Buffer, area: Rect, app: &App, c: &Confirm, hits: &mut HitMap) {
    let th = &app.theme;
    let r = centered(area, 46, 6);
    let inner = frame(buf, r, " Confirmation ", app, hits);
    put(buf, inner, inner.x + 1, inner.y, c.action.question(),
        Style::default().fg(th.fg).bg(th.surface).add_modifier(Modifier::BOLD));
    let y = inner.y + inner.height.saturating_sub(1);
    let on = Style::default().fg(th.bg).bg(th.accent).add_modifier(Modifier::BOLD);
    let off = Style::default().fg(th.fg).bg(th.surface);
    let mut x = inner.x + 1;
    let w = put(buf, inner, x, y, "[ Annuler ]", if c.yes_focused { off } else { on });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmNo);
    x = x.saturating_add(w + 2);
    let w = put(buf, inner, x, y, "[ Confirmer ]", if c.yes_focused { on } else { off });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmYes);
}
