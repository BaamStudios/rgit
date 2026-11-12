// SPDX-License-Identifier: MIT
//! Input side: the strict date parser (`GIT_AUTHOR_DATE`, `--date` of
//! `commit`) and the fuzzy "approxidate" parser (`--since`, `@{<date>}`,
//! expiry settings).
//!
//! Clean-room note: the rules below were derived only from
//! `Documentation/date-formats.adoc`, git's test expectations (t0006) and
//! black-box experiments with the `git` binary (`git var`, `git rev-parse
//! --since=`, `git config --type=expiry-date`); `tests/differential.rs`
//! keeps comparing against the binary.
//!
//! Model of the fuzzy parser, as observed:
//! * Fields (year, month, day, hour, minute, second) start unknown; unknown
//!   ones are taken from "now" (local time) at the end. A month without a
//!   year that has not come yet this year means last year.
//! * A number is first kept "pending": a following unit (`3 days`), weekday
//!   (`2 friday` = the second Friday back) or `am`/`pm` uses it as a count or
//!   hour; otherwise it becomes the day, then the month, then the year.
//!   `last` and `one`..`ten` work the same way.
//! * Seconds/minutes/hours/days/weeks, weekdays, `yesterday` and `never`
//!   recompute the whole date from an instant; months and years only change
//!   their fields. The weekday used by "last friday" and the summer time flag
//!   are only refreshed by such a recomputation.
//! * Numeric dates (`08/15/2009`, `15.08.2009`, `2009-08-15`) more than ten
//!   days in the future are rejected and their numbers read one by one.
//!
//! Inputs where this model is known to differ from git are listed in the PR
//! of issue #74 (mostly random token soup, not dates people write).

use crate::DateError;
use crate::civil::{MONTH_NAMES, Tm, WEEKDAY_NAMES, timegm};
use crate::context::{DateContext, secs_to_tz};

/// First timestamp that the strict parser rejects (2100-01-01T00:00:00Z);
/// also the upper bound for bare (non-`@`) epoch numbers.
const YEAR_2100: i64 = 4102444800;

/// Numeric time zones and well-known zone abbreviations (offset in hours,
/// whether the name denotes summer time, which adds an hour).
const ZONE_NAMES: &[(&str, i64, bool)] = &[
    ("IDLW", -12, false),
    ("NT", -11, false),
    ("CAT", -10, false),
    ("HST", -10, false),
    ("HDT", -10, true),
    ("YST", -9, false),
    ("YDT", -9, true),
    ("PST", -8, false),
    ("PDT", -8, true),
    ("MST", -7, false),
    ("MDT", -7, true),
    ("CST", -6, false),
    ("CDT", -6, true),
    ("EST", -5, false),
    ("EDT", -5, true),
    ("AST", -3, false),
    ("ADT", -3, true),
    ("WAT", -1, false),
    ("GMT", 0, false),
    ("UTC", 0, false),
    ("Z", 0, false),
    ("WET", 0, false),
    ("BST", 0, true),
    ("CET", 1, false),
    ("MET", 1, false),
    ("MEWT", 1, false),
    ("MEST", 1, true),
    ("CEST", 1, true),
    ("MESZ", 1, true),
    ("FWT", 1, false),
    ("FST", 1, true),
    ("EET", 2, false),
    ("EEST", 2, true),
    ("WAST", 7, false),
    ("WADT", 7, true),
    ("CCT", 8, false),
    ("JST", 9, false),
    ("EAST", 10, false),
    ("EADT", 10, true),
    ("GST", 10, false),
    ("NZT", 12, false),
    ("NZST", 12, false),
    ("NZDT", 12, true),
    ("IDLE", 12, false),
];

fn zone_minutes(word: &str) -> Option<i64> {
    ZONE_NAMES
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(word))
        .map(|&(_, h, dst)| (h + dst as i64) * 60)
}

/// Month 1..=12 for a month name or a prefix of at least three letters.
fn month_word(word: &str) -> Option<i64> {
    if word.len() < 3 {
        return None;
    }
    MONTH_NAMES
        .iter()
        .position(|m| m.len() >= word.len() && m[..word.len()].eq_ignore_ascii_case(word))
        .map(|i| i as i64 + 1)
}

