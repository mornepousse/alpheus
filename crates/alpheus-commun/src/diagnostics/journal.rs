//! `journalctl -o json`: one JSON object per line. Only the fields the view shows are kept;
//! everything else journald adds (hostname, machine and boot ids, the command line…) is dropped
//! at the parse.

use super::names::valid_cursor;
use crate::text::clean_line;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub cursor: String,
    pub time_us: u64,
    pub priority: u8,
    /// `SYSLOG_IDENTIFIER`, else `_COMM`; empty when neither.
    pub ident: String,
    /// `_SYSTEMD_UNIT`, else `_SYSTEMD_USER_UNIT`; empty when neither.
    pub unit: String,
    pub pid: Option<u32>,
    pub message: String,
}

/// Longest message kept, in characters.
pub const MAX_MESSAGE: usize = 1024;

/// The shown text of a field journald gave over 4,096 bytes of (JSON `null`).
pub const TOO_LONG: &str = "[message too long]";

/// The entries of `json`, at most `max`; true when there were more (the extra ones dropped, the
/// first `max` kept so a follow read continues from the last kept cursor). A line that is not
/// a whole JSON object (a read cut mid-line) or whose cursor is not valid is skipped.
pub fn parse_entries(json: &[u8], max: usize) -> (Vec<Entry>, bool) {
    let mut entries = Vec::new();
    for line in json.split(|&b| b == b'\n') {
        let Ok(serde_json::Value::Object(o)) = serde_json::from_slice::<serde_json::Value>(line) else { continue };
        let Some(entry) = entry(&o) else { continue };
        if entries.len() == max {
            return (entries, true);
        }
        entries.push(entry);
    }
    (entries, false)
}

fn entry(o: &serde_json::Map<String, serde_json::Value>) -> Option<Entry> {
    let str_of = |k: &str| o.get(k).and_then(|v| v.as_str());
    let cursor = str_of("__CURSOR").filter(|c| valid_cursor(c))?.to_string();
    let message = match o.get("MESSAGE") {
        Some(serde_json::Value::String(s)) => clean_line(s, MAX_MESSAGE),
        Some(serde_json::Value::Array(bytes)) => {
            let raw: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok())).collect();
            clean_line(&String::from_utf8_lossy(&raw), MAX_MESSAGE)
        }
        Some(serde_json::Value::Null) => TOO_LONG.to_string(),
        _ => String::new(),
    };
    Some(Entry {
        cursor,
        time_us: str_of("__REALTIME_TIMESTAMP").and_then(|t| t.parse().ok()).unwrap_or(0),
        priority: str_of("PRIORITY").and_then(|p| p.parse().ok()).unwrap_or(6),
        ident: clean_line(str_of("SYSLOG_IDENTIFIER").or(str_of("_COMM")).unwrap_or(""), 100),
        unit: clean_line(str_of("_SYSTEMD_UNIT").or(str_of("_SYSTEMD_USER_UNIT")).unwrap_or(""), 256),
        pid: str_of("_PID").and_then(|p| p.parse().ok()),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/diag-journal.jsonl");

    #[test]
    fn journal_entries_keep_only_named_fields() {
        let (entries, more) = parse_entries(FIXTURE, 100);
        assert!(!more);
        assert_eq!(entries.len(), 3, "the invalid cursor is dropped");
        assert_eq!(
            entries[0],
            Entry {
                cursor: "s=0000000000000000;i=1;b=0000000000000000;m=1;t=1;x=1".into(),
                time_us: 1_763_700_000_000_000,
                priority: 3,
                ident: "example-app".into(),
                unit: "example-app.service".into(),
                pid: Some(4242),
                message: "it broke".into(),
            }
        );
        let shown = format!("{entries:?}");
        for secret in ["host-01", "not-for-us", "00000000000000000000000000000000", "_CMDLINE"] {
            assert!(!shown.contains(secret), "{secret} reached an entry");
        }
    }

    #[test]
    fn journal_message_forms() {
        let (entries, _) = parse_entries(FIXTURE, 100);
        assert_eq!(entries[1].message, "hi\u{fffd}", "a byte array is read lossily");
        assert_eq!(
            (entries[1].ident.as_str(), entries[1].unit.as_str(), entries[1].pid),
            ("example-sync", "example-sync.service", None),
            "_COMM and the user unit"
        );
        assert_eq!(entries[2].message, TOO_LONG, "null");
        let long = format!(r#"{{"__CURSOR":"c1","MESSAGE":"{}\u001b[2J"}}"#, "m".repeat(2000));
        let (e, _) = parse_entries(long.as_bytes(), 10);
        assert_eq!(e[0].message.chars().count(), MAX_MESSAGE);
        assert_eq!(e[0].priority, 6, "no priority reads as info");
    }

    #[test]
    fn a_partial_last_line_is_ignored() {
        let cut = &FIXTURE[..FIXTURE.len() - 200];
        let (entries, more) = parse_entries(cut, 100);
        assert!(!more);
        assert!(entries.len() < 3 && !entries.is_empty());
        assert_eq!(parse_entries(b"{\"__CURSOR\":\"c1\",\"MESS", 10).0, vec![]);
        assert_eq!(parse_entries(b"", 10), (vec![], false));
    }

    #[test]
    fn more_than_max_keeps_the_first_and_says_so() {
        let lines: String = (0..5).map(|i| format!("{{\"__CURSOR\":\"c{i}\",\"MESSAGE\":\"m{i}\"}}\n")).collect();
        let (entries, more) = parse_entries(lines.as_bytes(), 3);
        assert!(more);
        assert_eq!(entries.iter().map(|e| e.cursor.as_str()).collect::<Vec<_>>(), ["c0", "c1", "c2"]);
        assert!(!parse_entries(lines.as_bytes(), 5).1);
    }
}
