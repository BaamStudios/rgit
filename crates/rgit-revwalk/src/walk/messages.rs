// SPDX-License-Identifier: MIT
//! User-visible messages of the walk (CLEANROOM.md: reproduced verbatim
//! where scripts compare them). The regular-expression reasons are the
//! texts of the POSIX `regerror` messages the reference git prints.

pub const REGEX_INVALID: &str = "Invalid regular expression";
pub const REGEX_UNMATCHED_PAREN: &str = "Unmatched ( or \\(";
pub const REGEX_UNMATCHED_BRACE: &str = "Unmatched \\{";
pub const REGEX_BAD_PRECEDING: &str = "Invalid preceding regular expression";
pub const REGEX_BAD_INTERVAL: &str = "Invalid content of \\{\\}";
pub const REGEX_BAD_CLASS: &str = "Invalid character class name";

/// A pattern of `--grep` (`header` false) or `--author`/`--committer`
/// that does not compile.
pub fn bad_pattern(header: bool, pattern: &str, reason: &str) -> String {
    let origin = if header { "header" } else { "command line" };
    format!("{origin}, '{pattern}': {reason}")
}
