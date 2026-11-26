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

/// A cone-mode sparse-checkout file with a pattern of another form.
pub fn unrecognized_pattern(p: &str) -> String {
    format!("unrecognized pattern: '{p}'")
}

/// A cone-mode sparse-checkout file with a negative pattern of another form.
pub fn unrecognized_negative_pattern(p: &str) -> String {
    format!("unrecognized negative pattern: '{p}'")
}

/// A cone-mode sparse-checkout file naming a pattern twice.
pub fn pattern_repeated(p: &str) -> String {
    format!("your sparse-checkout file may have issues: pattern '{p}' is repeated")
}

/// After one of the warnings above.
pub const DISABLING_CONE: &str = "disabling cone pattern matching";
