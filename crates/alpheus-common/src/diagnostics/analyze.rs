//! `systemd-analyze blame`: lines of systemd's own time spans, which it never localises (`1min
//! 2.345s`, `44.764s`, `812ms`).
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blame {
    pub duration_ms: u64,
    pub unit: String,
}

/// The first `max` lines of `blame` that read as a time span then a unit name.
pub fn parse_blame(text: &str, max: usize) -> Vec<Blame> {
    let mut out = Vec::new();
    for line in text.lines() {
        if out.len() >= max {
            break;
        }
        let mut words = line.split_whitespace().peekable();
        let mut total = 0u64;
        let mut seen = false;
        while let Some(&word) = words.peek() {
            match component(word) {
                Some(ms) => {
                    total = total.saturating_add(ms);
                    seen = true;
                    words.next();
                }
                None => break,
            }
        }
        let name = words.collect::<Vec<_>>().join(" ");
        if seen && !name.is_empty() && valid_unit(&name) {
            out.push(Blame { duration_ms: total, unit: name });
        }
    }
    out
}

/// One `<number><unit>` word of a time span, in milliseconds (microseconds rounded to the nearest).
fn component(word: &str) -> Option<u64> {
    let split = word.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = word.split_at(split);
    let n: f64 = number.parse().ok().filter(|n: &f64| n.is_finite())?;
    let per_ms = match unit {
        "us" => 0.001,
        "ms" => 1.0,
        "s" => 1_000.0,
        "min" => 60_000.0,
        "h" => 3_600_000.0,
        "d" => 86_400_000.0,
        "w" => 604_800_000.0,
        "month" => 2_629_800_000.0,
        "y" => 31_557_600_000.0,
        _ => return None,
    };
    Some((n * per_ms).round() as u64)
}

/// A whole time span (`1min 2.345s`) in milliseconds; None for anything else.
pub fn parse_timespan(s: &str) -> Option<u64> {
    let mut words = s.split_whitespace().peekable();
    words.peek()?;
    words.map(component).try_fold(0u64, |acc, ms| ms.map(|ms| acc.saturating_add(ms)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blame_and_timespans() {
        assert_eq!(parse_timespan("1min 2.345s"), Some(62_345));
        assert_eq!(parse_timespan("812ms"), Some(812));
        assert_eq!(parse_timespan("44.764s"), Some(44_764));
        assert_eq!(parse_timespan("1h 2min 3s"), Some(3_723_000));
        assert_eq!(parse_timespan("2d 1h"), Some(176_400_000));
        assert_eq!(parse_timespan("15us"), Some(0));
        assert_eq!(parse_timespan("1500us"), Some(2), "rounded to the nearest millisecond");
        assert_eq!(parse_timespan(""), None);
        assert_eq!(parse_timespan("garbage"), None);
        assert_eq!(parse_timespan("12"), None, "no unit");
        assert_eq!(parse_timespan("1s example.service"), None, "a name is not a time");

        let blame = parse_blame(include_str!("../../tests/fixtures/diag-blame.txt"), 10);
        assert_eq!(
            blame,
            [
                Blame { duration_ms: 62_345, unit: "example-sync.service".into() },
                Blame { duration_ms: 44_764, unit: "example-app.service".into() },
                Blame { duration_ms: 812, unit: "mnt-data.mount".into() },
                Blame { duration_ms: 0, unit: "example-fast.service".into() },
                Blame { duration_ms: 3_723_000, unit: "example-long.service".into() },
            ],
            "the garbage line is skipped"
        );
        assert_eq!(parse_blame(include_str!("../../tests/fixtures/diag-blame.txt"), 2).len(), 2);
    }

    #[test]
    fn hostile_spans_saturate_instead_of_overflowing() {
        let huge = "9999999999999999y ".repeat(40);
        assert_eq!(parse_timespan(&huge), Some(u64::MAX));
        let blame = parse_blame(&format!("{huge}example.service\n"), 10);
        assert_eq!(blame[0].duration_ms, u64::MAX);
    }

    #[test]
    fn a_hostile_unit_name_is_dropped() {
        assert!(parse_blame("1s --now\n2s a b.service\n", 10).is_empty(), "never a name that fails the unit check");
    }
}
