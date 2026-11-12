// SPDX-License-Identifier: MIT
//! Dates the way git reads and writes them.
//!
//! * git stores a time as `<seconds since the epoch> <±hhmm>`; here that is a
//!   `u64` plus an `i32` offset in the same decimal `hhmm` form (`+0200` is
//!   `200`, `-0130` is `-130`).
//! * [`show_date`] renders it in every `--date=` mode ([`parse_date_mode`]).
//! * [`parse_date`] is the strict parser (`GIT_AUTHOR_DATE`, `commit --date`),
//!   [`approxidate`] the fuzzy one (`--since`, `@{yesterday}`, expiry settings).
//!
//! Everything that depends on the current time or the local time zone takes a
//! [`DateContext`].

mod civil;
mod context;
mod format;
mod parse;
mod strftime;

pub use context::{DateContext, TEST_DATE_NOW_ENV, now_from_env, secs_to_tz, tz_to_minutes};
pub use format::{DateMode, DateStyle, parse_date_mode, show_date, show_date_relative};
pub use jiff::tz::TimeZone;
pub use parse::{approxidate, approxidate_careful, parse_date, parse_expiry_date};

/// Errors of this crate. The `Display` texts are git's messages (without the
/// `fatal: `/`error: ` prefix).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DateError {
    #[error("unknown date format {0}")]
    UnknownFormat(String),
    #[error("date format missing colon separator: {0}")]
    MissingColon(String),
    #[error("Timestamp before Unix epoch: {time} {tz}")]
    BeforeEpoch { time: u64, tz: i32 },
    #[error("'{0}' is not a valid timestamp")]
    InvalidTimestamp(String),
}

/// Render a git timestamp as `<epoch> <±hhmm>` (git's internal format).
pub fn format_raw(time: u64, tz: i32) -> String {
    let sign = if tz < 0 { '-' } else { '+' };
    format!("{time} {sign}{:04}", tz.unsigned_abs())
}
