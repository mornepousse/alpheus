//! The Wi-Fi password's buffer: the one place the password lives while it is being typed.
//!
//! It is a fixed buffer of `CAPACITY` bytes on the heap, so moving the panel never copies it. The
//! wipe is done with volatile writes (writes the compiler is not allowed to drop as dead stores):
//! `clear()` wipes the whole buffer, which the Wi-Fi code calls when the field closes and right after
//! the password has been handed to `nmcli`; a dropped buffer is cleared too, and a removed character
//! is wiped at once. `bytes()` is the only way to read it, and its `Debug` never shows a byte. A character that would
//! not fit is refused whole: the field then refuses to connect, so a password is never sent cut
//! short.
//!
//! Never:
//! - show a byte through `Debug`, or derive `Debug` or `Clone` (guardrail
//!   `the_wifi_password_goes_to_stdin_only_and_is_wiped_after`, which also allows this file exactly
//!   one block of low-level code, the volatile write);
//! - panic (guardrail `the_quick_panel_has_no_panicking_path`).
//!
//! What is not covered: the buffer is not locked in memory, and the key event that carried each
//! typed character comes from SCTK (the Wayland toolkit) as a heap string that is not wiped.
//!
//! See: docs/specs/2026-10-11-alpheus-quick-settings-wifi-design.md (§ 3), docs/features/quick-settings.md,
//! and docs/decisions.md ("The Wi-Fi password lives in one boxed buffer that is wiped").

use std::sync::atomic::{Ordering, compiler_fence};

/// Bytes a password may take.
pub const CAPACITY: usize = 256;

/// The password typed so far: `len` bytes of `buf`, the rest zero.
pub struct Secret {
    buf: Box<[u8; CAPACITY]>,
    len: usize,
}

impl Secret {
    /// An empty buffer, all zero.
    pub fn new() -> Secret {
        Secret { buf: Box::new([0u8; CAPACITY]), len: 0 }
    }

    /// The typed bytes, never the unused part of the buffer.
    pub fn bytes(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or(&[])
    }

    /// Whether nothing is typed.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Characters typed (UTF-8 code points, counted by their lead bytes): the field's dots. A byte
    /// that begins with the bits `10` continues a character, so it is not counted.
    pub fn chars(&self) -> usize {
        self.bytes().iter().filter(|b| **b & 0xC0 != 0x80).count()
    }

    /// Appends `c` whole; false, and nothing written, when it would not fit.
    pub fn push(&mut self, c: char) -> bool {
        // A character takes 1 to 4 bytes; the copy on the stack is wiped before returning.
        let mut tmp = [0u8; 4];
        let n = c.encode_utf8(&mut tmp).len();
        let end = self.len + n;
        let fits = match (self.buf.get_mut(self.len..end), tmp.get(..n)) {
            (Some(dst), Some(src)) => {
                dst.copy_from_slice(src);
                self.len = end;
                true
            }
            _ => false,
        };
        wipe_bytes(&mut tmp);
        fits
    }

    /// Removes the last character (all its bytes) and wipes them; false when empty.
    pub fn pop(&mut self) -> bool {
        if self.len == 0 {
            return false;
        }
        // Step back over continuation bytes (`10xxxxxx`) to the first byte of the last character.
        let mut start = self.len - 1;
        while start > 0 && self.buf.get(start).is_some_and(|b| b & 0xC0 == 0x80) {
            start -= 1;
        }
        if let Some(tail) = self.buf.get_mut(start..self.len) {
            wipe_bytes(tail);
        }
        self.len = start;
        true
    }

    /// Zeroes the whole buffer, not only the typed part.
    pub fn clear(&mut self) {
        wipe_bytes(&mut self.buf[..]);
        self.len = 0;
    }
}

/// The same as `Secret::new()`.
impl Default for Secret {
    fn default() -> Self {
        Secret::new()
    }
}

/// A dropped buffer is wiped whole, so a password never outlives its field in freed memory.
impl Drop for Secret {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The panel is cloned in tests only; each copy is wiped on its own drop.
impl Clone for Secret {
    fn clone(&self) -> Self {
        let mut s = Secret::new();
        s.buf[..].copy_from_slice(&self.buf[..]);
        s.len = self.len;
        s
    }
}

/// Two buffers are equal when they hold the same typed bytes.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

impl Eq for Secret {}

/// `Debug` prints a fixed word, never a byte.
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Zeroes `bytes` with volatile writes, which the compiler cannot remove as dead stores; the fence
/// keeps later code (a free, a reuse) from being moved before them.
pub fn wipe_bytes(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference to one byte.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Secret {
        /// The whole buffer, typed part or not: what a wipe must leave zeroed.
        fn raw(&self) -> &[u8; CAPACITY] {
            &self.buf
        }

        /// The whole buffer, to dirty past the typed part.
        fn raw_mut(&mut self) -> &mut [u8; CAPACITY] {
            &mut self.buf
        }
    }

    #[test]
    fn characters_go_in_whole_and_come_out_whole() {
        let mut s = Secret::new();
        assert!(s.push('a') && s.push('é') && s.push('🔑'));
        assert_eq!((s.bytes(), s.chars()), ("aé🔑".as_bytes(), 3));
        assert!(s.pop());
        assert_eq!(s.bytes(), "aé".as_bytes(), "the emoji's four bytes");
        assert!(s.raw()[3..8].iter().all(|b| *b == 0), "and they are wiped");
        assert!(s.pop() && s.pop() && !s.pop(), "empty: nothing to remove");
        assert!(s.is_empty());
    }

    #[test]
    fn a_character_that_does_not_fit_is_refused_whole() {
        let mut s = Secret::new();
        for _ in 0..CAPACITY - 1 {
            assert!(s.push('a'));
        }
        assert!(!s.push('é'), "two bytes, one left");
        assert_eq!(s.bytes().len(), CAPACITY - 1, "nothing half written");
        assert!(s.push('a') && !s.push('a'), "the last byte, then full");
    }

    #[test]
    fn clear_zeroes_the_whole_buffer() {
        let mut s = Secret::new();
        for c in "fake-pass".chars() {
            s.push(c);
        }
        s.clear();
        assert!(s.is_empty() && s.raw().iter().all(|b| *b == 0));
        // Bytes past the typed part (none should be there, but a wipe never trusts `len`).
        s.push('x');
        s.raw_mut()[CAPACITY - 1] = 0xAA;
        s.clear();
        assert!(s.raw().iter().all(|b| *b == 0), "the tail too");
    }

    #[test]
    fn debug_never_shows_a_byte() {
        let mut s = Secret::new();
        for c in "fake-pass".chars() {
            s.push(c);
        }
        let shown = format!("{s:?}");
        assert_eq!(shown, "Secret(…)");
        assert_eq!(s.clone(), s, "a copy (the tests clone the panel) holds the same bytes, wiped on its own drop");
    }
}
