//! Timestamps. Session records carry one fixed UTC shape,
//! `2026-10-22T20:31:05.123Z`, so they're parsed by hand; showing them in
//! local time asks the C library on Unix, and stays in UTC elsewhere.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, from `YYYY-MM-DDTHH:MM:SS…`. Anything after
/// the seconds (fractions, `Z`) is ignored; anything malformed is `None`.
pub fn parse(stamp: &str) -> Option<i64> {
    let b = stamp.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let digits = stamp.get(from..to)?;
        digits
            .bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// A moment broken down for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

impl Local {
    /// `2026-10-22`
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// `20:31`
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    fn same_day(&self, other: &Local) -> bool {
        (self.year, self.month, self.day) == (other.year, other.month, other.day)
    }
}

/// `secs` in the machine's time zone.
pub fn local(secs: i64) -> Local {
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes the struct it's handed.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return Local {
                year: i64::from(tm.tm_year) + 1900,
                month: (tm.tm_mon + 1) as u32,
                day: tm.tm_mday as u32,
                hour: tm.tm_hour as u32,
                minute: tm.tm_min as u32,
            };
        }
    }
    utc(secs)
}

pub fn utc(secs: i64) -> Local {
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rest = secs.rem_euclid(86_400);
    Local {
        year,
        month,
        day,
        hour: (rest / 3600) as u32,
        minute: (rest % 3600 / 60) as u32,
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Short and relative to `now`: `20:31` today, `10-21` this year, else
/// `2025-10-21`.
pub fn short(secs: i64, now: i64) -> String {
    let (at, today) = (local(secs), local(now));
    if at.same_day(&today) {
        at.clock()
    } else if at.year == today.year {
        format!("{:02}-{:02}", at.month, at.day)
    } else {
        at.date()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_record_shape() {
        assert_eq!(parse("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse("2026-10-22T20:31:05.123Z"), Some(1_792_701_065));
        assert_eq!(parse("2000-02-29T12:00:00Z"), Some(951_825_600));
        // No fraction, no zone: still the same second.
        assert_eq!(parse("2026-10-22T20:31:05"), Some(1_792_701_065));
    }

    #[test]
    fn rejects_what_isnt_a_timestamp() {
        for bad in [
            "",
            "yesterday",
            "2026-10-22",
            "2026-13-01T00:00:00Z",
            "2026-10-22 20:31:05",
            "2026-1a-22T20:31:05Z",
            "2026-10-22T2:31:05.1Z",
            "２０２６-10-22T20:31:05Z",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn civil_round_trip() {
        for days in [-800_000, -1, 0, 1, 11_016, 20_748, 1_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, i64::from(m), i64::from(d)), days);
        }
    }

    #[test]
    fn utc_breakdown() {
        let t = utc(1_792_701_065);
        assert_eq!((t.date(), t.clock()), ("2026-10-22".into(), "20:31".into()));
    }

    #[test]
    fn short_dates_drop_what_is_obvious() {
        let now = parse("2026-10-22T12:00:00Z").unwrap();
        // Days apart, so time zones can't move them onto the same day.
        let this_year = parse("2026-03-01T12:00:00Z").unwrap();
        let last_year = parse("2025-06-01T12:00:00Z").unwrap();
        assert_eq!(short(this_year, now), "03-01");
        assert_eq!(short(last_year, now), "2025-06-01");
        assert_eq!(short(now, now), local(now).clock());
    }
}
