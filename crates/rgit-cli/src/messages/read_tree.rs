// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git read-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git read-tree [(-m [--trivial] [--aggressive] | --reset | --prefix=<prefix>)\n              [-u | -i]] [--index-output=<file>] [--no-sparse-checkout]\n              (--empty | <tree-ish1> [<tree-ish2> [<tree-ish3>]])",
];

pub const HELP_EMPTY: &str = "only empty the index";

pub fn not_valid_object_name(name: &str) -> String {
    format!("Not a valid object name {name}")
}

pub fn failed_to_unpack(name: &str) -> String {
    format!("failed to unpack tree object {name}")
}

pub const EMPTY_DEPRECATED: &str =
    "read-tree: emptying the index with no arguments is deprecated; use --empty";