/// Weekday 0..=6 (Sunday = 0) for a name, a prefix of at least three
/// letters, or the plural ("fridays").
fn weekday_word(word: &str) -> Option<i64> {
    if word.len() < 3 {
        return None;
    }
    WEEKDAY_NAMES
        .iter()
        .position(|d| {
            (d.len() >= word.len() && d[..word.len()].eq_ignore_ascii_case(word))
                || (word.len() == d.len() + 1
                    && word[..d.len()].eq_ignore_ascii_case(d)
                    && word.as_bytes()[d.len()].eq_ignore_ascii_case(&b's'))
        })
        .map(|i| i as i64)
}

fn number_word(word: &str) -> Option<i64> {
    const WORDS: [&str; 11] = [
        "last", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    WORDS
        .iter()
        .position(|w| w.eq_ignore_ascii_case(word))
        .map(|i| if i == 0 { 1 } else { i as i64 })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

fn unit_word(word: &str) -> Option<Unit> {
    const UNITS: [(&str, Unit); 7] = [
        ("second", Unit::Second),
        ("minute", Unit::Minute),
        ("hour", Unit::Hour),
        ("day", Unit::Day),
        ("week", Unit::Week),
        ("month", Unit::Month),
        ("year", Unit::Year),
    ];
    let lower = word.to_ascii_lowercase();
    let singular = lower.strip_suffix('s').unwrap_or(&lower);
    UNITS
        .iter()
        .find(|(name, _)| *name == singular)
        .map(|&(_, u)| u)
}

/// Two-digit year as the strict parser reads it (`05` -> 2005, `99` -> 1999).
fn strict_two_digit_year(n: i64, len: usize) -> Option<i64> {
    if n >= 70 {
        Some(1900 + n)
    } else if len == 2 && n < 10 {
        Some(2000 + n)
    } else {
        None
    }
}

/// Year as written in a numeric date (`1.2.3`, `08/15/09`, `2009`).
fn date_year(n: i64, len: usize) -> Option<i64> {
    if len >= 3 {
        return (1970..2100).contains(&n).then_some(n);
    }
    match n {
        0..38 => Some(2000 + n),
        70.. => Some(1900 + n),
        _ => None,
    }
}

/// Fields of a date being assembled. `None` means "not given".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Fields {
    year: Option<i64>,
    /// 1..=12 (may go out of range through arithmetic)
    mon: Option<i64>,
    mday: Option<i64>,
    hour: Option<i64>,
    min: Option<i64>,
    sec: Option<i64>,
}

impl Fields {
    fn set_all(&mut self, tm: &Tm) {
        *self = Fields {
            year: Some(tm.year),
            mon: Some(tm.mon),
            mday: Some(tm.mday),
            hour: Some(tm.hour),
            min: Some(tm.min),
            sec: Some(tm.sec),
        };
    }

    /// Naive (zone-less) seconds, unset fields taken from `now`.
    fn naive(&self, now: &Tm) -> i64 {
        timegm(
            self.year.unwrap_or(default_year(self.mon, now)),
            self.mon.unwrap_or(now.mon) - 1,
            self.mday.unwrap_or(now.mday),
            self.hour.unwrap_or(now.hour),
            self.min.unwrap_or(now.min),
            self.sec.unwrap_or(now.sec),
        )
    }
}

/// The year assumed when only a month is known: the most recent such month,
/// i.e. last year for a month that has not come yet this year.
fn default_year(mon: Option<i64>, now: &Tm) -> i64 {
    match mon {
        Some(m) if m > now.mon => now.year - 1,
        _ => now.year,
    }
}

/// Byte cursor over the input.
struct Scanner<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn peek(&self, off: usize) -> Option<u8> {
        self.s.get(self.pos + off).copied()
    }

    fn digits_at(&self, at: usize) -> usize {
        self.s[at.min(self.s.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    }

    fn number_at(&self, at: usize, len: usize) -> i64 {
        self.s[at..at + len].iter().fold(0i64, |acc, &c| {
            acc.saturating_mul(10).saturating_add((c - b'0') as i64)
        })
    }
}

enum TzMatch {
    /// Minutes east of UTC, bytes consumed.
    Valid(i64, usize),
    /// Looks like an offset but is not a valid one (length in bytes).
    Invalid(usize),
    /// Not an offset at all (e.g. the `-12-32` of a date); skip the sign only.
    NotTz,
}

/// Parse `+hh`, `+hhmm` or `+hh:mm` at `pos` (which holds the sign).
fn match_tz(sc: &Scanner<'_>, pos: usize, allow_colon: bool) -> TzMatch {
    let sign = if sc.s[pos] == b'-' { -1 } else { 1 };
    let start = pos + 1;
    let len = sc.digits_at(start);
    let mut end = start + len;
    let mut colon_ok = true;
    if sc.s.get(end) == Some(&b':') && sc.digits_at(end + 1) > 0 {
        colon_ok = allow_colon && len == 2 && sc.digits_at(end + 1) == 2;
        end += 1 + sc.digits_at(end + 1);
    }
    // "-12-32" is a date fragment, not an offset.
    if matches!(sc.s.get(end), Some(b'-' | b'/' | b'.'))
        && sc.s.get(end + 1).is_some_and(u8::is_ascii_digit)
    {
        return TzMatch::NotTz;
    }
    let used = end - pos;
    let (h, m) = match (len, colon_ok) {
        (4, true) if end == start + 4 => (sc.number_at(start, 2), sc.number_at(start + 2, 2)),
        (2, true) if end == start + 2 => (sc.number_at(start, 2), 0),
        (2, true) => (sc.number_at(start, 2), sc.number_at(start + 3, 2)),
        _ => return TzMatch::Invalid(used),
    };
    if h >= 24 || m >= 60 {
        return TzMatch::Invalid(used);
    }
    TzMatch::Valid(sign * (h * 60 + m), used)
}

/// Parse `H:M[:S[.fraction]]` at `pos`. Returns (h, m, s, bytes consumed).
fn match_time(sc: &Scanner<'_>, pos: usize) -> Option<(i64, i64, i64, usize)> {
    let hl = sc.digits_at(pos);
    if hl == 0 || hl > 2 || sc.s.get(pos + hl) != Some(&b':') {
        return None;
    }
    let h = sc.number_at(pos, hl);
    let mpos = pos + hl + 1;
    let ml = sc.digits_at(mpos);
    if ml == 0 || ml > 2 {
        return None;
    }
    let m = sc.number_at(mpos, ml);
    let mut end = mpos + ml;
    let mut s = 0;
    if sc.s.get(end) == Some(&b':') && sc.digits_at(end + 1) > 0 && sc.digits_at(end + 1) <= 2 {
        let sl = sc.digits_at(end + 1);
        s = sc.number_at(end + 1, sl);
        end += 1 + sl;
    }
    if h > 24 || m >= 60 || s > 60 {
        return None;
    }
    Some((h, m, s, end - pos))
}

/// ISO 8601 basic time after a `T`: `hh`, `hhmm`, `hhmmss[.frac]`.
fn match_basic_time(sc: &Scanner<'_>, pos: usize) -> Option<(i64, i64, i64, usize)> {
    if let Some(t) = match_time(sc, pos) {
        return Some(t);
    }
    let len = sc.digits_at(pos);
    let (h, m, s) = match len {
        2 => (sc.number_at(pos, 2), 0, 0),
        4 => (sc.number_at(pos, 2), sc.number_at(pos + 2, 2), 0),
        6 => (
            sc.number_at(pos, 2),
            sc.number_at(pos + 2, 2),
            sc.number_at(pos + 4, 2),
        ),
        _ => return None,
    };
    let mut end = pos + len;
    if sc.s.get(end) == Some(&b'.') && sc.digits_at(end + 1) > 0 {
        end += 1 + sc.digits_at(end + 1);
    }
    if h > 24 || m >= 60 || s > 60 {
        return None;
    }
    Some((h, m, s, end - pos))
}

/// Step over a number that is used on its own; a date separator directly
/// after it is skipped too, so that `2009-05` reads as `2009` and `05`
/// rather than a year followed by the offset `-05`.
fn skip_number(sc: &mut Scanner<'_>, len: usize) {
    sc.pos += len;
    if matches!(len, 1 | 2 | 4)
        && matches!(sc.peek(0), Some(b'-' | b'/' | b'.'))
        && sc.peek(1).is_some_and(|c| c.is_ascii_digit())
    {
        sc.pos += 1;
    }
}

/// Days since the epoch of a (possibly unnormalised) date.
fn day_number(year: i64, mon: i64, mday: i64) -> i64 {
    crate::civil::days_from_civil(year, mon - 1, mday)
}

/// A multi-number date like `08/15/2009`, `15.08.09`, `2009-08-15`, `8/15`.
/// Returns (year, mon, mday, bytes consumed) or `None` when the group is
/// not an acceptable date. Depending on `check`, dates more than ten days
/// after `now` are not acceptable either (the numbers are then read one by
/// one).
fn match_numeric_date(
    sc: &Scanner<'_>,
    pos: usize,
    now: &Tm,
    check: FutureCheck,
) -> Option<(Option<i64>, i64, i64, usize)> {
    let sep = *sc.s.get(pos + sc.digits_at(pos))?;
    let mut parts: Vec<(i64, usize)> = Vec::new();
    let mut at = pos;
    loop {
        let len = sc.digits_at(at);
        if len == 0 {
            break;
        }
        parts.push((sc.number_at(at, len), len));
        at += len;
        if parts.len() == 3 || sc.s.get(at) != Some(&sep) || sc.digits_at(at + 1) == 0 {
            break;
        }
        at += 1;
    }
    if parts.len() < 2 {
        return None;
    }
    let consumed = at - pos;
    let valid = |y: Option<i64>, m: i64, d: i64, month_first: bool| -> bool {
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return false;
        }
        let checked = match check {
            FutureCheck::Never => false,
            FutureCheck::MonthFirst => month_first,
            FutureCheck::Always => true,
        };
        // Not more than ten days in the future.
        let y = y.unwrap_or(now.year);
        !checked || day_number(y, m, d) <= day_number(now.year, now.mon, now.mday) + 10
    };
    let (a, alen) = parts[0];
    if alen == 3 || alen > 4 {
        return None;
    }
    let (b, _) = parts[1];
    if alen == 4 {
        // Year first: YYYY-MM-DD in any separator.
        let (c, _) = *parts.get(2)?;
        let year = date_year(a, alen)?;
        if (1..=12).contains(&b) && (1..=31).contains(&c) {
            return Some((Some(year), b, c, consumed));
        }
        return None;
    }
    if a > 31 {
        // Two-digit year first (YY-MM-DD).
        let (c, _) = *parts.get(2)?;
        let year = date_year(a, alen)?;
        if year > 1970 && valid(Some(year), b, c, true) {
            return Some((Some(year), b, c, consumed));
        }
        return None;
    }
    let year = match parts.get(2) {
        Some(&(c, clen)) => Some(date_year(c, clen)?),
        None => None,
    };
    let orders: [(i64, i64); 2] = if sep == b'.' {
        [(b, a), (a, b)] // day.month first
    } else {
        [(a, b), (b, a)] // month/day first
    };
    for (m, d) in orders {
        if valid(year, m, d, m == a) {
            return Some((year, m, d, consumed));
        }
    }
    None
}

/// Which readings of a numeric date must not lie in the future.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FutureCheck {
    Never,
    /// Only the reading with the month first (`M/D`); `D/M` is accepted as is.
    MonthFirst,
    Always,
}

