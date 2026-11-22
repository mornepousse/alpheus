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
    exact: Vec<(String, &'static str)>,
}

impl Redactions {
    /// The user's names and homes, the host name and the machine id.
    pub fn new(users: &[String], homes: &[String], host: Option<&str>, machine_id: Option<&str>) -> Redactions {
        let mut exact: Vec<(String, &'static str)> = Vec::new();
        let mut add = |s: &str, to: &'static str, min: usize| {
            let s = s.trim();
            if s.chars().count() >= min && !exact.iter().any(|(e, _)| e == s) {
                exact.push((s.to_string(), to));
            }
        };
        if let Some(id) = machine_id {
            add(id, "<machine-id>", 16);
        }
        for h in homes {
            // "/" alone, or an empty home, would eat every path.
            add(h.trim_end_matches('/'), "~", 3);
        }
        if let Some(h) = host {
            add(h, "<host>", MIN_NAME);
        }
        for u in users {
            add(u, "<user>", MIN_NAME);
        }
        exact.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        Redactions { exact }
    }

    /// `text` with every listed string and every address-shaped token replaced.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (needle, to) in &self.exact {
            out = out.replace(needle.as_str(), to);
        }
        let out = replace_tokens(&out, mac, "<mac>");
        let out = replace_tokens(&out, ipv6, "<ip>");
        let out = replace_tokens(&out, ipv4, "<ip>");
        redact_emails(&out)
    }
}

/// A token is a maximal run of characters that can be part of an address; `f` says whether the
/// whole token is one.
fn replace_tokens(text: &str, f: fn(&str) -> bool, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if !token.is_empty() {
            // Brackets, a trailing colon or dot are punctuation around the address, not in it.
            let core = token.trim_matches(|c| c == ':' && !token.contains("::") || c == '.' || c == '-');
            if !core.is_empty() && f(core) {
                let start = token.find(core).unwrap_or(0);
                out.push_str(&token[..start]);
                out.push_str(to);
                out.push_str(&token[start + core.len()..]);
            } else {
                out.push_str(token);
            }
            token.clear();
        }
    };
    for c in text.chars() {
        if c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '-') {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

fn mac(t: &str) -> bool {
    let sep = if t.contains(':') { ':' } else { '-' };
    let parts: Vec<&str> = t.split(sep).collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn ipv4(t: &str) -> bool {
    let parts: Vec<&str> = t.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n <= 255))
}

/// Hex groups of at most four digits, either all eight of them or a `::` somewhere: a clock
/// (`21:02:11`) is neither.
fn ipv6(t: &str) -> bool {
    if t.contains('.') || t.contains('-') {
        return false;
    }
    let groups: Vec<&str> = t.split(':').collect();
    let compressed = t.contains("::");
    let full = groups.len() == 8 && groups.iter().all(|g| !g.is_empty());
    if !(compressed || full) || groups.len() > 8 + usize::from(compressed) || groups.len() < 3 {
        return false;
    }
    groups.iter().all(|g| g.len() <= 4 && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn redact_emails(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let local = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
    let domain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0; // chars before `copied` are already in `out`
    while i < chars.len() {
        if chars[i] != '@' {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > copied && local(chars[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < chars.len() && domain(chars[end]) {
            end += 1;
        }
        // A trailing dot is the sentence's.
        while end > i + 1 && chars[end - 1] == '.' {
            end -= 1;
        }
        let host: String = chars[i + 1..end].iter().collect();
        if start < i && host.contains('.') && !host.starts_with('.') {
            out.extend(&chars[copied..start]);
            out.push_str("<email>");
            copied = end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.extend(&chars[copied..]);
    out
}

/// Identical errors (same identifier and message) folded into one with a count, ordered by their
/// latest occurrence.
fn fold(errors: &[ReportError]) -> Vec<(ReportError, usize)> {
    let mut out: Vec<(ReportError, usize)> = Vec::new();
    for e in errors {
        let e = ReportError { time: e.time.clone(), ident: clean_line(&e.ident, 40), message: clean_line(&e.message, MESSAGE_MAX) };
        let seen = out.iter().position(|(o, _)| o.ident == e.ident && o.message == e.message);
        let count = seen.map_or(1, |i| out.remove(i).1 + 1);
        out.push((e, count));
    }
    out
}

fn fixed_part(i: &ReportInputs) -> Vec<String> {
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
        i.failed.as_ref().map(|v| v.iter().map(|u| format!("{} ({}: {})", u.unit, u.scope, clean_line(&u.description, 60))).collect()),
    ));
    lines.push(list(
        "Restarting in a loop",
        i.loops.as_ref().map(|v| v.iter().map(|l| format!("{} ({}, {} restarts)", l.unit, l.scope, l.restarts)).collect()),
    ));
    lines.push(list(
        "Crashes (7 days)",
        i.crashes.as_ref().map(|v| v.iter().map(|c| format!("{} {} {}", c.time, c.exe, c.signal)).collect()),
    ));
    lines
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

/// The report: the fixed part, then the latest 40 folded errors, redacted, in at most `MAX_INPUT`
/// bytes. Over the bound, the oldest errors go first and the errors heading says how many are
/// left; only if the fixed part alone is over it, it is cut on a line boundary with `… cut`.
pub fn build(i: &ReportInputs, r: &Redactions) -> String {
    let fixed = fixed_part(i);
    let mut folded = i.errors.as_deref().map(fold);
    if let Some(f) = &mut folded
        && f.len() > MAX_ERRORS
    {
        // "latest 40": the heading counts what was kept after the folding, not what came before.
        f.drain(..f.len() - MAX_ERRORS);
    }
    let count = folded.as_ref().map_or(0, Vec::len);
    for keep in (0..=count).rev() {
        let text = r.apply(&with_errors(&fixed, folded.as_deref(), keep));
        if text.len() <= MAX_INPUT {
            return text;
        }
    }
    // The fixed part alone is too long.
    let mut out = String::new();
    for line in r.apply(&with_errors(&fixed, None, 0)).lines() {
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
        unsafe {
            let mut pwd: libc::passwd = std::mem::zeroed();
            let mut buf = vec![0 as libc::c_char; 2048];
            let mut found: *mut libc::passwd = std::ptr::null_mut();
            if libc::getpwuid_r(libc::geteuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) == 0 && !found.is_null() {
                users.push(std::ffi::CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned());
                homes.push(std::ffi::CStr::from_ptr(pwd.pw_dir).to_string_lossy().into_owned());
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

    pub fn redactions(&self) -> Redactions {
        Redactions::new(&self.users, &self.homes, self.host.as_deref(), self.machine_id.as_deref())
    }
}

#[cfg(test)]
mod tests;
