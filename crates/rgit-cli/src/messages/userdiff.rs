// SPDX-License-Identifier: MIT
//! Messages of diff drivers: hunk-header patterns, textconv and external
//! diff programs.

pub fn invalid_funcname_regex(pattern: &str) -> String {
    format!("Invalid regexp to look for hunk header: {pattern}")
}

pub fn last_negated(pattern: &str) -> String {
    format!("Last expression must not be negated: {pattern}")
}

pub fn external_diff_died(path: &str) -> String {
    format!("external diff died, stopping at {path}")
}

pub const UNABLE_TO_READ_FILES: &str = "unable to read files to diff";