// ---------------------------------------------------------------------------
// Strict parser
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Strict {
    f: Fields,
    tz: Option<i64>,
    tz_numeric: bool,
    /// An epoch timestamp and the fields it expanded to.
    raw: Option<(i64, Fields)>,
}

impl Strict {
    /// An epoch timestamp counts only before any other date or time; its
    /// broken-down UTC fields can still be overridden by later tokens.
    fn set_raw(&mut self, n: i64) {
        if self.raw.is_some() || self.f != Fields::default() {
            return;
        }
        self.f.set_all(&Tm::from_secs(n));
        self.raw = Some((n, self.f));
    }

    fn number(&mut self, sc: &mut Scanner<'_>, now: &Tm) {
        let pos = sc.pos;
        let len = sc.digits_at(pos);
        let n = sc.number_at(pos, len);
        let next = sc.s.get(pos + len).copied();

        if let Some((h, m, s, used)) = match_time(sc, pos) {
            self.f.hour = Some(h);
            self.f.min = Some(m);
            self.f.sec = Some(s);
            sc.pos += used;
            return;
        }
        if len == 8 && next == Some(b'T') {
            // ISO 8601 basic date, the time follows the T.
            let (y, m, d) = (n / 10000, n / 100 % 100, n % 100);
            if (1..=12).contains(&m) && (1..=31).contains(&d) && (1970..2100).contains(&y) {
                self.f.year = Some(y);
                self.f.mon = Some(m);
                self.f.mday = Some(d);
            }
            sc.pos += len;
            return;
        }
        if matches!(len, 1 | 2 | 4)
            && matches!(next, Some(b'-' | b'/' | b'.'))
            && sc.digits_at(pos + len + 1) > 0
        {
            if let Some((y, m, d, used)) = match_numeric_date(
                sc,
                pos,
                now,
                if self.f.hour.is_some() {
                    FutureCheck::MonthFirst
                } else {
                    FutureCheck::Never
                },
            ) {
                if let Some(y) = y {
                    self.f.year = Some(y);
                }
                self.f.mon = Some(m);
                self.f.mday = Some(d);
                sc.pos += used;
                return;
            }
            // An invalid numeric date is ignored as a whole.
            let sep = sc.s[pos + len];
            let mut at = pos + len;
            for _ in 0..2 {
                if sc.s.get(at) != Some(&sep) || sc.digits_at(at + 1) == 0 {
                    break;
                }
                at += 1 + sc.digits_at(at + 1);
            }
            sc.pos = at;
            return;
        }
        skip_number(sc, len);
        if len >= 9 || n >= 100_000_000 {
            if (100_000_000..YEAR_2100).contains(&n) && len < 11 {
                self.set_raw(n);
            }
            return;
        }
        if len == 8 {
            let (y, m, d) = (n / 10000, n / 100 % 100, n % 100);
            if (1..=12).contains(&m) && (1..=31).contains(&d) && (1970..2100).contains(&y) {
                self.f.year = Some(y);
                self.f.mon = Some(m);
                self.f.mday = Some(d);
            }
            return;
        }
        if len == 4 {
            if (1970..2100).contains(&n) {
                self.f.year = Some(n);
            }
            return;
        }
        if len > 2 {
            return;
        }
        if self.f.mday.is_none() && (1..=31).contains(&n) {
            self.f.mday = Some(n);
        } else if self.f.mon.is_none() && (1..=12).contains(&n) {
            self.f.mon = Some(n);
        } else if self.f.year.is_none()
            && let Some(y) = strict_two_digit_year(n, len)
        {
            self.f.year = Some(y);
        }
    }
}

