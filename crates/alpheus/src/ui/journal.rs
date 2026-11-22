//! The journal view of the Diag tab: a header (what is asked, the filters), a status line, and the
//! entries, newest at the bottom. Everything that came from a tool passes `clean_line` again on its
//! way to a cell, whatever the parser did.

use super::Chrome;
use super::draw::{put, truncate};
use super::hit::HitMap;
use super::home::LocalTime;
use super::rail::tab_hue;
use crate::app::App;
use crate::app::diagnostics::{View, middle_cut};
use crate::app::journal::{JournalView, Load, Picker, Scroll};
use crate::diagnostics::journal::Entry;
use crate::diagnostics::query::{Priority, Scope};
use alpheus_common::text::clean_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "14:02:11", or "Nov 20 14:02:11" when the entry is not from today. In the tests the zone is
/// UTC, so a snapshot does not depend on the machine it was taken on.
pub fn entry_time(us: u64, today: Option<LocalTime>) -> String {
    let secs = libc::time_t::try_from(us / 1_000_000).unwrap_or(0);
    // SAFETY: gmtime_r and localtime_r write only into the zeroed `tm` owned here.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let ok = if cfg!(test) { libc::gmtime_r(&secs, &mut tm) } else { libc::localtime_r(&secs, &mut tm) };
        if ok.is_null() {
            return "?".into();
        }
        tm
    };
    let clock = format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
    let same_day = today.is_some_and(|t| i32::from(t.day) == tm.tm_mday && i32::from(t.month) == tm.tm_mon);
    if same_day { clock } else { format!("{} {:>2} {clock}", MONTHS[usize::try_from(tm.tm_mon).unwrap_or(0) % 12], tm.tm_mday) }
}

fn priority_word(p: Priority) -> &'static str {
    match p {
        Priority::Any => "any",
        Priority::Err => "err and worse",
        Priority::Warning => "warning and worse",
        Priority::Notice => "notice and worse",
        Priority::Info => "info and worse",
    }
}

fn title(j: &JournalView) -> String {
    if j.query.since_until.is_some() { "Journal · around a crash".to_string() } else { format!("Journal · {}", j.query.preset.label()) }
}

pub fn render(buf: &mut Buffer, body: Rect, app: &App, _hits: &mut HitMap, chrome: Chrome, today: Option<LocalTime>) {
    let View::Journal(j) = &app.diag.view else { return };
    let th = &app.theme;
    let hue = if chrome == Chrome::Rail { tab_hue(th, app.tab) } else { th.muted };
    let ink = |c: Color| th.ink(c, th.bg, 3.0);
    let (muted, err, warn) = (ink(th.muted), ink(th.error), ink(th.warn));
    let mut y = body.y;
    put(buf, body, body.x, y, &title(j), Style::default().fg(hue).add_modifier(Modifier::BOLD));
    let back = "Esc back";
    put(buf, body, body.right().saturating_sub(back.len() as u16), y, back, Style::default().fg(muted));
    y += 1;

    // What is asked and the filters.
    let room = 60.min(usize::from(body.width).saturating_sub(12)).max(8);
    let cut = |u: &str| middle_cut(&clean_line(u, 256), room);
    let unit = match &j.query.unit {
        Some((Scope::User, u)) => format!("{} (user)", cut(u)),
        Some((Scope::System, u)) => cut(u),
        None => "any".to_string(),
    };
    let mut parts = vec![format!("unit: {unit}"), format!("level: {}", priority_word(j.query.priority))];
    if j.query.since_until.is_none() {
        parts.push(format!("follow: {}", if j.follow { "on" } else { "off" }));
    }
    parts.push(if j.filter_editing {
        format!("/{}▌", clean_line(&j.filter, 40))
    } else if j.filter.is_empty() {
        "/ filter".to_string()
    } else {
        format!("filter: {}", clean_line(&j.filter, 40))
    });
    put(buf, body, body.x, y, &truncate(&parts.join("  ·  "), usize::from(body.width)), Style::default().fg(th.fg));
    y += 1;

    if j.system_readable == Some(false) {
        let text = "your own messages only (not in systemd-journal, adm or wheel)";
        put(buf, body, body.x, y, &truncate(text, usize::from(body.width)), Style::default().fg(warn));
        y += 1;
    }
    let status = match &j.load {
        Load::Reading => Some(("Reading…".to_string(), muted)),
        Load::Failed(e) => Some((format!("✗ {}", clean_line(e, 120)), err)),
        _ if j.more_waiting => Some(("… more lines are waiting".to_string(), muted)),
        _ if j.first_cut => Some(("… the read stopped at its limit; older entries are not shown".to_string(), muted)),
        _ => None,
    };
    if let Some((text, color)) = status {
        put(buf, body, body.x, y, &truncate(&text, usize::from(body.width)), Style::default().fg(color));
        y += 1;
    }
    let rest = Rect::new(body.x, y, body.width, body.bottom().saturating_sub(y));
    if let Some(p) = &j.picker {
        return picker(buf, rest, app, p);
    }
    entries(buf, rest, app, j, today);
}

