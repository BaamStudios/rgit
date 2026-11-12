// SPDX-License-Identifier: MIT
//! The two external inputs of every date operation: "now" and the local time zone.

use jiff::Timestamp as JTimestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;

use crate::civil::Tm;

/// Environment variable that overrides the current time (seconds since the
/// epoch). git's test suite uses it to make relative dates reproducible.
pub const TEST_DATE_NOW_ENV: &str = "GIT_TEST_DATE_NOW";

/// "Now" plus the local time zone, the inputs every date operation depends on.
///
/// Library code never reads the clock or `TZ` by itself; callers build a
/// context once (usually with [`DateContext::from_env`]) and pass it down.
#[derive(Clone, Debug)]
pub struct DateContext {
    /// Current time in seconds since the epoch.
    pub now: i64,
    /// The local time zone (`TZ`), used by the `-local` modes and the parser.
    pub zone: TimeZone,
}

impl DateContext {
    pub fn new(now: i64, zone: TimeZone) -> Self {
        DateContext { now, zone }
    }

    /// A context in UTC.
    pub fn utc(now: i64) -> Self {
        DateContext::new(now, TimeZone::UTC)
    }

    /// Current time from `GIT_TEST_DATE_NOW` or the system clock, local zone
    /// from `TZ` / the system configuration.
    pub fn from_env() -> Self {
        DateContext::new(now_from_env(), TimeZone::system())
    }

    /// Offset of local time from UTC in seconds at instant `t`.
    pub(crate) fn local_offset_secs(&self, t: i64) -> i64 {
        match JTimestamp::from_second(t) {
            Ok(ts) => self.zone.to_offset(ts).seconds() as i64,
            Err(_) => {
                let clamped = if t < 0 {
                    JTimestamp::MIN
                } else {
                    JTimestamp::MAX
                };
                self.zone.to_offset(clamped).seconds() as i64
            }
        }
    }

    /// Local offset at instant `t` in git's `±hhmm` integer form.
    pub(crate) fn local_tz(&self, t: i64) -> i32 {
        secs_to_tz(self.local_offset_secs(t))
    }

    /// Time zone abbreviation (`CEST`, `UTC`, ...) at instant `t`.
    pub(crate) fn zone_abbreviation(&self, t: i64) -> String {
        let ts = JTimestamp::from_second(t).unwrap_or(if t < 0 {
            JTimestamp::MIN
        } else {
            JTimestamp::MAX
        });
        self.zone.to_offset_info(ts).abbreviation().to_string()
    }

    /// Offset (seconds) and whether it is summer time, at instant `t`.
    fn offset_info(&self, t: i64) -> (i64, bool) {
        let ts = JTimestamp::from_second(t).unwrap_or(if t < 0 {
            JTimestamp::MIN
        } else {
            JTimestamp::MAX
        });
        let info = self.zone.to_offset_info(ts);
        (info.offset().seconds() as i64, info.dst().is_dst())
    }

    /// Whether local time at instant `t` is summer time.
    pub(crate) fn is_dst(&self, t: i64) -> bool {
        self.offset_info(t).1
    }

    /// Like [`DateContext::mktime`], but the wall clock time is read as summer
    /// time if `dst` is true and as standard time otherwise, even when that
    /// kind of time is not in effect on that date (what `mktime()` does with
    /// an explicit `tm_isdst`). Zones without summer time ignore the hint.
    pub(crate) fn mktime_dst(&self, naive: i64, dst: bool) -> i64 {
        let t = self.mktime(naive);
        let (off, is_dst) = self.offset_info(t);
        if is_dst == dst {
            return t;
        }
        // Find the offset of the requested kind of time around that date.
        // Nearby dates first, then other years (a zone may not have had
        // summer time in that year at all).
        let probes = [91, -91, 183, -183, 274, -274]
            .into_iter()
            .chain((1..=60).flat_map(|y| [y * 365 + 183, -y * 365 + 183, y * 365, -y * 365]));
        for days in probes {
            let (other, other_dst) = self.offset_info(t + days * 86400);
            if other_dst == dst && other != off {
                return naive - other;
            }
        }
        t
    }

    /// Local broken-down time of `now`.
    pub(crate) fn now_tm(&self) -> Tm {
        Tm::from_secs(self.now + self.local_offset_secs(self.now))
    }

    /// Convert "naive" local seconds (local wall clock read as if it were UTC)
    /// to an instant, like `mktime()`. Wall clock times that do not exist
    /// (DST gap) or exist twice (DST fold) resolve to the later instant.
    pub(crate) fn mktime(&self, naive: i64) -> i64 {
        let tm = Tm::from_secs(naive);
        let dt = (|| {
            DateTime::new(
                i16::try_from(tm.year).ok()?,
                tm.mon as i8,
                tm.mday as i8,
                tm.hour as i8,
                tm.min as i8,
                tm.sec as i8,
                0,
            )
            .ok()
        })();
        if let Some(dt) = dt
            && let Ok(ts) = self.zone.to_ambiguous_timestamp(dt).later()
        {
            return ts.as_second();
        }
        naive - self.local_offset_secs(naive)
    }
}

/// Read `GIT_TEST_DATE_NOW`, falling back to the system clock.
pub fn now_from_env() -> i64 {
    if let Ok(v) = std::env::var(TEST_DATE_NOW_ENV) {
        return leading_number(&v);
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Value of the leading decimal digits of `s` (0 if there are none).
fn leading_number(s: &str) -> i64 {
    let s = s.trim_start();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

/// Seconds east of UTC to git's `±hhmm` integer (e.g. 7200 -> 200).
pub fn secs_to_tz(secs: i64) -> i32 {
    let minutes = secs / 60;
    let a = minutes.abs();
    let v = (a / 60 * 100 + a % 60) as i32;
    if minutes < 0 { -v } else { v }
}

/// git's `±hhmm` integer to minutes east of UTC. The minute part is not
/// validated: `+0099` is 99 minutes, exactly like git.
pub fn tz_to_minutes(tz: i32) -> i64 {
    let a = (tz as i64).abs();
    let m = a / 100 * 60 + a % 100;
    if tz < 0 { -m } else { m }
}
