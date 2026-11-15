//! The pad's model: the buffer, the scroll, the goal column of vertical moves, the dirty and
//! save-failure flags and the one-line message. Pure: the glue does the I/O and tells the model what
//! happened (`after_copy`). Never panics.

use crate::notes::keys::{Motion, NoteKey};
use crate::notes::lines::{lines, locate, offset_at};
use crate::notes::text::{Fit, Pad, TAB};
use alpheus_commun::exec::MAX_INPUT;

/// Why a notes file is shown read only and never written (decision 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    TooLarge,
    NotUtf8,
    NotOurs,
    Unreadable,
}

impl Refusal {
    pub fn word(self) -> &'static str {
        match self {
            Refusal::TooLarge => "is too large",
            Refusal::NotUtf8 => "is not UTF-8",
            Refusal::NotOurs => "is not ours",
            Refusal::Unreadable => "could not be read",
        }
    }
}

/// What the glue does after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotesOutcome {
    Nothing,
    Redraw,
    Close,
    /// Run the copy command on `text`; `cut`: remove the selection once it started.
    Copy {
        text: String,
        cut: bool,
    },
    /// Read the clipboard and hand the text back through `paste`.
    Paste,
}

/// Wrap width and rows before the first `set_area` (the default box: 60 columns, 14 text rows).
const DEFAULT_WIDTH: u16 = 55;
const DEFAULT_ROWS: usize = 14;

pub struct Notes {
    pub pad: Pad,
    top: usize,
    /// The column vertical moves aim for; kept across them, reset by any other key.
    goal: Option<u16>,
    pub dirty: bool,
    pub read_only: Option<Refusal>,
    pub save_failed: bool,
    message: Option<String>,
    width: u16,
    rows: usize,
}

impl Notes {
    /// A pad on `text`, the caret at its end and scrolled to it.
    pub fn new(text: String, read_only: Option<Refusal>) -> Notes {
        let mut n = Notes {
            pad: Pad::new(text),
            top: 0,
            goal: None,
            dirty: false,
            read_only,
            save_failed: false,
            message: None,
            width: DEFAULT_WIDTH,
            rows: DEFAULT_ROWS,
        };
        n.follow_caret();
        n
    }

    /// The wrap width and the number of text rows; keeps the caret visible.
    pub fn set_area(&mut self, width: u16, rows: usize) {
        self.width = width.max(1);
        self.rows = rows.max(1);
        self.follow_caret();
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn show_message(&mut self, m: String) {
        self.message = Some(m);
    }

    /// The title row's word: read only beats not saved beats edited beats saved.
    pub fn status(&self) -> &'static str {
        if self.read_only.is_some() {
            "read only"
        } else if self.save_failed {
            "not saved"
        } else if self.dirty {
            "edited"
        } else {
            "saved"
        }
    }

    /// Scrolls so the caret's row is in `[top, top + rows)`.
    fn follow_caret(&mut self) {
        let ls = lines(self.pad.text(), self.width);
        let (row, _) = locate(self.pad.text(), &ls, self.pad.caret());
        if row < self.top {
            self.top = row;
        } else if row >= self.top + self.rows {
            self.top = row + 1 - self.rows;
        }
    }

    fn refuse(&mut self) -> NotesOutcome {
        let word = self.read_only.map_or("", Refusal::word);
        self.message = Some(format!("read only: notes.txt {word}"));
        NotesOutcome::Redraw
    }

    /// Redraw after an edit that may have changed the text (`before`: its length and whether a
    /// selection was replaced); sets `dirty` when it did.
    fn edited(&mut self, before: usize, had_selection: bool, fit: Option<Fit>) -> NotesOutcome {
        let changed = had_selection || self.pad.text().len() != before;
        if changed {
            self.dirty = true;
        }
        if fit == Some(Fit::Cut) {
            self.message = Some("the pad is full (64 KiB)".to_string());
        }
        self.follow_caret();
        if changed || self.message.is_some() { NotesOutcome::Redraw } else { NotesOutcome::Nothing }
    }

    fn insert(&mut self, s: &str) -> NotesOutcome {
        let before = self.pad.text().len();
        let had = self.pad.selection().is_some();
        let fit = self.pad.insert(s);
        self.edited(before, had, Some(fit))
    }

    pub fn key(&mut self, k: NoteKey) -> NotesOutcome {
        let had_message = self.message.take().is_some();
        let out = self.apply(k);
        match (out, had_message) {
            (NotesOutcome::Nothing, true) => NotesOutcome::Redraw,
            (out, _) => out,
        }
    }

