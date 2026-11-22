//! The Diag tab: the overview (session health, failed units, loops, crashes, boot, the journal's
//! presets, the bug report) and the confirmation of a unit action. Every string that came from a
//! tool, a unit file or the user's config passes `cell` on its way to a cell, whatever the parser
//! already did: nothing a unit can name reaches the terminal as an escape sequence.

use super::Chrome;
use super::draw::{centered, put, truncate};
use super::hit::{HitMap, Target};
use super::home::LocalTime;
use super::overlays::frame;
use super::rail::tab_hue;
use super::shape::{self, Ink};
use crate::app::diagnostics::{DiagItem, Section, View, unit_question};
use crate::app::{App, Item, Loadable};
use crate::backends::UnitAction;
use crate::diagnostics::crashes::signal_name;
use crate::diagnostics::query::Scope;
use crate::theme::Shape;
use alpheus_commun::text::clean_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/// The unit column of a row, in cells.
const UNIT_W: usize = 30;

/// Outside text, made safe for a cell and cut to `max` characters.
fn cell(s: &str, max: usize) -> String {
    clean_line(s, max)
}

/// "Nov 20 14:02" for a time in microseconds since the epoch. In the tests the zone is UTC, so a
/// snapshot does not depend on the machine it was taken on.
pub fn crash_time(us: u64) -> String {
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
    let month = MONTHS[usize::try_from(tm.tm_mon).unwrap_or(0) % 12];
    format!("{month} {:>2} {:02}:{:02}", tm.tm_mday, tm.tm_hour, tm.tm_min)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Fg,
    Muted,
    Ok,
    Err,
    Warn,
}

struct Seg(String, Tone);

fn seg(text: impl Into<String>, tone: Tone) -> Seg {
    Seg(text.into(), tone)
}

enum Kind {
    Plain,
    /// A section heading, with an optional summary at the right.
    Heading(Option<(String, Tone)>),
    /// Preset chips: the row's item (when it has one) and the label.
    Chips(Vec<(Option<usize>, String)>),
    /// The bug report button.
    Button,
}

struct Ln {
    kind: Kind,
    segs: Vec<Seg>,
    item: Option<usize>,
}

fn plain(segs: Vec<Seg>, item: Option<usize>) -> Ln {
    Ln { kind: Kind::Plain, segs, item }
}

fn note(text: impl Into<String>, tone: Tone) -> Ln {
    plain(vec![seg(text, tone)], None)
}

fn blank() -> Ln {
    plain(Vec::new(), None)
}

fn heading(title: impl Into<String>, right: Option<(String, Tone)>) -> Ln {
    Ln { kind: Kind::Heading(right), segs: vec![seg(title, Tone::Fg)], item: None }
}

/// The error line of a section and whether its list is stale (dimmed).
fn errors<T>(out: &mut Vec<Ln>, label: Option<&str>, s: &Section<T>) -> bool {
    if let Some(e) = &s.error {
        let prefix = label.map_or(String::new(), |l| format!("{l}: "));
        out.push(note(format!("✗ {prefix}{}", cell(&e.to_string(), 120)), Tone::Err));
    }
    s.error.is_some()
}

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}

