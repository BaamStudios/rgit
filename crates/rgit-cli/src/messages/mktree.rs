// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git mktree` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub use super::objects::*;

pub const MKTREE_USAGE: &[&str] = &["git mktree [-z] [--missing] [--batch]"];
pub const MKTREE_HELP_Z: &str = "input is NUL terminated";
pub const MKTREE_HELP_MISSING: &str = "allow missing objects";
pub const MKTREE_HELP_BATCH: &str = "allow creation of more than one tree";
pub const BLANK_LINE_ONLY_BATCH: &str = "input format error: (blank line only valid in batch mode)";
pub const INVALID_QUOTING: &str = "invalid quoting";

pub fn input_format_error(line: &str) -> String {
    format!("input format error: {line}")
}

pub fn path_contains_slash(path: &str) -> String {
    format!("path {path} contains slash")
}

pub fn mode_type_mismatch(path: &str, given: &str, mode_type: &str) -> String {
    format!("entry '{path}' object type ({given}) doesn't match mode type ({mode_type})")
}

pub fn entry_unavailable(path: &str, oid: &str) -> String {
    format!("entry '{path}' object {oid} is unavailable")
}

pub fn entry_wrong_type(path: &str, oid: &str, actual: &str, given: &str) -> String {
    format!("entry '{path}' object {oid} is a {actual} but specified type was ({given})")
}
