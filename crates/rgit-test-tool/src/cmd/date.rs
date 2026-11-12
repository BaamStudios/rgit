// SPDX-License-Identifier: MIT

//! `date` (t0006, t1300): formatting and parsing through `rgit-date`.
//!
//! Observed behaviour of the reference binary (`test-tool date -h` and its
//! answers):
//!
//! - `relative <time_t>...`, `human <time_t>...`, `show:<format> <time_t>...`
//!   print `<arg> -> <text>` per argument. An argument is a number of
//!   seconds, read like C `strtoumax` (no digits is 0, a minus sign wraps
//!   around, overflow saturates), optionally followed by a `±hhmm` zone read
//!   like `atoi` (only `show` uses it).
//! - `relative` appends to the previous result instead of replacing it, so a
//!   second argument prints both texts.
//! - `parse <date>...` prints the strictly parsed date in its own zone or
//!   `bad`; `approxidate <date>...` the approximate date in UTC;
//!   `timestamp <date>...` its seconds.
//! - `is64bit`, `time_t-is64bit` exit 0.
//! - `getnanos [<start>]` prints the current time in seconds with six
//!   decimals, minus `<start>` (a decimal number read like C `strtod`, 0
//!   when it does not parse), for measuring durations (t5584).
//! - Anything else prints the usage and exits 129.

use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};

use rgit_date::{
    DateContext, DateError, DateMode, DateStyle, approxidate_careful, parse_date, parse_date_mode,
    show_date, show_date_relative, tz_to_minutes,
};

use crate::messages;

fn usage() -> i32 {
    eprint!("{}", messages::DATE_USAGE);
    129
}

/// C `strtoumax` on the start of `s` (base 10): returns the value and the
/// rest of the string.
fn strtoumax(s: &str) -> (u64, &str) {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    if end == 0 {
        return (0, s);
    }
    let mut v: u64 = 0;
    let mut overflow = false;
    for b in t[..end].bytes() {
        match v
            .checked_mul(10)
            .and_then(|x| x.checked_add(u64::from(b - b'0')))
        {
            Some(x) => v = x,
            None => overflow = true,
        }
    }
    let v = if overflow {
        u64::MAX
    } else if neg {
        v.wrapping_neg()
    } else {
        v
    };
    (v, &t[end..])
}

/// C `atoi`: leading whitespace, sign, digits; 0 without digits.
fn atoi(s: &str) -> i32 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let mut v: i64 = 0;
    for b in t.bytes().take_while(u8::is_ascii_digit) {
        v = (v * 10 + i64::from(b - b'0')).min(i64::from(i32::MAX) + 1);
    }
    (if neg { -v } else { v }) as i32
}

/// `<seconds> [<zone>]`.
fn time_and_zone(arg: &str) -> (u64, i32) {
    let (t, rest) = strtoumax(arg);
    (t, atoi(rest))
}

/// The range checks the reference makes before computing the calendar
/// date in the recorded zone (not for `raw`, `unix`, `relative`, `human`
/// and the `-local` modes).
fn check_range(time: u64, tz: i32, mode: &DateMode) -> Result<(), String> {
    let calendar = !mode.local
        && !matches!(
            mode.style,
            DateStyle::Raw | DateStyle::Unix | DateStyle::Relative | DateStyle::Human
        );
    if !calendar {
        return Ok(());
    }
    let off = tz_to_minutes(tz) * 60;
    let shifted = if off >= 0 {
        time.checked_add(off as u64)
            .ok_or_else(|| messages::timestamp_tz_too_large(time, tz))?
    } else {
        time.checked_sub(off.unsigned_abs())
            .ok_or_else(|| messages::before_epoch(time, tz))?
    };
    if shifted > i64::MAX as u64 {
        return Err(messages::timestamp_too_large(shifted));
    }
    Ok(())
}

fn out_fatal(out: String, msg: impl std::fmt::Display) -> i32 {
    let mut o = io::stdout().lock();
    let _ = o.write_all(out.as_bytes()).and_then(|()| o.flush());
    eprintln!("{}", messages::fatal(msg));
    128
}

fn finish(out: String) -> i32 {
    let mut o = io::stdout().lock();
    if o.write_all(out.as_bytes())
        .and_then(|()| o.flush())
        .is_err()
    {
        return 128;
    }
    0
}

