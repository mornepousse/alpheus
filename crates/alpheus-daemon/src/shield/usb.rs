//! USBGuard's `list-devices` text (shield spec § 6), read by alpheus itself: the blocked devices a
//! `[shield] blocked` command prints. Pure: bytes in, devices out. A device's name comes from a
//! hostile source (a USB descriptor), so it is read at token level, cleaned, cut and never logged.

/// At most this many devices are kept.
pub const DEVICES_MAX: usize = 64;
/// A device name is cut to this many characters.
pub const NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub id: u32,
    pub vid_pid: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub devices: Vec<UsbDevice>,
    /// A device past `DEVICES_MAX` was dropped.
    pub more: bool,
}

/// The blocked devices in `out`: lossy UTF-8 per line, only `<id>: block id <vid:pid> …` lines.
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

fn is_vid_pid(s: &str) -> bool {
    let hex4 = |h: &str| h.len() == 4 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    s.split_once(':').is_some_and(|(v, p)| hex4(v) && hex4(p))
}

fn parse_line(line: &str) -> Option<UsbDevice> {
    let (id, rest) = line.trim_start().split_once(": ")?;
    if id.is_empty() || id.len() > 10 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id: u32 = id.parse().ok()?;
    let toks = tokens(rest);
    let bare = |i: usize, w: &str| toks.get(i).is_some_and(|t| !t.quoted && t.text == w);
    if !bare(0, "block") || !bare(1, "id") {
        return None;
    }
    let vid_pid = toks.get(2).filter(|t| !t.quoted && is_vid_pid(&t.text))?.text.clone();
    let name = toks
        .windows(2)
        .find_map(|w| match w {
            [key, value] if !key.quoted && key.text == "name" && value.quoted => Some(value.text.as_str()),
            _ => None,
        })
        .unwrap_or("");
    let name: String = crate::text::clean(name).trim().chars().take(NAME_CHARS).collect();
    let name = if name.trim().is_empty() { "?".to_string() } else { name };
    Some(UsbDevice { id, vid_pid, name })
}

/// `allow` with its `{id}` element replaced by the decimal id; None when `allow` is empty.
pub fn allow_argv(allow: &[String], id: u32) -> Option<Vec<String>> {
    if allow.is_empty() {
        return None;
    }
    Some(allow.iter().map(|a| if a == "{id}" { id.to_string() } else { a.clone() }).collect())
}

/// The picker's row: `<vid:pid>  ·  <name>`, no id. The checked `vid:pid` comes first, in a fixed
/// width: a name (hostile text, cut at 64 characters) can never pose as one by ending in
/// `(1234:5678)`, because the real one is always what the row starts with.
pub fn picker_line(d: &UsbDevice) -> String {
    format!("{}  ·  {}", d.vid_pid, d.name)
}

#[cfg(test)]
mod tests;
