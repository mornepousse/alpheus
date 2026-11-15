//! The notes file: `$XDG_STATE_HOME/alpheus/notes.txt`, read once when the pad opens and written
//! after a pause in typing. Pure and silent: it never panics, never logs (the note is private), and
//! reaches the disk only through `state_file`. A file over the cap, not UTF-8 or not ours opens
//! read only and is never overwritten (the app builds that pad with a `Refusal`, which never turns
//! dirty).

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
    /// The text, already `clean`; an absent file is an empty pad.
    Text(String),
    Refused(Refusal),
}

/// The notes file under `state_dir`, as a pad's text or the reason it stays read only.
pub fn load(state_dir: &Path) -> Loaded {
    match read_private(state_dir, FILE, CAP as u64) {
        PrivateRead::Absent => Loaded::Text(String::new()),
        PrivateRead::TooLarge => Loaded::Refused(Refusal::TooLarge),
        PrivateRead::NotOurs => Loaded::Refused(Refusal::NotOurs),
        PrivateRead::Failed => Loaded::Refused(Refusal::Unreadable),
        PrivateRead::Bytes(bytes) => match String::from_utf8(bytes) {
            Err(_) => Loaded::Refused(Refusal::NotUtf8),
            Ok(text) => {
                let cleaned = clean(&text);
                // Cleaning can grow a file (a tab is four spaces): a pad never holds more than CAP.
                if cleaned.len() > CAP { Loaded::Refused(Refusal::TooLarge) } else { Loaded::Text(cleaned) }
            }
        },
    }
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