fn show(fmt: &str, args: &[String], ctx: &DateContext) -> i32 {
    let mode = match parse_date_mode(fmt, io::stdout().is_terminal()) {
        Ok(m) => m,
        Err(e) => return out_fatal(String::new(), e),
    };
    let mut out = String::new();
    for arg in args {
        let (time, tz) = time_and_zone(arg);
        if let Err(msg) = check_range(time, tz, &mode) {
            return out_fatal(out, msg);
        }
        match show_date(time, tz, &mode, ctx) {
            Ok(s) => {
                let _ = writeln!(out, "{arg} -> {s}");
            }
            Err(DateError::BeforeEpoch { time, tz }) => {
                return out_fatal(out, messages::before_epoch(time, tz));
            }
            Err(e) => return out_fatal(out, e),
        }
    }
    finish(out)
}

fn relative(args: &[String], ctx: &DateContext) -> i32 {
    let mut out = String::new();
    // The reference keeps appending to one buffer.
    let mut acc = String::new();
    for arg in args {
        let (time, _) = strtoumax(arg);
        acc.push_str(&show_date_relative(time, ctx.now));
        let _ = writeln!(out, "{arg} -> {acc}");
    }
    finish(out)
}

fn human(args: &[String], ctx: &DateContext) -> i32 {
    // Shown with zone +0000, whatever the local zone is.
    let mode = DateMode::new(DateStyle::Human);
    let mut out = String::new();
    for arg in args {
        let (time, _) = strtoumax(arg);
        match show_date(time, 0, &mode, ctx) {
            Ok(s) => {
                let _ = writeln!(out, "{arg} -> {s}");
            }
            Err(e) => return out_fatal(out, e),
        }
    }
    finish(out)
}

fn parse(args: &[String], ctx: &DateContext) -> i32 {
    let iso = DateMode::new(DateStyle::Iso8601);
    let mut out = String::new();
    for arg in args {
        match parse_date(arg, ctx) {
            Some((time, tz)) => {
                let shown = show_date(time, tz, &iso, ctx).unwrap_or_else(|e| e.to_string());
                let _ = writeln!(out, "{arg} -> {shown}");
            }
            None => {
                let _ = writeln!(out, "{arg} -> bad");
            }
        }
    }
    finish(out)
}

fn approx(args: &[String], ctx: &DateContext, seconds: bool) -> i32 {
    let iso = DateMode::new(DateStyle::Iso8601);
    let mut out = String::new();
    for arg in args {
        let (time, _) = approxidate_careful(arg, ctx);
        if seconds {
            let _ = writeln!(out, "{arg} -> {time}");
        } else {
            let shown = show_date(time, 0, &iso, ctx).unwrap_or_else(|e| e.to_string());
            let _ = writeln!(out, "{arg} -> {shown}");
        }
    }
    finish(out)
}

/// The longest leading decimal number of `s` (like `strtod`), else 0.
fn leading_f64(s: &str) -> f64 {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !matches!(c, '0'..='9' | '+' | '-' | '.' | 'e' | 'E'))
        .unwrap_or(s.len());
    (1..=end)
        .rev()
        .find_map(|n| s[..n].parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn getnanos(args: &[String]) -> i32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let start = args.first().map_or(0.0, |a| leading_f64(a));
    let secs = now as f64 / 1e9 - start;
    finish(format!("{secs:.6}\n"))
}

pub fn date(args: &[String]) -> i32 {
    let Some(action) = args.first() else {
        return usage();
    };
    let rest = &args[1..];
    let ctx = DateContext::from_env();
    match action.as_str() {
        "relative" => relative(rest, &ctx),
        "human" => human(rest, &ctx),
        "parse" => parse(rest, &ctx),
        "approxidate" => approx(rest, &ctx, false),
        "timestamp" => approx(rest, &ctx, true),
        "is64bit" | "time_t-is64bit" => 0,
        "getnanos" => getnanos(rest),
        a => match a.strip_prefix("show:") {
            Some(fmt) => show(fmt, rest, &ctx),
            None => usage(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtod_prefix() {
        assert_eq!(leading_f64("1.5 x"), 1.5);
        assert_eq!(leading_f64("abc"), 0.0);
        assert_eq!(leading_f64("12e"), 12.0);
        assert_eq!(leading_f64(""), 0.0);
    }

    #[test]
    fn c_number_parsing() {
        assert_eq!(strtoumax("123 +0200"), (123, " +0200"));
        assert_eq!(strtoumax("abc").0, 0);
        assert_eq!(strtoumax("-5").0, u64::MAX - 4);
        assert_eq!(strtoumax("99999999999999999999").0, u64::MAX);
        assert_eq!(atoi(" +0200"), 200);
        assert_eq!(atoi("-02"), -2);
        assert_eq!(atoi("x"), 0);
    }
}