/// Strictly parse a date such as `GIT_COMMITTER_DATE` accepts: git's
/// internal format (`<epoch> <tz>`, `@<epoch>`), RFC 2822, ISO 8601 and a
/// few numeric date forms. Returns (timestamp, offset in `±hhmm` form).
pub fn parse_date(input: &str, ctx: &DateContext) -> Option<(u64, i32)> {
    let now = ctx.now_tm();
    let mut st = Strict::default();
    let mut sc = Scanner {
        s: input.as_bytes(),
        pos: 0,
    };
    while let Some(c) = sc.peek(0) {
        if c.is_ascii_alphabetic() {
            let start = sc.pos;
            while sc.peek(0).is_some_and(|c| c.is_ascii_alphabetic()) {
                sc.pos += 1;
            }
            let word = &input[start..sc.pos];
            if word.eq_ignore_ascii_case("T") && sc.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                if let Some((h, m, s, used)) = match_basic_time(&sc, sc.pos) {
                    st.f.hour = Some(h);
                    st.f.min = Some(m);
                    st.f.sec = Some(s);
                    sc.pos += used;
                }
                continue;
            }
            if word.eq_ignore_ascii_case("am") || word.eq_ignore_ascii_case("pm") {
                if let Some(h) = st.f.hour {
                    let pm = word.eq_ignore_ascii_case("pm");
                    st.f.hour = Some(h % 12 + if pm { 12 } else { 0 });
                }
            } else if let Some(m) = month_word(word) {
                st.f.mon = Some(m);
            } else if let Some(z) = zone_minutes(word) {
                if !st.tz_numeric {
                    st.tz = Some(z);
                }
            }
            continue;
        }
        if c.is_ascii_digit() {
            st.number(&mut sc, &now);
            continue;
        }
        if (c == b'+' || c == b'-') && sc.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            match match_tz(&sc, sc.pos, true) {
                TzMatch::Valid(z, used) => {
                    st.tz = Some(z);
                    st.tz_numeric = true;
                    sc.pos += used;
                }
                TzMatch::Invalid(used) => sc.pos += used,
                TzMatch::NotTz => sc.pos += 1,
            }
            continue;
        }
        if c == b'@' && sc.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            let len = sc.digits_at(sc.pos + 1);
            let digits = &input[sc.pos + 1..sc.pos + 1 + len];
            sc.pos += 1 + len;
            match digits.parse::<u64>() {
                Ok(v) if v <= i64::MAX as u64 => st.set_raw(v as i64),
                _ => return None,
            }
            continue;
        }
        sc.pos += 1;
    }

    if let Some((raw, fields)) = st.raw
        && fields == st.f
    {
        let tz = match st.tz {
            Some(z) => secs_to_tz(z * 60),
            // A short `@<epoch>` needs an explicit offset.
            None if raw < 100_000_000 => return None,
            None => ctx.local_tz(raw),
        };
        return Some((raw as u64, tz));
    }
    let f = st.f;
    let (Some(year), Some(mon), Some(mday), Some(hour), Some(min)) =
        (f.year, f.mon, f.mday, f.hour, f.min)
    else {
        return None;
    };
    let naive = timegm(year, mon - 1, mday, hour, min, f.sec.unwrap_or(0));
    let (t, tz_secs) = match st.tz {
        Some(z) => (naive - z * 60, z * 60),
        // Fields that came from an epoch timestamp stay in UTC; only the
        // reported offset is the local one.
        None if st.raw.is_some() => (naive, ctx.local_offset_secs(naive)),
        None => {
            let t = ctx.mktime(naive);
            (t, naive - t)
        }
    };
    if !(0..YEAR_2100).contains(&t) {
        return None;
    }
    Some((t as u64, secs_to_tz(tz_secs)))
}

