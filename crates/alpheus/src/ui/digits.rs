//! The block digits of Home's dashboard clock: a 3×5 font drawn three cells tall with half blocks
//! (the `spike/tui-dashboard` font, ported as is).
//!
//! Never:
//! - write outside the clip it is given (`draw::put`);
//! - draw anything but the ten digits and the colon: any other character is blank.
//!
//! See: docs/features/control-center.md (Home).

use super::draw::put;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// 3×5 bitmaps, one row per `u8` (the 3 low bits, the left pixel = 4); blank for any other char.
pub fn bitmap(ch: char) -> [u8; 5] {
    match ch {
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 7, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 2, 2, 2],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        ':' => [0, 2, 0, 2, 0],
        _ => [0; 5],
    }
}

/// Draws `ch` three rows tall with half blocks, each pixel `scale` cells wide, without leaving
/// `clip`; returns its width (a colon is one pixel wide).
pub fn big_glyph(buf: &mut Buffer, clip: Rect, x: u16, y: u16, ch: char, scale: u16, style: Style) -> u16 {
    let bm = bitmap(ch);
    let (cols, shift) = if ch == ':' { (1, 1) } else { (3, 0) };
    for row in 0..3u16 {
        // Three cells hold six pixel rows and a glyph has five: the sixth, past the end of the
        // bitmap, is blank. `get` gives None there.
        let top = bm.get(usize::from(row * 2)).copied().unwrap_or(0);
        let bot = bm.get(usize::from(row * 2 + 1)).copied().unwrap_or(0);
        for c in 0..cols {
            let bit = 1 << (2 - c - shift);
            let s = match (top & bit != 0, bot & bit != 0) {
                (true, true) => "█",
                (true, false) => "▀",
                (false, true) => "▄",
                (false, false) => " ",
            };
            for k in 0..scale {
                put(buf, clip, x.saturating_add(c * scale + k), y.saturating_add(row), s, style);
            }
        }
    }
    cols * scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    fn rows(buf: &Buffer, w: u16) -> Vec<String> {
        (0..3).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect()).collect()
    }

    #[test]
    fn big_digits() {
        let area = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::empty(area);
        assert_eq!(big_glyph(&mut buf, area, 0, 0, '1', 1, Style::default()), 3);
        assert_eq!(rows(&buf, 3), ["▄█ ", " █ ", "▀▀▀"]);
        let mut buf = Buffer::empty(area);
        assert_eq!(big_glyph(&mut buf, area, 0, 0, ':', 1, Style::default()), 1);
        assert_eq!(rows(&buf, 1), ["▄", "▄", " "]);
        let mut buf = Buffer::empty(area);
        assert_eq!(big_glyph(&mut buf, area, 0, 0, '1', 2, Style::default()), 6);
        assert_eq!(rows(&buf, 6), ["▄▄██  ", "  ██  ", "▀▀▀▀▀▀"]);
        let mut buf = Buffer::empty(area);
        assert_eq!(big_glyph(&mut buf, area, 0, 0, ':', 2, Style::default()), 2);
        assert_eq!(rows(&buf, 2), ["▄▄", "▄▄", "  "]);
    }

    #[test]
    fn every_digit_has_a_bitmap_and_stays_in_its_clip() {
        for ch in "0123456789:".chars() {
            assert_ne!(bitmap(ch), [0; 5], "{ch}");
        }
        assert_eq!(bitmap('x'), [0; 5]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        let clip = Rect::new(0, 0, 2, 3);
        big_glyph(&mut buf, clip, 0, 0, '8', 1, Style::default());
        assert_eq!(buf[(2, 0)].symbol(), " ", "nothing past the clip");
    }
}