    fn apply(&mut self, k: NoteKey) -> NotesOutcome {
        let vertical = matches!(k, NoteKey::Move { to: Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown, .. });
        if !vertical {
            self.goal = None;
        }
        match k {
            NoteKey::Escape => NotesOutcome::Close,
            NoteKey::Move { to, extend } => {
                self.motion(to, extend);
                self.follow_caret();
                NotesOutcome::Redraw
            }
            NoteKey::SelectAll => {
                self.pad.select_all();
                self.follow_caret();
                NotesOutcome::Redraw
            }
            NoteKey::Copy | NoteKey::Cut => self.copy(k == NoteKey::Cut),
            NoteKey::Paste if self.read_only.is_some() => self.refuse(),
            NoteKey::Paste => NotesOutcome::Paste,
            _ if self.read_only.is_some() => self.refuse(),
            NoteKey::Char(c) => self.insert(c.encode_utf8(&mut [0; 4])),
            NoteKey::Enter => self.insert("\n"),
            NoteKey::Tab => self.insert(TAB),
            NoteKey::Backspace => self.delete_with(Pad::backspace),
            NoteKey::Delete => self.delete_with(Pad::delete),
            NoteKey::WordBackspace => self.delete_with(Pad::delete_word_back),
            NoteKey::WordDelete => self.delete_with(Pad::delete_word_forward),
        }
    }

    fn delete_with(&mut self, f: fn(&mut Pad) -> bool) -> NotesOutcome {
        if f(&mut self.pad) {
            self.dirty = true;
            self.follow_caret();
            NotesOutcome::Redraw
        } else {
            NotesOutcome::Nothing
        }
    }

    fn copy(&mut self, cut: bool) -> NotesOutcome {
        let Some(text) = self.pad.selected() else { return NotesOutcome::Nothing };
        if text.len() > MAX_INPUT {
            self.message = Some("selection too large to copy (4 KiB at most)".to_string());
            return NotesOutcome::Redraw;
        }
        NotesOutcome::Copy { text: text.to_string(), cut: cut && self.read_only.is_none() }
    }

    fn motion(&mut self, to: Motion, extend: bool) {
        let p = &self.pad;
        let at = match to {
            Motion::Left => p.left_of(p.caret()),
            Motion::Right => p.right_of(p.caret()),
            Motion::WordLeft => p.word_left_of(p.caret()),
            Motion::WordRight => p.word_right_of(p.caret()),
            Motion::Home => p.line_start_of(p.caret()),
            Motion::End => p.line_end_of(p.caret()),
            Motion::DocStart => 0,
            Motion::DocEnd => p.text().len(),
            Motion::Up => return self.vertical(false, 1, extend),
            Motion::Down => return self.vertical(true, 1, extend),
            Motion::PageUp => return self.vertical(false, self.rows.saturating_sub(1).max(1), extend),
            Motion::PageDown => return self.vertical(true, self.rows.saturating_sub(1).max(1), extend),
        };
        self.pad.set_caret(at, extend);
    }

    /// Moves `step` rows keeping the goal column; the first and last rows do not move.
    fn vertical(&mut self, down: bool, step: usize, extend: bool) {
        let ls = lines(self.pad.text(), self.width);
        let (row, col) = locate(self.pad.text(), &ls, self.pad.caret());
        let goal = *self.goal.get_or_insert(col);
        let last = ls.len().saturating_sub(1);
        let target = if down { (row + step).min(last) } else { row.saturating_sub(step) };
        if target != row {
            let at = offset_at(self.pad.text(), &ls, target, goal);
            self.pad.set_caret(at, extend);
        }
    }

    /// The clipboard's text, as typed: cleaned, cut to the cap.
    pub fn paste(&mut self, text: &str) -> NotesOutcome {
        self.message = None;
        if self.read_only.is_some() {
            return self.refuse();
        }
        self.goal = None;
        self.insert(text)
    }

    /// The copy command's result: a cut removes the selection once it started.
    pub fn after_copy(&mut self, cut: bool, result: Result<(), String>) -> NotesOutcome {
        match result {
            Ok(()) => {
                if cut && self.read_only.is_none() && self.pad.take_selection().is_some() {
                    self.dirty = true;
                    self.follow_caret();
                    return NotesOutcome::Redraw;
                }
                NotesOutcome::Nothing
            }
            Err(m) => {
                self.message = Some(m);
                NotesOutcome::Redraw
            }
        }
    }

    /// A left press on visible text row `row`, cell `column`.
    pub fn click(&mut self, row: usize, column: u16) -> NotesOutcome {
        let ls = lines(self.pad.text(), self.width);
        let at = offset_at(self.pad.text(), &ls, self.top + row, column);
        self.pad.set_caret(at, false);
        self.goal = None;
        NotesOutcome::Redraw
    }

    /// The wheel: scrolls `rows` (positive: down) without moving the caret.
    pub fn scroll(&mut self, rows: i32) -> NotesOutcome {
        let total = lines(self.pad.text(), self.width).len();
        let max = total.saturating_sub(self.rows);
        let top = if rows >= 0 { self.top.saturating_add(rows as usize) } else { self.top.saturating_sub(rows.unsigned_abs() as usize) };
        let top = top.min(max);
        if top == self.top {
            return NotesOutcome::Nothing;
        }
        self.top = top;
        NotesOutcome::Redraw
    }
}

#[cfg(test)]
mod tests;