// ---------------------------------------------------------------------------
// Approxidate
// ---------------------------------------------------------------------------

/// A number waiting to be used: as the count of a following unit or
/// weekday, or else as a part of the date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// Digits: value and number of digits.
    Digit(i64, usize),
    /// "last", "one" .. "ten".
    Word(i64),
}

impl Pending {
    fn value(self) -> i64 {
        match self {
            Pending::Digit(n, _) | Pending::Word(n) => n,
        }
    }
}

struct Approx<'c> {
    ctx: &'c DateContext,
    now: Tm,
    f: Fields,
    /// Weekday used by "last friday" etc. Only updated when the whole date
    /// is recomputed (seconds arithmetic, `never`), not by single fields.
    wday: i64,
    /// Whether wall clock times are read as summer time. Like the weekday it
    /// comes from "now" and is only updated when the whole date is recomputed.
    dst: bool,
    pending: Option<Pending>,
    /// noon/midnight/tea asked for the previous day unless a date follows.
    rollback: bool,
    /// A day of the month was given or computed (month and year arithmetic
    /// do not count).
    mday_given: bool,
    recognised: bool,
}

impl<'c> Approx<'c> {
    fn new(ctx: &'c DateContext) -> Self {
        let now = ctx.now_tm();
        Approx {
            ctx,
            now,
            f: Fields::default(),
            wday: now.wday,
            dst: ctx.is_dst(ctx.now),
            pending: None,
            rollback: false,
            mday_given: false,
            recognised: false,
        }
    }

