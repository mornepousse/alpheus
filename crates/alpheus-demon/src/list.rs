//! Lists drawn as rows of cells, shared by the launcher's box, a tray menu
//! and the calendar (spec: "the list rendering and key handling are shared
//! with the launcher"): a pixel frame, a result or menu row (icon, text,
//! right-aligned muted text, the selection inverted to the pane's end), the
//! lines the painter draws between rows (`Boxed`), and the selection
//! arithmetic (clamped, never wrapping, skipping rows that cannot be
//! selected). Pure: every row is a `Grid` written only through
//! `Grid::write`, so controls never reach a cell and wide characters are
//! never cut.

use crate::grid::{Grid, Style, text_width, truncate};

/// Most cells a right-aligned kind takes (the launcher's KIND_MAX).
pub const RIGHT_MAX: u16 = 20;

/// A boxed surface's content: rows of cells, with the lines the painter draws in pixels between them
/// (`Painter::paint_boxed`). The frame itself (a 1 px stroke at the surface edge, rounded corners) and
/// the margin around the cells belong to the painter, not to the grid: a box-drawing glyph sits
/// in the middle of its cell, half a row from the edge at the top and bottom and half a column at
/// the sides, so a frame made of them never has even margins.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Boxed {
    /// Every row as wide as the others (`columns()`).
    pub rows: Vec<Grid>,
    /// Rows drawn as a 1 px horizontal line across the box (their cells stay blank).
    pub separators: Vec<usize>,
    /// A column drawn as a 1 px vertical line through the box (the launcher's preview divider).
    pub divider: Option<u16>,
}

impl Boxed {
    /// The size in cells, frame margin excluded.
    pub fn cells(&self) -> (u16, u16) {
        (self.rows.first().map_or(0, Grid::width), self.rows.len().min(usize::from(u16::MAX)) as u16)
    }
}

/// `rows` as the layouts build them: inside a ring of one blank cell all around (the row above and
/// below, the column left and right), where a frame of box-drawing glyphs used to be. The ring is
/// dropped; `separators` (row indexes) and `divider` (a column) are given in the ring's
/// coordinates and shift with it. Everything column- and row-based in the callers (hit-testing,
/// scrolling) keeps its numbers: it is the same offset of one cell, in a single place.
pub fn boxed(rows: &[Grid], separators: &[usize], divider: Option<u16>) -> Boxed {
    let inner = rows.len().saturating_sub(2);
    let rows = rows.iter().skip(1).take(inner).map(Grid::without_edges).collect();
    Boxed {
        rows,
        separators: separators.iter().filter_map(|r| r.checked_sub(1)).filter(|r| *r < inner).collect(),
        divider: divider.and_then(|c| c.checked_sub(1)),
    }
}

/// Content rows alone (the calendar, a tooltip): each as wide as the widest plus a blank cell on each
/// side (the pad every menu row carries, so the text keeps the same distance from the frame), a
/// narrower row padded with blanks, styles kept.
pub fn padded(content: &[Grid]) -> Boxed {
    let inner = content.iter().map(Grid::width).max().unwrap_or(0);
    let columns = inner.saturating_add(2);
    let rows = content
        .iter()
        .map(|c| {
            let mut g = Grid::empty(columns);
            for (i, cell) in c.cells().iter().enumerate() {
                // Cell by cell, styles kept; a wide character's continuation comes with it.
                if cell.ch != crate::grid::CONT {
                    g.write(1 + i as u16, columns - 1, &cell.ch.to_string(), cell.style);
                }
            }
            g
        })
        .collect();
    Boxed { rows, separators: Vec::new(), divider: None }
}

/// One list row's content.
pub struct Row<'a> {
    pub icon: Option<char>,
    pub text: &'a str,
    /// Right-aligned, MUTED (launcher kinds; menu submenu marker "▸").
    pub right: &'a str,
    /// NORMAL, or MUTED for a disabled menu entry.
    pub style: Style,
    pub selected: bool,
}

/// Writes `row` between `start` and `end` (excluded) of `g`, exactly as the launcher draws a result:
/// the selection INVERTED across the whole span, one blank cell, the icon (ACCENT) and a space, the
/// text truncated with "…", the right text (at most RIGHT_MAX cells) right-aligned one cell from `end`.
pub fn row(g: &mut Grid, start: u16, end: u16, r: &Row) {
    let pick = |s: Style| if r.selected { Style::INVERTED } else { s };
    if r.selected {
        g.write(start, end, &" ".repeat(usize::from(end.saturating_sub(start))), Style::INVERTED);
    }
    let mut x = start + 1;
    if let Some(c) = r.icon {
        x = g.write(x, end, &c.to_string(), pick(Style::ACCENT));
        x = g.write(x, end, " ", pick(Style::NORMAL));
    }
    // At the minimum width, RIGHT_MAX alone can still reach past the icon (whose space `x`
    // already claimed) into the frame: clamped so the right column never starts to the left
    // of where the icon ends.
    let max_right = RIGHT_MAX.min(end.saturating_sub(x).saturating_sub(2));
    let right = truncate(r.right, max_right);
    let right_width = text_width(&right);
    let text_end = if right_width > 0 { end.saturating_sub(right_width + 2) } else { end.saturating_sub(1) };
    g.write(x, text_end, &truncate(r.text, text_end.saturating_sub(x)), pick(r.style));
    if right_width > 0 {
        g.write(end.saturating_sub(right_width + 1), end, &right, pick(Style::MUTED));
    }
}

