//! The boot report and the bug report views of the Diag tab. Text from a tool passes `clean_line`
//! again on its way to a cell.

use super::Chrome;
use super::draw::{put, truncate};
use super::hit::{HitMap, Target};
use super::rail::tab_hue;
use crate::app::diagnostics::View;
use crate::app::report_views::wrap_rows;
use crate::app::{App, Loadable};
use alpheus_common::text::clean_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

/// "812ms", "12.345s", "1min 2.345s".
pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{}.{:03}s", ms / 1000, ms % 1000)
    } else {
        format!("{}min {}.{:03}s", ms / 60_000, ms % 60_000 / 1000, ms % 1000)
    }
}

fn header(buf: &mut Buffer, body: Rect, app: &App, chrome: Chrome, title: &str) {
    let th = &app.theme;
    let hue = if chrome == Chrome::Rail { tab_hue(th, app.tab) } else { th.muted };
    put(buf, body, body.x, body.y, title, Style::default().fg(hue).add_modifier(Modifier::BOLD));
    let back = "Esc back";
    put(buf, body, body.right().saturating_sub(back.len() as u16), body.y, back, Style::default().fg(th.ink(th.muted, th.bg, 3.0)));
}

pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, chrome: Chrome) {
    match &app.diag.view {
        View::Boot(_) => boot(buf, body, app, chrome),
        View::Report(_) => report(buf, body, app, hits, chrome),
        _ => {}
    }
}

fn boot(buf: &mut Buffer, body: Rect, app: &App, chrome: Chrome) {
    let View::Boot(b) = &app.diag.view else { return };
    let th = &app.theme;
    let muted = th.ink(th.muted, th.bg, 3.0);
    header(buf, body, app, chrome, "Boot report");
    let mut lines: Vec<(String, ratatui::style::Color)> = Vec::new();
    lines.push((
        match &app.diag.boot {
            Loadable::Ready(t) => clean_line(t, 200),
            Loadable::Failed(e) => format!("✗ {}", clean_line(e, 120)),
            Loadable::Loading => "…".to_string(),
        },
        th.fg,
    ));
    match &b.read {
        None => lines.push(("Reading…".into(), muted)),
        Some((blame, chain)) => {
            lines.push((String::new(), th.fg));
            lines.push(("Slowest units".into(), th.accent));
            match blame {
                Ok(v) if v.is_empty() => lines.push(("  none".into(), muted)),
                Ok(v) => lines.extend(v.iter().map(|u| (format!("  {:>12}  {}", duration(u.duration_ms), clean_line(&u.unit, 80)), th.fg))),
                Err(e) => lines.push((format!("  ✗ {}", clean_line(&e.to_string(), 120)), th.ink(th.error, th.bg, 3.0))),
            }
            lines.push((String::new(), th.fg));
            lines.push(("Critical chain".into(), th.accent));
            match chain {
                Ok(v) if v.is_empty() => lines.push(("  none".into(), muted)),
                Ok(v) => lines.extend(v.iter().map(|l| (format!("  {}", clean_line(l, 160)), th.fg))),
                Err(e) => lines.push((format!("  ✗ {}", clean_line(&e.to_string(), 120)), th.ink(th.error, th.bg, 3.0))),
            }
        }
    }
    let area = Rect::new(body.x, body.y + 2, body.width, body.height.saturating_sub(2));
    b.page.set(usize::from(area.height));
    for (n, (text, color)) in lines.iter().skip(b.scroll).take(usize::from(area.height)).enumerate() {
        put(buf, area, area.x, area.y + n as u16, &truncate(text, usize::from(area.width)), Style::default().fg(*color));
    }
}

fn report(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, chrome: Chrome) {
    let View::Report(r) = &app.diag.view else { return };
    let th = &app.theme;
    header(buf, body, app, chrome, "Bug report");
    put(
        buf,
        body,
        body.x,
        body.y + 1,
        "Read before copying: logs can name things",
        Style::default().fg(th.ink(th.warn, th.bg, 3.0)).add_modifier(Modifier::BOLD),
    );
    let y = body.y + 2;
    if app.report_can_copy() {
        let w = put(buf, body, body.x, y, "[ Copy ]", th.filled());
        put(
            buf,
            body,
            body.x + w + 2,
            y,
            "Enter copies exactly this text; Esc closes and copies nothing",
            Style::default().fg(th.ink(th.muted, th.bg, 3.0)),
        );
        if r.text.is_some() {
            hits.push(Rect::new(body.x, y, w, 1), Target::ReportCopy);
        }
    } else {
        put(buf, body, body.x, y, "No copy program configured ([diagnostics] copy)", Style::default().fg(th.ink(th.muted, th.bg, 3.0)));
    }
    let area = Rect::new(body.x, body.y + 4, body.width, body.height.saturating_sub(4));
    r.page.set(usize::from(area.height));
    r.width.set(usize::from(area.width));
    match &r.text {
        None => {
            put(buf, area, area.x, area.y, "Building the report…", Style::default().fg(th.ink(th.muted, th.bg, 3.0)));
        }
        Some(text) => {
            // Wrapped, never cut: everything Copy sends is somewhere on screen.
            let rows = wrap_rows(text, usize::from(area.width));
            let scroll = r.scroll.min(rows.len().saturating_sub(usize::from(area.height)));
            for (n, line) in rows.iter().skip(scroll).take(usize::from(area.height)).enumerate() {
                put(buf, area, area.x, area.y + n as u16, line, Style::default().fg(th.fg));
            }
        }
    }
}

#[cfg(test)]
mod tests;
