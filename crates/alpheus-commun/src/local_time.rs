//! The next instant the local clock reads a given time: how `alpheus-launch --dnd on --until
//! HH:MM` turns a wall-clock time into a Unix time the daemon needs no time zone for.

// The `libc` crate only declares `tzset` for Windows; on Unix it is a standard C function
// (time.h), already linked through the system's C library (as in the daemon's `clock.rs`).
unsafe extern "C" {
    fn tzset();
}

/// The first instant after `now` (Unix seconds) at which the local clock reads `hour:minute:00`:
/// today when still ahead, else tomorrow. None for an hour above 23, a minute above 59, or when
/// the C library cannot place the time.
///
/// The day is bumped through `tm_mday` and `mktime` (`tm_isdst = -1`, so it picks the zone's
/// rule), never by adding 86,400 s: that stays right across a DST change. A time that a spring
/// change skips lands an hour later, the C library's reading of it.
pub fn next_local(hour: u32, minute: u32, now: i64) -> Option<i64> {
    if hour > 23 || minute > 59 {
        return None;
    }
    // Like `clock.rs`: re-read TZ, so a zone change during a long-lived process is seen.
    // SAFETY: `tzset` takes no argument and only refreshes libc's own zone state.
    unsafe { tzset() };
    // SAFETY: tm is a plain C struct, valid when zero-filled; localtime_r writes into it and keeps
    // no pointer.
    let mut today: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `now` and `today` live for the duration of the call.
    if unsafe { libc::localtime_r(&now, &mut today) }.is_null() {
        return None;
    }
    today.tm_hour = hour as i32;
    today.tm_min = minute as i32;
    today.tm_sec = 0;
    today.tm_isdst = -1;
    for day in 0..2 {
        let mut tm = today;
        tm.tm_mday += day;
        // SAFETY: `tm` is a valid, initialised struct; mktime normalises it in place.
        let t = unsafe { libc::mktime(&mut tm) };
        if t == -1 {
            return None;
        }
        if t > now {
            return Some(t);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
    }

    /// `(hour, minute, second)` the local clock reads at `t`.
    fn local(t: i64) -> (u32, u32, u32) {
        // SAFETY: tm is a plain C struct, valid when zero-filled; localtime_r keeps no pointer.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `t` and `tm` live for the duration of the call.
        assert!(!unsafe { libc::localtime_r(&t, &mut tm) }.is_null());
        (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }

    /// Holds in any time zone: the result is ahead, within a day (and an hour of DST), and reads
    /// the asked time; one minute ahead of now is at most two minutes away, one behind is
    /// tomorrow's.
    #[test]
    fn next_local_is_ahead_and_reads_that_local_time() {
        let now = now();
        let (h, m, _) = local(now);
        let minute_of_day = h * 60 + m;
        for (delta, soon) in [(1u32, true), (1439, false)] {
            let at = (minute_of_day + delta) % 1440;
            let (th, tm) = (at / 60, at % 60);
            let r = next_local(th, tm, now).expect("a valid time");
            assert!(now < r && r <= now + 86_400 + 3_600, "{delta}: {} s ahead", r - now);
            assert_eq!(local(r), (th, tm, 0), "{delta}");
            if soon {
                assert!(r - now <= 120, "one minute ahead is soon: {} s", r - now);
            } else {
                assert!(r - now > 82_800, "one minute behind is tomorrow: {} s", r - now);
            }
        }
    }

    #[test]
    fn next_local_refuses_out_of_range() {
        let now = now();
        assert_eq!(next_local(24, 0, now), None);
        assert_eq!(next_local(0, 60, now), None);
    }
}
