// SPDX-License-Identifier: MIT
//! Messages about the index file and its configuration (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

/// `GIT_INDEX_VERSION` is not 2, 3 or 4.
pub fn invalid_version_env(using: u32) -> String {
    format!("GIT_INDEX_VERSION set, but the value is invalid.\nUsing version {using}")
}

/// A numeric configuration value is too large.
pub fn value_exceeds_maximum(key: &str, max: i64) -> String {
    format!("value for '{key}' exceeds maximum of {max}")
}

/// `index.version` is not 2, 3 or 4.
pub fn invalid_version_config(using: u32) -> String {
    format!("index.version set, but the value is invalid.\nUsing version {using}")
}
