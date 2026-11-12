// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git check-ignore` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git check-ignore [<options>] <pathname>...",
    "git check-ignore [<options>] --stdin",
];

pub const HELP_QUIET: &str = "suppress progress reporting";
pub const HELP_VERBOSE: &str = "be verbose";
pub const HELP_STDIN: &str = "read file names from stdin";
pub const HELP_Z: &str = "terminate input and output records by a NUL character";
pub const HELP_NON_MATCHING: &str = "show non-matching input paths";
pub const HELP_NO_INDEX: &str = "ignore index when checking";

pub const STDIN_WITH_PATHS: &str = "cannot specify pathnames with --stdin";
pub const Z_WITHOUT_STDIN: &str = "-z only makes sense with --stdin";
pub const NO_PATH: &str = "no path specified";
pub const QUIET_SINGLE: &str = "--quiet is only valid with a single pathname";
pub const QUIET_VERBOSE: &str = "cannot have both --quiet and --verbose";
pub const NON_MATCHING_VERBOSE: &str = "--non-matching is only valid with --verbose";
pub const BADLY_QUOTED: &str = "line is badly quoted";

pub fn beyond_symlink(path: &str) -> String {
    format!("pathspec '{path}' is beyond a symbolic link")
}

pub fn in_submodule(path: &str, submodule: &str) -> String {
    format!("Pathspec '{path}' is in submodule '{submodule}'")
}