fn lines(app: &App, width: u16) -> Vec<Ln> {
    let items = app.items();
    let idx = |d: DiagItem| items.iter().position(|i| matches!(i, Item::Diag(x) if *x == d));
    let d = &app.diag;
    let mut out: Vec<Ln> = Vec::new();

    // Session health.
    let health = app.health_rows();
    let summary = if health.is_empty() {
        ("…".to_string(), Tone::Muted)
    } else {
        let s = crate::app::diagnostics::health_summary(&health);
        let tone = if s.starts_with('✓') { Tone::Ok } else { Tone::Err };
        (s, tone)
    };
    out.push(heading("Session health", Some(summary)));
    for (n, row) in health.iter().enumerate() {
        let (glyph, tone) = if row.ok { ("✓ ", Tone::Ok) } else { ("✗ ", Tone::Err) };
        out.push(plain(vec![seg(glyph, tone), seg(cell(&row.text, 160), Tone::Fg)], idx(DiagItem::HealthRow(n))));
        if let Some(fix) = &row.fix {
            out.push(note(format!("    fix: {}", cell(fix, 160)), Tone::Muted));
        }
    }
    out.push(blank());

    // Failed units, system then user.
    let total = d.failed_system.list().len() + d.failed_user.list().len();
    let title = if total > 0 { format!("Failed units ({total})") } else { "Failed units".to_string() };
    out.push(heading(title, None));
    let scopes = [(Scope::System, "system", &d.failed_system), (Scope::User, "user", &d.failed_user)];
    let mut stale = [false; 2];
    for (n, (_, label, s)) in scopes.iter().enumerate() {
        stale[n] = errors(&mut out, Some(label), s);
    }
    for (n, (scope, label, s)) in scopes.iter().enumerate() {
        if s.missing() {
            continue;
        }
        let dim = if stale[n] { Tone::Muted } else { Tone::Fg };
        for u in s.list() {
            out.push(plain(
                vec![
                    seg("● ", Tone::Err),
                    seg(pad(&cell(&u.unit, UNIT_W), UNIT_W), dim),
                    seg(format!("  {label:<6}  "), Tone::Muted),
                    seg(cell(&u.description, 120), Tone::Muted),
                ],
                idx(DiagItem::Failed(*scope, u.unit.clone())),
            ));
        }
    }
    if total == 0 {
        if d.failed_system.loading() && d.failed_user.loading() {
            out.push(note("…", Tone::Muted));
        } else if d.failed_system.error.is_none() && d.failed_user.error.is_none() {
            out.push(note("no failed unit", Tone::Muted));
        }
    }
    out.push(blank());

    // Units restarting in a loop.
    out.push(heading("Restarting in a loop", None));
    let loops = [(Scope::System, "system", &d.loops_system), (Scope::User, "user", &d.loops_user)];
    let mut stale = [false; 2];
    for (n, (_, label, s)) in loops.iter().enumerate() {
        stale[n] = errors(&mut out, Some(label), s);
    }
    let any = d.loops_system.list().len() + d.loops_user.list().len() > 0;
    for (n, (scope, label, s)) in loops.iter().enumerate() {
        if s.missing() {
            continue;
        }
        let dim = if stale[n] { Tone::Muted } else { Tone::Fg };
        for l in s.list() {
            out.push(plain(
                vec![
                    seg("↻ ", Tone::Warn),
                    seg(pad(&cell(&l.unit, UNIT_W), UNIT_W), dim),
                    seg(format!("  {label:<6}  "), Tone::Muted),
                    seg(format!("{} restarts, {}", l.restarts, cell(&l.active, 24)), Tone::Muted),
                ],
                idx(DiagItem::Loop(*scope, l.unit.clone())),
            ));
        }
    }
    if !any {
        if d.loops_system.loading() && d.loops_user.loading() {
            out.push(note("…", Tone::Muted));
        } else if d.loops_system.error.is_none() && d.loops_user.error.is_none() {
            out.push(note("none", Tone::Muted));
        }
    }
    out.push(blank());

    // Crashes of the last seven days.
    out.push(heading("Crashes (7 days)", None));
    let stale = errors(&mut out, None, &d.crashes);
    if !d.crashes.missing() {
        let dim = if stale { Tone::Muted } else { Tone::Fg };
        for (i, c) in d.crashes.list().iter().enumerate() {
            out.push(plain(
                vec![
                    seg("✗ ", Tone::Err),
                    seg(crash_time(c.time_us), dim),
                    seg(format!("  {}  ", pad(&cell(&c.exe_name, 20), 16)), dim),
                    seg(format!("{:<8}", signal_name(c.signal)), Tone::Warn),
                    seg(if c.core_present { "core present" } else { "core missing" }, Tone::Muted),
                ],
                idx(DiagItem::Crash(i)),
            ));
        }
    }
    if d.crashes.list().is_empty() && !d.crashes.missing() {
        if d.crashes.loading() {
            out.push(note("…", Tone::Muted));
        } else if d.crashes.error.is_none() {
            out.push(note("no crash in 7 days", Tone::Muted));
        }
    }
    out.push(blank());

    // The boot line.
    let boot = match &d.boot {
        Loadable::Loading => seg("…", Tone::Muted),
        Loadable::Failed(e) => seg(format!("✗ {}", cell(e, 120)), Tone::Err),
        Loadable::Ready(l) => seg(cell(l, 160), Tone::Fg),
    };
    out.push(plain(vec![seg("Boot   ", Tone::Muted), boot], idx(DiagItem::Boot)));
    out.push(blank());

    // The journal's presets, as chips that wrap.
    out.push(heading("Journal", None));
    let room = usize::from(width.saturating_sub(2));
    let mut row: Vec<(Option<usize>, String)> = Vec::new();
    let mut used = 0usize;
    for p in crate::diagnostics::query::Preset::ALL {
        let label = format!(" {} ", p.label());
        let w = label.chars().count() + 2;
        if !row.is_empty() && used + w > room {
            out.push(Ln { kind: Kind::Chips(std::mem::take(&mut row)), segs: Vec::new(), item: None });
            used = 0;
        }
        used += w;
        row.push((idx(DiagItem::Preset(p)), label));
    }
    if !row.is_empty() {
        out.push(Ln { kind: Kind::Chips(row), segs: Vec::new(), item: None });
    }
    out.push(blank());
    out.push(Ln { kind: Kind::Button, segs: Vec::new(), item: idx(DiagItem::Report) });
    out
}

