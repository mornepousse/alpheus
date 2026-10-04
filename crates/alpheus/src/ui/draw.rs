//! Bounded writes into the buffer: nothing ever overflows its frame,
//! whatever the window size.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// Writes `text` at (x, y) without leaving `clip`; returns the width written.
pub fn put(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let clip = clip.intersection(buf.area);
    if y < clip.top() || y >= clip.bottom() || x < clip.left() || x >= clip.right() {
        return 0;
    }
    let (end, _) = buf.set_stringn(x, y, text, usize::from(clip.right() - x), style);
    end.saturating_sub(x)
}

/// Clears `r` and gives it `style`.
pub fn clear(buf: &mut Buffer, r: Rect, style: Style) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            let c = &mut buf[(x, y)];
            c.reset();
            c.set_style(style);
        }
    }
}

/// Rectangle (w, h) centered in `area`, shrunk if it doesn't fit.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// Truncates to `n` characters, trailing "…" if cut.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else if n == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}
