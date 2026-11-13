// SPDX-License-Identifier: MIT

//! User-visible messages of this crate, verbatim as git prints them (see
//! CLEANROOM.md, "User-visible messages"). Each function returns the whole
//! text git writes to standard error, including a `warning: ` prefix where
//! git prints one (some of these messages have none).

/// An attribute name that is malformed or uses the reserved `builtin_`
/// namespace.
pub fn invalid_name(name: &[u8], source: &str, line: usize) -> String {
    format!(
        "{} is not a valid attribute name: {source}:{line}",
        String::from_utf8_lossy(name)
    )
}

/// A macro definition outside a top-level attributes file. `text` is the
/// line from `[attr]` on.
pub fn macro_not_allowed(text: &[u8], source: &str, line: usize) -> String {
    format!(
        "{} not allowed: {source}:{line}",
        String::from_utf8_lossy(text)
    )
}

/// A pattern starting with `!`.
pub const NEGATIVE_PATTERN: &str = "warning: Negative patterns are ignored in git attributes\nUse '\\!' for literal leading exclamation.";

/// A line longer than [`crate::MAX_LINE_LEN`].
pub fn line_too_long(line: usize) -> String {
    format!("warning: ignoring overly long attributes line {line}")
}

/// A file larger than [`crate::MAX_FILE_SIZE`].
pub fn file_too_large(path: &str) -> String {
    format!("warning: ignoring overly large gitattributes file '{path}'")
}

/// A blob larger than [`crate::MAX_FILE_SIZE`].
pub fn blob_too_large(path: &str) -> String {
    format!("warning: ignoring overly large gitattributes blob '{path}'")
}

/// An attributes file that exists but cannot be read.
pub fn unable_to_access(path: &str, err: &str) -> String {
    format!("warning: unable to access '{path}': {err}")
}