fn entries(buf: &mut Buffer, area: Rect, app: &App, j: &JournalView, today: Option<LocalTime>) {
    let th = &app.theme;
    let ink = |c: Color| th.ink(c, th.bg, 3.0);
    if area.height == 0 {
        return;
    }
    let paused = matches!(j.scroll, Scroll::At(_));
    let footer = paused && j.new_below > 0;
    let rows = usize::from(area.height) - usize::from(footer && area.height > 1);
    j.page.set(rows);
    let all = j.visible();
    if all.is_empty() {
        let text = match (&j.load, j.filter.is_empty()) {
            (Load::Ready, true) => "No entries",
            (Load::Ready, false) => "Nothing matches the filter",
            _ => "",
        };
        put(buf, area, area.x + 2, area.y, text, Style::default().fg(ink(th.muted)));
        return;
    }
    let below = match j.scroll {
        Scroll::Tail => 0,
        Scroll::At(k) => k.min(all.len() - 1),
    };
    let end = all.len() - below;
    let start = end.saturating_sub(rows);
    for (n, e) in all[start..end].iter().enumerate() {
        line(buf, Rect::new(area.x, area.y + n as u16, area.width, 1), app, e, today);
    }
    if footer {
        let text = format!("▼ {} new  (End to follow)", j.new_below);
        put(buf, area, area.x, area.bottom() - 1, &text, Style::default().fg(th.accent).add_modifier(Modifier::BOLD));
    }
}

fn line(buf: &mut Buffer, row: Rect, app: &App, e: &Entry, today: Option<LocalTime>) {
    let th = &app.theme;
    let ink = |c: Color| th.ink(c, th.bg, 3.0);
    let color = match e.priority {
        0..=3 => ink(th.error),
        4 => ink(th.warn),
        5 => th.fg,
        _ => ink(th.muted),
    };
    let ident = match (clean_line(&e.ident, 40), e.pid) {
        (i, Some(p)) if !i.is_empty() => format!("{i}[{p}]"),
        (i, _) => i,
    };
    let time = entry_time(e.time_us, today);
    let text = if ident.is_empty() {
        format!("{time} {}", clean_line(&e.message, 400))
    } else {
        format!("{time} {ident}: {}", clean_line(&e.message, 400))
    };
    put(buf, row, row.x, row.y, &truncate(&text, usize::from(row.width)), Style::default().fg(color));
}

fn picker(buf: &mut Buffer, area: Rect, app: &App, p: &Picker) {
    let th = &app.theme;
    let ink = |c: Color| th.ink(c, th.bg, 3.0);
    if area.height == 0 {
        return;
    }
    put(
        buf,
        area,
        area.x,
        area.y,
        &format!("Service: {}▌", clean_line(&p.query, 60)),
        Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
    );
    let list = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(2));
    let hint = "Enter choose  ·  Backspace clears the unit  ·  Esc closes";
    put(buf, area, area.x, area.bottom().saturating_sub(1), &truncate(hint, usize::from(area.width)), Style::default().fg(ink(th.muted)));
    if let Some(e) = &p.error {
        put(buf, list, list.x + 2, list.y, &format!("✗ {}", clean_line(e, 100)), Style::default().fg(ink(th.error)));
        return;
    }
    if p.units.is_none() {
        put(buf, list, list.x + 2, list.y, "Reading…", Style::default().fg(ink(th.muted)));
        return;
    }
    let matches = p.matches();
    if matches.is_empty() {
        put(buf, list, list.x + 2, list.y, "No service matches", Style::default().fg(ink(th.muted)));
        return;
    }
    let h = usize::from(list.height);
    let top = if p.selected >= h { p.selected + 1 - h } else { 0 };
    for (n, (scope, unit)) in matches.iter().skip(top).take(h).enumerate() {
        let y = list.y + n as u16;
        let on = top + n == p.selected;
        let style = if on { th.selected() } else { Style::default().fg(th.fg) };
        let tag = if *scope == Scope::User { "  user" } else { "" };
        let text = format!(" {}{tag}", clean_line(unit, usize::from(list.width).saturating_sub(8)));
        let w = put(buf, list, list.x, y, &format!("{text:<w$}", w = usize::from(list.width)), style);
        let _ = w;
    }
}

#[cfg(test)]
mod tests;
