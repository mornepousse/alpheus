//! The theme's shape applied to controls (see the UI refresh spec). One `chip`
//! draws a filled label the way the style cuts it (square, round caps, an
//! arrow tip, or coloured underlined text); `band`, `frame` and `tile_fill` do
//! the same for rows, cards and tiles. Every write goes through `draw::put` /
//! `draw::clear`, so nothing leaves its clip.

use super::draw::{clear, put};
use crate::theme::Shape;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{BorderType, Borders};

/// The colours of a chip: `fg` text on `fill`, over `behind`.
#[derive(Debug, Clone, Copy)]
pub struct Ink {
    pub fg: Color,
    pub fill: Color,
    pub behind: Color,
    pub bold: bool,
}

/// A filled label, cut by the shape. Returns the width drawn.
pub fn chip(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, ink: Ink, shape: Shape) -> u16 {
    let m = if ink.bold { Modifier::BOLD } else { Modifier::empty() };
    let body = Style::default().fg(ink.fg).bg(ink.fill).add_modifier(m);
    let cap = Style::default().fg(ink.fill).bg(ink.behind);
    match shape {
        Shape::Flat | Shape::Float => put(buf, clip, x, y, text, body),
        Shape::Round => {
            let mut w = put(buf, clip, x, y, "\u{e0b6}", cap);
            w += put(buf, clip, x + w, y, text, body);
            w + put(buf, clip, x + w, y, "\u{e0b4}", cap)
        }
        Shape::Arrow => {
            let w = put(buf, clip, x, y, text, body);
            w + put(buf, clip, x + w, y, "\u{e0b0}", cap)
        }
        // No fill: the fill colour becomes the ink, underlined.
        Shape::Line => put(buf, clip, x, y, text, Style::default().fg(ink.fill).bg(ink.behind).add_modifier(m | Modifier::UNDERLINED)),
    }
}

/// A full-width band (a focused row, an active rail entry), cut by the shape.
pub fn band(buf: &mut Buffer, r: Rect, fill: Color, edge: Color, behind: Color, shape: Shape) {
    let y = r.y;
    match shape {
        Shape::Flat | Shape::Float => {
            clear(buf, r, Style::default().bg(fill));
            put(buf, r, r.x, y, "▌", Style::default().fg(edge).bg(fill));
        }
        Shape::Round => {
            clear(buf, r, Style::default().bg(fill));
            put(buf, r, r.x, y, "\u{e0b6}", Style::default().fg(fill).bg(behind));
            put(buf, r, r.right() - 1, y, "\u{e0b4}", Style::default().fg(fill).bg(behind));
        }
        Shape::Arrow => {
            let inner = Rect::new(r.x, y, r.width.saturating_sub(1), 1);
            clear(buf, inner, Style::default().bg(fill));
            put(buf, r, r.x, y, "▌", Style::default().fg(edge).bg(fill));
            put(buf, r, r.right() - 1, y, "\u{e0b0}", Style::default().fg(fill).bg(behind));
        }
        // No band: a bar on the left only; the label underlines itself.
        Shape::Line => {
            put(buf, r, r.x, y, "▎", Style::default().fg(edge).bg(behind));
        }
    }
}

/// The frame a card takes: border type and sides.
pub fn frame(shape: Shape, focused: bool) -> (BorderType, Borders) {
    match (shape, focused) {
        (Shape::Line, _) => (BorderType::Plain, Borders::TOP),
        (Shape::Float, _) => (BorderType::Double, Borders::ALL),
        (Shape::Round, false) => (BorderType::Rounded, Borders::ALL),
        (Shape::Arrow, false) | (Shape::Flat, false) => (BorderType::Plain, Borders::ALL),
        (_, true) => (BorderType::Thick, Borders::ALL),
    }
}

