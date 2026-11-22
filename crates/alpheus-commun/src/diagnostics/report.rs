//! The bug report: a short text built from a fixed list of fields and nothing else, redacted
//! before anyone sees it (so what is shown is exactly what is copied), and held under the copy
//! program's 4 KiB standard-input bound. Pure: the facts come in as plain strings.
//!
//! What it never holds, by construction: an environment, a command line, a boot id, a machine id
//! (replaced if it slipped into a message), a cursor, a pid, a host name. The inputs have no
//! field for them.

use crate::exec::MAX_INPUT;
use crate::text::clean_line;

/// Errors kept, after folding duplicates.
const MAX_ERRORS: usize = 40;
/// Longest message kept per error line, in characters.
const MESSAGE_MAX: usize = 200;
/// Shortest user or host name replaced everywhere: a two-letter name would shred ordinary words.
const MIN_NAME: usize = 3;
/// Top-level directories of a Linux root and the words next to them: a login or a real-name word
/// spelled like one (`dev`, `run`) is not replaced as a word, or `/dev/sda` and `dev-sda.device`
/// would read `/<user>/sda`. The home path is replaced first, so such a login still leaves no
/// path behind.
const COMMON_DIRS: [&str; 18] =
    ["dev", "run", "etc", "usr", "var", "bin", "sbin", "sys", "proc", "tmp", "opt", "srv", "mnt", "lib", "boot", "home", "nix", "root"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportUnit {
    pub unit: String,
    /// "system" or "user".
    pub scope: &'static str,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLoop {
    pub unit: String,
    pub scope: &'static str,
    pub restarts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportCrash {
    /// "Nov 20 14:02".
    pub time: String,
    pub exe: String,
    pub signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportError {
    /// "21:02:11".
    pub time: String,
    pub ident: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportInputs {
    /// "2026-11-21 21:40 UTC".
    pub when: String,
    pub alpheus: String,
    /// The first line of `systemctl --version`'s second word onward: "260 (260.5)".
    pub systemd: String,
    pub kernel: String,
    pub compositor: String,
    pub os: String,
    /// `None`: that read failed, and the report says "unknown" rather than "none".
    pub failed: Option<Vec<ReportUnit>>,
    pub loops: Option<Vec<ReportLoop>>,
    pub crashes: Option<Vec<ReportCrash>>,
    /// Oldest first, as the journal prints them.
    pub errors: Option<Vec<ReportError>>,
}

/// What is replaced, and by what: exact strings first (the longest first), then the shapes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Redactions {
    exact: Vec<(String, &'static str, bool)>,
}

impl Redactions {
    /// The user's names and homes, the host name (and its short name when it is fully
    /// qualified) and the machine id. Names match in any ASCII case.
    pub fn new(users: &[String], homes: &[String], host: Option<&str>, machine_id: Option<&str>) -> Redactions {
        Redactions::build(users, homes, host, true, machine_id)
    }

    /// `short_host`: the first label of a qualified host is replaced on its own too (left out
    /// when it is a word of the distribution's name).
    fn build(users: &[String], homes: &[String], host: Option<&str>, short_host: bool, machine_id: Option<&str>) -> Redactions {
        let mut exact: Vec<(String, &'static str, bool)> = Vec::new();
        // `word`: matched as a whole word only (a login or one word of a real name would shred
        // `event` or `systemd` as a substring).
        let mut add = |s: &str, to: &'static str, min: usize, word: bool| {
            let s = s.trim();
            if s.chars().count() >= min && !exact.iter().any(|(e, ..)| e.to_lowercase() == s.to_lowercase()) {
                exact.push((s.to_string(), to, word));
            }
        };
        if let Some(id) = machine_id {
            add(id, "<machine-id>", 16, false);
        }
        for h in homes {
            // "/" alone, or an empty home, would eat every path.
            add(h.trim_end_matches('/'), "~", 3, false);
        }
        // Whole words, like the login: a host called `work` must not shred `network`.
        if let Some(h) = host {
            add(h, "<host>", MIN_NAME, true);
            if let Some((short, _)) = h.split_once('.').filter(|_| short_host) {
                add(short, "<host>", MIN_NAME, true);
            }
        }
        for u in users {
            let word = !u.trim().contains(char::is_whitespace);
            if word && COMMON_DIRS.iter().any(|d| d.eq_ignore_ascii_case(u.trim())) {
                continue;
            }
            add(u, "<user>", MIN_NAME, word);
        }
        exact.sort_by_key(|(s, ..)| std::cmp::Reverse(s.len()));
        Redactions { exact }
    }

    /// `text` with every listed string and every address-shaped token replaced.
    pub fn apply(&self, text: &str) -> String {
        // The command of a sudo or pkexec line goes first: its arguments can hold anything. E-mail
        // addresses go before the names, so the user's own keeps no domain behind a placeholder.
        let mut out = redact_emails(&cut_commands(text));
        for (needle, to, word) in &self.exact {
            out = if *word { replace_word_ci(&out, needle, to) } else { replace_ci(&out, needle, to) };
        }
        redact_emails(&replace_shapes(&out))
    }
}

/// Two characters equal up to Unicode case (single-character mappings: `É`/`é`).
fn eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// `text` with each case-insensitive occurrence of `needle` replaced by `to`.
fn replace_ci(text: &str, needle: &str, to: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pin: Vec<char> = needle.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let end = i + pin.len();
        if !pin.is_empty() && end <= chars.len() && chars[i..end].iter().zip(&pin).all(|(a, b)| eq_ci(*a, *b)) {
            out.push_str(to);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `text` with each case-insensitive whole-word occurrence of `needle` replaced by `to`: no
/// letter right before or after it (a digit, a dash, a dot or the end all count as the word's
/// edge, and so does a systemd `\xHH` escape before it).
fn replace_word_ci(text: &str, needle: &str, to: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let pin: Vec<char> = needle.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let end = i + pin.len();
        let hit = end <= chars.len()
            && chars[i..end].iter().zip(&pin).all(|(a, b)| eq_ci(*a, *b))
            && !chars.get(end).is_some_and(|c| c.is_alphabetic())
            && (i == 0 || !chars[i - 1].is_alphabetic() || escaped_before(&chars, i));
        if hit {
            out.push_str(to);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The four characters before `i` are a systemd `\xHH` escape.
fn escaped_before(chars: &[char], i: usize) -> bool {
    i >= 4
        && chars[i - 4] == '\\'
        && chars[i - 3].eq_ignore_ascii_case(&'x')
        && chars[i - 2].is_ascii_hexdigit()
        && chars[i - 1].is_ascii_hexdigit()
}

/// Everything after `COMMAND=` (sudo's and pkexec's own field) is replaced by `<command>`, up to
/// the end of the line, or up to a closing `]` when pkexec brackets it: the arguments are
/// anything the user typed, a password included.
fn cut_commands(text: &str) -> String {
    const MARK: &str = "COMMAND=";
    if !text.contains(MARK) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        out.push_str(&rest[..at + MARK.len()]);
        out.push_str("<command>");
        let tail = &rest[at + MARK.len()..];
        let bracketed = rest[..at].ends_with('[');
        let line_end = tail.find('\n').unwrap_or(tail.len());
        let stop = if bracketed { tail[..line_end].rfind(']').unwrap_or(line_end) } else { line_end };
        rest = &tail[stop..];
    }
    out.push_str(rest);
    out
}

/// Replaces every MAC, UUID, IPv6, IPv4 and e-mail shape. The scan tries each shape at every
/// position that follows a boundary (a character that is not part of a word), so an address
/// glued to a label by punctuation (`BSSID:aa:…`, `wlan0-aa:…`, `dev_AA_…`) is found. A dotted
/// quad right after the word "version" is a version, not an address; any other is redacted.
fn replace_shapes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c.is_ascii_hexdigit() || c == ':') && boundary_before(&chars, i) {
            let hit = uuid(&chars[i..])
                .map(|n| (n, "<uuid>"))
                .or_else(|| hex_id(&chars[i..]).map(|n| (n, "<id>")))
                .or_else(|| mac(&chars[i..]).map(|n| (n, "<mac>")))
                .or_else(|| cisco_mac(&chars[i..]).map(|n| (n, "<mac>")))
                .or_else(|| bare_mac(&chars[i..]).map(|n| (n, "<mac>")))
                .or_else(|| ipv6(&chars[i..]).map(|n| (n, "<ip>")))
                .or_else(|| {
                    (!after_version_word(&chars, i) && !inside_dotted_number(&chars, i) && !after_port_digit(&chars, i))
                        .then(|| ipv4(&chars[i..]).map(|n| (n, "<ip>")))
                        .flatten()
                });
            if let Some((n, to)) = hit {
                out.push_str(to);
                i += n;
                continue;
            }
        }
        // `0xaabbccddeeff`: a bare 12-hex right after a hex prefix.
        if c.is_ascii_hexdigit() && after_hex_prefix(&chars, i) {
            if let Some(n) = bare_mac(&chars[i..]) {
                out.push_str("<mac>");
                i += n;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Not in the middle of a word. A systemd `\x2d` escape (a dash in a unit name) counts as a
/// boundary: the text after it starts a new token.
fn boundary_before(chars: &[char], i: usize) -> bool {
    let Some(&prev) = i.checked_sub(1).map(|p| &chars[p]) else { return true };
    if !prev.is_alphanumeric() {
        return true;
    }
    escaped_before(chars, i)
}

/// `0x` right before `i`, itself at a word start.
fn after_hex_prefix(chars: &[char], i: usize) -> bool {
    i >= 2 && chars[i - 2] == '0' && chars[i - 1].eq_ignore_ascii_case(&'x') && (i == 2 || !chars[i - 3].is_alphanumeric())
}

/// A dot right after a digit: the tail of a longer dotted number (`1.2.3.4.5`), not an address
/// of its own. Only a dotted quad is held to it, never a MAC or a UUID.
fn inside_dotted_number(chars: &[char], i: usize) -> bool {
    i >= 2 && chars[i - 1] == '.' && chars[i - 2].is_ascii_digit()
}

/// The next character is not part of the same word.
fn boundary_after(chars: &[char], n: usize) -> bool {
    chars.get(n).is_none_or(|c| !c.is_alphanumeric())
}

/// A digit and a dash right before `i`: a USB port path (`usb 2-1.2.1.3:`), not an address.
fn after_port_digit(chars: &[char], i: usize) -> bool {
    i >= 2 && chars[i - 1] == '-' && chars[i - 2].is_ascii_digit()
}

/// The word before `i` (after one space) is one that introduces a version number.
fn after_version_word(chars: &[char], i: usize) -> bool {
    let end = if i > 0 && chars[i - 1] == ' ' { i - 1 } else { return false };
    let start = (0..end).rev().find(|&k| !chars[k].is_alphabetic()).map_or(0, |k| k + 1);
    let word: String = chars[start..end].iter().collect();
    ["version", "firmware", "kernel"].iter().any(|w| word.eq_ignore_ascii_case(w))
}

/// Thirty-two hex digits as a token of their own: a boot, invocation or machine id. Returns the
/// length.
fn hex_id(s: &[char]) -> Option<usize> {
    (hex_run(s, 32) && boundary_after(s, 32)).then_some(32)
}

fn hex_run(s: &[char], n: usize) -> bool {
    s.len() >= n && s[..n].iter().all(char::is_ascii_hexdigit)
}

/// Six pairs of hex digits joined by one of `:`, `-` or `_`. Returns the length.
fn mac(s: &[char]) -> Option<usize> {
    // An optional `0x` prefix.
    let off = if s.first() == Some(&'0') && s.get(1).is_some_and(|c| c.eq_ignore_ascii_case(&'x')) { 2 } else { 0 };
    let t = &s[off..];
    let sep = *t.get(2)?;
    if !matches!(sep, ':' | '-' | '_') {
        return None;
    }
    for g in 0..6 {
        if !hex_run(&t[g * 3..], 2) || (g < 5 && t.get(g * 3 + 2) != Some(&sep)) {
            return None;
        }
    }
    // A longer chain of pairs (an EUI-64, a stray byte) goes with it, whole.
    let mut n = 17;
    while t.get(n) == Some(&sep) && hex_run(t.get(n + 1..)?, 2) {
        n += 3;
    }
    boundary_after(t, n).then_some(off + n)
}

/// Cisco's `aabb.ccdd.eeff`. Returns the length.
fn cisco_mac(s: &[char]) -> Option<usize> {
    for g in 0..3 {
        if !hex_run(s.get(g * 5..)?, 4) || (g < 2 && s.get(g * 5 + 4) != Some(&'.')) {
            return None;
        }
    }
    boundary_after(s, 14).then_some(14)
}

/// Twelve hex digits as a token of their own (`AABBCCDDEEFF`), not all decimal (a plain number
/// is not one). Returns the length.
fn bare_mac(s: &[char]) -> Option<usize> {
    if !hex_run(s, 12) || !boundary_after(s, 12) {
        return None;
    }
    s[..12].iter().any(|c| !c.is_ascii_digit()).then_some(12)
}

/// 8-4-4-4-12 hex digits joined by `-` or by systemd's `\x2d` escape. Returns the length.
fn uuid(s: &[char]) -> Option<usize> {
    let sep_len = if s.get(8) == Some(&'-') {
        1
    } else if s.get(8) == Some(&'\\')
        && s.get(9).is_some_and(|c| c.eq_ignore_ascii_case(&'x'))
        && s.get(10) == Some(&'2')
        && s.get(11).is_some_and(|c| c.eq_ignore_ascii_case(&'d'))
    {
        4
    } else {
        return None;
    };
    let mut at = 0;
    for (g, len) in [8, 4, 4, 4, 12].into_iter().enumerate() {
        if g > 0 {
            let sep = s.get(at..at + sep_len)?;
            if !sep.iter().zip(&s[8..8 + sep_len]).all(|(a, b)| a.eq_ignore_ascii_case(b)) {
                return None;
            }
            at += sep_len;
        }
        if !hex_run(s.get(at..)?, len) {
            return None;
        }
        at += len;
    }
    boundary_after(s, at).then_some(at)
}

/// Four decimal parts of at most 255, then optionally a `:port` that stays. Returns the length of
/// the address alone.
fn ipv4(s: &[char]) -> Option<usize> {
    let mut at = 0;
    for part in 0..4 {
        let digits = s[at..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || digits > 3 || s[at..at + digits].iter().collect::<String>().parse::<u16>().ok()? > 255 {
            return None;
        }
        at += digits;
        if part < 3 {
            if s.get(at) != Some(&'.') {
                return None;
            }
            at += 1;
        }
    }
    // A fifth part of one digit is a longer version number, not an address; tcpdump's `a.b.c.d.443`
    // has a port of two or more digits there, which stays.
    if s.get(at) == Some(&'.') && s.get(at + 1).is_some_and(char::is_ascii_digit) {
        let port = s[at + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        let ends = boundary_after(s, at + 1 + port) && s.get(at + 1 + port) != Some(&'.');
        return ((2..=5).contains(&port) && ends).then_some(at);
    }
    boundary_after(s, at).then_some(at)
}

/// Hex groups of at most four digits: eight of them, or a `::` somewhere with at least two
/// non-empty groups and one of three or more digits (so `a::b`, `std::io` and a clock are not
/// addresses), with an IPv4 tail allowed (`::ffff:10.1.2.3`). Returns the length.
fn ipv6(s: &[char]) -> Option<usize> {
    let run = s.iter().take_while(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.')).count();
    let mut n = run;
    // A trailing dot or single colon is the sentence's; a trailing `::` is part of the address.
    while n > 0 && (s[n - 1] == '.' || (s[n - 1] == ':' && !(n >= 2 && s[n - 2] == ':'))) {
        n -= 1;
    }
    let token: String = s[..n].iter().collect();
    let compressed = token.contains("::");
    if token.contains(":::") || token.matches("::").count() > 1 || !boundary_after(s, n) {
        return None;
    }
    let (head, tail) = match token.rsplit_once(':') {
        Some((h, t)) if t.contains('.') => (h.to_string(), Some(t)),
        _ => (token.clone(), None),
    };
    if let Some(t) = tail {
        let q: Vec<char> = t.chars().collect();
        if ipv4(&q) != Some(q.len()) {
            return None;
        }
    }
    let groups: Vec<&str> = head.split(':').collect();
    let non_empty: Vec<&&str> = groups.iter().filter(|g| !g.is_empty()).collect();
    let wide = groups.iter().all(|g| g.len() <= 4 && g.bytes().all(|b| b.is_ascii_hexdigit()));
    let want = 8 - usize::from(tail.is_some()) * 2;
    let full = !compressed && groups.len() == want && groups.iter().all(|g| !g.is_empty());
    let ok = wide
        && (full
            || (compressed
                && groups.len() <= want + 1
                && non_empty.len() + usize::from(tail.is_some()) >= 2
                && (tail.is_some() || non_empty.iter().any(|g| g.len() >= 3))));
    ok.then_some(n)
}

/// Last labels of a systemd unit name.
const UNIT_SUFFIXES: [&str; 11] =
    ["service", "socket", "mount", "timer", "target", "device", "scope", "slice", "path", "swap", "automount"];

fn redact_emails(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let local = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
    let domain = |c: char| c.is_alphanumeric() || matches!(c, '.' | '-');
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0; // chars before `copied` are already in `out`
    while i < chars.len() {
        // `@`, or systemd's `\x40` escape of it.
        let marker = if chars[i] == '@' {
            1
        } else if i + 4 <= chars.len()
            && chars[i] == '\\'
            && chars[i + 1].eq_ignore_ascii_case(&'x')
            && chars[i + 2] == '4'
            && chars[i + 3] == '0'
        {
            4
        } else {
            i += 1;
            continue;
        };
        let mut start = i;
        while start > copied && local(chars[start - 1]) {
            start -= 1;
        }
        let mut end = i + marker;
        while end < chars.len() && domain(chars[end]) {
            end += 1;
        }
        // A trailing dot is the sentence's.
        while end > i + marker && chars[end - 1] == '.' {
            end -= 1;
        }
        let host: String = chars[i + marker..end].iter().collect();
        // A templated unit (`getty@tty1.service`, `user@1000.service`) is not an address: an
        // instance name, then the suffix. `mbsync@alice@example.org.service` is one.
        // The instance may hold dots (`openvpn-client@my.vpn.service`); a second `@` before it
        // (the local part is itself preceded by one) makes it an address.
        let unit = marker == 1
            && !(start > 0 && chars[start - 1] == '@')
            && host.rsplit('.').next().is_some_and(|l| UNIT_SUFFIXES.iter().any(|u| l.eq_ignore_ascii_case(u)));
        if start < i && host.contains('.') && !host.starts_with('.') && !unit {
            out.extend(&chars[copied..start]);
            out.push_str("<email>");
            copied = end;
            i = end;
        } else {
            i += marker;
        }
    }
    out.extend(&chars[copied..]);
    out
}

/// `s` cleaned, redacted, and only then cut to `max` characters: a name straddling the cut would
/// otherwise survive in pieces. Tag characters (U+E0000-E007F) stay only inside a subdivision
/// flag (after U+1F3F4): anywhere else they are invisible text.
fn safe(r: &Redactions, s: &str, max: usize) -> String {
    let cleaned = clean_line(s, usize::MAX);
    let mut flag = false;
    let kept: String = cleaned
        .chars()
        .filter(|&c| {
            let tag = ('\u{e0000}'..='\u{e007f}').contains(&c);
            if c == '\u{1f3f4}' || (tag && flag) {
                flag = true;
                return true;
            }
            flag = false;
            !tag
        })
        .collect();
    clean_line(&r.apply(&kept), max)
}

/// Identical errors (same identifier and message, after redaction) folded into one with a count,
/// ordered by their latest occurrence.
fn fold(errors: &[ReportError], r: &Redactions) -> Vec<(ReportError, usize)> {
    let mut out: Vec<(ReportError, usize)> = Vec::new();
    for e in errors {
        let e = ReportError { time: e.time.clone(), ident: safe(r, &e.ident, 40), message: safe(r, &e.message, MESSAGE_MAX) };
        let seen = out.iter().position(|(o, _)| o.ident == e.ident && o.message == e.message);
        let count = seen.map_or(1, |i| out.remove(i).1 + 1);
        out.push((e, count));
    }
    out
}

fn fixed_part(i: &ReportInputs, r: &Redactions) -> Vec<String> {
    let mut lines = vec![format!("alpheus diagnostics report, {}", i.when)];
    let mut versions = vec![format!("alpheus {}", i.alpheus)];
    for (label, v) in [("systemd", &i.systemd), ("kernel", &i.kernel)] {
        if !v.is_empty() {
            versions.push(format!("{label} {v}"));
        }
    }
    for v in [&i.compositor, &i.os] {
        if !v.is_empty() {
            versions.push(v.clone());
        }
    }
    lines.push(versions.join(" · "));
    let list = |label: &str, items: Option<Vec<String>>| match items {
        None => format!("{label}: unknown"),
        Some(items) if items.is_empty() => format!("{label}: none"),
        Some(items) => format!("{label}: {}", items.join("; ")),
    };
    lines.push(list(
        "Failed units",
        i.failed.as_ref().map(|v| v.iter().map(|u| format!("{} ({}: {})", u.unit, u.scope, safe(r, &u.description, 60))).collect()),
    ));
    lines.push(list(
        "Restarting in a loop",
        i.loops.as_ref().map(|v| v.iter().map(|l| format!("{} ({}, {} restarts)", l.unit, l.scope, l.restarts)).collect()),
    ));
    lines.push(list(
        "Crashes (7 days)",
        i.crashes.as_ref().map(|v| v.iter().map(|c| format!("{} {} {}", c.time, c.exe, c.signal)).collect()),
    ));
    // The fields that were not cut above are redacted as whole lines.
    lines.iter().map(|l| r.apply(l)).collect()
}

/// The report with the latest `keep` of `folded` (None: the errors could not be read).
fn with_errors(fixed: &[String], folded: Option<&[(ReportError, usize)]>, keep: usize) -> String {
    let mut lines: Vec<String> = fixed.to_vec();
    match folded {
        None => lines.push("Errors this boot: unknown".to_string()),
        Some([]) => lines.push("Errors this boot: none".to_string()),
        Some(folded) => {
            let total = folded.len();
            let shown = &folded[total - keep.min(total)..];
            lines.push(if shown.len() == total {
                format!("Errors this boot (latest {total}, duplicates folded):")
            } else if keep == 0 {
                // Too long for the heading below: a shorter line that still says what was read.
                format!("Errors this boot: latest 0 of {total}, cut")
            } else {
                format!("Errors this boot (latest {} of {total}, older ones cut):", shown.len())
            });
            for (e, n) in shown {
                let times = if *n > 1 { format!(" (×{n})") } else { String::new() };
                lines.push(format!(" {} {}: {}{times}", e.time, e.ident, e.message));
            }
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The report: the fixed part, then the latest 40 folded errors, in at most `MAX_INPUT` bytes.
/// Every field is redacted before it is cut, so what is shown and copied is what was redacted.
/// Over the bound, the oldest errors go first and the errors heading says how many are left;
/// only if the fixed part alone is over it, it is cut on a line boundary with `… cut`.
pub fn build(i: &ReportInputs, r: &Redactions) -> String {
    let fixed = fixed_part(i, r);
    let mut folded = i.errors.as_deref().map(|e| fold(e, r));
    if let Some(f) = &mut folded
        && f.len() > MAX_ERRORS
    {
        // "latest 40": the heading counts what was kept after the folding, not what came before.
        f.drain(..f.len() - MAX_ERRORS);
    }
    let count = folded.as_ref().map_or(0, Vec::len);
    for keep in (0..=count).rev() {
        let text = with_errors(&fixed, folded.as_deref(), keep);
        if text.len() <= MAX_INPUT {
            return text;
        }
    }
    // The fixed part alone is too long.
    let mut out = String::new();
    for line in with_errors(&fixed, folded.as_deref(), 0).lines() {
        if out.len() + line.len() + 1 + "… cut\n".len() > MAX_INPUT {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("… cut\n");
    out
}

fn utc_tm(secs: u64) -> libc::tm {
    let t = libc::time_t::try_from(secs).unwrap_or(0);
    // SAFETY: gmtime_r writes only into the zeroed `tm` owned here.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&t, &mut tm);
        tm
    }
}

/// "2026-11-21 21:40 UTC".
pub fn utc_stamp(secs: u64) -> String {
    let tm = utc_tm(secs);
    format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

/// "21:02:11" in UTC (the report says UTC once, in its first line).
pub fn utc_clock(secs: u64) -> String {
    let tm = utc_tm(secs);
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// "Nov 20 14:02" in UTC.
pub fn utc_month_day(secs: u64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let tm = utc_tm(secs);
    format!("{} {:>2} {:02}:{:02}", MONTHS[usize::try_from(tm.tm_mon).unwrap_or(0) % 12], tm.tm_mday, tm.tm_hour, tm.tm_min)
}

/// What the report and its redactions need to know about this machine and this user.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostFacts {
    pub kernel: String,
    pub os: String,
    pub users: Vec<String>,
    pub homes: Vec<String>,
    pub host: Option<String>,
    pub machine_id: Option<String>,
}

/// The `release` field of a `uname(2)` answer: never the node name next to it.
pub fn release_of(u: &libc::utsname) -> String {
    // SAFETY: `release` is a NUL-terminated array the kernel filled.
    let c = unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) };
    clean_line(&c.to_string_lossy(), 64)
}

/// `PRETTY_NAME` of an os-release file's text (quotes removed).
pub fn os_pretty(os_release: &str) -> String {
    os_release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| clean_line(v.trim().trim_matches('"').trim_matches('\''), 80))
        .unwrap_or_default()
}

/// The real name (the first comma-separated field of GECOS) and each word of it of
/// `MIN_NAME` characters or more.
fn real_name_words(gecos: &str) -> Vec<String> {
    let name = gecos.split(',').next().unwrap_or("").trim();
    let mut out = Vec::new();
    if !name.is_empty() {
        out.push(name.to_string());
        out.extend(name.split_whitespace().filter(|w| w.chars().count() >= MIN_NAME).map(str::to_string));
    }
    out
}

impl HostFacts {
    /// Reads them: `uname`, `/etc/os-release`, `$USER`, `$HOME`, the passwd entry, the host name and
    /// the machine id. Small files, read once per report.
    pub fn gather() -> HostFacts {
        // SAFETY: uname fills the zeroed struct owned here.
        let kernel = unsafe {
            let mut u: libc::utsname = std::mem::zeroed();
            if libc::uname(&mut u) == 0 { release_of(&u) } else { String::new() }
        };
        let read = |p: &str| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let (mut users, mut homes) = (Vec::new(), Vec::new());
        for var in ["USER", "LOGNAME"] {
            users.extend(std::env::var(var).ok());
        }
        homes.extend(std::env::var("HOME").ok());
        // SAFETY: getpwuid_r writes into the buffers owned here; the strings are copied at once.
        // A buffer too small answers ERANGE: retried bigger, up to 64 KiB.
        unsafe {
            let mut size = 2048;
            while size <= 65536 {
                let mut pwd: libc::passwd = std::mem::zeroed();
                let mut buf = vec![0 as libc::c_char; size];
                let mut found: *mut libc::passwd = std::ptr::null_mut();
                let rc = libc::getpwuid_r(libc::geteuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found);
                if rc == libc::ERANGE {
                    size *= 2;
                    continue;
                }
                if rc == 0 && !found.is_null() {
                    users.push(std::ffi::CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned());
                    homes.push(std::ffi::CStr::from_ptr(pwd.pw_dir).to_string_lossy().into_owned());
                    // The real name (the first comma-separated field of GECOS), when there is one.
                    if !pwd.pw_gecos.is_null() {
                        let gecos = std::ffi::CStr::from_ptr(pwd.pw_gecos).to_string_lossy().into_owned();
                        users.extend(real_name_words(&gecos));
                    }
                }
                break;
            }
        }
        HostFacts {
            kernel,
            os: read("/etc/os-release").map(|s| os_pretty(&s)).unwrap_or_default(),
            users,
            homes,
            host: read("/proc/sys/kernel/hostname"),
            machine_id: read("/etc/machine-id"),
        }
    }

    /// A host name that is one of the words of the distribution's own name (`nixos` on NixOS, the
    /// installer's default) is no secret, and replacing it would shred `NixOS 25.05` and the store
    /// paths: it is left out. So is the first label of a qualified one (`nixos.lan`), which would
    /// shred the same words; the whole name is still replaced. Without an os name, `nixos` and
    /// `localhost` stand in.
    pub fn redactions(&self) -> Redactions {
        let words: Vec<&str> = self.os.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let common = |label: &str| {
            let label = label.trim();
            if words.is_empty() {
                ["nixos", "localhost"].iter().any(|w| w.eq_ignore_ascii_case(label))
            } else {
                words.iter().any(|w| w.eq_ignore_ascii_case(label))
            }
        };
        let host = self.host.as_deref().filter(|h| !common(h));
        let short = host.is_none_or(|h| h.split_once('.').is_none_or(|(first, _)| !common(first)));
        Redactions::build(&self.users, &self.homes, host, short, self.machine_id.as_deref())
    }
}

#[cfg(test)]
mod tests;
