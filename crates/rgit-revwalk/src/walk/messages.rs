// SPDX-License-Identifier: MIT
//! User-visible messages of the walk (CLEANROOM.md: reproduced verbatim
//! where scripts compare them). The regular-expression reasons are the
//! texts of the POSIX `regerror` messages the reference git prints: the
//! words of the C library, which are glibc's on Linux and others on macOS.

/// `glibc` on most platforms, `macos` where the C library of macOS words
/// the error (git prints what `regerror` returns).
const fn regerror(glibc: &'static str, macos: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        macos
    } else {
        glibc
    }
}

pub const REGEX_INVALID: &str = "Invalid regular expression";
/// `REG_EPAREN`.
pub const REGEX_UNMATCHED_PAREN: &str = regerror("Unmatched ( or \\(", "parentheses not balanced");
/// `REG_EBRACE`.
pub const REGEX_UNMATCHED_BRACE: &str = regerror("Unmatched \\{", "braces not balanced");
/// `REG_BADRPT`.
pub const REGEX_BAD_PRECEDING: &str = regerror(
    "Invalid preceding regular expression",
    "repetition-operator operand invalid",
);
/// `REG_BADBR`.
pub const REGEX_BAD_INTERVAL: &str =
    regerror("Invalid content of \\{\\}", "invalid repetition count(s)");
/// `REG_ECTYPE`.
pub const REGEX_BAD_CLASS: &str =
    regerror("Invalid character class name", "invalid character class");
/// `REG_ESUBREG`.
pub const REGEX_BAD_BACKREF: &str =
    regerror("Invalid back reference", "invalid backreference number");

/// A pattern of `--grep` (`header` false) or `--author`/`--committer`
/// that does not compile.
pub fn bad_pattern(header: bool, pattern: &str, reason: &str) -> String {
    let origin = if header { "header" } else { "command line" };
    format!("{origin}, '{pattern}': {reason}")
}
