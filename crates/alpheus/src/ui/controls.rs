//! The rail's controls (UI refresh spec): hued headings, knob gauges, ON/off switches and glyph
//! buttons. Drawn only under `Chrome::Rail`; the classic chrome keeps `tabs.rs`'s own bars and
//! brackets. Every write goes through `draw::put`, so nothing leaves its clip.

use super::draw::put;
use super::shape::{self, Ink};
use crate::app::Loadable;
use crate::app::action::{Command, External};
use crate::backends::PowerAction;
use crate::theme::{Shape, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

/// Maximum width of a gauge, and of a heading's rule.
pub const GAUGE_W: u16 = 40;

/// The cells a gauge's tail (`" 100%"` plus `" 󰝟 muted"`) needs after the bar.
const TAIL_ROOM: u16 = 13;

/// A section heading: upper-case, bold, in the tab's hue, then a quiet rule.
pub fn heading(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, hue: Color, th: &Theme) {
    let w = put(buf, clip, x, y, &text.to_uppercase(), Style::default().fg(hue).add_modifier(Modifier::BOLD));
    let end = x.saturating_add(w + 1 + GAUGE_W).min(clip.right());
    for k in x.saturating_add(w + 1)..end {
        put(buf, clip, k, y, "─", Style::default().fg(th.raised));
    }
}

/// A thin line gauge with a knob, the value right of it. Returns the bar's rect (for clicks), or
/// none when the room is too narrow for a bar (then only `"62 %"`).
#[allow(clippy::too_many_arguments)]
pub fn gauge(buf: &mut Buffer, clip: Rect, x: u16, y: u16, v: &Loadable<u8>, muted: bool, th: &Theme, hue: Color) -> Option<Rect> {
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
            let w = GAUGE_W.min(clip.right().saturating_sub(x).saturating_sub(TAIL_ROOM));
            if w < 3 {
                put(buf, clip, x, y, &format!("{p} %"), Style::default().fg(th.fg));
                return None;
            }
            let ink = if muted { th.muted } else { hue };
            let filled = (u32::from((*p).min(100)) * u32::from(w - 1) + 50) / 100;
            for k in 0..w {
                let k32 = u32::from(k);
                let (g, c) = match k32 {
                    k if k < filled => ("━", ink),
                    k if k == filled => ("●", ink),
                    _ => ("─", th.raised),
                };
                put(buf, clip, x + k, y, g, Style::default().fg(c));
            }
            let mut tail = format!(" {p:>3}%");
            if muted {
                tail.push_str(" 󰝟 muted");
            }
            put(buf, clip, x + w, y, &tail, Style::default().fg(if muted { th.muted } else { th.fg }).add_modifier(Modifier::BOLD));
            Some(Rect::new(x, y, w, 1))
        }
    }
}

/// A pill switch: `ON` filled with the hue, `off` on `raised`; `…` while loading, `✗ e` on failure.
pub fn switch(buf: &mut Buffer, clip: Rect, x: u16, y: u16, v: &Loadable<bool>, th: &Theme, hue: Color) {
    let behind = match buf.cell((x, y)).map(|c| c.bg) {
        Some(Color::Reset) | None => th.bg,
        Some(c) => c,
    };
    match v {
        Loadable::Loading => {
            put(buf, clip, x, y, "…", Style::default().fg(th.muted));
        }
        Loadable::Failed(e) => {
            put(buf, clip, x, y, &format!("✗ {e}"), Style::default().fg(th.error));
        }
        Loadable::Ready(true) => {
            shape::chip(buf, clip, x, y, " ON   ● ", Ink { fg: th.on_accent, fill: hue, behind, bold: true }, th.shape);
        }
        Loadable::Ready(false) => {
            let fill = if th.shape == Shape::Line { th.muted } else { th.raised };
            shape::chip(buf, clip, x, y, " ○  off ", Ink { fg: th.muted, fill, behind, bold: false }, th.shape);
        }
    }
}

/// The glyph in front of a button's label, by what the command does (not by its text).
pub fn command_glyph(c: &Command) -> &'static str {
    match c {
        Command::Power(PowerAction::Lock) => "󰌾",
        Command::Power(PowerAction::Logout) => "󰍃",
        Command::Power(PowerAction::Suspend) => "󰤄",
        Command::Power(PowerAction::Reboot) => "󰜉",
        Command::Power(PowerAction::Poweroff) => "󰐥",
        Command::Open(External::Mixer) => "󰝚",
        Command::Open(External::Network) => "󰖩",
        Command::Open(External::Bluetooth) => "󰂯",
        _ => "󰐕",
    }
}

