//! Text from outside (applications on D-Bus, PipeWire's device names) made safe to draw: shared
//! by the tray, the media module, the notifications and the sound model, so that none of them
//! depends on another for it.

/// The longest text kept from outside, in characters.
pub const MAX_TEXT: usize = 256;

/// Text from an application, drawable on one line: `\n`, `\r`, `\t` → a space, every other control
/// character and the invisible formatting characters (U+200B–U+200F, U+202A–U+202E, U+2066–U+2069)
/// removed, at most MAX_TEXT characters.
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

/// Zero-width and bidirectional formatting characters: drawn as nothing,
/// they could hide or reorder what the bar shows (a title posing as
/// another). U+200C (ZWNJ) and U+200D (ZWJ) are excluded: they are ordinary
/// text shaping (Arabic/Indic ligature control, emoji ZWJ sequences), not
/// spoofing tools, so removing them would corrupt otherwise legible text.
pub fn invisible(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}
