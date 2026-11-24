//! The in-memory history (docs/decisions.md, "The clipboard history is off by default, kept in memory, and
//! wiped when removed"): the last `MAX_ENTRIES` plain-text copies of at most `MAX_BYTES`, newest first, with pins. Nothing here logs, panics or prints a text: `Debug` shows
//! ids and flags only, and every text that leaves is zeroed before it is freed.

use alpheus_common::launcher_ipc::wipe;
use std::fmt;

pub const MAX_ENTRIES: usize = 50;
pub const MAX_BYTES: usize = 4096;
pub const MAX_PINS: usize = 10;
pub const PREVIEW_BYTES: usize = 1024;

/// One kept copy. `pinned` is the pin's sequence number ("most recently pinned first").
pub struct Entry {
    id: u64,
    text: String,
    pinned: Option<u64>,
}

impl Entry {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry").field("id", &self.id).field("pinned", &self.pinned).finish()
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        wipe(&mut self.text);
    }
}

/// What `push` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    Added(u64),
    Moved(u64),
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinError {
    Full,
    Gone,
}

/// A row for the launcher: a clean one-line preview, never the whole text.
#[derive(Clone, PartialEq, Eq)]
pub struct ClipRow {
    pub id: u64,
    pub preview: String,
    /// The first non-blank line, folded: what the row shows (the launcher's row).
    pub first: String,
    /// The lines after `first` (trailing blank ones not counted): the row's dim `+N lines`.
    pub more_lines: usize,
    pub pinned: bool,
}

impl fmt::Debug for ClipRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipRow").field("id", &self.id).field("pinned", &self.pinned).finish()
    }
}

impl Drop for ClipRow {
    fn drop(&mut self) {
        wipe(&mut self.preview);
        wipe(&mut self.first);
    }
}

/// Newest first.
#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
    next_id: u64,
    next_pin: u64,
}

impl fmt::Debug for History {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<u64> = self.entries.iter().map(Entry::id).collect();
        f.debug_struct("History").field("entries", &ids).finish()
    }
}

impl History {
    pub fn push(&mut self, mut text: String) -> Pushed {
        if text.len() > MAX_BYTES || text.trim().is_empty() {
            wipe(&mut text);
            return Pushed::Skipped;
        }
        if let Some(i) = self.entries.iter().position(|e| e.text == text) {
            wipe(&mut text);
            let e = self.entries.remove(i);
            let id = e.id;
            self.entries.insert(0, e);
            return Pushed::Moved(id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.insert(0, Entry { id, text, pinned: None });
        while self.entries.len() > MAX_ENTRIES {
            match self.entries.iter().rposition(|e| e.pinned.is_none()) {
                Some(i) => drop(self.entries.remove(i)),
                None => break,
            }
        }
        Pushed::Added(id)
    }

    /// Enter: the entry goes to the front.
    pub fn touch(&mut self, id: u64) -> bool {
        match self.entries.iter().position(|e| e.id == id) {
            Some(i) => {
                let e = self.entries.remove(i);
                self.entries.insert(0, e);
                true
            }
            None => false,
        }
    }

    /// Pins or unpins; the new state.
    pub fn toggle_pin(&mut self, id: u64) -> Result<bool, PinError> {
        let pinned = self.entries.iter().filter(|e| e.is_pinned()).count();
        let seq = self.next_pin;
        let e = self.entries.iter_mut().find(|e| e.id == id).ok_or(PinError::Gone)?;
        if e.pinned.is_some() {
            e.pinned = None;
            // The launcher's box puts an unpinned row right behind the pinned group: here it
            // heads the unpinned ones, so the next open lists the same order.
            let i = self.entries.iter().position(|e| e.id == id).unwrap_or(0);
            let e = self.entries.remove(i);
            self.entries.insert(0, e);
            return Ok(false);
        }
        if pinned >= MAX_PINS {
            return Err(PinError::Full);
        }
        e.pinned = Some(seq);
        self.next_pin += 1;
        Ok(true)
    }

    pub fn delete(&mut self, id: u64) -> bool {
        match self.entries.iter().position(|e| e.id == id) {
            Some(i) => {
                drop(self.entries.remove(i));
                true
            }
            None => false,
        }
    }

    /// Forgets every unpinned entry; how many went (for tests, never logged).
    pub fn clear(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.pinned.is_some());
        before - self.entries.len()
    }

    pub fn text_of(&self, id: u64) -> Option<&str> {
        self.entries.iter().find(|e| e.id == id).map(Entry::text)
    }

    /// Pinned first (most recently pinned first), then the rest newest first.
    pub fn rows(&self) -> Vec<ClipRow> {
        let mut pinned: Vec<&Entry> = self.entries.iter().filter(|e| e.is_pinned()).collect();
        pinned.sort_by_key(|e| std::cmp::Reverse(e.pinned));
        let rest = self.entries.iter().filter(|e| !e.is_pinned());
        pinned.into_iter().chain(rest).map(row).collect()
    }

    pub fn has_unpinned(&self) -> bool {
        self.entries.iter().any(|e| !e.is_pinned())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `s` cut at `PREVIEW_BYTES` on a character boundary.
fn cap(s: &str) -> &str {
    let mut cut = s.len().min(PREVIEW_BYTES);
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.get(..cut).unwrap_or_default()
}

/// Whitespace runs folded to one space, controls and invisible characters dropped.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            space = true;
        } else if !c.is_control() && !crate::text::invisible(c) {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

fn row(e: &Entry) -> ClipRow {
    let text = e.text();
    let mut lines = text.lines();
    let first = fold(cap(lines.by_ref().find(|l| !l.trim().is_empty()).unwrap_or_default()));
    let more_lines = lines.rev().skip_while(|l| l.trim().is_empty()).count();
    ClipRow { id: e.id, preview: fold(cap(text)), first, more_lines, pinned: e.is_pinned() }
}

#[cfg(test)]
mod tests;
