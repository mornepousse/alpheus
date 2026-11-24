//! `systemctl show --property=Id,NRestarts,ActiveState,SubState` on several units: `key=value`
//! blocks separated by an empty line.
//!
//! Never:
//! - launch a tool or open a file: the backend does that, and these only read what it printed;
//! - keep a field the view does not show: what journald or `coredumpctl` adds is dropped at the
//!   parse, so a secret in the environment or a command line is never carried;
//! - trust a text: everything from outside passes `text::clean_line` before it is kept.
//!
//! See: docs/features/control-center.md ("Diag"), docs/decisions.md (the Diag entries),
//! AGENTS.md ("No private data in fixtures or logs").

use super::names::valid_unit;
use crate::text::clean_line;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restarts {
    pub unit: String,
    pub restarts: u32,
    pub active: String,
    pub sub: String,
}

/// The most loops kept.
pub const MAX_LOOPS: usize = 64;

/// The units of `text` restarted at least `threshold` times, highest first, at most MAX_LOOPS. A
/// block with no valid `Id` or no numeric `NRestarts` is skipped.
pub fn parse_restarts(text: &[u8], threshold: u32) -> Vec<Restarts> {
    let text = String::from_utf8_lossy(text);
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let (mut unit, mut restarts, mut active, mut sub) = (None, None, String::new(), String::new());
        for line in block.lines() {
            match line.split_once('=') {
                Some(("Id", v)) => unit = Some(clean_line(v, 256)),
                Some(("NRestarts", v)) => restarts = v.trim().parse::<u32>().ok(),
                Some(("ActiveState", v)) => active = clean_line(v, 40),
                Some(("SubState", v)) => sub = clean_line(v, 40),
                _ => {}
            }
        }
        if let (Some(unit), Some(restarts)) = (unit, restarts)
            && valid_unit(&unit)
            && restarts >= threshold
        {
            out.push(Restarts { unit, restarts, active, sub });
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.restarts));
    out.truncate(MAX_LOOPS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_from_show_blocks() {
        let loops = parse_restarts(include_bytes!("../../tests/fixtures/diag-show-restarts.txt"), 3);
        assert_eq!(
            loops,
            [
                Restarts { unit: "example-sync.service".into(), restarts: 7, active: "activating".into(), sub: "auto-restart".into() },
                Restarts { unit: "example-flaky.service".into(), restarts: 3, active: "active".into(), sub: "running".into() },
            ]
        );
        assert_eq!(parse_restarts(include_bytes!("../../tests/fixtures/diag-show-restarts.txt"), 1).len(), 3);
        assert!(parse_restarts(b"", 3).is_empty());
    }

    #[test]
    fn a_block_without_a_valid_unit_or_count_is_skipped_and_the_list_bounded() {
        let text = "Id=--now\nNRestarts=9\n\nId=a.service\nNRestarts=many\n\nId=b.service\nNRestarts=5\n";
        let loops = parse_restarts(text.as_bytes(), 3);
        assert_eq!(loops.iter().map(|l| l.unit.as_str()).collect::<Vec<_>>(), ["b.service"]);
        let many: String = (0..100).map(|i| format!("Id=u{i}.service\nNRestarts={}\n\n", 10 + i)).collect();
        let loops = parse_restarts(many.as_bytes(), 3);
        assert_eq!(loops.len(), MAX_LOOPS);
        assert_eq!(loops[0].restarts, 109, "highest first");
    }
}
