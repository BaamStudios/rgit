// SPDX-License-Identifier: MIT
//! Messages and usage of `git last-modified` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git last-modified [--recursive] [--show-trees] [--max-depth=<depth>] [-z]\n                         [<revision-range>] [[--] <pathspec>...]",
];
pub const H_RECURSIVE: &str = "recurse into subtrees";
pub const H_SHOW_TREES: &str = "show tree entries when recursing into subtrees";
pub const H_MAX_DEPTH: &str = "maximum tree depth to recurse";
pub const H_Z: &str = "lines are separated with NUL character";

pub const ONE_COMMIT: &str = "last-modified can only operate on one commit at a time";

pub fn unknown_argument(arg: &str) -> String {
    format!("unknown last-modified argument: {arg}")
}

pub fn not_commitish(arg: &str, kind: &str) -> String {
    format!("revision argument '{arg}' is a {kind}, not a commit-ish")
}
