// SPDX-License-Identifier: MIT

//! User-visible message texts reproduced from git for compatibility.
//!
//! Collected here so they are reviewable in one place (see `CLEANROOM.md`).
//! Messages that carry data are in [`crate::PathspecError`]'s `Display`
//! implementation; these are the ones commands print themselves.

/// `""` given as a pathspec.
pub const EMPTY_STRING: &str =
    "empty string is not a valid pathspec. please use . instead if you meant to match all paths";

/// Unmatched pathspec in commands like `add` and `rm` (`fatal:`).
#[must_use]
pub fn did_not_match_any_files(original: &[u8]) -> String {
    format!(
        "pathspec '{}' did not match any files",
        String::from_utf8_lossy(original)
    )
}

/// Unmatched pathspec in `ls-files --error-unmatch`, `checkout`, `commit`
/// (`error:`).
#[must_use]
pub fn did_not_match_known_to_git(original: &[u8]) -> String {
    format!(
        "pathspec '{}' did not match any file(s) known to git",
        String::from_utf8_lossy(original)
    )
}

/// Hint printed after [`did_not_match_known_to_git`] by `ls-files`.
pub const FORGOT_GIT_ADD: &str = "Did you forget to 'git add'?";