fn tone_color(app: &App, tone: Tone, over: Color) -> Color {
    let th = &app.theme;
    let c = match tone {
        Tone::Fg => return th.fg,
        Tone::Muted => th.muted,
        Tone::Ok => th.ok,
        Tone::Err => th.error,
        Tone::Warn => th.warn,
    };
    th.ink(c, over, 3.0)
}

pub fn render(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, chrome: Chrome, now: Option<LocalTime>) {
    match &app.diag.view {
        View::Overview => overview(buf, body, app, hits, chrome),
        View::Journal(_) => super::journal::render(buf, body, app, hits, chrome, now),
        other => view_stub(buf, body, app, other),
    }
}

/// A view opened from the overview: its own content comes with its own code; for now a title and
/// the way back.
fn view_stub(buf: &mut Buffer, body: Rect, app: &App, view: &View) {
    let th = &app.theme;
    let title = match view {
        View::Journal(_) => return,
        View::Boot(_) => "Boot report".to_string(),
        View::Report(_) => "Bug report".to_string(),
        View::Overview => return,
    };
    put(buf, body, body.x, body.y, &title, Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
    put(buf, body, body.x + 2, body.y + 2, "Esc goes back", Style::default().fg(th.fg));
}

fn overview(buf: &mut Buffer, body: Rect, app: &App, hits: &mut HitMap, chrome: Chrome) {
    let th = &app.theme;
    let rail = chrome == Chrome::Rail;
    let hue = tab_hue(th, app.tab);
    let ls = lines(app, body.width);
    let focus_row = ls
        .iter()
        .position(|l| l.item == Some(app.focus) || matches!(&l.kind, Kind::Chips(c) if c.iter().any(|(i, _)| *i == Some(app.focus))))
        .unwrap_or(0);
    let h = usize::from(body.height);
    let offset = if focus_row >= h { focus_row + 1 - h } else { 0 };
    for (n, l) in ls.iter().skip(offset).take(h).enumerate() {
        let y = body.y + n as u16;
        let row = Rect::new(body.x, y, body.width, 1);
        let focused = l.item == Some(app.focus) && l.item.is_some();
        let line = th.shape == Shape::Line;
        let behind = if focused && rail && !line { th.selection } else { th.bg };
        match &l.kind {
            Kind::Heading(right) => {
                let title = l.segs.first().map_or("", |s| s.0.as_str());
                if rail {
                    // The rule runs up to one space before the summary.
                    let rule_end = right.as_ref().map(|(t, _)| body.right().saturating_sub(t.chars().count() as u16 + 1));
                    super::controls::heading_to(buf, body, body.x, y, title, hue, th, rule_end);
                } else {
                    put(buf, body, body.x, y, title, Style::default().fg(th.muted).add_modifier(Modifier::BOLD));
                }
                if let Some((text, tone)) = right {
                    let w = text.chars().count() as u16;
                    let x = body.right().saturating_sub(w);
                    put(buf, body, x, y, text, Style::default().fg(tone_color(app, *tone, th.bg)));
                }
            }
            Kind::Plain => {
                if let Some(i) = l.item {
                    hits.push(row, Target::Item(i));
                    if focused {
                        if rail {
                            shape::band(buf, row, th.selection, hue, th.bg, th.shape);
                        } else {
                            put(buf, body, body.x, y, "▶", Style::default().fg(th.accent));
                        }
                    }
                }
                let mut x = body.x + 2;
                for Seg(text, tone) in &l.segs {
                    let mut style = Style::default().fg(tone_color(app, *tone, behind));
                    if focused {
                        style = style.add_modifier(Modifier::BOLD);
                        if rail && !line {
                            style = style.bg(th.selection);
                        }
                    }
                    // Cut with a visible `…` rather than by the clip.
                    let room = usize::from(body.right().saturating_sub(x));
                    x = x.saturating_add(put(buf, body, x, y, &truncate(text, room), style));
                }
            }
            Kind::Chips(chips) => {
                let mut x = body.x + 2;
                for (item, label) in chips {
                    let on = *item == Some(app.focus) && item.is_some();
                    let w = if rail {
                        let (fg, fill) = if on { (th.on_accent, hue) } else { (th.ink(th.muted, th.raised, 3.0), th.raised) };
                        let fill = if line && !on { th.muted } else { fill };
                        shape::chip(buf, body, x, y, label, Ink { fg, fill, behind: th.bg, bold: on }, th.shape)
                    } else {
                        let style = if on { th.filled() } else { Style::default().fg(th.accent) };
                        put(buf, body, x, y, &format!("[{}]", label.trim()), style)
                    };
                    if let Some(i) = item {
                        hits.push(Rect::new(x, y, w, 1), Target::Item(*i));
                    }
                    x = x.saturating_add(w + 1);
                }
            }
            Kind::Button => {
                let Some(i) = l.item else { continue };
                let label = " 󰃤 Bug report… ";
                let w = if rail {
                    let (fg, fill) = if focused { (th.on_accent, hue) } else { (th.fg, th.raised) };
                    let fill = if line { hue } else { fill };
                    shape::chip(buf, body, body.x + 2, y, label, Ink { fg, fill, behind: th.bg, bold: focused }, th.shape)
                } else {
                    put(buf, body, body.x + 2, y, "[ Bug report… ]", if focused { th.filled() } else { Style::default().fg(th.accent) })
                };
                if !rail && focused {
                    put(buf, body, body.x, y, "▶", Style::default().fg(th.accent));
                }
                hits.push(Rect::new(body.x, y, w + 2, 1), Target::Item(i));
            }
        }
    }
}

/// Greedy word wrap of `text` in `width` cells (a word longer than a line is cut).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = truncate(word, width);
        match out.last_mut() {
            Some(last) if last.chars().count() + 1 + word.chars().count() <= width => {
                last.push(' ');
                last.push_str(&word);
            }
            _ => out.push(word),
        }
    }
    out
}

