// SPDX-License-Identifier: MIT
//! A `strftime` for `--date=format:`, behaving like the C library's in the
//! "C"/POSIX locale (English names, `%c` = `%a %b %e %H:%M:%S %Y`).
//!
//! `%s`, `%z` and `%Z` without flags are handled the git way: `%s` is the
//! timestamp, `%z` the commit's own offset and `%Z` is empty unless the
//! `-local` variant is used. Locale-specific output of the C library (when
//! `LC_TIME` is not "C") is not reproduced.

use crate::civil::{MONTH_NAMES, Tm, WEEKDAY_NAMES};

/// Everything a conversion may need.
pub(crate) struct StrftimeInput<'a> {
    pub tm: Tm,
    /// The timestamp, for `%s`.
    pub timestamp: u64,
    /// Offset in git's `±hhmm` form, for `%z`.
    pub tz: i32,
    /// Zone abbreviation for `%Z`; `None` prints nothing (non-local mode).
    pub zone_name: Option<&'a str>,
}

enum Piece {
    Num { value: i64, width: usize, pad: u8 },
    Str(String),
}

fn iso_week(tm: &Tm) -> (i64, i64) {
    // ISO 8601 week number and week-based year.
    let wday_mon = (tm.wday + 6) % 7; // Monday = 0
    let mut year = tm.year;
    let mut week = (tm.yday - wday_mon + 10) / 7;
    let days_in = |y: i64| {
        if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            366
        } else {
            365
        }
    };
    if week < 1 {
        year -= 1;
        let yday = tm.yday + days_in(year);
        week = (yday - wday_mon + 10) / 7;
    } else if week == 53 {
        // The week belongs to next year if Thursday of it is in next year.
        let thursday_yday = tm.yday - wday_mon + 3;
        if thursday_yday >= days_in(tm.year) {
            year += 1;
            week = 1;
        }
    }
    (week, year)
}

fn conversion(c: char, input: &StrftimeInput<'_>, flags_present: bool) -> Option<Piece> {
    let tm = &input.tm;
    let num = |value: i64, width: usize| Piece::Num {
        value,
        width,
        pad: b'0',
    };
    let spnum = |value: i64, width: usize| Piece::Num {
        value,
        width,
        pad: b' ',
    };
    let hour12 = if tm.hour % 12 == 0 { 12 } else { tm.hour % 12 };
    Some(match c {
        'a' => Piece::Str(WEEKDAY_NAMES[tm.wday as usize][..3].to_string()),
        'A' => Piece::Str(WEEKDAY_NAMES[tm.wday as usize].to_string()),
        'b' | 'h' => Piece::Str(MONTH_NAMES[(tm.mon - 1) as usize][..3].to_string()),
        'B' => Piece::Str(MONTH_NAMES[(tm.mon - 1) as usize].to_string()),
        'c' => Piece::Str(expand("%a %b %e %H:%M:%S %Y", input)),
        'C' => num(tm.year.div_euclid(100), 2),
        'd' => num(tm.mday, 2),
        'D' | 'x' => Piece::Str(expand("%m/%d/%y", input)),
        'e' => spnum(tm.mday, 2),
        'F' => Piece::Str(expand("%Y-%m-%d", input)),
        'g' => num(iso_week(tm).1.rem_euclid(100), 2),
        'G' => num(iso_week(tm).1, 1),
        'H' => num(tm.hour, 2),
        'I' => num(hour12, 2),
        'j' => num(tm.yday + 1, 3),
        'k' => spnum(tm.hour, 2),
        'l' => spnum(hour12, 2),
        'm' => num(tm.mon, 2),
        'M' => num(tm.min, 2),
        'n' => Piece::Str("\n".into()),
        'p' => Piece::Str(if tm.hour >= 12 { "PM" } else { "AM" }.into()),
        'P' => Piece::Str(if tm.hour >= 12 { "pm" } else { "am" }.into()),
        'r' => Piece::Str(expand("%I:%M:%S %p", input)),
        'R' => Piece::Str(expand("%H:%M", input)),
        's' => num(input.timestamp as i64, 1),
        'S' => num(tm.sec, 2),
        't' => Piece::Str("\t".into()),
        'T' | 'X' => Piece::Str(expand("%H:%M:%S", input)),
        'u' => num(if tm.wday == 0 { 7 } else { tm.wday }, 1),
        'U' => num((tm.yday + 7 - tm.wday) / 7, 2),
        'V' => num(iso_week(tm).0, 2),
        'w' => num(tm.wday, 1),
        'W' => num((tm.yday + 7 - (tm.wday + 6) % 7) / 7, 2),
        'y' => num(tm.year.rem_euclid(100), 2),
        'Y' => num(tm.year, 1),
        'z' => {
            if flags_present {
                // The C library sees the broken-down time's own offset.
                let tz = input.tz;
                let sign = if tz < 0 { "-" } else { "+" };
                return Some(Piece::Str(format!("{sign}{:04}", tz.abs())));
            }
            Piece::Str(format_tz(input.tz))
        }
        'Z' => match (input.zone_name, flags_present) {
            (Some(name), _) => Piece::Str(name.to_string()),
            (None, true) => Piece::Str("GMT".into()),
            (None, false) => Piece::Str(String::new()),
        },
        '%' => Piece::Str("%".into()),
        _ => return None,
    })
}

