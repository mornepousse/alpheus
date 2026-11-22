//! USBGuard's `list-devices` text, read by alpheus itself: the blocked devices a `[shield] blocked`
//! command prints.
//!
//! Bytes go in and devices come out, so nothing here starts anything or reads a file. A device's
//! name comes from a USB descriptor, so it is hostile text: it is read at token level (shield spec
//! § 6), cleaned, cut to a length and never logged.
//!
//! Never:
//! - start a thread or a command, or read a file (`run.rs` runs the command and hands the bytes);
//! - write a log line, or panic: guardrail tests (`the_shield_logs_name_nothing`,
//!   `the_shield_files_have_no_panicking_path`) read this file's code.
//!
//! See: docs/specs/2026-11-21-alpheus-shield-design.md (§ 6), docs/features/shield.md, and
//! docs/decisions.md ("The picker line starts with the checked vendor and product ids").

/// At most this many devices are kept.
pub const DEVICES_MAX: usize = 64;
/// A device name is cut to this many characters.
pub const NAME_CHARS: usize = 64;

/// One blocked device: USBGuard's id for it, its `vid:pid` and its cleaned name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub id: u32,
    pub vid_pid: String,
    pub name: String,
}

/// The devices of one listing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub devices: Vec<UsbDevice>,
    /// A device past `DEVICES_MAX` was dropped.
    pub more: bool,
}

/// The blocked devices in `out`: lossy UTF-8 per line (invalid bytes become U+FFFD rather than an
/// error), only `<id>: block id <vid:pid> …` lines.
pub fn parse_blocked(out: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    for raw in out.split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        let Some(device) = parse_line(&line) else { continue };
        if parsed.devices.len() == DEVICES_MAX {
            parsed.more = true;
            break;
        }
        parsed.devices.push(device);
    }
    parsed
}

/// One token of the attribute text: a bare word or a quoted string (escapes decoded).
struct Token {
    quoted: bool,
    text: String,
}

/// A quote-aware tokenizer: bare words up to a space, `"…"` strings where only `\"` and `\\` are
/// decoded (any other backslash stays as text, never a control character).
fn tokens(s: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut text = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.peek() {
                        Some(&e @ ('"' | '\\')) => {
                            text.push(e);
                            chars.next();
                        }
                        _ => text.push('\\'),
                    },
                    c => text.push(c),
                }
            }
            out.push(Token { quoted: true, text });
        } else {
            let mut text = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                text.push(c);
                chars.next();
            }
            out.push(Token { quoted: false, text });
        }
    }
    out
}

/// Whether `text` is a vendor and product id: two groups of four lower-case hex digits around `:`.
fn is_vid_pid(text: &str) -> bool {
    let four_hex_digits = |group: &str| group.len() == 4 && group.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    match text.split_once(':') {
        Some((vendor, product)) => four_hex_digits(vendor) && four_hex_digits(product),
        None => false,
    }
}

/// Whether the token at `index` is the bare (unquoted) word `word`. A quoted string never counts,
/// so a device name cannot pose as the word block, id or name.
fn is_bare_word(tokens: &[Token], index: usize, word: &str) -> bool {
    match tokens.get(index) {
        Some(token) => !token.quoted && token.text == word,
        None => false,
    }
}

/// The value of the `name` attribute: the quoted token right after a bare `name` token, else "".
/// Text inside another attribute's quoted value is one token of its own, so it is never found here.
fn name_value(tokens: &[Token]) -> &str {
    for pair in tokens.windows(2) {
        if let [key, value] = pair {
            if !key.quoted && key.text == "name" && value.quoted {
                return value.text.as_str();
            }
        }
    }
    ""
}

/// One line of the listing as a device, or None when it is not a `<id>: block id <vid:pid> …` line.
fn parse_line(line: &str) -> Option<UsbDevice> {
    let (id, rest) = line.trim_start().split_once(": ")?;
    // The id is 1 to 10 digits that fit a `u32` (shield spec § 6).
    if id.is_empty() || id.len() > 10 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id: u32 = id.parse().ok()?;
    let tokens = tokens(rest);
    if !is_bare_word(&tokens, 0, "block") || !is_bare_word(&tokens, 1, "id") {
        return None;
    }
    let vid_pid = match tokens.get(2) {
        Some(token) if !token.quoted && is_vid_pid(&token.text) => token.text.clone(),
        _ => return None,
    };
    let name: String = crate::text::clean(name_value(&tokens)).trim().chars().take(NAME_CHARS).collect();
    let name = if name.trim().is_empty() { "?".to_string() } else { name };
    Some(UsbDevice { id, vid_pid, name })
}

/// `allow` with its `{id}` element replaced by the decimal id; None when `allow` is empty. Only a
/// whole element equal to `{id}` is replaced: the id comes from a parsed line and goes into one
/// argument, never into a larger one (shield spec § 2: `{id}` is one whole element).
pub fn allow_argv(allow: &[String], id: u32) -> Option<Vec<String>> {
    if allow.is_empty() {
        return None;
    }
    Some(allow.iter().map(|element| if element == "{id}" { id.to_string() } else { element.clone() }).collect())
}

/// The picker's row: `<vid:pid>  ·  <name>`, no id. The checked `vid:pid` comes first, in a fixed
/// width: a name (hostile text, cut at 64 characters) can never pose as one by ending in
/// `(1234:5678)`, because the real one is always what the row starts with. See docs/decisions.md,
/// "The picker line starts with the checked vendor and product ids".
pub fn picker_line(device: &UsbDevice) -> String {
    format!("{}  ·  {}", device.vid_pid, device.name)
}

#[cfg(test)]
mod tests;