/// A filled tile, cut by the shape: quadrant corners when round, a drop
/// shadow when floating, a left bar and no fill under the line style.
/// Returns the colour the tile's content sits on.
pub fn tile_fill(buf: &mut Buffer, r: Rect, fill: Color, behind: Color, shape: Shape) -> Color {
    match shape {
        Shape::Line => {
            clear(buf, r, Style::default().bg(behind));
            for y in r.top()..r.bottom() {
                put(buf, r, r.x, y, "▎", Style::default().fg(fill).bg(behind));
            }
            behind
        }
        Shape::Round if r.height >= 3 => {
            clear(buf, r, Style::default().bg(fill));
            let top = Rect::new(r.x, r.y, r.width, 1);
            let bot = Rect::new(r.x, r.bottom() - 1, r.width, 1);
            let edge = Style::default().fg(fill).bg(behind);
            clear(buf, top, edge);
            clear(buf, bot, edge);
            for x in r.left()..r.right() {
                let (t, b) = if x == r.left() {
                    ("▗", "▝")
                } else if x == r.right() - 1 {
                    ("▖", "▘")
                } else {
                    ("▄", "▀")
                };
                put(buf, r, x, r.y, t, edge);
                put(buf, r, x, r.bottom() - 1, b, edge);
            }
            fill
        }
        Shape::Float => {
            let body = Rect::new(r.x, r.y, r.width.saturating_sub(1), r.height.saturating_sub(1));
            clear(buf, r, Style::default().bg(behind));
            clear(buf, body, Style::default().bg(fill));
            let shadow = Style::default().fg(darker(behind)).bg(behind);
            for y in (r.y + 1)..r.bottom() {
                put(buf, r, r.right() - 1, y, "█", shadow);
            }
            for x in (r.x + 1)..r.right() {
                put(buf, r, x, r.bottom() - 1, "▀", shadow);
            }
            fill
        }
        _ => {
            clear(buf, r, Style::default().bg(fill));
            fill
        }
    }
}

