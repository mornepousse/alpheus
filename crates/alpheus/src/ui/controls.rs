//! The rail layout's controls: hued headings, knob gauges, ON/off switches and glyph buttons.
//!
//! They are drawn only under `Chrome::Rail`; the classic layout keeps `tabs.rs`'s own bars and
//! brackets. `heading` and `heading_to` draw a title and a rule and return nothing; `switch` draws
//! a `Loadable<bool>` (`…` while loading, `✗` and the message when failed) and returns nothing;
//! `gauge` draws a `Loadable<u8>` and returns the rectangle a click needs (`Option<Rect>`);
//! `button` draws a `Command` and returns the width it took.
//!
//! Never:
//! - write outside its clip: text goes through `draw::put`, and the one cell `button` recolours is
//!   written only after its own clip check;
//! - change the `App`, or know what the control does when used: `app/mouse.rs` does that from the
//!   regions the caller records.
//!
//! See: docs/specs/2026-11-15-alpheus-ui-refresh-design.md, docs/features/control-center.md.

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
    heading_to(buf, clip, x, y, text, hue, th, None);
}

/// `heading`, its rule running up to column `rule_end` (exclusive) instead of its usual length.
// It takes more arguments than clippy's default limit allows.
#[allow(clippy::too_many_arguments)]
pub fn heading_to(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, hue: Color, th: &Theme, rule_end: Option<u16>) {
    let w = put(buf, clip, x, y, &text.to_uppercase(), Style::default().fg(hue).add_modifier(Modifier::BOLD));
    let end = match rule_end {
        Some(end) => end,
        None => x.saturating_add(w + 1 + GAUGE_W),
    }
    .min(clip.right());
    for k in x.saturating_add(w + 1)..end {
        put(buf, clip, k, y, "─", Style::default().fg(th.raised));
    }
}

/// A thin line gauge with a knob, the value right of it. Returns the bar's rect (for clicks), or
/// none when the room is too narrow for a bar (then only `"62 %"`).
// It takes more arguments than clippy's default limit allows.
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
            // The knob and the filled part are glyphs: the hue (or `fg_dim`) where it reads on what is under
            // the gauge (a focused row's band), else `fg`.
            let under = match buf.cell((x, y)) {
                Some(c) if c.bg != Color::Reset => c.bg,
                _ => th.bg,
            };
            let ink = th.ink(if muted { th.muted } else { hue }, under, 3.0);
            let filled = (u32::from((*p).min(100)) * u32::from(w - 1) + 50) / 100;
            for k in 0..w {
                let k32 = u32::from(k);
                // The filled part, then the knob, then the rest of the line.
                let (g, c) = if k32 < filled {
                    ("━", ink)
                } else if k32 == filled {
                    ("●", ink)
                } else {
                    ("─", th.raised)
                };
                put(buf, clip, x + k, y, g, Style::default().fg(c));
            }
            let mut tail = format!(" {p:>3}%");
            if muted {
                tail.push_str(" 󰝟 muted");
            }
            let tail_ink = if muted { th.ink(th.muted, under, 3.0) } else { th.fg };
            put(buf, clip, x + w, y, &tail, Style::default().fg(tail_ink).add_modifier(Modifier::BOLD));
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
            let fg = th.ink(th.muted, fill, 3.0);
            shape::chip(buf, clip, x, y, " ○  off ", Ink { fg, fill, behind, bold: false }, th.shape);
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
// It takes more arguments than clippy's default limit allows.
#[allow(clippy::too_many_arguments)]
pub fn button(buf: &mut Buffer, clip: Rect, x: u16, y: u16, c: &Command, focused: bool, th: &Theme, hue: Color) -> u16 {
    let line = th.shape == Shape::Line;
    // Resting: the label in `fg` on `raised` (the data's 3.5:1), only the glyph in the hue, and only where
    // the hue reads there (3:1); focused: `on_accent` on the hue.
    let (fg, fill) = if focused { (th.on_accent, hue) } else { (th.fg, th.raised) };
    let fill = if line { hue } else { fill };
    let behind = if focused && !line { th.selection } else { th.bg };
    let glyph = command_glyph(c);
    let text = format!("  {glyph}  {:<20} 󰁔  ", c.label());
    let w = shape::chip(buf, clip, x, y, &text, Ink { fg, fill, behind, bold: focused }, th.shape);
    if !focused && !line {
        // The glyph is the third cell: after the two leading spaces, and after the round cap when there is one.
        let gx = x + 2 + u16::from(th.shape == Shape::Round);
        let gh = th.ink(hue, th.raised, 3.0);
        // The glyph's cell is recoloured only when it lies inside the clip and the buffer.
        if clip.contains((gx, y).into()) {
            if let Some(cell) = buf.cell_mut((gx, y)) {
                cell.set_fg(gh);
            }
        }
    }
    w
}

#[cfg(test)]
mod tests {
    //! Tests of the rail's controls (`controls.rs`): gauge, switch, button and glyphs, drawn into a
    //! plain buffer.

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

    /// A resting button is `fg` on `raised` with only its glyph in the hue (where the hue reads on
    /// `raised`); a focused one is `on_accent` on the hue throughout. The glyph is the third cell,
    /// one further with round caps.
    #[test]
    fn a_resting_button_colours_only_its_glyph() {
        let mut th = th();
        let area = Rect::new(0, 0, 40, 1);
        let lock = Command::Power(PowerAction::Lock);
        let hue_on_raised = th.ink(th.blue, th.raised, 3.0);
        assert_ne!(hue_on_raised, th.fg, "the test needs a hue that differs from the text colour");
        for (shape, glyph_x) in [(Shape::Flat, 3), (Shape::Round, 4)] {
            th.shape = shape;
            let mut buf = Buffer::empty(area);
            button(&mut buf, area, 1, 0, &lock, false, &th, th.blue);
            assert_eq!(buf[(glyph_x, 0)].symbol(), "󰌾", "{shape:?}");
            assert_eq!(buf[(glyph_x, 0)].fg, hue_on_raised, "{shape:?}: the glyph");
            assert_eq!(buf[(glyph_x + 1, 0)].fg, th.fg, "{shape:?}: the cell after it");
            assert_eq!(buf[(glyph_x + 3, 0)].fg, th.fg, "{shape:?}: the label");
            let mut buf = Buffer::empty(area);
            button(&mut buf, area, 1, 0, &lock, true, &th, th.blue);
            assert_eq!(buf[(glyph_x, 0)].fg, th.on_accent, "{shape:?}: focused");
        }
        th.shape = Shape::Line;
        let mut buf = Buffer::empty(area);
        button(&mut buf, area, 1, 0, &lock, false, &th, th.blue);
        assert_eq!(buf[(3, 0)].fg, th.blue, "the line style keeps the hue as the whole ink");
    }

    /// The glyph's recolouring stays inside the clip too: a clip that ends before the glyph leaves
    /// that cell as it was.
    #[test]
    fn a_resting_button_recolours_nothing_outside_its_clip() {
        let th = th();
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);
        let clip = Rect::new(0, 0, 3, 1);
        button(&mut buf, clip, 1, 0, &Command::Power(PowerAction::Lock), false, &th, th.blue);
        assert_eq!(buf[(3, 0)].fg, Color::Reset);
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
