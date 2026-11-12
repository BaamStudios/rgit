// SPDX-License-Identifier: MIT

//! Identities in `author`, `committer` and `tagger` lines:
//! `Name <email> <seconds> <+hhmm>`.

use std::fmt;

/// A point in time with the author's time zone, as stored in an identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Time {
    /// Seconds since the Unix epoch.
    pub seconds: i64,
    /// Offset from UTC in minutes (east positive).
    pub offset_minutes: i32,
    /// Whether the offset is written with `-` (distinguishes `-0000` from `+0000`).
    pub negative: bool,
}

impl Time {
    /// A time with the given UTC offset; the sign follows the offset.
    pub fn new(seconds: i64, offset_minutes: i32) -> Time {
        Time {
            seconds,
            offset_minutes,
            negative: offset_minutes < 0,
        }
    }

    /// Parse `<seconds> <+hhmm>` leniently: one or more spaces may precede
    /// the seconds; the zone must be a sign and four digits. Anything after
    /// the zone is ignored.
    pub fn parse(raw: &[u8]) -> Option<Time> {
        let raw = trim_start_spaces(raw);
        let digits = raw.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        let seconds = parse_decimal(&raw[..digits])?;
        let rest = raw[digits..].strip_prefix(b" ")?;
        let (negative, tz) = match rest.split_first()? {
            (b'+', tz) => (false, tz),
            (b'-', tz) => (true, tz),
            _ => return None,
        };
        if tz.len() < 4 || !tz[..4].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let hh = i32::from(tz[0] - b'0') * 10 + i32::from(tz[1] - b'0');
        let mm = i32::from(tz[2] - b'0') * 10 + i32::from(tz[3] - b'0');
        let abs = hh * 60 + mm;
        Some(Time {
            seconds,
            offset_minutes: if negative { -abs } else { abs },
            negative,
        })
    }
}

impl fmt::Display for Time {
    /// `<seconds> <+hhmm>`, the form stored in objects.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let abs = self.offset_minutes.unsigned_abs();
        let sign = if self.negative || self.offset_minutes < 0 {
            '-'
        } else {
            '+'
        };
        write!(f, "{} {sign}{:02}{:02}", self.seconds, abs / 60, abs % 60)
    }
}

/// A borrowed, leniently parsed identity.
///
/// The raw line is never rewritten; this is a view for readers. Use the
/// validator ([`crate::fsck`]) to check that a line is well formed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignatureRef<'a> {
    /// The name, without the space before `<`.
    pub name: &'a [u8],
    /// The text between `<` and `>`.
    pub email: &'a [u8],
    /// Everything after `>` (normally ` <seconds> <+hhmm>`).
    pub time_raw: &'a [u8],
}

impl<'a> SignatureRef<'a> {
    /// Split an identity line (the header value) into its parts. Needs a `<`
    /// and a later `>`; returns `None` otherwise.
    pub fn parse(raw: &'a [u8]) -> Option<SignatureRef<'a>> {
        let lt = raw.iter().position(|&b| b == b'<')?;
        let gt = lt + 1 + raw[lt + 1..].iter().position(|&b| b == b'>')?;
        let mut name = &raw[..lt];
        while let [rest @ .., b' '] = name {
            name = rest;
        }
        Some(SignatureRef {
            name,
            email: &raw[lt + 1..gt],
            time_raw: &raw[gt + 1..],
        })
    }

    /// The parsed time, if well formed.
    pub fn time(&self) -> Option<Time> {
        Time::parse(self.time_raw)
    }

    /// Convert to an owned signature (fails when the time is malformed).
    pub fn to_owned(&self) -> Option<Signature> {
        Some(Signature {
            name: self.name.to_vec(),
            email: self.email.to_vec(),
            time: self.time()?,
        })
    }
}

/// An owned identity, used to build new objects.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signature {
    /// The name.
    pub name: Vec<u8>,
    /// The email address (without `<>`).
    pub email: Vec<u8>,
    /// The time.
    pub time: Time,
}

impl Signature {
    /// The line value `Name <email> <seconds> <+hhmm>`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.name.len() + self.email.len() + 32);
        v.extend_from_slice(&self.name);
        v.extend_from_slice(b" <");
        v.extend_from_slice(&self.email);
        v.extend_from_slice(b"> ");
        v.extend_from_slice(self.time.to_string().as_bytes());
        v
    }
}

fn trim_start_spaces(mut s: &[u8]) -> &[u8] {
    while let [b' ', rest @ ..] = s {
        s = rest;
    }
    s
}

fn parse_decimal(d: &[u8]) -> Option<i64> {
    let mut v: i64 = 0;
    for &c in d {
        v = v.checked_mul(10)?.checked_add(i64::from(c - b'0'))?;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format() {
        let s = SignatureRef::parse(b"A U Thor <a@b.c> 1234567890 -0130").unwrap();
        assert_eq!(s.name, b"A U Thor");
        assert_eq!(s.email, b"a@b.c");
        let t = s.time().unwrap();
        assert_eq!(t.seconds, 1234567890);
        assert_eq!(t.offset_minutes, -90);
        let o = s.to_owned().unwrap();
        assert_eq!(o.to_bytes(), b"A U Thor <a@b.c> 1234567890 -0130");
        let z = Time::parse(b" 0 -0000").unwrap();
        assert!(z.negative);
        assert_eq!(z.to_string(), "0 -0000");
        assert!(Time::parse(b" x +0000").is_none());
        assert!(SignatureRef::parse(b"no email").is_none());
    }
}
