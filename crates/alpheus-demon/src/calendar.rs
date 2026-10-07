//! The clock's calendar, pure (spec § 1 Calendar): one month as a grid of
//! cells, today inverted like the active workspace, the first day of the
//! week from the locale's LC_TIME, optional ISO week numbers. English like
//! the clock (question 3); only the weeks of the shown month (4 to 6 rows). No events, nothing kept:
//! the popup (task 18) builds a `Calendar` at each open and drops it.

use crate::clock::{LocalTime, MONTHS, SHORT_DAYS};
use crate::grid::{Grid, Style, text_width};
use std::ffi::CStr;

/// Monday, 0 = Sunday: the first day when the locale says nothing usable
/// (C/POSIX, unreadable — question 4, ISO 8601).
pub const MONDAY: u8 = 1;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`); `day` may run past the month's end or below 1.
fn days_from_civil(year: i32, month: u8, day: i32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day count since 1970-01-01 (Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    ((yoe + era * 400 + i64::from(month <= 2)) as i32, month, day)
}

/// Weekday of a day count, 0 = Sunday (1970-01-01 was a Thursday).
fn weekday(days: i64) -> u8 {
    (days + 4).rem_euclid(7) as u8
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Month {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
}

impl Month {
    pub fn of(t: &LocalTime) -> Month {
        Month { year: t.year, month: t.month.clamp(1, 12) }
    }

    /// `n` months later (earlier when negative).
    fn add(self, n: i32) -> Month {
        let index = i64::from(self.year) * 12 + i64::from(self.month) - 1 + i64::from(n);
        let year = index.div_euclid(12).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        Month { year, month: index.rem_euclid(12) as u8 + 1 }
    }

    pub fn next(self) -> Month {
        self.add(1)
    }

    pub fn previous(self) -> Month {
        self.add(-1)
    }

    pub fn days(self) -> u8 {
        match self.month {
            2 if is_leap(self.year) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        }
    }

    /// Weekday of its first day, 0 = Sunday.
    pub fn first_weekday(self) -> u8 {
        weekday(days_from_civil(self.year, self.month, 1))
    }
}

/// ISO 8601 week number: weeks start on Monday, week 1 holds the year's
/// first Thursday (so 1 January can be in week 52 or 53, 31 December in
/// week 1).
pub fn iso_week(year: i32, month: u8, day: u8) -> u8 {
    let days = days_from_civil(year, month, i32::from(day));
    // Monday = 1 … Sunday = 7.
    let iso_weekday = match weekday(days) {
        0 => 7,
        w => i64::from(w),
    };
    // The Thursday of this date's week decides its year; its week is the
    // number of that Thursday's ordinal day, by sevens.
    let thursday = days - iso_weekday + 4;
    let (thursday_year, _, _) = civil_from_days(thursday);
    ((thursday - days_from_civil(thursday_year, 1, 1)) / 7 + 1) as u8
}

/// glibc's rule: `week_1stday` (YYYYMMDD) is a known day, `first_weekday`
/// (1-based) counts from it → the first day of the week, 0 = Sunday.
/// Anything unreadable (a date that does not exist, a count outside 1–7):
/// Monday.
pub fn first_day_from(week_1stday: u32, first_weekday: u8) -> u8 {
    let (year, month, day) = ((week_1stday / 10_000) as i32, (week_1stday / 100 % 100) as u8, (week_1stday % 100) as u8);
    let valid_day = (1..=12).contains(&month) && day >= 1 && day <= Month { year, month }.days();
    if !valid_day || !(1..=7).contains(&first_weekday) {
        return MONDAY;
    }
    (weekday(days_from_civil(year, month, i32::from(day))) + first_weekday - 1) % 7
}

/// `C`, `POSIX`, `C.UTF-8`…: the portable locale, which says Sunday only
/// because it says nothing (question 4: Monday then).
fn is_c_locale(name: &str) -> bool {
    name == "C" || name == "POSIX" || name.starts_with("C.")
}

// glibc's langinfo.h, LC_TIME = 2: _NL_ITEM(LC_TIME, index) = (2 << 16) | index
// (checked against glibc 2.44's header, tray plan 2 task 12; the libc crate
// does not define these three).
/// `_NL_TIME_WEEK_1STDAY`: an integer YYYYMMDD in the pointer's place.
const NL_TIME_WEEK_1STDAY: libc::nl_item = 0x20066;
/// `_NL_TIME_FIRST_WEEKDAY`: a `char*` whose first byte is 1-based.
const NL_TIME_FIRST_WEEKDAY: libc::nl_item = 0x20068;
/// `_NL_LOCALE_NAME(LC_TIME)`: `_NL_ITEM(LC_TIME, 0xffff)`.
const NL_LOCALE_NAME_TIME: libc::nl_item = 0x2ffff;

/// The locale's first day (LC_TIME, read like `setlocale(LC_ALL, "")` would:
/// LC_ALL, LC_TIME, LANG), through newlocale/nl_langinfo_l/freelocale:
/// thread-safe, no global locale changed (controller decision 11). glibc
/// keeps the locale archive mapped after this first call to `newlocale(…,
/// "")` — later calls only touch the handful of pages this locale's data
/// lives on, not a fresh load of the archive. Monday for C/POSIX or when
/// unreadable (question 4).
pub fn locale_first_day() -> u8 {
    // SAFETY: newlocale(3) with a valid mask, a NUL-terminated name and no
    // base locale; null on failure, checked.
    let locale = unsafe { libc::newlocale(libc::LC_TIME_MASK, c"".as_ptr(), std::ptr::null_mut()) };
    if locale.is_null() {
        return MONDAY;
    }
    // SAFETY: `locale` is a valid locale object until freelocale below; the
    // name and first-weekday items are NUL-terminated strings owned by it,
    // read before it is freed; the week_1stday item is an integer stored in
    // the pointer itself, never dereferenced.
    let day = unsafe {
        let name = libc::nl_langinfo_l(NL_LOCALE_NAME_TIME, locale);
        if name.is_null() || is_c_locale(&CStr::from_ptr(name).to_string_lossy()) {
            MONDAY
        } else {
            // `_NL_TIME_WEEK_1STDAY` is stored as an `unsigned int` packed into
            // the pointer's bytes (glibc's `union { const char *string; unsigned
            // int word; }`), not sign/zero-extended into it: `as usize as u32`
            // would assume little-endian and read garbage on a big-endian
            // target. Reading the first 4 bytes in native order works on both
            // 32- and 64-bit pointers and on either endianness.
            let bytes = (libc::nl_langinfo_l(NL_TIME_WEEK_1STDAY, locale) as usize).to_ne_bytes();
            let [b0, b1, b2, b3, ..] = bytes;
            let week_1stday = u32::from_ne_bytes([b0, b1, b2, b3]);
            let first_weekday = libc::nl_langinfo_l(NL_TIME_FIRST_WEEKDAY, locale);
            if first_weekday.is_null() { MONDAY } else { first_day_from(week_1stday, *first_weekday as u8) }
        }
    };
    // SAFETY: the locale newlocale gave us, freed once, never used after.
    unsafe { libc::freelocale(locale) };
    day
}

/// Cells of a day number ("dd", right-aligned) and of the gap after it.
const DAY_CELLS: u16 = 2;
const COLUMN: u16 = 3;
/// The week number column ("41 ") before the days.
const WEEK_CELLS: u16 = 3;

pub struct Calendar {
    pub shown: Month,
    /// (year, month, day) of today.
    pub today: (i32, u8, u8),
    /// 0 = Sunday.
    pub first_day: u8,
    pub week_numbers: bool,
}

impl Calendar {
    pub fn new(today: &LocalTime, first_day: u8, week_numbers: bool) -> Calendar {
        Calendar { shown: Month::of(today), today: (today.year, today.month, today.day), first_day: first_day % 7, week_numbers }
    }

    /// `months` forward (negative: back); true if the shown month changed.
    pub fn scroll(&mut self, months: i32) -> bool {
        let before = self.shown;
        self.shown = self.shown.add(months);
        self.shown != before
    }

    /// Content size in cells (no frame): 20 wide (+3 with week numbers), 2 + the shown month's
    /// weeks rows (title, weekdays, 4 to 6 weeks).
    pub fn size(&self) -> (u16, u16) {
        (7 * COLUMN - 1 + if self.week_numbers { WEEK_CELLS } else { 0 }, 2 + self.weeks() as u16)
    }

    /// The week rows of the shown month: the cells before the 1st plus its days, rounded up to
    /// whole weeks (4 for a February starting on the week's first day, up to 6).
    fn weeks(&self) -> usize {
        let shown = Month { year: self.shown.year, month: self.shown.month.clamp(1, 12) };
        let offset = usize::from((shown.first_weekday() + 7 - self.first_day % 7) % 7);
        (offset + usize::from(shown.days())).div_ceil(7)
    }

    /// The content rows, each `size().0` wide (`list::padded` adds the pad
    /// cells, the painter the frame): title "October 2026" centred BOLD_ACCENT, weekday
    /// initials MUTED rotated to first_day, days NORMAL, today ACTIVE (on accent_container) (like
    /// the active workspace), days of other months blank, ISO week numbers
    /// MUTED on the left — the week of the row's Monday (question 5), on rows
    /// holding a day of the month.
    pub fn render(&self) -> Vec<Grid> {
        // `shown` and `first_day` are public: a caller (or a test) may set
        // them to anything representable, so render() normalises its own
        // copies rather than trusting them — an out-of-range month would
        // index MONTHS out of bounds, an out-of-range first_day would
        // overflow the u8 arithmetic below.
        let shown = Month { year: self.shown.year, month: self.shown.month.clamp(1, 12) };
        let first_day = self.first_day % 7;
        let (width, _) = self.size();
        let left = if self.week_numbers { WEEK_CELLS } else { 0 };
        let weeks = self.weeks();
        let mut rows = Vec::with_capacity(2 + weeks);
        let mut title = Grid::empty(width);
        let text = format!("{} {}", MONTHS[usize::from(shown.month) - 1], shown.year);
        title.write(width.saturating_sub(text_width(&text)) / 2, width, &text, Style::BOLD_ACCENT);
        rows.push(title);
        let mut weekdays = Grid::empty(width);
        for c in 0..7u16 {
            let day = usize::from((first_day + c as u8) % 7);
            weekdays.write(left + c * COLUMN, width, &SHORT_DAYS[day][..2], Style::MUTED);
        }
        rows.push(weekdays);
        let first = days_from_civil(shown.year, shown.month, 1);
        // Cells before the 1st: how far it lies from the week's first day.
        let offset = i64::from((shown.first_weekday() + 7 - first_day) % 7);
        let monday_column = i64::from((MONDAY + 7 - first_day) % 7);
        let days = i64::from(shown.days());
        for r in 0..weeks as i64 {
            let mut g = Grid::empty(width);
            let row_start = r * 7 - offset; // days after the 1st of the row's first cell
            if self.week_numbers && row_start + 6 >= 0 && row_start < days {
                let (y, m, d) = civil_from_days(first + row_start + monday_column);
                g.write(0, DAY_CELLS, &format!("{:>2}", iso_week(y, m, d)), Style::MUTED);
            }
            for c in 0..7i64 {
                let n = row_start + c + 1;
                if !(1..=days).contains(&n) {
                    continue;
                }
                let is_today = (shown.year, shown.month, n as u8) == self.today;
                let style = if is_today { Style::ACTIVE } else { Style::NORMAL };
                let x = left + c as u16 * COLUMN;
                g.write(x, x + DAY_CELLS, &format!("{n:>2}"), style);
            }
            rows.push(g);
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monday 5 October 2026.
    const TODAY: LocalTime = LocalTime { year: 2026, month: 10, day: 5, weekday: 1, hour: 9, minute: 30, second: 0 };
    const SUNDAY: u8 = 0;

    fn snapshot(rows: &[Grid]) -> String {
        rows.iter().map(|g| format!("{}\n{}", g.text(), g.styles())).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn october_2026_monday_first() {
        let rows = Calendar::new(&TODAY, MONDAY, false).render();
        assert_eq!(rows.len(), 7, "title, weekdays, five weeks: October 2026 needs no sixth");
        assert!(rows.iter().all(|g| g.width() == 20));
        assert_eq!(rows[0].text(), "    October 2026    ");
        assert_eq!(rows[1].text(), "Mo Tu We Th Fr Sa Su");
        assert_eq!(rows[2].text(), "          1  2  3  4", "1 October 2026 is a Thursday");
        assert_eq!(rows[3].text(), " 5  6  7  8  9 10 11");
        assert_eq!(&rows[3].styles()[..3], "AA.", "today inverted, its two cells");
        assert_eq!(rows[6].text(), "26 27 28 29 30 31   ", "the last week");
        insta::assert_snapshot!("october_2026_monday_first", snapshot(&rows));
    }

    #[test]
    fn october_2026_sunday_first() {
        let rows = Calendar::new(&TODAY, SUNDAY, false).render();
        assert_eq!(rows[1].text(), "Su Mo Tu We Th Fr Sa");
        assert_eq!(rows[2].text(), "             1  2  3");
        assert_eq!(rows[3].text(), " 4  5  6  7  8  9 10");
        insta::assert_snapshot!("october_2026_sunday_first", snapshot(&rows));
    }

    #[test]
    fn week_numbers_column() {
        let c = Calendar::new(&TODAY, MONDAY, true);
        assert_eq!(c.size(), (23, 7));
        let rows = c.render();
        assert_eq!(rows[1].text(), "   Mo Tu We Th Fr Sa Su");
        assert_eq!(rows[2].text(), "40           1  2  3  4");
        assert_eq!(&rows[2].styles()[..2], "mm", "muted");
        assert_eq!(rows[6].text(), "44 26 27 28 29 30 31   ");
        assert_eq!(rows.len(), 7, "no row without a day of the month");
        // Sunday first: the row of Sunday 4 October shows the week of Monday 5 (41).
        let rows = Calendar::new(&TODAY, SUNDAY, true).render();
        assert_eq!(rows[2].text(), "40              1  2  3");
        assert_eq!(rows[3].text(), "41  4  5  6  7  8  9 10");
        insta::assert_snapshot!("week_numbers_column", snapshot(&Calendar::new(&TODAY, MONDAY, true).render()));
    }

    #[test]
    fn today_inverted_only_in_its_month() {
        let mut c = Calendar::new(&TODAY, MONDAY, false);
        let inverted = |c: &Calendar| c.render().iter().map(|g| g.styles().matches('A').count()).sum::<usize>();
        assert_eq!(inverted(&c), 2);
        assert!(c.scroll(12));
        assert_eq!((c.shown.year, c.shown.month), (2027, 10));
        assert_eq!(inverted(&c), 0, "the 5th of October 2027 is not today");
        assert!(c.scroll(-11));
        assert_eq!(inverted(&c), 0, "November 2026 neither");
        assert!(c.scroll(-1));
        assert_eq!(inverted(&c), 2);
        insta::assert_snapshot!(
            "today_inverted_only_in_its_month",
            snapshot(&{
                let mut next = Calendar::new(&TODAY, MONDAY, false);
                next.scroll(1);
                next.render()
            })
        );
    }

    /// The height follows the shown month: 4 weeks (February 2027 starts on a Monday, Monday first),
    /// 5 (October 2026), 6 (August 2026); `size` and `render` agree, whatever the first day.
    #[test]
    fn the_height_follows_the_weeks_of_the_month() {
        for (months, first_day, weeks) in [(4, MONDAY, 4), (4, SUNDAY, 5), (0, MONDAY, 5), (0, SUNDAY, 5), (-2, MONDAY, 6), (-2, SUNDAY, 6)]
        {
            let mut c = Calendar::new(&TODAY, first_day, true);
            c.scroll(months);
            assert_eq!(c.size().1, 2 + weeks, "{months:+} months, first day {first_day}");
            assert_eq!(c.render().len(), usize::from(c.size().1), "render and size agree");
        }
        let mut feb = Calendar::new(&TODAY, MONDAY, false);
        feb.scroll(4);
        let rows = feb.render();
        assert_eq!(rows[0].text(), "   February 2027    ");
        assert_eq!(rows[5].text(), "22 23 24 25 26 27 28", "the fourth and last week");
        assert_eq!(rows.len(), 6);
        insta::assert_snapshot!("february_2027_four_weeks", snapshot(&rows));
    }

    #[test]
    fn a_month_spanning_six_weeks() {
        // August 2026: Saturday the 1st, 31 days, Monday first: six week rows.
        let mut c = Calendar::new(&TODAY, MONDAY, true);
        c.scroll(-2);
        let rows = c.render();
        assert_eq!(rows[0].text(), "      August 2026      ");
        assert_eq!(rows[2].text(), "31                 1  2", "1 August 2026: ISO week 31");
        assert_eq!(rows.len(), 8, "title, weekdays, six weeks");
        assert_eq!(rows[7].text(), "36 31                  ", "Monday the 31st, alone on the sixth row");
        insta::assert_snapshot!("a_month_spanning_six_weeks", snapshot(&rows));
    }

    #[test]
    fn iso_week_edges() {
        assert_eq!(iso_week(2021, 1, 1), 53, "a Friday: the last week of 2020");
        assert_eq!(iso_week(2024, 12, 31), 1, "a Tuesday: the first week of 2025");
        assert_eq!(iso_week(2026, 12, 28), 53, "2026 starts on a Thursday: 53 weeks");
        assert_eq!(iso_week(2026, 1, 1), 1);
        assert_eq!(iso_week(2026, 10, 5), 41);
        assert_eq!(iso_week(2027, 1, 3), 53, "a Sunday: still 2026's last week");
        assert_eq!(iso_week(2027, 1, 4), 1, "the next Monday");
        assert_eq!(iso_week(2022, 1, 1), 52, "a Saturday: still 2021's last week");
        assert_eq!(iso_week(2020, 12, 31), 53, "a Thursday: 2020 has 53 weeks");
        assert_eq!(iso_week(2032, 12, 31), 53, "a Friday: still week 53");
    }

    #[test]
    fn months_scroll_across_years() {
        let dec = Month { year: 2026, month: 12 };
        assert_eq!(dec.next(), Month { year: 2027, month: 1 });
        assert_eq!(Month { year: 2026, month: 1 }.previous(), Month { year: 2025, month: 12 });
        let mut c = Calendar::new(&TODAY, MONDAY, false);
        assert!(c.scroll(27));
        assert_eq!(c.shown, Month { year: 2029, month: 1 });
        assert!(c.scroll(-40));
        assert_eq!(c.shown, Month { year: 2025, month: 9 });
        assert!(!c.scroll(0), "no move: nothing to repaint");
        assert_eq!(Month::of(&TODAY).first_weekday(), 4, "1 October 2026: a Thursday");
    }

    #[test]
    fn render_normalises_out_of_range_fields() {
        // `shown.month` and `first_day` are public fields: a caller setting
        // them directly to something out of the documented range must not
        // make render() panic (MONTHS index, u8 overflow in the rotation
        // arithmetic).
        let mut c = Calendar::new(&TODAY, MONDAY, false);
        c.shown.month = 0;
        c.first_day = 255;
        c.render();
        c.shown.month = 13;
        c.first_day = 200;
        c.render();
    }

    #[test]
    fn february_in_leap_years() {
        let feb = |year| Month { year, month: 2 }.days();
        assert_eq!((feb(2024), feb(2026), feb(1900), feb(2000), feb(2100)), (29, 28, 28, 29, 28));
        assert_eq!((Month { year: 2026, month: 4 }.days(), Month { year: 2026, month: 12 }.days()), (30, 31));
    }

    #[test]
    fn first_day_from_known_locales() {
        // glibc's locales all say 19971130 (a Sunday) then count from 1.
        assert_eq!(first_day_from(19971130, 1), SUNDAY, "en_US");
        assert_eq!(first_day_from(19971130, 2), MONDAY, "fr_FR");
        assert_eq!(first_day_from(19971130, 7), 6, "Saturday");
        assert_eq!(first_day_from(19971201, 1), MONDAY, "another known day: 1 December 1997 was a Monday");
        for (week_1stday, first_weekday) in [(19971130, 0), (19971130, 8), (19971332, 1), (0, 1)] {
            assert_eq!(first_day_from(week_1stday, first_weekday), MONDAY, "unreadable {week_1stday} {first_weekday}: Monday");
        }
        assert!(is_c_locale("C") && is_c_locale("POSIX") && is_c_locale("C.UTF-8"));
        assert!(!is_c_locale("fr_FR.UTF-8") && !is_c_locale("en_US.UTF-8") && !is_c_locale("ca_ES.UTF-8"));
    }
}