/// The end of `s` that fits in `max` cells, "…" first when cut: the
/// input line keeps showing what is being typed.
pub fn tail(s: &str, max: u16) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    let reversed: String = s.chars().rev().collect();
    let cut = truncate(&reversed, max);
    // Exactly the one ellipsis `truncate` just appended: `s` may legitimately end with its
    // own "…", right where the cut falls — `trim_end_matches` would eat that one too.
    let stripped = cut.strip_suffix('…').unwrap_or(&cut);
    let kept: String = stripped.chars().rev().collect();
    format!("…{kept}")
}

/// Greedy word wrap to `width` cells, words too long cut with "…". A word that `split_whitespace`
/// handed us but that displays as nothing (a lone bidi mark between two spaces) is skipped
/// outright: kept, it would still occupy a word slot and force a separating space around
/// something invisible — a double space, or a "blank" line made only of such marks.
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let word = truncate(word, width);
        if text_width(&word) == 0 {
            continue;
        }
        if !line.is_empty() && text_width(&line) + 1 + text_width(&word) > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Moves `selected` by `delta` over `len` rows, skipping unselectable ones, clamped (no wrap); stays put
/// when nothing selectable lies that way. The target is `selected + delta` clamped to the list; an
/// unselectable target goes on in the same direction to the next selectable row, else back to the
/// selectable row nearest the target (still beyond `selected`).
pub fn step(selected: usize, delta: i32, len: usize, selectable: impl Fn(usize) -> bool) -> usize {
    if len == 0 || delta == 0 {
        return selected;
    }
    let last = len - 1;
    let target = (selected as i64).saturating_add(i64::from(delta)).clamp(0, last as i64) as usize;
    if delta > 0 {
        (target..=last).find(|&i| selectable(i)).or_else(|| (selected + 1..target).rev().find(|&i| selectable(i))).unwrap_or(selected)
    } else {
        (0..=target).rev().find(|&i| selectable(i)).or_else(|| (target + 1..selected).find(|&i| selectable(i))).unwrap_or(selected)
    }
}

pub fn first_selectable(len: usize, selectable: impl Fn(usize) -> bool) -> Option<usize> {
    (0..len).find(|&i| selectable(i))
}

pub fn last_selectable(len: usize, selectable: impl Fn(usize) -> bool) -> Option<usize> {
    (0..len).rev().find(|&i| selectable(i))
}