    fn flush_value(&mut self, n: i64) {
        if self.f.mday.is_none() && (1..=31).contains(&n) {
            self.f.mday = Some(n);
            self.mday_given = true;
        } else if self.f.mon.is_none() && (1..=12).contains(&n) {
            self.f.mon = Some(n);
        } else if self.f.year.is_none() {
            if (1..38).contains(&n) {
                self.f.year = Some(2000 + n);
            } else if (70..100).contains(&n) {
                self.f.year = Some(1900 + n);
            }
        }
    }

    fn flush(&mut self) {
        match self.pending.take() {
            Some(Pending::Digit(n, len)) if len > 2 => {
                if len == 4 && (1970..2100).contains(&n) && self.f.year.is_none() {
                    self.f.year = Some(n);
                }
            }
            Some(p) => self.flush_value(p.value()),
            None => {}
        }
    }

    fn take_count(&mut self) -> Option<i64> {
        self.pending.take().map(Pending::value)
    }

    /// Local instant of the current fields (unset ones from now).
    fn instant(&self) -> i64 {
        self.ctx.mktime_dst(self.f.naive(&self.now), self.dst)
    }

    /// Replace all fields by the local breakdown of instant `t`.
    fn set_instant(&mut self, t: i64) {
        let tm = Tm::from_secs(t + self.ctx.local_offset_secs(t));
        self.f.set_all(&tm);
        self.mday_given = true;
        self.wday = tm.wday;
        self.dst = self.ctx.is_dst(t);
    }

    /// The current date (unset parts from now) with "Feb 30" normalised.
    fn normalised_date(&self) -> (i64, i64, i64) {
        let year = self.f.year.unwrap_or(default_year(self.f.mon, &self.now));
        let mon = self.f.mon.unwrap_or(self.now.mon);
        let mday = self.f.mday.unwrap_or(self.now.mday);
        crate::civil::civil_from_days(day_number(year, mon, mday))
    }

