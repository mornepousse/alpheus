//! Text from outside made safe to draw, in one place.
//!
//! The tray, the media module, the notifications and the sound model all pass what an application
//! or PipeWire gives them through `clean`, so that none of them depends on another for it:
//! separators become a space, every other control character and the invisible formatting
//! characters go, and what is left is cut to `MAX_TEXT` characters.
//!
//! Never:
//! - keep a control or bidirectional formatting character: a title from an application is hostile
//!   input, and the bar draws one line (no guardrail checks this file; the callers' own tests pin
//!   what they draw);
//! - remove ZWNJ (U+200C) or ZWJ (U+200D): they shape ordinary text, and the tag characters that
//!   make a subdivision flag stay for the same reason;
//! - allocate per character beyond the one `String` that comes back.
//!
//! See: `alpheus_common::text::invisible` (the one list, and why those two stay),
//! docs/features/tray.md, docs/features/media.md, docs/features/notifications.md.

/// The longest text kept from outside, in characters.
pub const MAX_TEXT: usize = 256;

/// Text from an application, drawable on one line: `\n`, `\r`, `\t` → a space, every other control
/// character and the invisible formatting characters `invisible` names (U+200B, U+200E, U+200F,
/// U+061C, U+202A–U+202E, U+2066–U+2069, and the soft hyphen, U+2060–U+2064, BOM) removed (U+2028 and
/// U+2029 become a space; the tag characters stay, a subdivision flag is made of them), at most MAX_TEXT characters. ZWNJ (U+200C) and
/// ZWJ (U+200D) are kept on purpose: they shape ordinary text (see `invisible`).
pub fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}' => Some(' '),
            c if c.is_control() || invisible(c) => None,
            c => Some(c),
        })
        .take(MAX_TEXT)
        .collect()
}

/// Zero-width and bidirectional formatting characters, the one list alpheus-common keeps (see
/// there for what it holds and why U+200C and U+200D stay out).
pub use alpheus_common::text::invisible;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_are_spaces_and_a_subdivision_flag_survives() {
        assert_eq!(clean("a\u{2028}b\u{2029}c"), "a b c");
        let flag = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
        assert_eq!(clean(flag), flag, "the tag characters of England's flag are kept");
    }
}
