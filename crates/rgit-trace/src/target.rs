// SPDX-License-Identifier: MIT
//! Parsing of a `GIT_TRACE*` variable value into a destination.
//!
//! Rules (Documentation/git.adoc, ENVIRONMENT, and observed from git 2.56.0):
//!
//! * unset, empty, `0`, `false` (any case): disabled
//! * `1`, `2`, `true` (any case): standard error
//! * a single digit `3`..=`9`: that already open file descriptor
//!   (`03`, `10` and the like are rejected)
//! * a value starting with `/`: append to that file
//! * anything else: a warning is printed and tracing stays off
//!
//! `af_unix:` sockets are only documented for trace2; git 2.56.0 rejects
//! them for the `GIT_TRACE*` family, so rgit does the same.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Where a trace key writes to, as requested by its environment value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Tracing is off.
    Disabled,
    /// Write to standard error.
    Stderr,
    /// Write to an inherited file descriptor (3..=9).
    Fd(i32),
    /// Append to a file at this absolute path.
    File(PathBuf),
    /// The value was not understood; tracing is off and a warning is due.
    Unknown(String),
}

impl Target {
    /// Interpret the value of a trace variable. `None` means unset.
    pub fn parse(value: Option<&OsStr>) -> Target {
        let Some(value) = value else {
            return Target::Disabled;
        };
        let bytes = value.as_encoded_bytes();
        if bytes.is_empty() || bytes == b"0" || bytes.eq_ignore_ascii_case(b"false") {
            return Target::Disabled;
        }
        if bytes == b"1" || bytes == b"2" || bytes.eq_ignore_ascii_case(b"true") {
            return Target::Stderr;
        }
        if let [d @ b'3'..=b'9'] = bytes {
            return Target::Fd(i32::from(d - b'0'));
        }
        if bytes.first() == Some(&b'/') {
            return Target::File(PathBuf::from(value));
        }
        Target::Unknown(value.to_string_lossy().into_owned())
    }

    /// Whether this target produces output.
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Target::Disabled | Target::Unknown(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Target {
        Target::parse(Some(OsStr::new(s)))
    }

    #[test]
    fn values() {
        assert_eq!(Target::parse(None), Target::Disabled);
        for off in ["", "0", "false", "FALSE", "False"] {
            assert_eq!(p(off), Target::Disabled, "{off:?}");
        }
        for on in ["1", "2", "true", "TRUE", "True"] {
            assert_eq!(p(on), Target::Stderr, "{on:?}");
        }
        for fd in 3..=9 {
            assert_eq!(p(&fd.to_string()), Target::Fd(fd));
        }
        assert_eq!(p("/tmp/x"), Target::File(PathBuf::from("/tmp/x")));
        for bad in [
            "10",
            "03",
            "01",
            "02",
            "yes",
            "-1",
            "relative",
            "af_unix:/x",
            " 1",
        ] {
            assert_eq!(p(bad), Target::Unknown(bad.to_string()), "{bad:?}");
        }
    }
}