/// A labelled button, every one of the same width; the focused one is filled with the hue.
/// Returns the width drawn.
#[allow(clippy::too_many_arguments)]
pub fn button(buf: &mut Buffer, clip: Rect, x: u16, y: u16, c: &Command, focused: bool, th: &Theme, hue: Color) -> u16 {
    let line = th.shape == Shape::Line;
    let (fg, fill) = if focused { (th.on_accent, hue) } else { (hue, th.raised) };
    let fill = if line { hue } else { fill };
    let behind = if focused && !line { th.selection } else { th.bg };
    let text = format!("  {}  {:<20} 󰁔  ", command_glyph(c), c.label());
    shape::chip(buf, clip, x, y, &text, Ink { fg, fill, behind, bold: focused }, th.shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn th() -> Theme {
        Theme::default()
    }

    fn cells(buf: &Buffer, x: u16, n: u16) -> String {
        (x..x + n).map(|k| buf[(k, 0)].symbol()).collect()
    }

    #[test]
    fn gauge_with_a_knob() {
        let th = th();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        let r = gauge(&mut buf, area, 3, 0, &Loadable::Ready(62), false, &th, th.blue);
        assert_eq!(r, Some(Rect::new(3, 0, 40, 1)));
        let bar = cells(&buf, 3, 40);
        assert_eq!(bar, format!("{}●{}", "━".repeat(24), "─".repeat(15)));
        assert_eq!(buf[(3, 0)].fg, th.blue);
        assert_eq!(buf[(3 + 24, 0)].fg, th.blue, "the knob");
        assert_eq!(buf[(3 + 25, 0)].fg, th.raised);
        assert_eq!(cells(&buf, 43, 5), "  62%");
        // Muted: the ink is `muted` and the tail says so.
        let mut buf = Buffer::empty(area);
        gauge(&mut buf, area, 3, 0, &Loadable::Ready(62), true, &th, th.blue);
        assert_eq!(buf[(3, 0)].fg, th.muted);
        assert_eq!(buf[(3 + 24, 0)].fg, th.muted);
        let tail: String = (43..80).map(|k| buf[(k, 0)].symbol()).collect();
        assert_eq!(tail.trim_end(), "  62% 󰝟 muted");
        // Too narrow for a bar: the number alone.
        let narrow = Rect::new(0, 0, 18, 1);
        let mut buf = Buffer::empty(narrow);
        assert_eq!(gauge(&mut buf, narrow, 3, 0, &Loadable::Ready(62), false, &th, th.blue), None);
        assert_eq!(cells(&buf, 3, 4), "62 %");
    }

    #[test]
    fn switch_on_off_and_line() {
        let mut th = th();
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        switch(&mut buf, area, 2, 0, &Loadable::Ready(true), &th, th.cyan);
        assert_eq!(cells(&buf, 2, 8), " ON   ● ");
        assert_eq!((buf[(2, 0)].bg, buf[(2, 0)].fg), (th.cyan, th.on_accent));
        let mut buf = Buffer::empty(area);
        switch(&mut buf, area, 2, 0, &Loadable::Ready(false), &th, th.cyan);
        assert_eq!(cells(&buf, 2, 8), " ○  off ");
        assert_eq!(buf[(2, 0)].bg, th.raised);
        th.shape = Shape::Line;
        for on in [true, false] {
            let mut buf = Buffer::empty(area);
            switch(&mut buf, area, 2, 0, &Loadable::Ready(on), &th, th.cyan);
            let c = &buf[(3, 0)];
            assert_eq!(c.bg, th.bg, "no fill under the line style");
            assert!(c.modifier.contains(Modifier::UNDERLINED));
            assert_ne!(c.bg, Color::Reset);
        }
        let mut buf = Buffer::empty(area);
        switch(&mut buf, area, 2, 0, &Loadable::Loading, &th, th.cyan);
        assert_eq!(cells(&buf, 2, 1), "…");
        switch(&mut buf, area, 2, 0, &Loadable::Failed("boom".into()), &th, th.cyan);
        assert_eq!(cells(&buf, 2, 6), "✗ boom");
    }

    #[test]
    fn command_glyphs() {
        use crate::app::action::Command::*;
        for (c, g) in [
            (Power(PowerAction::Lock), "󰌾"),
            (Power(PowerAction::Logout), "󰍃"),
            (Power(PowerAction::Suspend), "󰤄"),
            (Power(PowerAction::Reboot), "󰜉"),
            (Power(PowerAction::Poweroff), "󰐥"),
            (Open(External::Mixer), "󰝚"),
            (Open(External::Network), "󰖩"),
            (Open(External::Bluetooth), "󰂯"),
            (ToggleDnd, "󰐕"),
            (GoTab(crate::app::action::Tab::Home), "󰐕"),
        ] {
            assert_eq!(command_glyph(&c), g, "{c:?}");
        }
    }
}
