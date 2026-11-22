//! The quick panel's cells.

use super::{FAILED_TAG, GLYPH_AT, Line, Live, MORE_AT, PERCENT_AT, PROFILE, Question, Quick, RUNNING_TAG, Row, WIDTH, lines};
use crate::grid::{Grid, Style, text_width};
use crate::list::{self, Boxed};
use crate::osd::{self, BRIGHTNESS, MIC_OFF, MIC_ON, PERCENT_CELLS, volume_glyph};
use crate::segments::SHIELD;
use crate::session::Session;
use crate::system::{HeadsetMode, Volume};

/// The panel's cells: `WIDTH + 2` wide, the lines in the window, the separators for the painter.
pub fn rows(live: &Live, q: &Quick) -> Boxed {
    let all = lines(live, q);
    let visible = q.visible();
    let top = q.top.min(all.len().saturating_sub(visible));
    let mut out = Boxed::default();
    let nets = if q.wifi.open { q.wifi.networks() } else { Vec::new() };
    let devices = if q.bt.open { q.bt.devices() } else { Vec::new() };
    let profiles = if q.vpn.open { q.vpn.listed() } else { Vec::new() };
    for (r, line) in all.iter().skip(top).take(visible).enumerate() {
        let mut g = Grid::empty(WIDTH + 2);
        match *line {
            Line::Separator => out.separators.push(r),
            Line::Header => text_row(&mut g, None, "‹ Outputs", "", Style::NORMAL, false),
            Line::Sink(i) => {
                if let Some(s) = q.sinks.get(i) {
                    text_row(&mut g, Some(if s.default { '●' } else { ' ' }), &s.label, "", Style::NORMAL, q.sink == Some(s.id));
                }
            }
            Line::Row(row) => draw(&mut g, row, live, q),
            l @ (Line::WifiHeader | Line::Network(_) | Line::WifiNote(_) | Line::WifiMore | Line::Password) => {
                q.wifi.draw(&mut g, l, &nets, q.wifi.is_selected(l, &nets))
            }
            l @ (Line::BtHeader | Line::BtDevice(_) | Line::BtNearby | Line::BtNote(_) | Line::BtCode) => {
                q.bt.draw(&mut g, l, &devices, q.confirm.as_ref(), q.bt.is_selected(l, &devices))
            }
            l @ (Line::VpnHeader | Line::VpnProfile(_) | Line::VpnMore) => {
                q.vpn.draw(&mut g, l, &profiles, q.vpn.is_selected(l, &profiles))
            }
            l @ (Line::DndHeader | Line::DndChoice(_)) => q.dnd.draw(&mut g, live, l),
            l @ (Line::ShieldHeader | Line::ShieldDevice(_) | Line::ShieldNote(_)) => {
                if let Some(v) = live.shield {
                    q.shield.draw(&mut g, l, &v);
                }
            }
        }
        out.rows.push(g);
    }
    out
}

pub(super) fn draw(g: &mut Grid, row: Row, live: &Live, q: &Quick) {
    (row.spec().draw)(g, live, q, q.selected == row);
}

pub(super) fn draw_volume(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    if let Some(v) = live.sound.and_then(|s| s.sink) {
        let v = Volume { percent: q.volume_level.unwrap_or(v.percent), ..v };
        slider(g, volume_glyph(v), v, q.sinks.len() >= 2, selected);
    }
}

pub(super) fn draw_headset(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    if let Some(h) = &q.headset {
        let mode = if h.active == HeadsetMode::HiFi { "‹ Hi-Fi ›" } else { "‹ Mic ›" };
        text_row(g, None, &h.label, mode, Style::NORMAL, selected);
    }
}

pub(super) fn draw_brightness(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    if let Some(b) = &q.backlight {
        slider(g, BRIGHTNESS, Volume { percent: b.percent, muted: false }, false, selected);
    }
}

pub(super) fn draw_wifi(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    text_row(g, Some(q.wifi.glyph()), "Wi-Fi", &format!("{} ›", q.wifi.value()), Style::NORMAL, selected);
}

