//! The pad's soft wrap: visual rows over the text, and the maps between a byte offset
//! and a (row, column) cell. Pure, and never panics: slices only through `str::get`.

use crate::grid::char_width;

/// One visual row: the byte range shown on it (a `\n` is never inside).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub end: usize,
}

/// Cells taken by `s`.
fn cells(s: &str) -> u16 {
    s.chars().map(char_width).fold(0u16, u16::saturating_add)
}

/// The rows of `text` at `width` cells: a row breaks after its last space that fits (the space stays
/// on it), else at the last scalar that fits; a `\n` ends a row, so a text ending in one ends with an
/// empty row. Always at least one row.
pub fn lines(text: &str, width: u16) -> Vec<Line> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut base = 0;
    for logical in text.split('\n') {
        wrap_one(logical, base, width, &mut out);
        base += logical.len() + 1;
    }
    out
}

/// Wraps one logical line starting at byte `base` of the text.
fn wrap_one(line: &str, base: usize, width: u16, out: &mut Vec<Line>) {
    let mut row_start = 0;
    let mut col = 0u16;
    // Offset right after the last space of the current row.
    let mut after_space: Option<usize> = None;
    for (i, c) in line.char_indices() {
        let w = char_width(c);
        // A scalar wider than the row stays on its own row (`i > row_start` ends the loop).
        while w > 0 && col.saturating_add(w) > width && i > row_start {
            let cut = match after_space {
                Some(b) if b > row_start => b,
                _ => i,
            };
            out.push(Line { start: base + row_start, end: base + cut });
            row_start = cut;
            after_space = None;
            col = line.get(cut..i).map_or(0, cells);
        }
        col = col.saturating_add(w);
        if c == ' ' {
            after_space = Some(i + 1);
        }
    }
    out.push(Line { start: base + row_start, end: base + line.len() });
}

/// The (row, column in cells) of byte offset `at`; an offset at a soft break belongs to the next row.
pub fn locate(text: &str, lines: &[Line], at: usize) -> (usize, u16) {
    let row = lines.partition_point(|l| l.start <= at).saturating_sub(1);
    let Some(l) = lines.get(row) else { return (0, 0) };
    let col = text.get(l.start..at.min(l.end)).map_or(0, cells);
    (row, col)
}

/// The byte offset under (`row`, `column`), clamped to the row; the cell of a wide character gives its
/// start. Past the end of a row that wraps softly, the row's last scalar (its end belongs to the next
/// row).
pub fn offset_at(text: &str, lines: &[Line], row: usize, column: u16) -> usize {
    let row = row.min(lines.len().saturating_sub(1));
    let Some(l) = lines.get(row) else { return 0 };
    let mut col = 0u16;
    let mut last = None;
    for (i, c) in text.get(l.start..l.end).unwrap_or("").char_indices() {
        let w = char_width(c);
        if w == 0 {
            continue;
        }
        if column < col.saturating_add(w) {
            return l.start + i;
        }
        col = col.saturating_add(w);
        last = Some(l.start + i);
    }
    let soft = lines.get(row + 1).is_some_and(|n| n.start == l.end);
    match last {
        Some(at) if soft => at,
        _ => l.end,
    }
}

#[cfg(test)]
mod tests;
