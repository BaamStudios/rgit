// SPDX-License-Identifier: MIT
//! Output side: the `--date=<format>` modes.

use crate::DateError;
use crate::civil::{Tm, month_abbr, weekday_abbr};
use crate::context::{DateContext, tz_to_minutes};
use crate::strftime::{StrftimeInput, format_tz, strftime};

/// The style of a `--date=` mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DateStyle {
    /// `default`: `Thu Jan 1 00:00:00 1970 +0000`
    Normal,
    /// `relative`: `2 hours ago`
    Relative,
    /// `short`: `1970-01-01`
    Short,
    /// `iso` / `iso8601`: `1970-01-01 00:00:00 +0000`
    Iso8601,
    /// `iso-strict` / `iso8601-strict`: `1970-01-01T00:00:00Z`
    Iso8601Strict,
    /// `rfc` / `rfc2822`: `Thu, 1 Jan 1970 00:00:00 +0000`
    Rfc2822,
    /// `raw`: `0 +0000`
    Raw,
    /// `unix`: `0`
    Unix,
    /// `human`
    Human,
    /// `format:<strftime>`
    Strftime(String),
}

/// A parsed `--date=` argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateMode {
    pub style: DateStyle,
    /// Show the time in the local time zone instead of the recorded one.
    pub local: bool,
}

impl DateMode {
    pub const fn new(style: DateStyle) -> Self {
        DateMode {
            style,
            local: false,
        }
    }
}

impl Default for DateMode {
    fn default() -> Self {
        DateMode::new(DateStyle::Normal)
    }
}

impl std::str::FromStr for DateMode {
    type Err = DateError;

    /// Parse as if standard output were not a terminal (`auto:` resolves to `default`).
    fn from_str(s: &str) -> Result<Self, DateError> {
        parse_date_mode(s, false)
    }
}

/// Parse a `--date=` / `log.date` value.
///
/// `stdout_is_tty` decides `auto:<mode>`: the mode is used when standard output
/// is a terminal, otherwise `default`.
pub fn parse_date_mode(s: &str, stdout_is_tty: bool) -> Result<DateMode, DateError> {
    if let Some(rest) = s.strip_prefix("auto:") {
        if stdout_is_tty {
            return parse_date_mode(rest, false);
        }
        return Ok(DateMode::default());
    }
    if s == "local" {
        return Ok(DateMode {
            style: DateStyle::Normal,
            local: true,
        });
    }
    if let Some(fmt) = s.strip_prefix("format:") {
        return Ok(DateMode::new(DateStyle::Strftime(fmt.to_string())));
    }
    if let Some(fmt) = s.strip_prefix("format-local:") {
        return Ok(DateMode {
            style: DateStyle::Strftime(fmt.to_string()),
            local: true,
        });
    }
    if s == "format" || s == "format-local" {
        return Err(DateError::MissingColon(s.to_string()));
    }
    let (name, local) = match s.strip_suffix("-local") {
        Some(name) => (name, true),
        None => (s, false),
    };
    let style = match name {
        "relative" => DateStyle::Relative,
        "iso8601-strict" | "iso-strict" => DateStyle::Iso8601Strict,
        "iso8601" | "iso" => DateStyle::Iso8601,
        "rfc2822" | "rfc" => DateStyle::Rfc2822,
        "short" => DateStyle::Short,
        "default" => DateStyle::Normal,
        "human" => DateStyle::Human,
        "raw" => DateStyle::Raw,
        "unix" => DateStyle::Unix,
        _ => return Err(DateError::UnknownFormat(s.to_string())),
    };
    Ok(DateMode { style, local })
}

fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("{n} {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// `--date=relative` text for a time `diff` seconds before now
/// (`None` = in the future).
fn relative_text(diff: Option<u64>) -> String {
    let Some(diff) = diff else {
        return "in the future".to_string();
    };
    if diff < 90 {
        return format!("{} ago", plural(diff, "second"));
    }
    let minutes = (diff + 30) / 60;
    if minutes < 90 {
        return format!("{} ago", plural(minutes, "minute"));
    }
    let hours = (minutes + 30) / 60;
    if hours < 36 {
        return format!("{} ago", plural(hours, "hour"));
    }
    let days = (hours + 12) / 24;
    if days < 14 {
        return format!("{} ago", plural(days, "day"));
    }
    if days < 70 {
        return format!("{} ago", plural((days + 3) / 7, "week"));
    }
    if days < 365 {
        return format!("{} ago", plural((days + 15) / 30, "month"));
    }
    if days < 1825 {
        let total_months = (days * 12 * 2 + 365) / (365 * 2);
        let years = total_months / 12;
        let months = total_months % 12;
        if months > 0 {
            return format!("{}, {} ago", plural(years, "year"), plural(months, "month"));
        }
        return format!("{} ago", plural(years, "year"));
    }
    format!("{} ago", plural((days + 183) / 365, "year"))
}

/// Relative description of `time` as seen from `now`.
pub fn show_date_relative(time: u64, now: i64) -> String {
    if now < 0 || time > now as u64 {
        return relative_text(None);
    }
    relative_text(Some(now as u64 - time))
}

/// `--date=human`.
fn show_human(time: u64, tz: i32, tm: &Tm, ctx: &DateContext) -> String {
    let now_tm = ctx.now_tm();
    let hide_tz = tz == ctx.local_tz(ctx.now);
    if (tm.year, tm.mon, tm.mday) == (now_tm.year, now_tm.mon, now_tm.mday) {
        return show_date_relative(time, ctx.now);
    }
    if tm.year != now_tm.year {
        return format!("{} {} {}", month_abbr(tm.mon), tm.mday, tm.year);
    }
    let days_ago = now_tm.mday - tm.mday;
    if tm.mon == now_tm.mon && (1..5).contains(&days_ago) {
        let mut s = format!("{} {:02}:{:02}", weekday_abbr(tm.wday), tm.hour, tm.min);
        if !hide_tz {
            s.push(' ');
            s.push_str(&format_tz(tz));
        }
        return s;
    }
    format!(
        "{} {} {} {:02}:{:02}",
        weekday_abbr(tm.wday),
        month_abbr(tm.mon),
        tm.mday,
        tm.hour,
        tm.min
    )
}

/// Format a git timestamp (`time` seconds since the epoch, recorded offset
/// `tz` in `±hhmm` integer form) in `mode`.
///
/// Fails only for times that fall before the epoch once shifted into their
/// time zone (`Timestamp before Unix epoch`), like git.
pub fn show_date(
    time: u64,
    tz: i32,
    mode: &DateMode,
    ctx: &DateContext,
) -> Result<String, DateError> {
    match mode.style {
        DateStyle::Unix => return Ok(time.to_string()),
        DateStyle::Relative => return Ok(show_date_relative(time, ctx.now)),
        _ => {}
    }
    let t = i64::try_from(time).unwrap_or(i64::MAX / 2);
    let mut tz = tz;
    let local_secs;
    if mode.local {
        let off = ctx.local_offset_secs(t);
        local_secs = t + off;
        // git cannot determine the offset of local times before the epoch
        // or from the year 2100 on, and prints +0000 for them.
        tz = if local_secs < 0 || t >= 4102444800 {
            0
        } else {
            crate::context::secs_to_tz(off)
        };
    } else {
        local_secs = t.saturating_add(tz_to_minutes(tz) * 60);
    }
    if mode.style == DateStyle::Raw {
        return Ok(format!("{time} {}", format_tz(tz)));
    }
    if local_secs < 0 && !mode.local {
        return Err(DateError::BeforeEpoch { time, tz });
    }
    let tm = Tm::from_secs(local_secs);
    let hms = format!("{:02}:{:02}:{:02}", tm.hour, tm.min, tm.sec);
    Ok(match &mode.style {
        DateStyle::Human => show_human(time, tz, &tm, ctx),
        DateStyle::Short => format!("{:04}-{:02}-{:02}", tm.year, tm.mon, tm.mday),
        DateStyle::Iso8601 => format!(
            "{:04}-{:02}-{:02} {hms} {}",
            tm.year,
            tm.mon,
            tm.mday,
            format_tz(tz)
        ),
        DateStyle::Iso8601Strict => {
            let zone = if tz == 0 {
                "Z".to_string()
            } else {
                let sign = if tz < 0 { '-' } else { '+' };
                let a = tz.unsigned_abs();
                format!("{sign}{:02}:{:02}", a / 100, a % 100)
            };
            format!("{:04}-{:02}-{:02}T{hms}{zone}", tm.year, tm.mon, tm.mday)
        }
        DateStyle::Rfc2822 => format!(
            "{}, {} {} {} {hms} {}",
            weekday_abbr(tm.wday),
            tm.mday,
            month_abbr(tm.mon),
            tm.year,
            format_tz(tz)
        ),
        DateStyle::Strftime(fmt) => {
            let name = if mode.local {
                Some(ctx.zone_abbreviation(t))
            } else {
                None
            };
            strftime(
                fmt,
                &StrftimeInput {
                    tm,
                    timestamp: time,
                    tz,
                    zone_name: name.as_deref(),
                },
            )
        }
        DateStyle::Normal => {
            let mut s = format!(
                "{} {} {} {hms} {}",
                weekday_abbr(tm.wday),
                month_abbr(tm.mon),
                tm.mday,
                tm.year
            );
            if !mode.local {
                s.push(' ');
                s.push_str(&format_tz(tz));
            }
            s
        }
        DateStyle::Unix | DateStyle::Relative | DateStyle::Raw => unreachable!(),
    })
}

#[cfg(test)]
mod tests {
    // Expected values were produced by git 2.56.0 (`git show --date=...`).
    use super::*;
    use jiff::tz::TimeZone;

    const NOW: i64 = 1600000000;

    fn show(time: u64, tz: i32, mode: &str) -> String {
        show_date(time, tz, &mode.parse().unwrap(), &DateContext::utc(NOW)).unwrap()
    }

    #[test]
    fn fixed_formats() {
        let t = 1500000000;
        assert_eq!(show(t, 530, "iso8601"), "2017-07-14 08:10:00 +0530");
        assert_eq!(show(t, -730, "iso"), "2017-07-13 19:10:00 -0730");
        assert_eq!(show(t, 530, "iso8601-strict"), "2017-07-14T08:10:00+05:30");
        assert_eq!(show(t, -730, "iso-strict"), "2017-07-13T19:10:00-07:30");
        assert_eq!(show(t, 0, "iso-strict"), "2017-07-14T02:40:00Z");
        assert_eq!(show(t, 530, "rfc2822"), "Fri, 14 Jul 2017 08:10:00 +0530");
        assert_eq!(show(t, -730, "rfc"), "Thu, 13 Jul 2017 19:10:00 -0730");
        assert_eq!(show(t, -730, "short"), "2017-07-13");
        assert_eq!(show(t, 530, "default"), "Fri Jul 14 08:10:00 2017 +0530");
        assert_eq!(show(t, 530, "local"), "Fri Jul 14 02:40:00 2017");
        assert_eq!(show(t, -730, "raw"), "1500000000 -0730");
        assert_eq!(show(t, 530, "unix"), "1500000000");
        assert_eq!(show(t, 530, "iso-local"), "2017-07-14 02:40:00 +0000");
        assert_eq!(show(t, -730, "raw-local"), "1500000000 +0000");
        assert_eq!(show(0, 0, "default"), "Thu Jan 1 00:00:00 1970 +0000");
        // Offsets are not validated: +0099 is 99 minutes.
        assert_eq!(show(t, 99, "iso-strict"), "2017-07-14T04:19:00+00:99");
    }

    #[test]
    fn strftime_modes() {
        let t = 1500000000;
        assert_eq!(
            show(t, 530, "format:%z|%Z|%s|%Y-%m-%d %H:%M:%S"),
            "+0530||1500000000|2017-07-14 08:10:00"
        );
        assert_eq!(show(t, -730, "format-local:%z|%Z"), "+0000|UTC");
        assert_eq!(show(t, 530, "format:%%z"), "%z");
    }

    #[test]
    fn local_zone() {
        let berlin = DateContext::new(NOW, TimeZone::get("Europe/Berlin").unwrap());
        let mode = "iso-local".parse().unwrap();
        assert_eq!(
            show_date(1500000000, 0, &mode, &berlin).unwrap(),
            "2017-07-14 04:40:00 +0200"
        );
        let mode = "format-local:%Z %z".parse().unwrap();
        assert_eq!(
            show_date(1490000000, 0, &mode, &berlin).unwrap(),
            "CET +0100"
        );
    }

    #[test]
    fn before_epoch() {
        let err = show_date(1, -100, &DateMode::default(), &DateContext::utc(NOW)).unwrap_err();
        assert_eq!(err.to_string(), "Timestamp before Unix epoch: 1 -100");
    }

    #[test]
    fn relative() {
        for (ago, want) in [
            (7, "7 seconds ago"),
            (450, "8 minutes ago"),
            (21000, "6 hours ago"),
            (500000, "6 days ago"),
            (2000000, "3 weeks ago"),
            (15000000, "6 months ago"),
            (40000000, "1 year, 3 months ago"),
            (60000000, "1 year, 11 months ago"),
            (700000000, "22 years ago"),
            (31000000, "12 months ago"),
            (63000000, "2 years ago"),
        ] {
            assert_eq!(show(NOW as u64 - ago, 0, "relative"), want);
        }
        assert_eq!(show(NOW as u64 + 1, 0, "relative"), "in the future");
    }

    #[test]
    fn human() {
        for (ago, want) in [
            (21000, "6 hours ago"),
            (500000, "Mon Sep 7 17:33"),
            (2000000, "Fri Aug 21 08:53"),
            (15000000, "Mon Mar 23 21:46"),
            (40000000, "Jun 8 2019"),
            (700000000, "Jul 9 1998"),
        ] {
            assert_eq!(show(NOW as u64 - ago, 0, "human"), want);
        }
        assert_eq!(show(1500000000, 530, "human"), "Jul 14 2017");
    }

    #[test]
    fn modes() {
        assert!(matches!(
            "bogus".parse::<DateMode>(),
            Err(DateError::UnknownFormat(_))
        ));
        assert_eq!(
            "format".parse::<DateMode>().unwrap_err().to_string(),
            "date format missing colon separator: format"
        );
        assert_eq!("auto:iso".parse::<DateMode>().unwrap(), DateMode::default());
        assert_eq!(
            parse_date_mode("auto:iso", true).unwrap(),
            DateMode::new(DateStyle::Iso8601)
        );
        assert!("local".parse::<DateMode>().unwrap().local);
        assert!("local-local".parse::<DateMode>().is_err());
    }
}