pub(super) fn draw_bluetooth(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    if let Some(s) = live.bluetooth {
        text_row(g, Some(q.bt.glyph(s)), "Bluetooth", &format!("{} ›", q.bt.value(s)), Style::NORMAL, selected);
    }
}

pub(super) fn draw_vpn(g: &mut Grid, _: &Live, q: &Quick, selected: bool) {
    text_row(g, Some(crate::segments::VPN), "VPN", &format!("{} ›", q.vpn.value()), Style::NORMAL, selected);
}

pub(super) fn draw_shield(g: &mut Grid, live: &Live, _: &Quick, selected: bool) {
    let Some(v) = live.shield else { return };
    text_row(g, Some(SHIELD), "Shield", "", Style::NORMAL, selected);
    let (text, style) = if v.failed_until.is_some() {
        (FAILED_TAG, Style::WARN)
    } else if v.switching {
        (RUNNING_TAG, Style::MUTED)
    } else {
        switch(v.state)
    };
    write_cell(g, text, style);
}

pub(super) fn draw_mic(g: &mut Grid, live: &Live, _: &Quick, selected: bool) {
    if let Some(v) = live.sound.and_then(|s| s.source) {
        text_row(g, Some(if v.muted { MIC_OFF } else { MIC_ON }), "Microphone", "", Style::NORMAL, selected);
        write_switch(g, Some(!v.muted));
    }
}

pub(super) fn draw_profile(g: &mut Grid, live: &Live, _: &Quick, selected: bool) {
    if let Some(p) = live.profile.and_then(|p| p.active) {
        text_row(g, Some(PROFILE), "Power profile", &format!("{} ›", p.label()), Style::NORMAL, selected);
    }
}

/// A row with a switch on the right.
pub(super) fn draw_mark(g: &mut Grid, glyph: char, label: &str, on: Option<bool>, selected: bool) {
    text_row(g, Some(glyph), label, "", Style::NORMAL, selected);
    write_switch(g, on);
}

/// A toggle's state, 7 cells (UI refresh, Decision 8): ` ON  ● ` filled in the accent,
/// ` ○ off ` on a pill, `·` dim when unknown.
pub(crate) fn switch(on: Option<bool>) -> (&'static str, Style) {
    match on {
        Some(true) => (" ON  ● ", Style::ACTIVE),
        Some(false) => (" ○ off ", Style::SURFACE),
        None => ("·", Style::MUTED),
    }
}

/// The do-not-disturb row's right side: the switch, or ` until HH:MM ● ` while a timed one is on.
pub(crate) fn dnd_switch(live: &Live) -> (String, Style) {
    if let (Some(true), Some((h, m))) = (live.dnd, live.dnd_end) {
        return (format!(" until {h:02}:{m:02} ● "), Style::ACTIVE);
    }
    let (text, style) = switch(live.dnd);
    (text.to_string(), style)
}

/// `switch(on)` right-aligned to end one cell before the row's end.
pub(super) fn write_switch(g: &mut Grid, on: Option<bool>) {
    let (text, style) = switch(on);
    write_cell(g, text, style);
}

/// `text` right-aligned to end one cell before the row's end.
pub(super) fn write_cell(g: &mut Grid, text: &str, style: Style) {
    let end = g.width();
    g.write(end.saturating_sub(text_width(text) + 1), end, text, style);
}

/// "G ━━━━━━━━━━━━━━────────  64%  ›" (spec § 2), the OSD's level (`osd::level`) one pad cell in:
/// the glyph dim when muted; filled in the accent, the rest dim; muted, all dim; above 100 %, the
/// whole bar in warn. Selected: the band across the whole row, the track included; the bar
/// keeps its own foreground on it.
pub(super) fn slider(g: &mut Grid, glyph: char, v: Volume, more: bool, selected: bool) {
    let end = g.width();
    if selected {
        g.write(0, end, &" ".repeat(usize::from(end)), Style::SELECTED);
    }
    osd::level(g, GLYPH_AT, PERCENT_AT + PERCENT_CELLS, glyph, v, selected);
    if more {
        g.write(MORE_AT, MORE_AT + 1, "›", if selected { Style::SELECTED } else { Style::MUTED });
    }
}

