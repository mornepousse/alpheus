//! The pad's text buffer: a `String` of at most `CAP` bytes, a caret and an optional anchor, both
//! byte offsets on scalar boundaries and never inside a glued mark (decision 7: a zero-width scalar
//! belongs to the scalar before it). Pure, and never panics: slices only through `str::get`.

use crate::grid::char_width;
use std::ops::Range;

/// Most bytes a pad holds (decision 10).
pub const CAP: usize = 65_536;
/// What Tab inserts.
pub const TAB: &str = "    ";

/// `s` as the pad keeps it (decision 9): `\r\n` and `\r` become `\n`, a tab four spaces, every other
/// control is dropped.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str(TAB),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Whether an insertion went in whole or was cut to the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Whole,
    Cut,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pad {
    text: String,
    caret: usize,
    anchor: Option<usize>,
}

/// Letters, digits and `_` make a word.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Pad {
    /// A pad holding `text`, the caret at its end.
    pub fn new(text: String) -> Pad {
        let caret = text.len();
        Pad { text, caret, anchor: None }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selected byte range, `None` when empty.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        if a == self.caret { None } else { Some(a.min(self.caret)..a.max(self.caret)) }
    }

    pub fn selected(&self) -> Option<&str> {
        self.text.get(self.selection()?)
    }

    /// The scalar starting at `at`, if any.
    fn char_at(&self, at: usize) -> Option<char> {
        self.text.get(at..)?.chars().next()
    }

    /// `at` clamped to the text and moved back to a scalar boundary that no zero-width scalar
    /// follows (offset 0 always counts).
    fn snap(&self, at: usize) -> usize {
        let mut at = at.min(self.text.len());
        while at > 0 {
            if self.text.is_char_boundary(at) && !self.char_at(at).is_some_and(|c| char_width(c) == 0) {
                break;
            }
            at -= 1;
        }
        at
    }

    /// Moves the caret to `at` (snapped); `extend` keeps or starts the selection, otherwise it is
    /// cleared.
    pub fn set_caret(&mut self, at: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = self.snap(at);
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
    }

    /// Replaces `range` by `with` when both ends are valid boundaries; the caret goes after `with`.
    fn splice(&mut self, range: Range<usize>, with: &str) -> bool {
        if range.start > range.end || !self.text.is_char_boundary(range.start) || !self.text.is_char_boundary(range.end) {
            return false;
        }
        let end = range.start + with.len();
        self.text.replace_range(range, with);
        self.anchor = None;
        self.caret = self.snap(end);
        true
    }

    /// Inserts `s` cleaned at the caret, replacing the selection; what passes the cap is dropped
    /// (the longest prefix that fits, on a scalar boundary).
    pub fn insert(&mut self, s: &str) -> Fit {
        let cleaned = clean(s);
        let range = self.selection().unwrap_or(self.caret..self.caret);
        let room = CAP.saturating_sub(self.text.len().saturating_sub(range.len()));
        let mut keep = 0;
        for (i, c) in cleaned.char_indices() {
            let next = i + c.len_utf8();
            if next > room {
                break;
            }
            keep = next;
        }
        let fit = if keep < cleaned.len() { Fit::Cut } else { Fit::Whole };
        self.splice(range, cleaned.get(..keep).unwrap_or(""));
        fit
    }

    fn delete_range(&mut self, range: Range<usize>) -> bool {
        if range.is_empty() { false } else { self.splice(range, "") }
    }

    /// Deletes the selection, else the scalar (and its marks) before the caret.
    pub fn backspace(&mut self) -> bool {
        match self.selection() {
            Some(r) => self.delete_range(r),
            None => self.delete_range(self.left_of(self.caret)..self.caret),
        }
    }

    /// Deletes the selection, else the scalar (and its marks) after the caret.
    pub fn delete(&mut self) -> bool {
        match self.selection() {
            Some(r) => self.delete_range(r),
            None => self.delete_range(self.caret..self.right_of(self.caret)),
        }
    }

    pub fn delete_word_back(&mut self) -> bool {
        match self.selection() {
            Some(r) => self.delete_range(r),
            None => self.delete_range(self.word_left_of(self.caret)..self.caret),
        }
    }

    pub fn delete_word_forward(&mut self) -> bool {
        match self.selection() {
            Some(r) => self.delete_range(r),
            None => self.delete_range(self.caret..self.word_right_of(self.caret)),
        }
    }

    /// Removes the selection and returns it.
    pub fn take_selection(&mut self) -> Option<String> {
        let r = self.selection()?;
        let taken = self.text.get(r.clone())?.to_string();
        self.delete_range(r);
        Some(taken)
    }

    /// The offset one scalar (and its marks) left of `at`.
    pub fn left_of(&self, at: usize) -> usize {
        let mut at = self.snap(at);
        if at == 0 {
            return 0;
        }
        at -= 1;
        while at > 0 && !self.text.is_char_boundary(at) {
            at -= 1;
        }
        self.snap(at)
    }

    /// The offset one scalar (and its marks) right of `at`.
    pub fn right_of(&self, at: usize) -> usize {
        let mut at = self.snap(at);
        let Some(c) = self.char_at(at) else { return self.text.len() };
        at += c.len_utf8();
        while let Some(m) = self.char_at(at) {
            if char_width(m) != 0 {
                break;
            }
            at += m.len_utf8();
        }
        at
    }

    /// Start of the word at or before `at`: skips the non-word scalars, then the word.
    pub fn word_left_of(&self, at: usize) -> usize {
        let mut at = self.snap(at);
        for want_word in [false, true] {
            while at > 0 {
                let p = self.left_of(at);
                if self.char_at(p).is_some_and(is_word) != want_word {
                    break;
                }
                at = p;
            }
        }
        at
    }

    /// End of the word at or after `at`: skips the non-word scalars, then the word.
    pub fn word_right_of(&self, at: usize) -> usize {
        let mut at = self.snap(at);
        for want_word in [false, true] {
            while at < self.text.len() {
                if self.char_at(at).is_some_and(is_word) != want_word {
                    break;
                }
                at = self.right_of(at);
            }
        }
        at
    }

    /// Start of the logical line holding `at`.
    pub fn line_start_of(&self, at: usize) -> usize {
        let at = self.snap(at);
        self.text.get(..at).and_then(|s| s.rfind('\n')).map_or(0, |i| i + 1)
    }

    /// End of the logical line holding `at` (before its `\n`).
    pub fn line_end_of(&self, at: usize) -> usize {
        let at = self.snap(at);
        self.text.get(at..).and_then(|s| s.find('\n')).map_or(self.text.len(), |i| at + i)
    }
}

#[cfg(test)]
mod tests;
