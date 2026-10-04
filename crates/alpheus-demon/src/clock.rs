//! Local time (system timezone, `localtime_r`) and strftime-style
//! formatting, in English, with no dependency. Directives: %a %A %e %d %b
//! %B %m %Y %H %M %S %%. `%e` = day without leading zero or space ("4").

use std::time::{Duration, SystemTime, UNIX_EPOCH};

// The `libc` crate of this repo only declares `tzset` for Windows: on Unix,
// it is nonetheless a standard C function (time.h), already linked through
// the system's C library. We declare it ourselves.
unsafe extern "C" {
    fn tzset();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
    pub day: u8,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const SHORT_DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const SHORT_MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DIRECTIVES: &str = "aAedbBmYHMS%";

impl LocalTime {
    /// Now, and the nanoseconds elapsed in the current second (to time the
    /// next tick).
    pub fn now() -> (LocalTime, u32) {
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        (Self::from_epoch(d.as_secs() as i64), d.subsec_nanos())
    }

    /// Seconds since the Unix epoch → the system's local time.
    pub fn from_epoch(secs: i64) -> LocalTime {
        let t = secs as libc::time_t;
        // SAFETY: tzset() only re-reads TZ and the zone file; no pointer is
        // kept. Without this call, `localtime_r` keeps the timezone read at
        // first use: a timezone change during the daemon's lifetime (a
        // laptop changing continent, a TZ change made and then fixed) would
        // never be taken into account.
        unsafe { tzset() };
        // SAFETY: tm is a plain C struct, valid when zero-filled;
        // localtime_r writes into it and keeps no pointer.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `t` and `tm` live for the duration of the call.
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return LocalTime { year: 1970, month: 1, day: 1, weekday: 4, hour: 0, minute: 0, second: 0 };
        }
        LocalTime {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1).clamp(1, 12) as u8,
            day: tm.tm_mday.clamp(1, 31) as u8,
            weekday: tm.tm_wday.clamp(0, 6) as u8,
            hour: tm.tm_hour.clamp(0, 23) as u8,
            minute: tm.tm_min.clamp(0, 59) as u8,
            second: tm.tm_sec.clamp(0, 59) as u8, // leap second (60) brought back to 59
        }
    }
}

/// Does the format use only known directives?
pub fn validate(fmt: &str) -> Result<(), String> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c == '%' {
            match it.next() {
                Some(d) if DIRECTIVES.contains(d) => {}
                Some(d) => return Err(format!("unknown directive \"%{d}\" (known: %a %A %e %d %b %B %m %Y %H %M %S %%)")),
                None => return Err("\"%\" alone at the end of the format".into()),
            }
        }
    }
    Ok(())
}

pub fn format(fmt: &str, h: &LocalTime) -> String {
    let mut s = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            s.push(c);
            continue;
        }
        match it.next() {
            Some('a') => s.push_str(SHORT_DAYS[h.weekday as usize % 7]),
            Some('A') => s.push_str(DAYS[h.weekday as usize % 7]),
            Some('e') => s.push_str(&h.day.to_string()),
            Some('d') => s.push_str(&format!("{:02}", h.day)),
            Some('b') => s.push_str(SHORT_MONTHS[(h.month as usize + 11) % 12]),
            Some('B') => s.push_str(MONTHS[(h.month as usize + 11) % 12]),
            Some('m') => s.push_str(&format!("{:02}", h.month)),
            Some('Y') => s.push_str(&h.year.to_string()),
            Some('H') => s.push_str(&format!("{:02}", h.hour)),
            Some('M') => s.push_str(&format!("{:02}", h.minute)),
            Some('S') => s.push_str(&format!("{:02}", h.second)),
            Some('%') => s.push('%'),
            // Validated at load time: an unknown directive should not reach
            // here; it is copied as-is instead of panicking.
            Some(d) => {
                s.push('%');
                s.push(d);
            }
            None => s.push('%'),
        }
    }
    s
}

pub fn has_seconds(fmt: &str) -> bool {
    fmt.contains("%S")
}

/// Margin past the boundary: we never wake up a hair early.
pub const MARGIN: Duration = Duration::from_millis(20);

