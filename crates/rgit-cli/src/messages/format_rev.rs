// SPDX-License-Identifier: MIT
//! Messages and usage of `git format-rev` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "(EXPERIMENTAL!) git format-rev --stdin-mode=<mode> --format=<pretty> [--[no-]notes=<ref>] [-z] [--[no-]null-output] [--[no-]null-input]",
];
pub const H_FORMAT: &str = "pretty format to use";
pub const H_STDIN_MODE: &str = "how revs are processed";
pub const H_NOTES: &str = "display notes for pretty format";
pub const H_NULL: &str = "Use NUL for input and output termination";
pub const H_NULL_INPUT: &str = "Use NUL for input termination";
pub const H_NULL_OUTPUT: &str = "Use NUL for output termination";

pub const FORMAT_REQUIRED: &str = "'--format' is required";
pub const MODE_REQUIRED: &str = "'--stdin-mode' is required";
pub const BAD_MODE: &str = "'--stdin-mode' needs to be either text, revs, or rev";
pub const TOO_MANY_ARGUMENTS: &str = "too many arguments";

pub fn no_object_name(rev: &str) -> String {
    format!("Could not get object name for {rev}. Skipping.")
}

pub fn no_object(rev: &str) -> String {
    format!("Could not get object for {rev}. Skipping.")
}

pub fn no_commit(rev: &str) -> String {
    format!("Could not get commit for {rev}. Skipping.")
}
