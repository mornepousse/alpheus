//! What may become an argument: a unit name and a journal cursor, checked before any command line
//! is built (a name from a list or a file is never trusted to be only a name).

/// A systemd unit name: 1 to 256 bytes of `[A-Za-z0-9:_.\@-]`, with a `.` and an alphabetic type
/// after the last one (`-.mount` is the root mount and valid; `--now` and `a b.service` are not).
pub fn valid_unit(s: &str) -> bool {
    if s.is_empty() || s.len() > 256 {
        return false;
    }
    if !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'\\' | b'@' | b'-')) {
        return false;
    }
    match s.rsplit_once('.') {
        Some((_, kind)) => !kind.is_empty() && kind.bytes().all(|b| b.is_ascii_alphabetic()),
        None => false,
    }
}

/// A journal cursor: 1 to 512 printable ASCII bytes, no space.
pub fn valid_cursor(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && s.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_unit_is_dropped() {
        for bad in ["--now", "a b.service", "", "nodot", "trailing.", "x.se rvice", "x.ser1", "é.service"] {
            assert!(!valid_unit(bad), "{bad:?}");
        }
        assert!(!valid_unit(&format!("{}.service", "a".repeat(250))), "257 bytes");
        assert!(valid_unit(&format!("{}.mount", "a".repeat(249))), "256 bytes");
        for good in ["-.mount", "example-sync.service", "dev-disk-by\\x2did.device", "user@1000.service", "a:b_c.timer"] {
            assert!(valid_unit(good), "{good:?}");
        }
    }

    #[test]
    fn cursors_are_printable_ascii_without_spaces() {
        assert!(valid_cursor("s=0123;i=1;b=ab;m=1;t=1;x=1"));
        for bad in ["", "a b", "tab\there", "é", "nul\0", "line\n"] {
            assert!(!valid_cursor(bad), "{bad:?}");
        }
        assert!(valid_cursor(&"a".repeat(512)));
        assert!(!valid_cursor(&"a".repeat(513)));
    }
}
