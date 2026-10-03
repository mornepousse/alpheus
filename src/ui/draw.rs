//! Écriture bornée dans le tampon : rien ne déborde jamais de son cadre,
//! quelle que soit la taille de la fenêtre.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// Écrit `text` à (x, y) sans sortir de `clip` ; renvoie la largeur écrite.
pub fn put(buf: &mut Buffer, clip: Rect, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let clip = clip.intersection(buf.area);
    if y < clip.top() || y >= clip.bottom() || x < clip.left() || x >= clip.right() {
        return 0;
    }
    let (end, _) = buf.set_stringn(x, y, text, usize::from(clip.right() - x), style);
    end.saturating_sub(x)
}

/// Efface `r` et lui donne `style`.
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

/// Rectangle (w, h) centré dans `area`, réduit s'il ne tient pas.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// Tronque à `n` caractères, « … » final si coupé.
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
