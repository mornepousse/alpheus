//! Text from outside (a unit's description, a journal message) made safe to put in a terminal
//! cell: no control character, no escape sequence, no bidirectional override gets through.
//!
//! Never:
//! - let an escape sequence or a bidirectional override through: a unit's description and a journal
//!   message are hostile input, and the terminal would obey them;
//! - remove ZWNJ (U+200C) or ZWJ (U+200D): they shape ordinary text (the one list of invisible
//!   characters lives here, and the daemon's `text.rs` uses it).
//!
//! See: docs/features/control-center.md, docs/features/tray.md (the same rule for the bar).

/// Zero-width and bidirectional formatting characters: drawn as nothing, they could hide or
/// reorder what a cell shows (soft hyphen, word joiners, the byte-order mark). U+200C and U+200D
/// stay out of this list on purpose (ordinary text
/// shaping: ligatures, emoji sequences), and so do the tag characters (U+E0000–E007F: a subdivision
/// flag emoji is made of them). U+2028 and U+2029 are not here either: each cleaner turns them
/// into a space.
pub fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{200b}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// One line of text that is safe to draw: C0 and C1 controls dropped (tab, CR and LF become one
/// space), whole ANSI sequences dropped (ESC `[` … final byte; ESC `]` … BEL or ESC `\`; any
/// other ESC alone), bidirectional controls dropped, then cut to `max_chars` characters with a
/// trailing `…` (the result never has more than `max_chars`).
pub fn clean_line(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let kept = match c {
            '\u{1b}' => {
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        // A CSI ends at its first byte in 0x40..=0x7E.
                        for d in chars.by_ref() {
                            if ('\u{40}'..='\u{7e}').contains(&d) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(d) = chars.next() {
                            if d == '\u{7}' {
                                break;
                            }
                            if d == '\u{1b}' {
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            '\t' | '\r' | '\n' | '\u{2028}' | '\u{2029}' => ' ',
            c if c.is_control() || invisible(c) => continue,
            c => c,
        };
        n += 1;
        out.push(kept);
    }
    cut(out, n, max_chars)
}

fn cut(mut text: String, count: usize, max_chars: usize) -> String {
    if count <= max_chars {
        return text;
    }
    let keep = max_chars.saturating_sub(1);
    let end = text.char_indices().nth(keep).map_or(text.len(), |(i, _)| i);
    text.truncate(end);
    if max_chars > 0 {
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_text_is_cleaned() {
        assert_eq!(clean_line("a\u{1b}[2Jb\u{202e}c\rd\te", 100), "abc d e");
        assert_eq!(clean_line("x\u{1b}]0;owned title\u{7}y", 100), "xy", "OSC ended by BEL");
        assert_eq!(clean_line("x\u{1b}]0;owned\u{1b}\\y", 100), "xy", "OSC ended by ST");
        assert_eq!(clean_line("a\u{85}b\u{9b}c\u{200f}\u{feff}d\u{2066}e", 100), "abcde", "C1 and bidi controls");
        assert_eq!(clean_line("a\u{1b}[31;1mred\u{1b}[0m", 100), "ared");
        assert_eq!(clean_line("a\u{ad}b\u{2060}c\u{2064}d", 100), "abcd");
        assert_eq!(clean_line("a\u{2028}b\u{2029}c", 100), "a b c", "line and paragraph separators are spaces");
        let flag = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}";
        assert_eq!(clean_line(flag, 100), flag, "a subdivision flag (tag characters) is an emoji, kept");
        assert_eq!(clean_line("a\u{200c}b\u{200d}c", 100), "a\u{200c}b\u{200d}c", "ZWNJ and ZWJ stay");
        let long = "é".repeat(300);
        let cut = clean_line(&long, 200);
        assert_eq!(cut.chars().count(), 200);
        assert!(cut.ends_with('…'));
        assert_eq!(clean_line("abc", 3), "abc", "exactly the limit is kept");
        assert_eq!(clean_line("abcd", 3), "ab…");
    }
}
