//! The query prefixes of the All mode (launcher 2 spec): `?` searches the web, `:` finds an
//! emoji, `$` runs a shell command. Pure: the query in, the prefix and its text out. The text is
//! never logged (it is what the user typed), and nothing here can panic.

use crate::actions::substitute;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    Search,
    Emoji,
    Shell,
    /// `"`: the clipboard history (clipboard spec), only while it is available.
    Clipboard,
}

/// The prefix a query starts with, and the rest of it trimmed at both ends. Only the query's very
/// first character counts (` ?x` is plain text). `?` counts only when `search` is on, `"` only
/// when `clipboard` is (the history is on and a protocol bound): otherwise the query is plain text.
pub fn split(query: &str, search: bool, clipboard: bool) -> Option<(Prefix, &str)> {
    let mut chars = query.chars();
    let prefix = match chars.next()? {
        '?' if search => Prefix::Search,
        ':' => Prefix::Emoji,
        '$' => Prefix::Shell,
        '"' if clipboard => Prefix::Clipboard,
        _ => return None,
    };
    Some((prefix, chars.as_str().trim()))
}

/// Every UTF-8 byte but RFC 3986's unreserved characters as `%XX` (uppercase).
pub fn percent_encode(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(text.len());
    for &b in text.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
    out
}

/// `template` with each `{query}` replaced, in one pass, by the percent-encoded `text`.
pub fn search_url(template: &str, text: &str) -> String {
    substitute(template, &[("{query}", &percent_encode(text))])
}

/// What the empty list says under a bare prefix.
pub fn hint(p: Prefix) -> &'static str {
    match p {
        Prefix::Search => "type to search the web",
        Prefix::Emoji => "type an emoji name",
        Prefix::Shell => "type a command to run in a shell",
        Prefix::Clipboard => "nothing copied yet",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_reads_the_first_character() {
        assert_eq!(split("? rust ratatui", true, false), Some((Prefix::Search, "rust ratatui")));
        assert_eq!(split("?rust", true, false), Some((Prefix::Search, "rust")));
        assert_eq!(split(":heart ", true, false), Some((Prefix::Emoji, "heart")));
        assert_eq!(split("$ ls -la", true, false), Some((Prefix::Shell, "ls -la")));
        assert_eq!(split("?", true, false), Some((Prefix::Search, "")));
        assert_eq!(split("? rust", false, false), None);
        assert_eq!(split("firefox", true, false), None);
        assert_eq!(split(" ?x", true, false), None);
        assert_eq!(split("", true, false), None);
        assert_eq!(split(":x", false, false), Some((Prefix::Emoji, "x")));
    }

    #[test]
    fn quote_prefix_is_claimed_only_with_the_history() {
        assert_eq!(split("\"milk", true, true), Some((Prefix::Clipboard, "milk")));
        assert_eq!(split("\" milk ", true, true), Some((Prefix::Clipboard, "milk")));
        assert_eq!(split("\"", false, true), Some((Prefix::Clipboard, "")));
        assert_eq!(split("\"milk", true, false), None, "no history: plain text");
        assert_eq!(split("x\"milk", true, true), None);
    }

    #[test]
    fn percent_encode_keeps_unreserved_only() {
        assert_eq!(percent_encode("a-Z_0.~"), "a-Z_0.~");
        assert_eq!(percent_encode("c++ & rust #tips"), "c%2B%2B%20%26%20rust%20%23tips");
        assert_eq!(percent_encode("café/50%"), "caf%C3%A9%2F50%25");
    }

    #[test]
    fn search_url_replaces_every_placeholder() {
        assert_eq!(search_url("https://duckduckgo.com/?q={query}", "rust ratatui"), "https://duckduckgo.com/?q=rust%20ratatui");
        assert_eq!(search_url("x/{query}/{query}", "a b"), "x/a%20b/a%20b");
        assert_eq!(search_url("x/{query}", "{query}"), "x/%7Bquery%7D");
    }

    #[test]
    fn hints_are_the_spec_strings() {
        assert_eq!(hint(Prefix::Search), "type to search the web");
        assert_eq!(hint(Prefix::Emoji), "type an emoji name");
        assert_eq!(hint(Prefix::Shell), "type a command to run in a shell");
        assert_eq!(hint(Prefix::Clipboard), "nothing copied yet");
    }
}