/// The first visible row so that `selected` is visible and no blank row is left at the bottom.
pub fn keep_visible(selected: usize, top: usize, rows: usize, len: usize) -> usize {
    let rows = rows.max(1);
    let last_top = len.saturating_sub(rows);
    let top = if selected < top {
        selected
    } else if selected >= top.saturating_add(rows) {
        selected + 1 - rows
    } else {
        top
    };
    top.min(last_top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_skips_unselectable_rows_and_never_wraps() {
        // 0 a, 1 separator, 2 disabled, 3 b, 4 c, 5 separator
        let ok = |i: usize| matches!(i, 0 | 3 | 4);
        assert_eq!(step(0, 1, 6, ok), 3, "over the separator and the disabled entry");
        assert_eq!(step(3, -1, 6, ok), 0, "and back");
        assert_eq!(step(4, 1, 6, ok), 4, "the last selectable: no wrap to the top");
        assert_eq!(step(0, -1, 6, ok), 0, "the first: no wrap to the bottom");
        assert_eq!(step(0, 100, 6, ok), 4, "End: the last selectable, not the trailing separator");
        assert_eq!(step(4, -100, 6, ok), 0, "Home");
        assert_eq!(step(0, 2, 6, ok), 3, "a page landing on the disabled entry goes on to the next");
        let all = |_: usize| true;
        assert_eq!(step(2, 3, 10, all), 5, "the launcher: every row selectable, a plain clamp");
        assert_eq!(step(8, 3, 10, all), 9);
        assert_eq!(step(1, -3, 10, all), 0);
    }

    #[test]
    fn step_stays_when_nothing_selectable_that_way() {
        let ok = |i: usize| i == 2;
        assert_eq!(step(2, 1, 5, ok), 2, "nothing selectable below");
        assert_eq!(step(2, -1, 5, ok), 2, "nothing selectable above");
        assert_eq!(step(0, 1, 0, ok), 0, "an empty list");
        assert_eq!(step(3, 0, 5, ok), 3, "no move");
        assert_eq!(first_selectable(5, ok), Some(2));
        assert_eq!(last_selectable(5, ok), Some(2));
        assert_eq!(first_selectable(5, |_| false), None, "a menu of separators only");
        assert_eq!(last_selectable(0, |_| true), None);
    }

    #[test]
    fn keep_visible_never_leaves_blank_rows() {
        assert_eq!(keep_visible(0, 0, 3, 10), 0);
        assert_eq!(keep_visible(5, 0, 3, 10), 3, "scrolled just enough to show it");
        assert_eq!(keep_visible(1, 3, 3, 10), 1, "back up");
        assert_eq!(keep_visible(9, 0, 3, 10), 7, "the last row at the bottom");
        assert_eq!(keep_visible(8, 9, 3, 10), 7, "a top past the end comes back: no blank row below");
        assert_eq!(keep_visible(4, 0, 10, 5), 0, "everything fits: never scrolled");
        assert_eq!(keep_visible(0, 4, 0, 5), 0, "no rows: treated as one");
    }

    /// The launcher's result row, rebuilt through `list::row`: the same text and styles as its
    /// snapshots (icon ACCENT, name NORMAL, kind MUTED one cell from the end; selected: INVERTED
    /// from `start` to the pane's end).
    #[test]
    fn row_draws_like_the_launcher_result() {
        let mut g = Grid::empty(30);
        let r = Row { icon: Some('#'), text: "Firefox", right: "Web Browser", style: Style::NORMAL, selected: false };
        row(&mut g, 1, 29, &r);
        assert_eq!(g.text(), "  # Firefox      Web Browser  ");
        assert_eq!(g.styles(), format!("..a{}{}..", ".".repeat(14), "m".repeat(11)));
        let mut g = Grid::empty(30);
        row(&mut g, 1, 29, &Row { selected: true, ..r });
        assert_eq!(g.text(), "  # Firefox      Web Browser  ");
        assert_eq!(g.styles(), ".AAAAAAAAAAAAAAAAAAAAAAAAAAAA.");
        let mut g = Grid::empty(30);
        row(&mut g, 1, 29, &Row { icon: None, text: &"x".repeat(40), right: "", style: Style::MUTED, selected: false });
        assert_eq!(g.text(), format!("  {}…  ", "x".repeat(25)), "no right text: the text runs to one cell from the end");
        assert_eq!(g.styles(), format!("..{}..", "m".repeat(26)));
    }

    fn row_of(text: &str, columns: u16) -> Grid {
        let mut g = Grid::empty(columns);
        g.write(0, columns, text, Style::NORMAL);
        g
    }

    #[test]
    fn padded_pads_each_side_makes_rows_as_wide_as_the_widest_and_keeps_styles() {
        let mut a = Grid::empty(3);
        a.write(0, 3, "abc", Style::INVERTED);
        let b = Grid::empty(2);
        let boxed = padded(&[a, b]);
        let text: Vec<String> = boxed.rows.iter().map(Grid::text).collect();
        assert_eq!(text, [" abc ", "     "]);
        assert_eq!(boxed.rows[0].styles(), ".AAA.", "styles kept, the pad cells plain");
        assert_eq!(boxed.cells(), (5, 2));
        assert!(boxed.separators.is_empty() && boxed.divider.is_none());
        assert_eq!(padded(&[]).cells(), (0, 0), "no rows, no cells");
        let mut wide = Grid::empty(4);
        wide.write(0, 4, "日本", Style::NORMAL);
        assert_eq!(padded(&[wide]).rows[0].text(), " 日本 ", "a wide character comes whole");
    }

    #[test]
    fn boxed_drops_the_ring_and_shifts_what_is_given_in_its_coordinates() {
        // Ring row, three content rows (the middle one a separator), ring row; ring column each side.
        let rows = ["      ", " ab c ", "      ", " de   ", "      "].map(|t| row_of(t, 6));
        let b = boxed(&rows, &[2, 0, 4], Some(3));
        assert_eq!(b.cells(), (4, 3), "the ring is gone: 2 columns and 2 rows less");
        assert_eq!(b.rows[0].text(), "ab c");
        assert_eq!(b.rows[2].text(), "de  ");
        assert_eq!(b.separators, [1], "row 2 of the ring is row 1 inside; the ring rows themselves are no separator");
        assert_eq!(b.divider, Some(2), "column 3 of the ring is column 2 inside");
        assert_eq!(boxed(&rows, &[], Some(0)).divider, None, "a divider on the ring has nothing to draw");
        assert_eq!(boxed(&[], &[], None).cells(), (0, 0), "nothing at all: nothing panics");
        assert_eq!(boxed(&rows[..1], &[], None).cells(), (0, 0));
    }
}