/// `+0200` / `-0130` from git's integer form.
pub(crate) fn format_tz(tz: i32) -> String {
    let sign = if tz < 0 { '-' } else { '+' };
    format!("{sign}{:04}", tz.unsigned_abs())
}

fn expand(fmt: &str, input: &StrftimeInput<'_>) -> String {
    strftime(fmt, input)
}

/// Format `input` according to `fmt`.
pub(crate) fn strftime(fmt: &str, input: &StrftimeInput<'_>) -> String {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        // Flags.
        let mut pad: Option<u8> = None;
        let mut no_pad = false;
        let mut upper = false;
        let mut swap = false;
        let mut any_flag = false;
        while i < chars.len() {
            match chars[i] {
                '_' => pad = Some(b' '),
                '0' => pad = Some(b'0'),
                '-' => no_pad = true,
                '^' => upper = true,
                '#' => swap = true,
                _ => break,
            }
            any_flag = true;
            i += 1;
        }
        // Width.
        let mut width: Option<usize> = None;
        while i < chars.len() && chars[i].is_ascii_digit() {
            let d = chars[i] as usize - '0' as usize;
            width = Some(width.unwrap_or(0).saturating_mul(10).saturating_add(d));
            any_flag = true;
            i += 1;
        }
        // E and O modifiers are accepted and ignored.
        if i < chars.len() && (chars[i] == 'E' || chars[i] == 'O') {
            if i + 1 < chars.len() && conversion(chars[i + 1], input, true).is_some() {
                any_flag = true;
                i += 1;
            } else {
                // Not a valid modified conversion: print literally.
                out.extend(&chars[start..=i]);
                i += 1;
                continue;
            }
        }
        if i >= chars.len() {
            out.extend(&chars[start..]);
            break;
        }
        let conv = chars[i];
        i += 1;
        let Some(piece) = conversion(conv, input, any_flag) else {
            out.extend(&chars[start..i]);
            continue;
        };
        let mut s = match piece {
            Piece::Num {
                value,
                width: default_width,
                pad: default_pad,
            } => {
                let digits = value.unsigned_abs().to_string();
                let neg = if value < 0 { "-" } else { "" };
                let w = if no_pad {
                    0
                } else {
                    width.unwrap_or(default_width)
                };
                let padc = pad.unwrap_or(default_pad) as char;
                let len = neg.len() + digits.len();
                if len >= w {
                    format!("{neg}{digits}")
                } else if padc == '0' {
                    format!("{neg}{}{digits}", "0".repeat(w - len))
                } else {
                    format!("{}{neg}{digits}", " ".repeat(w - len))
                }
            }
            Piece::Str(s) => {
                let w = width.unwrap_or(0);
                let n = s.chars().count();
                if n < w {
                    let padc = if pad == Some(b'0') { '0' } else { ' ' };
                    format!("{}{s}", padc.to_string().repeat(w - n))
                } else {
                    s
                }
            }
        };
        if upper {
            s = s.to_uppercase();
        } else if swap {
            s = match conv {
                'p' | 'Z' => s.to_lowercase(),
                'a' | 'A' | 'b' | 'B' | 'h' => s.to_uppercase(),
                _ => s,
            };
        }
        out.push_str(&s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(t: i64) -> StrftimeInput<'static> {
        StrftimeInput {
            tm: Tm::from_secs(t),
            timestamp: t as u64,
            tz: 0,
            zone_name: None,
        }
    }

    // Expected values were produced by git 2.56.0 with LC_ALL=C
    // (`--date=format:...` of `1500000000 +0530`).
    #[test]
    fn c_locale() {
        let i = input(1500000000 + 19800);
        assert_eq!(
            strftime("%c|%x|%X|%r", &i),
            "Fri Jul 14 08:10:00 2017|07/14/17|08:10:00|08:10:00 AM"
        );
        assert_eq!(
            strftime("%e|%k|%l|%j|%U|%W|%V|%G", &i),
            "14| 8| 8|195|28|28|28|2017"
        );
        assert_eq!(strftime("%Q|%+|%E|%", &i), "%Q|%+|%E|%");
        assert_eq!(
            strftime("%-d|%_m|%05Y|%^a|%#b|%12A", &i),
            "14| 7|02017|FRI|JUL|      Friday"
        );
        assert_eq!(strftime("%Ec|%Oy", &i), "Fri Jul 14 08:10:00 2017|17");
    }

    #[test]
    fn iso_weeks() {
        // 2010-01-03 is in week 53 of 2009, 2008-12-29 in week 1 of 2009.
        let i = input(crate::civil::timegm(2010, 0, 3, 0, 0, 0));
        assert_eq!(strftime("%G-%V %g", &i), "2009-53 09");
        let i = input(crate::civil::timegm(2008, 11, 29, 0, 0, 0));
        assert_eq!(strftime("%G-%V", &i), "2009-01");
    }
}