/// "Restart mnt-data.mount (system)? …", with Cancel focused first.
#[allow(clippy::too_many_arguments)]
pub fn unit_confirm(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    scope: Scope,
    unit: &str,
    action: UnitAction,
    yes_focused: bool,
    hits: &mut HitMap,
) {
    let th = &app.theme;
    let width = 60.min(area.width);
    let text = wrap(&unit_question(scope, unit, action), usize::from(width.saturating_sub(4)));
    let r = centered(area, width, (text.len() as u16 + 4).min(area.height));
    let inner = frame(buf, r, " Confirmation ", app, hits);
    for (n, line) in text.iter().enumerate() {
        put(buf, inner, inner.x + 1, inner.y + n as u16, line, Style::default().fg(th.fg).bg(th.surface).add_modifier(Modifier::BOLD));
    }
    let y = inner.y + inner.height.saturating_sub(1);
    let on = th.filled();
    let off = Style::default().fg(th.fg).bg(th.surface);
    let mut x = inner.x + 1;
    let w = put(buf, inner, x, y, "[ Cancel ]", if yes_focused { off } else { on });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmNo);
    x = x.saturating_add(w + 2);
    let w = put(buf, inner, x, y, "[ Confirm ]", if yes_focused { on } else { off });
    hits.push(Rect::new(x, y, w, 1), Target::ConfirmYes);
}

#[cfg(test)]
mod tests;
