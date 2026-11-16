//! The pad drawn as rows of cells: title and status, the wrapped text with its caret and selection,
//! a help row (or the last message). The frame is the painter's (`list::Boxed`). Every number below is
//! in the ring's coordinates, as the launcher's (`render` drops the ring through `list::boxed`).

use crate::grid::{Grid, Style, truncate};
use crate::list::{self, Boxed};
use crate::notes::lines::{lines, locate};
use crate::notes::model::Notes;

pub const COLUMNS: u16 = 60;
pub const TEXT_ROWS: u16 = 14;
pub const MIN_COLUMNS: u16 = 24;
pub const MIN_TEXT_ROWS: u16 = 3;

const HINT: &str = "Type here. Esc closes; the text is kept.";
const HELP: &str = "esc close · ctrl+c copy · ctrl+v paste";
const HELP_READ_ONLY: &str = "esc close · ctrl+c copy";

/// Rows: ring, title, separator, `text_rows` text rows, separator, help, ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotesLayout {
    pub columns: u16,
    pub rows: u16,
    pub text_top: u16,
    pub text_rows: u16,
    pub text_columns: u16,
}

/// 60 columns, 14 text rows, fitted to the output: never more than `output_columns − 2` columns nor
/// less than `MIN_COLUMNS` (or the output's width if smaller), never fewer than `MIN_TEXT_ROWS` rows.
pub fn layout(output_columns: u16, output_rows: u16) -> NotesLayout {
    let columns = COLUMNS.min(output_columns.saturating_sub(2)).max(MIN_COLUMNS.min(output_columns));
    let text_rows = TEXT_ROWS.min(output_rows.saturating_sub(6)).max(MIN_TEXT_ROWS);
    NotesLayout { columns, rows: text_rows + 6, text_top: 3, text_rows, text_columns: columns.saturating_sub(4) }
}

/// The wrap width: one cell is kept for the caret at a row's end (decision 8).
pub fn wrap_width(b: &NotesLayout) -> u16 {
    b.text_columns.saturating_sub(1).max(1)
}

/// The text rows' first column.
const TEXT_LEFT: u16 = 2;

pub fn render(n: &Notes, b: &NotesLayout) -> Boxed {
    let end = b.columns.saturating_sub(2);
    let mut rows: Vec<Grid> = (0..b.rows).map(|_| Grid::empty(b.columns)).collect();
    let mut set = |r: u16, f: &dyn Fn(&mut Grid)| {
        if let Some(g) = rows.get_mut(usize::from(r)) {
            f(g);
        }
    };
    // Title row: the name on the left, the status word flush right.
    let status = n.status();
    // Decision 4: `read only` in warn, `not saved` in error, the others muted.
    let status_style = if n.read_only.is_some() {
        Style::WARN
    } else if n.save_failed {
        Style::ERROR
    } else {
        Style::MUTED
    };
    set(1, &|g| {
        g.write(TEXT_LEFT, end, "Notes", Style::BOLD_ACCENT);
        let w = crate::grid::text_width(status);
        g.write(end.saturating_sub(w), end, status, status_style);
    });
    // Text rows.
    let text = n.pad.text();
    let ls = lines(text, wrap_width(b));
    let selection = n.pad.selection();
    let text_end = TEXT_LEFT + b.text_columns;
    for r in 0..b.text_rows {
        let Some(line) = ls.get(n.top() + usize::from(r)) else { break };
        let Some(shown) = text.get(line.start..line.end) else { continue };
        set(b.text_top + r, &|g| {
            let mut x = TEXT_LEFT;
            let mut at = line.start;
            let mut rest = shown;
            while let Some(c) = rest.chars().next() {
                // A scalar and the zero-width marks glued to it, written together so NFC composes.
                let mut len = c.len_utf8();
                for m in rest.get(len..).unwrap_or("").chars() {
                    if crate::grid::char_width(m) != 0 {
                        break;
                    }
                    len += m.len_utf8();
                }
                let cluster = rest.get(..len).unwrap_or("");
                let style = if selection.as_ref().is_some_and(|s| s.contains(&at)) { Style::SELECTED } else { Style::NORMAL };
                x = g.write(x, text_end, cluster, style);
                at += len;
                rest = rest.get(len..).unwrap_or("");
            }
        });
    }
    // The caret: the cell under it, or a blank one at a row's end.
    let (row, col) = locate(text, &ls, n.pad.caret());
    if let Some(r) = row.checked_sub(n.top()).filter(|r| *r < usize::from(b.text_rows)) {
        let under = n.pad.text().get(n.pad.caret()..).and_then(|s| s.chars().next()).filter(|c| *c != '\n');
        let on_row = ls.get(row).is_some_and(|l| n.pad.caret() < l.end);
        let shown = match under {
            Some(c) if on_row => {
                let mut len = c.len_utf8();
                for m in text.get(n.pad.caret() + len..).unwrap_or("").chars() {
                    if crate::grid::char_width(m) != 0 {
                        break;
                    }
                    len += m.len_utf8();
                }
                text.get(n.pad.caret()..n.pad.caret() + len).unwrap_or(" ")
            }
            _ => " ",
        };
        set(b.text_top + r as u16, &|g| {
            let x = g.write(TEXT_LEFT + col, text_end, shown, Style::ACTIVE);
            // A read-only pad never invites typing.
            if text.is_empty() && n.read_only.is_none() {
                g.write(x + 1, text_end, &truncate(HINT, text_end.saturating_sub(x + 1)), Style::MUTED);
            }
        });
    }
    // Help row: the last message in the warning colour, else the keys.
    let help = if n.read_only.is_some() { HELP_READ_ONLY } else { HELP };
    set(b.text_top + b.text_rows + 1, &|g| match n.message() {
        Some(m) => {
            g.write(TEXT_LEFT, end, &truncate(m, b.text_columns), Style::WARN);
        }
        None => {
            g.write(TEXT_LEFT, end, help, Style::MUTED);
        }
    });
    list::boxed(&rows, &[2, usize::from(b.text_top + b.text_rows)], None)
}

/// The text cell under (`column`, `row`) in ring coordinates, as (visible text row, column in the
/// text area); `None` on the title, the separators, the help row and the frame.
pub fn hit(b: &NotesLayout, column: u16, row: u16) -> Option<(usize, u16)> {
    let r = row.checked_sub(b.text_top).filter(|r| *r < b.text_rows)?;
    let c = column.checked_sub(TEXT_LEFT).filter(|c| *c < b.text_columns)?;
    Some((usize::from(r), c))
}

#[cfg(test)]
mod tests;
