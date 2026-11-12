// SPDX-License-Identifier: MIT
//! The trace line layout.
//!
//! Observed from git 2.56.0:
//!
//! ```text
//! HH:MM:SS.uuuuuu <location padded to 23> <message>
//! ```
//!
//! The time is local wall-clock time with microseconds. The location column
//! is left-aligned in a field of 23 characters followed by one space, so the
//! message starts at column 41. With `GIT_TRACE_BARE` set the first two
//! columns are left out.

use std::time::Duration;

use chrono::{Local, NaiveTime, Timelike};

use crate::key::Location;

/// Header for the given local time of day and location.
pub fn header(time: NaiveTime, loc: Location) -> String {
    let micros = time.nanosecond() % 1_000_000_000 / 1_000;
    format!(
        "{:02}:{:02}:{:02}.{:06} {:<23} ",
        time.hour(),
        time.minute(),
        time.second(),
        micros,
        loc.to_string()
    )
}

/// A complete line, newline-terminated.
pub fn line(bare: bool, loc: Location, msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 48);
    if !bare {
        out.extend_from_slice(header(Local::now().time(), loc).as_bytes());
    }
    out.extend_from_slice(msg);
    if out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    out
}

/// Seconds with nine decimals, as in `performance: 0.000171820 s: ...`.
pub fn seconds(d: Duration) -> String {
    format!("{}.{:09}", d.as_secs(), d.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout() {
        let t = NaiveTime::from_hms_micro_opt(9, 5, 7, 42).unwrap();
        let loc = Location {
            file: "crates/rgit-trace/src/setup.rs",
            line: 31,
        };
        let h = header(t, loc);
        assert_eq!(h, "09:05:07.000042 setup.rs:31             ");
        assert_eq!(h.len(), 16 + 24);
    }

    #[test]
    fn bare_line() {
        let loc = Location {
            file: "x.rs",
            line: 1,
        };
        assert_eq!(line(true, loc, b"setup: cwd: /a"), b"setup: cwd: /a\n");
        assert_eq!(line(true, loc, b"m\n"), b"m\n");
        let l = line(false, loc, b"m");
        assert_eq!(&l[15..], b" x.rs:1                  m\n");
    }

    #[test]
    fn seconds_format() {
        assert_eq!(seconds(Duration::from_nanos(171_820)), "0.000171820");
        assert_eq!(seconds(Duration::new(3, 5)), "3.000000005");
    }
}