    /// Set the year to `base - n`. A result before 1900 leaves the year unknown.
    fn sub_years(&mut self, base: i64, n: i64) {
        let year = base.saturating_sub(n);
        let mon = self.f.mon.unwrap_or(self.now.mon);
        let mday = self.f.mday.unwrap_or(self.now.mday);
        if year < 1900 {
            self.f.year = None;
            self.f.mon = Some(mon);
            self.f.mday = Some(mday);
            return;
        }
        // Normalise "Feb 30" right away; the weekday is not updated.
        let (y, m, d) = crate::civil::civil_from_days(day_number(year, mon, mday));
        self.f.year = Some(y);
        self.f.mon = Some(m);
        self.f.mday = Some(d);
    }

    fn fill_date(&mut self) {
        self.f
            .year
            .get_or_insert(default_year(self.f.mon, &self.now));
        self.f.mon.get_or_insert(self.now.mon);
        self.f.mday.get_or_insert(self.now.mday);
        self.mday_given = true;
    }

    fn special_hour(&mut self, h: i64) {
        self.flush();
        let current = self.f.hour.unwrap_or(self.now.hour);
        if h > current && !self.mday_given {
            self.rollback = true;
        }
        self.f.hour = Some(h);
        self.f.min = Some(0);
        self.f.sec = Some(0);
    }

    fn word(&mut self, word: &str) {
        let lower = word.to_ascii_lowercase();
        match lower.as_str() {
            "now" => {
                // "0 seconds ago": eats a pending number, pins the date.
                self.pending = None;
                let t = self.instant();
                self.set_instant(t);
                self.recognised = true;
                return;
            }
            "today" => {
                self.pending = None;
                self.f.mday = Some(self.now.mday);
                self.fill_date();
                // A time of day equal to the current one counts as "not
                // given" and becomes midnight.
                let now = &self.now;
                let time = (
                    self.f.hour.unwrap_or(now.hour),
                    self.f.min.unwrap_or(now.min),
                    self.f.sec.unwrap_or(now.sec),
                );
                if time == (now.hour, now.min, now.sec) {
                    self.f.hour = Some(0);
                    self.f.min = Some(0);
                    self.f.sec = Some(0);
                }
                self.recognised = true;
                return;
            }
            "yesterday" => {
                self.pending = None;
                self.f.mday = Some(self.now.mday);
                let t = self.instant() - 86400;
                self.set_instant(t);
                self.recognised = true;
                return;
            }
            "never" => {
                self.pending = None;
                self.set_instant(0);
                self.recognised = true;
                return;
            }
            "noon" => {
                self.special_hour(12);
                self.recognised = true;
                return;
            }
            "midnight" => {
                self.special_hour(0);
                self.recognised = true;
                return;
            }
            "tea" => {
                self.special_hour(17);
                self.recognised = true;
                return;
            }
            "am" | "pm" => {
                let pm = lower == "pm";
                let h = match self.pending.take() {
                    Some(p) => {
                        self.f.min = Some(0);
                        self.f.sec = Some(0);
                        p.value()
                    }
                    None => self.f.hour.unwrap_or(self.now.hour),
                };
                self.f.hour = Some(h % 12 + if pm { 12 } else { 0 });
                self.recognised = true;
                return;
            }
            _ => {}
        }
        if let Some(n) = number_word(&lower) {
            if self.pending.is_none() {
                self.pending = Some(Pending::Word(n));
            }
            self.recognised = true;
            return;
        }
        if let Some(m) = month_word(&lower) {
            self.f.mon = Some(m);
            self.recognised = true;
            return;
        }
        if let Some(target) = weekday_word(&lower) {
            let n = self.take_count().unwrap_or(0);
            if n > 0 {
                let mut delta = (self.wday - target).rem_euclid(7);
                if delta == 0 {
                    delta = 7;
                }
                delta += 7 * (n - 1);
                let t = self.instant() - delta * 86400;
                self.set_instant(t);
                self.recognised = true;
            }
            return;
        }
        if let Some(unit) = unit_word(&lower) {
            let Some(n) = self.take_count() else {
                return;
            };
            self.recognised = true;
            let secs = match unit {
                Unit::Second => 1,
                Unit::Minute => 60,
                Unit::Hour => 3600,
                Unit::Day => 86400,
                Unit::Week => 7 * 86400,
                Unit::Month => {
                    let (base, mon, mday) = self.normalised_date();
                    let mut mon = mon - n % 12;
                    let mut years = n / 12;
                    if mon < 1 {
                        mon += 12;
                        years += 1;
                    }
                    self.f.mon = Some(mon);
                    self.f.mday = Some(mday);
                    self.sub_years(base, years);
                    return;
                }
                Unit::Year => {
                    let (base, mon, mday) = self.normalised_date();
                    self.f.mon = Some(mon);
                    self.f.mday = Some(mday);
                    self.sub_years(base, n);
                    return;
                }
            };
            let t = self.instant() - n * secs;
            self.set_instant(t);
        }
    }

