// SPDX-License-Identifier: MIT
//! Messages and option help of `git diff-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git diff-tree [<options>] <tree-ish> <tree-ish>"];

pub const HELP_R: &str = "recurse into subtrees";
pub const HELP_T: &str = "show tree entry itself before recursing";
pub const HELP_P: &str = "generate patch";
pub const HELP_Z: &str = "terminate entries with NUL byte";
pub const HELP_NAME_ONLY: &str = "show only names of changed files";
pub const HELP_NAME_STATUS: &str = "show only names and status of changed files";

pub fn bad_object(name: &str) -> String {
    format!("ambiguous argument '{name}': unknown revision or path not in the working tree.")
}

pub fn could_not_read(id: &str) -> String {
    format!("unable to read tree ({id})")
}
