// SPDX-License-Identifier: MIT
//! Messages and usage of `git cherry` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git cherry [-v] [<upstream> [<head> [<limit>]]]"];
pub const HELP_ABBREV: &str = "use <n> digits to display object names";
pub const HELP_VERBOSE: &str = "be verbose";
pub const NO_UPSTREAM: &str =
    "Could not find a tracked remote branch, please specify <upstream> manually.";

pub fn unknown_commit(name: &str) -> String {
    format!("unknown commit {name}")
}