/// `c` moved 45 % toward black: a shadow under anything on `c`.
pub fn darker(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            let k = |v: u8| (u16::from(v) * 55 / 100) as u8;
            Color::Rgb(k(r), k(g), k(b))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILL: Color = Color::Rgb(10, 20, 30);
    const BEHIND: Color = Color::Rgb(1, 2, 3);
    const FG: Color = Color::Rgb(200, 210, 220);
    const EDGE: Color = Color::Rgb(250, 0, 0);

    fn buf() -> Buffer {
        let mut b = Buffer::empty(Rect::new(0, 0, 20, 4));
        for c in b.content.iter_mut() {
            c.set_bg(BEHIND);
        }
        b
    }

    fn ink() -> Ink {
        Ink { fg: FG, fill: FILL, behind: BEHIND, bold: false }
    }

    fn sym(b: &Buffer, x: u16, y: u16) -> String {
        b[(x, y)].symbol().to_string()
    }

    #[test]
    fn chip_per_shape() {
        let all = Rect::new(0, 0, 20, 4);
        for shape in [Shape::Flat, Shape::Float] {
            let mut b = buf();
            assert_eq!(chip(&mut b, all, 2, 1, "abc", ink(), shape), 3);
            assert_eq!((sym(&b, 2, 1), b[(2, 1)].bg, b[(2, 1)].fg), ("a".into(), FILL, FG));
            assert_eq!(sym(&b, 5, 1), " ", "{shape:?}: nothing after the text");
        }
        let mut b = buf();
        assert_eq!(chip(&mut b, all, 2, 1, "abc", ink(), Shape::Round), 5);
        assert_eq!((sym(&b, 2, 1), b[(2, 1)].fg, b[(2, 1)].bg), ("\u{e0b6}".into(), FILL, BEHIND));
        assert_eq!((sym(&b, 3, 1), b[(3, 1)].bg), ("a".into(), FILL));
        assert_eq!((sym(&b, 6, 1), b[(6, 1)].fg, b[(6, 1)].bg), ("\u{e0b4}".into(), FILL, BEHIND));
        let mut b = buf();
        assert_eq!(chip(&mut b, all, 2, 1, "abc", ink(), Shape::Arrow), 4);
        assert_eq!((sym(&b, 2, 1), b[(2, 1)].bg), ("a".into(), FILL));
        assert_eq!((sym(&b, 5, 1), b[(5, 1)].fg, b[(5, 1)].bg), ("\u{e0b0}".into(), FILL, BEHIND));
        let mut b = buf();
        assert_eq!(chip(&mut b, all, 2, 1, "abc", ink(), Shape::Line), 3);
        let c = &b[(2, 1)];
        assert_eq!((c.symbol(), c.fg, c.bg), ("a", FILL, BEHIND));
        assert!(c.modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn chip_stays_inside_its_clip() {
        let clip = Rect::new(0, 0, 10, 4);
        let mut b = buf();
        chip(&mut b, clip, 8, 1, "abc", ink(), Shape::Round);
        for x in clip.right()..20 {
            assert_eq!(sym(&b, x, 1), " ", "cell {x} is outside the clip");
        }
    }

    #[test]
    fn band_per_shape() {
        let r = Rect::new(2, 1, 8, 1);
        let mut b = buf();
        band(&mut b, r, FILL, EDGE, BEHIND, Shape::Flat);
        assert_eq!((sym(&b, 2, 1), b[(2, 1)].fg), ("▌".into(), EDGE));
        for x in 2..10 {
            assert_eq!(b[(x, 1)].bg, FILL);
        }
        let mut b = buf();
        band(&mut b, r, FILL, EDGE, BEHIND, Shape::Round);
        assert_eq!((sym(&b, 2, 1), sym(&b, 9, 1)), ("\u{e0b6}".into(), "\u{e0b4}".into()));
        assert_eq!(b[(5, 1)].bg, FILL);
        let mut b = buf();
        band(&mut b, r, FILL, EDGE, BEHIND, Shape::Arrow);
        assert_eq!((sym(&b, 9, 1), b[(9, 1)].bg), ("\u{e0b0}".into(), BEHIND));
        assert_eq!(b[(8, 1)].bg, FILL);
        let mut b = buf();
        band(&mut b, r, FILL, EDGE, BEHIND, Shape::Line);
        assert_eq!((sym(&b, 2, 1), b[(2, 1)].fg), ("▎".into(), EDGE));
        for x in 3..10 {
            assert_eq!(b[(x, 1)].bg, BEHIND);
            assert_eq!(sym(&b, x, 1), " ");
        }
    }

    #[test]
    fn frame_table() {
        use ratatui::widgets::BorderType::*;
        let rows = [
            (Shape::Line, false, (Plain, Borders::TOP)),
            (Shape::Line, true, (Plain, Borders::TOP)),
            (Shape::Float, false, (Double, Borders::ALL)),
            (Shape::Float, true, (Double, Borders::ALL)),
            (Shape::Round, false, (Rounded, Borders::ALL)),
            (Shape::Flat, false, (Plain, Borders::ALL)),
            (Shape::Arrow, false, (Plain, Borders::ALL)),
            (Shape::Round, true, (Thick, Borders::ALL)),
            (Shape::Flat, true, (Thick, Borders::ALL)),
            (Shape::Arrow, true, (Thick, Borders::ALL)),
        ];
        for (shape, focused, want) in rows {
            assert_eq!(frame(shape, focused), want, "{shape:?} focused={focused}");
        }
    }

    #[test]
    fn tile_fill_per_shape() {
        let r = Rect::new(2, 0, 6, 4);
        let mut b = buf();
        assert_eq!(tile_fill(&mut b, r, FILL, BEHIND, Shape::Round), FILL);
        assert_eq!([sym(&b, 2, 0), sym(&b, 7, 0), sym(&b, 2, 3), sym(&b, 7, 3)], ["▗", "▖", "▝", "▘"].map(String::from));
        assert_eq!(b[(4, 1)].bg, FILL);
        assert_eq!(b[(4, 2)].bg, FILL);
        // Two rows: square.
        let mut b = buf();
        let flat = Rect::new(2, 0, 6, 2);
        assert_eq!(tile_fill(&mut b, flat, FILL, BEHIND, Shape::Round), FILL);
        assert_eq!((sym(&b, 2, 0), b[(2, 0)].bg, b[(7, 1)].bg), (" ".into(), FILL, FILL));
        // Float.
        let mut b = buf();
        assert_eq!(tile_fill(&mut b, r, FILL, BEHIND, Shape::Float), FILL);
        assert_eq!((sym(&b, 7, 1), b[(7, 1)].fg), ("█".into(), darker(BEHIND)));
        assert_eq!((sym(&b, 4, 3), b[(4, 3)].fg), ("▀".into(), darker(BEHIND)));
        assert_eq!(b[(6, 2)].bg, FILL, "the body ends one cell short");
        assert_eq!(b[(7, 0)].bg, BEHIND);
        // Line.
        let mut b = buf();
        assert_eq!(tile_fill(&mut b, r, FILL, BEHIND, Shape::Line), BEHIND);
        for y in 0..4 {
            assert_eq!((sym(&b, 2, y), b[(2, y)].fg), ("▎".into(), FILL));
        }
        assert_eq!(b[(4, 1)].bg, BEHIND);
        // Flat and Arrow: plain fill.
        let mut b = buf();
        assert_eq!(tile_fill(&mut b, r, FILL, BEHIND, Shape::Flat), FILL);
        assert_eq!(b[(2, 0)].bg, FILL);
    }

    #[test]
    fn darker_moves_45_percent_to_black() {
        assert_eq!(darker(Color::Rgb(200, 100, 0)), Color::Rgb(110, 55, 0));
        assert_eq!(darker(Color::Reset), Color::Reset);
    }
}
