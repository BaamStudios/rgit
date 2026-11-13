// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git merge-base` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git merge-base [-a | --all] <commit> <commit>...",
    "git merge-base [-a | --all] --octopus <commit>...",
    "git merge-base --is-ancestor <commit> <commit>",
    "git merge-base --independent <commit>...",
    "git merge-base --fork-point <ref> [<commit>]",
];

pub const HELP_ALL: &str = "output all common ancestors";
pub const HELP_OCTOPUS: &str = "find ancestors for a single n-way merge";
pub const HELP_INDEPENDENT: &str = "list revs not reachable from others";
pub const HELP_IS_ANCESTOR: &str = "is the first one ancestor of the other?";
pub const HELP_FORK_POINT: &str = "find where <commit> forked from reflog of <ref>";

pub const IS_ANCESTOR_TWO: &str = "--is-ancestor takes exactly two commits";
pub const FORK_POINT_NOT_SUPPORTED: &str = "rgit: merge-base: --fork-point is not supported yet";

pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn not_a_valid_object(name: &str) -> String {
    format!("Not a valid object name {name}")
}

pub fn not_a_valid_commit(name: &str) -> String {
    format!("Not a valid commit name {name}")
}
