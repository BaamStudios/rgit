// SPDX-License-Identifier: MIT
//! CGI response headers (RFC 3875 section 6): `Name: value` lines ended
//! by CRLF, an empty line before the body, the status as a `Status:`
//! header. The cache headers and their order follow git 2.56.

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// One year: how long objects and packs may be cached.
const FOREVER: u64 = 31_536_000;

/// Collects the header lines of one response.
pub(crate) struct Headers<'w> {
    out: &'w mut dyn Write,
}

impl<'w> Headers<'w> {
    pub fn new(out: &'w mut dyn Write) -> Self {
        Headers { out }
    }

    pub fn str(&mut self, name: &str, value: &str) -> io::Result<()> {
        write!(self.out, "{name}: {value}\r\n")
    }

    pub fn status(&mut self, code: u16, text: &str) -> io::Result<()> {
        write!(self.out, "Status: {code} {text}\r\n")
    }

    /// The response must not be cached.
    pub fn nocache(&mut self) -> io::Result<()> {
        self.str("Expires", "Fri, 01 Jan 1980 00:00:00 GMT")?;
        self.str("Pragma", "no-cache")?;
        self.str("Cache-Control", "no-cache, max-age=0, must-revalidate")
    }

    /// The response never changes (objects and packs are named by their
    /// content).
    pub fn cache_forever(&mut self) -> io::Result<()> {
        let now = now();
        self.str("Date", &http_date(now))?;
        self.str("Expires", &http_date(now + FOREVER))?;
        self.str("Cache-Control", &format!("public, max-age={FOREVER}"))
    }

    /// The empty line after the headers.
    pub fn end(&mut self) -> io::Result<()> {
        self.out.write_all(b"\r\n")?;
        self.out.flush()
    }
}

/// Seconds since the epoch.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `Thu, 26 Nov 2026 10:32:35 +0000`, the format git uses for `Date`,
/// `Expires` and `Last-Modified` (RFC 2822 date in UTC).
pub(crate) fn http_date(secs: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
        DAYS[(days % 7) as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::http_date;

    #[test]
    fn dates() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(http_date(1_795_689_155), "Thu, 26 Nov 2026 10:32:35 +0000");
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 +0000");
    }
}