/// A text row as a menu draws it (`list::row`: the icon in the accent, the label), then `right`
/// right-aligned one cell from the end in its own style.
pub(crate) fn text_row(g: &mut Grid, icon: Option<char>, label: &str, right: &str, right_style: Style, selected: bool) {
    let end = g.width();
    list::row(g, 0, end, &list::Row { icon, icon_style: Style::ACCENT, text: label, right: "", style: Style::NORMAL, selected });
    let w = text_width(right);
    if w > 0 {
        g.write(end.saturating_sub(w + 1), end, right, if selected { Style::SELECTED } else { right_style });
    }
}

/// A list's note in place of its entries, dim.
pub(crate) fn note_row(g: &mut Grid, note: &str) {
    let end = g.width();
    g.write(3, end.saturating_sub(1), note, Style::MUTED);
}

/// The session buttons' cells (UI refresh, Decision 7): columns `1..end - 1`, one-cell gaps, equal
/// widths, the remainder on the last. (start, width); drawing and clicks both come from here.
pub(crate) fn session_cells(end: u16, n: usize) -> Vec<(u16, u16)> {
    let n = n as u16;
    if n == 0 {
        return Vec::new();
    }
    let room = end.saturating_sub(2).saturating_sub(n - 1);
    let w = room / n;
    let rest = room - w * n;
    (0..n).map(|i| (1 + i * (w + 1), if i == n - 1 { w + rest } else { w })).collect()
}

/// A button's word on the panel.
pub(crate) fn session_word(s: Session) -> &'static str {
    match s {
        Session::Lock => "Lock",
        Session::Logout => "Log out",
        Session::Reboot => "Reboot",
        Session::Poweroff => "Off",
    }
}

/// How the buttons are labelled, the same for every cell of the row: the richest form that fits all of
/// them (a mix of "󰌾 Lock", "Log out" and "󰐥 Off" reads as a mistake).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LabelForm {
    GlyphAndWord,
    Word,
    Glyph,
}

/// The richest form whose labels all fit their `cells`.
pub(crate) fn session_form(buttons: &[Session], cells: &[(u16, u16)]) -> LabelForm {
    [LabelForm::GlyphAndWord, LabelForm::Word]
        .into_iter()
        .find(|f| buttons.iter().zip(cells).all(|(s, (_, w))| text_width(&session_label(*s, *f)) <= *w))
        .unwrap_or(LabelForm::Glyph)
}

/// A button's label in `form`.
pub(crate) fn session_label(s: Session, form: LabelForm) -> String {
    match form {
        LabelForm::GlyphAndWord => format!("{} {}", s.glyph(), session_word(s)),
        LabelForm::Word => session_word(s).to_string(),
        LabelForm::Glyph => s.glyph().to_string(),
    }
}

/// The session buttons in equal cells on `SURFACE`, the chosen one `ACTIVE` when the row is
/// selected, the label centred. Waiting for a second activation, the row is the question instead,
/// in warn.
pub(super) fn session_row(g: &mut Grid, live: &Live, q: &Quick, selected: bool) {
    let end = g.width();
    if let Some((Question::Session(s), _)) = &q.confirm {
        g.write(1, end.saturating_sub(1), &s.question(), Style::WARN);
        return;
    }
    let buttons = q.buttons(live);
    let cells = session_cells(end, buttons.len());
    let form = session_form(&buttons, &cells);
    for (s, (at, w)) in buttons.iter().zip(cells) {
        let style = if selected && *s == q.button { Style::ACTIVE } else { Style::SURFACE };
        g.write(at, at + w, &" ".repeat(usize::from(w)), style);
        let label = session_label(*s, form);
        let x = at + w.saturating_sub(text_width(&label)) / 2;
        g.write(x, at + w, &label, style);
    }
}
