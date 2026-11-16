//! The notes file: `$XDG_STATE_HOME/alpheus/notes.txt`, read once when the pad opens and written
//! after a pause in typing. Pure and silent: it never panics, never logs (the note is private), and
//! reaches the disk only through `state_file`. A file over the cap, not UTF-8, not ours or that
//! `clean` would change opens read only and is never overwritten (the app builds that pad with a
//! `Refusal`, which never turns dirty): the first `CAP` bytes of one too large or not UTF-8 are shown
//! (lossy, cleaned), nothing of one not ours or unreadable (nothing behind a symlink is ever read).

use super::model::Refusal;
use super::text::{CAP, clean};
use crate::state_file::{PrivateRead, read_private, remove_private, write_private};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

pub const FILE: &str = "notes.txt";
/// Quiet time after the last edit before the pad is saved.
pub const SAVE_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub enum Loaded {
    /// The text, which `clean` leaves as is; an absent file is an empty pad.
    Text(String),
    /// Read only: why, and what is shown (cleaned, at most `CAP` bytes; empty for a file not ours
    /// or unreadable).
    Refused(Refusal, String),
}

/// The notes file under `state_dir`, as a pad's text or the reason it stays read only.
pub fn load(state_dir: &Path) -> Loaded {
    match read_private(state_dir, FILE, CAP as u64) {
        PrivateRead::Absent => Loaded::Text(String::new()),
        PrivateRead::TooLarge(start) => Loaded::Refused(Refusal::TooLarge, shown(&start)),
        PrivateRead::NotOurs => Loaded::Refused(Refusal::NotOurs, String::new()),
        PrivateRead::Failed => Loaded::Refused(Refusal::Unreadable, String::new()),
        PrivateRead::Bytes(bytes) => match String::from_utf8(bytes) {
            Err(e) => Loaded::Refused(Refusal::NotUtf8, shown(e.as_bytes())),
            Ok(text) => {
                let cleaned = clean(&text);
                // Edited outside the pad: shown cleaned, never silently rewritten (M3).
                if cleaned == text { Loaded::Text(text) } else { Loaded::Refused(Refusal::NotClean, capped(&cleaned)) }
            }
        },
    }
}

/// What a refused file shows: its first `CAP` bytes as lossy UTF-8 (a scalar cut by the cap dropped
/// whole, not shown as U+FFFD), cleaned, capped again (a tab grows to four spaces).
fn shown(bytes: &[u8]) -> String {
    let bytes = bytes.get(..CAP).unwrap_or(bytes);
    let bytes = match std::str::from_utf8(bytes) {
        Err(e) if e.error_len().is_none() => bytes.get(..e.valid_up_to()).unwrap_or(bytes),
        _ => bytes,
    };
    capped(&clean(&String::from_utf8_lossy(bytes)))
}

/// `s`'s longest prefix of at most `CAP` bytes on a scalar boundary.
fn capped(s: &str) -> String {
    let mut end = s.len().min(CAP);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("").to_string()
}

/// Saves `text`; an empty pad removes the file.
pub fn save(state_dir: &Path, text: &str) -> io::Result<()> {
    if text.is_empty() { remove_private(state_dir, FILE) } else { write_private(state_dir, FILE, text.as_bytes()) }
}

/// When the pad is due for a save: `SAVE_AFTER` after the first unsaved edit, only while open.
pub fn save_deadline(open: bool, dirty_since: Option<Instant>) -> Option<Instant> {
    if open { dirty_since.map(|t| t + SAVE_AFTER) } else { None }
}

#[cfg(test)]
mod tests;
