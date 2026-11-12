// SPDX-License-Identifier: MIT
//! Proleptic Gregorian calendar arithmetic on plain integers.
//!
//! All functions accept out-of-range fields (month 13, day 0, hour -3, ...)
//! and normalise them the way `timegm()` does, which is what the date parser
//! relies on ("Feb 31" is "Mar 3", "0 days after Jan 1" is "Dec 31").

/// Days since 1970-01-01 for a normalised year/month (1-12)/day (1-31).
fn days_from_civil_norm(y: i64, m: i64, d: i64) -> i64 {
    // Shift the year so that it starts in March; February is then the last
    // month and leap days fall at the end of a cycle.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // March = 0
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Days since 1970-01-01. `mon0` is 0-based and may be out of range, as may `mday`.
pub fn days_from_civil(year: i64, mon0: i64, mday: i64) -> i64 {
    let y = year + mon0.div_euclid(12);
    let m = mon0.rem_euclid(12) + 1;
    days_from_civil_norm(y, m, 1) + (mday - 1)
}

/// Inverse of [`days_from_civil`]: (year, month 1-12, day 1-31).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Seconds since the epoch for broken-down UTC fields (any may be out of range).
pub fn timegm(year: i64, mon0: i64, mday: i64, hour: i64, min: i64, sec: i64) -> i64 {
    days_from_civil(year, mon0, mday) * 86400 + hour * 3600 + min * 60 + sec
}

/// Broken-down time, like `struct tm` but with a full year and 1-based month.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tm {
    pub year: i64,
    /// 1..=12
    pub mon: i64,
    /// 1..=31
    pub mday: i64,
    pub hour: i64,
    pub min: i64,
    pub sec: i64,
    /// 0 = Sunday
    pub wday: i64,
    /// 0-based day of the year
    pub yday: i64,
}

impl Tm {
    /// Break down seconds since the epoch (already shifted into the wanted zone).
    pub fn from_secs(t: i64) -> Tm {
        let days = t.div_euclid(86400);
        let rem = t.rem_euclid(86400);
        let (year, mon, mday) = civil_from_days(days);
        Tm {
            year,
            mon,
            mday,
            hour: rem / 3600,
            min: rem % 3600 / 60,
            sec: rem % 60,
            wday: (days + 4).rem_euclid(7),
            yday: days - days_from_civil(year, 0, 1),
        }
    }
}

pub const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

pub const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub fn weekday_abbr(wday: i64) -> &'static str {
    &WEEKDAY_NAMES[wday.rem_euclid(7) as usize][..3]
}

pub fn month_abbr(mon: i64) -> &'static str {
    &MONTH_NAMES[(mon - 1).rem_euclid(12) as usize][..3]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for z in -800_000..800_000 {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m - 1, d), z);
        }
    }

    #[test]
    fn normalises() {
        assert_eq!(days_from_civil(2009, 1, 31), days_from_civil(2009, 2, 3));
        assert_eq!(days_from_civil(2009, 0, 0), days_from_civil(2008, 11, 31));
        assert_eq!(days_from_civil(2009, -2, 30), days_from_civil(2008, 10, 30));
        assert_eq!(timegm(1970, 0, 1, 0, 0, 0), 0);
        assert_eq!(timegm(2009, 7, 30, 19, 20, 0), 1251660000);
    }

    #[test]
    fn breakdown() {
        let tm = Tm::from_secs(1251660000);
        assert_eq!((tm.year, tm.mon, tm.mday), (2009, 8, 30));
        assert_eq!((tm.hour, tm.min, tm.sec, tm.wday), (19, 20, 0, 0));
        assert_eq!(Tm::from_secs(0).wday, 4);
    }
}
