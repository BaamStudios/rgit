// SPDX-License-Identifier: MIT

//! User-visible messages of this crate, verbatim as git prints them (see
//! CLEANROOM.md, "User-visible messages"). Callers add the `warning: `
//! prefix.

/// An in-tree pattern file larger than [`crate::ignore::MAX_PATTERN_FILE`].
pub fn too_large_pattern_file(path: &str) -> String {
    format!("ignoring excessively large pattern file: {path}")
}

/// A pattern file that exists but could not be read.
pub fn unable_to_access(path: &str, err: &str) -> String {
    format!("unable to access '{path}': {err}")
}
