//! What to do with an offer (docs/features/clipboard.md): the verdict on its MIME types, the password quiet
//! window, and the bounded read buffer. Pure: no log, no panic, no text in `Debug`.

use crate::clipboard::history::MAX_BYTES;
use alpheus_common::launcher_ipc::zero;
use std::fmt;
use std::io::{self, ErrorKind};
use std::time::{Duration, Instant};

/// Read preference, best first, and the own source's offer list.
pub const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];
/// Password-manager hints, by presence: an offer carrying one is never received.
pub const SENSITIVE: [&str; 3] =
    ["x-kde-passwordManagerHint", "application/x-nspasteboard-concealed-type", "ExcludeClipboardContentFromMonitorProcessing"];
/// Added by the daemon's own source, so its re-copy is never read back.
pub const MARKER: &str = "application/x-alpheus-clipboard";
/// What the daemon's own source offers: the text types, then the marker that makes its re-copy
/// skipped when the compositor announces it back (one list for the source and for the verdict).
pub fn own_offer_types() -> impl Iterator<Item = &'static str> {
    TEXT_TYPES.into_iter().chain([MARKER])
}

/// A read that has not ended by then is skipped.
pub const READ_DEADLINE: Duration = Duration::from_secs(1);
/// Selections this soon after a `--password` prompt closed are skipped unread.
pub const PASSWORD_QUIET: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Sensitive,
    Own,
    Quiet,
    NoText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Read(&'static str),
    Skip(Skip),
}

/// Sensitive first, then the own marker, then the quiet window, then the first text type offered.
pub fn verdict<'a>(mimes: impl IntoIterator<Item = &'a str>, quiet: bool) -> Verdict {
    let offered: Vec<&str> = mimes.into_iter().collect();
    if SENSITIVE.iter().any(|s| offered.contains(s)) {
        return Verdict::Skip(Skip::Sensitive);
    }
    if offered.contains(&MARKER) {
        return Verdict::Skip(Skip::Own);
    }
    if quiet {
        return Verdict::Skip(Skip::Quiet);
    }
    match TEXT_TYPES.into_iter().find(|t| offered.contains(t)) {
        Some(t) => Verdict::Read(t),
        None => Verdict::Skip(Skip::NoText),
    }
}

/// A password prompt is open, or closed less than `PASSWORD_QUIET` ago.
pub fn in_quiet_window(prompt_open: bool, closed_at: Option<Instant>, now: Instant) -> bool {
    prompt_open || closed_at.is_some_and(|t| now.saturating_duration_since(t) < PASSWORD_QUIET)
}

/// Whether an alpheus `--password` prompt is open, or closed at some instant: the quiet window's
/// inputs (the launcher stamps them, the verdict asks).
#[derive(Debug, Default)]
pub struct PasswordQuiet {
    open: bool,
    closed_at: Option<Instant>,
}

impl PasswordQuiet {
    pub fn opened(&mut self) {
        self.open = true;
    }

    pub fn closed(&mut self, now: Instant) {
        self.open = false;
        self.closed_at = Some(now);
    }

    /// A close whose prompt kind is unknown (after a panic): stamped only if one was open, so
    /// a launcher that was never a password prompt starts no window.
    pub fn closed_if_open(&mut self, now: Instant) {
        if self.open {
            self.closed(now);
        }
    }

    pub fn active(&self, now: Instant) -> bool {
        in_quiet_window(self.open, self.closed_at, now)
    }
}

/// What a read has so far: at most `MAX_BYTES + 1` bytes (one past the cap tells "too much").
#[derive(Default)]
pub struct ReadBuffer {
    bytes: Vec<u8>,
    over: bool,
}

impl fmt::Debug for ReadBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadBuffer").field("over", &self.over).finish()
    }
}

impl Drop for ReadBuffer {
    fn drop(&mut self) {
        zero(&mut self.bytes);
    }
}

impl ReadBuffer {
    pub fn push(&mut self, chunk: &[u8]) {
        // Room for the cap and the byte past it at once: the buffer never reallocates, so no
        // freed, unwiped fragment is left behind.
        self.bytes.reserve_exact((MAX_BYTES + 1).saturating_sub(self.bytes.capacity()));
        let room = (MAX_BYTES + 1).saturating_sub(self.bytes.len());
        let take = chunk.len().min(room);
        self.bytes.extend_from_slice(chunk.get(..take).unwrap_or_default());
        self.over = self.bytes.len() > MAX_BYTES;
    }

    pub fn over(&self) -> bool {
        self.over
    }

    /// The text, or `None` when over the cap or not UTF-8 (the bytes zeroed either way).
    pub fn finish(mut self) -> Option<String> {
        let mut bytes = std::mem::take(&mut self.bytes);
        if self.over {
            zero(&mut bytes);
            return None;
        }
        match String::from_utf8(bytes) {
            Ok(s) => Some(s),
            Err(e) => {
                let mut bytes = e.into_bytes();
                zero(&mut bytes);
                None
            }
        }
    }
}

/// What one read wake-up tells the loop to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Continue,
    /// The end of the data, or the cap.
    Stop,
    Failed(ErrorKind),
}

/// The pure part of one read wake-up: `chunk` is the buffer the read filled.
pub fn step(read: io::Result<usize>, chunk: &[u8], buffer: &mut ReadBuffer) -> Step {
    match read {
        Ok(n) if n > 0 => {
            buffer.push(chunk.get(..n).unwrap_or_default());
            if buffer.over() { Step::Stop } else { Step::Continue }
        }
        Ok(_) => Step::Stop,
        Err(e) if e.kind() == ErrorKind::WouldBlock => Step::Continue,
        Err(e) => Step::Failed(e.kind()),
    }
}

#[cfg(test)]
mod tests;
