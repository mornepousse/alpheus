//! Text from outside (applications on D-Bus, PipeWire's device names) made safe to draw: shared
//! by the tray, the media module, the notifications and the sound model, so that none of them
//! depends on another for it.

/// The longest text kept from outside, in characters.
pub const MAX_TEXT: usize = 256;

/// Text from an application, drawable on one line: `\n`, `\r`, `\t` → a space, every other control
/// character and the invisible formatting characters `invisible` names (U+200B, U+200E, U+200F,
/// U+061C, U+202A–U+202E, U+2066–U+2069, and the soft hyphen, U+2028/9, U+2060–U+2064, tags, BOM) removed, at most MAX_TEXT characters. ZWNJ (U+200C) and
/// ZWJ (U+200D) are kept on purpose: they shape ordinary text (see `invisible`).
pub fn clean(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() || invisible(c) => None,
            c => Some(c),
        })
        .take(MAX_TEXT)
        .collect()
}

/// Zero-width and bidirectional formatting characters, the one list alpheus-commun keeps (see
/// there for what it holds and why U+200C and U+200D stay out).
pub use alpheus_commun::text::invisible;