    fn number(&mut self, sc: &mut Scanner<'_>) {
        let pos = sc.pos;
        let len = sc.digits_at(pos);
        let n = sc.number_at(pos, len);
        let next = sc.s.get(pos + len).copied();
        self.recognised = true;

        if let Some((h, m, s, used)) = match_time(sc, pos) {
            self.flush();
            self.f.hour = Some(h);
            self.f.min = Some(m);
            self.f.sec = Some(s);
            sc.pos += used;
            return;
        }
        if matches!(next, Some(b'-' | b'/' | b'.')) && sc.digits_at(pos + len + 1) > 0 {
            if let Some((y, m, d, used)) =
                match_numeric_date(sc, pos, &self.now, FutureCheck::Always)
            {
                self.flush();
                if let Some(y) = y {
                    self.f.year = Some(y);
                }
                self.f.mon = Some(m);
                self.f.mday = Some(d);
                self.mday_given = true;
                sc.pos += used;
                return;
            }
        }
        skip_number(sc, len);
        self.flush();
        if n == 0 {
            return;
        }
        self.pending = Some(Pending::Digit(n, len));
    }

    fn finish(mut self) -> (i64, bool) {
        self.flush();
        if self.rollback && !self.mday_given {
            self.f.mday = Some(self.now.mday - 1);
            self.fill_date();
        }
        (self.instant(), !self.recognised)
    }
}

/// Parse `input` like git's `--since`/`--until`/`@{...}`: strict formats
/// first, then the fuzzy parser. Returns the timestamp and whether nothing
/// at all could be recognised (`error`); in that case the timestamp is "now".
///
/// Results before the epoch wrap around like git's unsigned timestamps.
pub fn approxidate_careful(input: &str, ctx: &DateContext) -> (u64, bool) {
    if let Some((t, _)) = parse_date(input, ctx) {
        return (t, false);
    }
    let mut ap = Approx::new(ctx);
    let mut sc = Scanner {
        s: input.as_bytes(),
        pos: 0,
    };
    while let Some(c) = sc.peek(0) {
        if c.is_ascii_alphabetic() {
            let start = sc.pos;
            while sc.peek(0).is_some_and(|c| c.is_ascii_alphabetic()) {
                sc.pos += 1;
            }
            ap.word(&input[start..sc.pos]);
            continue;
        }
        if c.is_ascii_digit() {
            ap.number(&mut sc);
            continue;
        }
        if (c == b'+' || c == b'-') && sc.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            // Offsets are only honoured by the strict parser.
            match match_tz(&sc, sc.pos, false) {
                TzMatch::Valid(_, used) => {
                    ap.flush();
                    ap.recognised = true;
                    sc.pos += used;
                }
                // The fuzzy parser reads the digits of a bad offset as a number.
                TzMatch::Invalid(_) | TzMatch::NotTz => sc.pos += 1,
            }
            continue;
        }
        sc.pos += 1;
    }
    let (t, error) = ap.finish();
    (t as u64, error)
}

/// [`approxidate_careful`] without the error flag.
pub fn approxidate(input: &str, ctx: &DateContext) -> u64 {
    approxidate_careful(input, ctx).0
}

/// Parse an expiry setting (`gc.reflogExpire`, `--expire=`): `never` is 0,
/// `now`/`all` mean "everything" (`u64::MAX`), anything else goes through
/// [`approxidate_careful`].
pub fn parse_expiry_date(input: &str, ctx: &DateContext) -> Result<u64, DateError> {
    match input {
        "never" => return Ok(0),
        "now" | "all" => return Ok(u64::MAX),
        _ => {}
    }
    match approxidate_careful(input, ctx) {
        (_, true) => Err(DateError::InvalidTimestamp(input.to_string())),
        (t, false) => Ok(t),
    }
}