/// Delay until the next display change, based on the current second (0-59)
/// and the nanoseconds elapsed in that second.
pub fn next_tick(second: u8, nanos: u32, seconds: bool) -> Duration {
    let rest_of_second = Duration::from_nanos(1_000_000_000 - u64::from(nanos.min(999_999_999)));
    if seconds {
        return rest_of_second + MARGIN;
    }
    let seconds_left = 59 - u64::from(second.min(59));
    Duration::from_secs(seconds_left) + rest_of_second + MARGIN
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sunday, October 4, 2026, 14:05:09.
    const SUN: LocalTime = LocalTime { year: 2026, month: 10, day: 4, weekday: 0, hour: 14, minute: 5, second: 9 };

    #[test]
    fn english_format() {
        assert_eq!(format("%a %e %b", &SUN), "Sun 4 Oct");
        assert_eq!(format("%A %d %B %Y", &SUN), "Sunday 04 October 2026");
        assert_eq!(format("%H:%M:%S", &SUN), "14:05:09");
        assert_eq!(format("%d/%m 100%%", &SUN), "04/10 100%");
        let feb = LocalTime { month: 2, weekday: 3, ..SUN };
        assert_eq!(format("%a %b", &feb), "Wed Feb");
        let dec = LocalTime { month: 12, weekday: 6, ..SUN };
        assert_eq!(format("%A %B", &dec), "Saturday December");
        assert_eq!(format("", &SUN), "");
    }

    #[test]
    fn format_validation() {
        assert_eq!(validate("%H:%M"), Ok(()));
        assert_eq!(validate("%a %e %b"), Ok(()));
        assert_eq!(validate(""), Ok(()));
        assert!(validate("%H:%Q").unwrap_err().contains("%Q"));
        assert!(validate("%H%").unwrap_err().contains("end"));
    }

    #[test]
    fn tick_at_the_minute_or_the_second() {
        assert_eq!(next_tick(30, 0, false), Duration::from_secs(30) + MARGIN);
        assert_eq!(next_tick(0, 0, false), Duration::from_secs(60) + MARGIN);
        assert_eq!(next_tick(59, 999_000_000, false), Duration::from_millis(1) + MARGIN);
        assert_eq!(next_tick(10, 500_000_000, true), Duration::from_millis(500) + MARGIN);
        assert!(next_tick(60, 2_000_000_000, false) <= Duration::from_secs(1) + MARGIN, "out of bounds: no overflow");
    }

    #[test]
    fn tzset_called_again_on_each_read() {
        // Fixed offsets ("FOO-3" = UTC+3, without a ":") rather than zone
        // names ("Europe/Paris"): no tzdata base needed, the test stays
        // portable (sandbox or CI without /usr/share/zoneinfo). Only THIS
        // function reads TZ (`tzset`, see `from_epoch`); no other test in
        // this suite depends on it, so the risk from writing an environment
        // variable shared across test threads is nil in practice here.
        let before = std::env::var("TZ").ok();
        // SAFETY: only this test reads or writes TZ in this suite.
        unsafe { std::env::set_var("TZ", "FOO-3") }; // UTC+3
        let h1 = LocalTime::from_epoch(1_700_000_000);
        // SAFETY: same.
        unsafe { std::env::set_var("TZ", "BAR2") }; // UTC-2
        let h2 = LocalTime::from_epoch(1_700_000_000);
        assert_ne!(h1.hour, h2.hour, "the timezone changes: without tzset(), the second read would keep the first timezone");
        match before {
            // SAFETY: same.
            Some(v) => unsafe { std::env::set_var("TZ", v) },
            None => unsafe { std::env::remove_var("TZ") },
        }
    }

    #[test]
    fn system_local_time_consistent() {
        // 2026-10-15 12:00 UTC: same month and year in any timezone (±14 h).
        let h = LocalTime::from_epoch(1_792_065_600);
        assert_eq!((h.year, h.month), (2026, 10), "{h:?}");
        assert!((14..=16).contains(&h.day), "{h:?}");
        // 2026-10-15 is a Thursday (4); the day before or after depending on timezone.
        assert!((3..=5).contains(&h.weekday), "{h:?}");
        let (m, nanos) = LocalTime::now();
        assert!(m.year >= 2026 && nanos < 1_000_000_000);
    }
}
