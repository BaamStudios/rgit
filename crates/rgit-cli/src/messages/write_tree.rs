// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git write-tree` and
//! `git commit-tree` (CLEANROOM.md: user-visible strings are reproduced
//! verbatim and collected here).

pub const WRITE_TREE_USAGE: &[&str] = &["git write-tree [--missing-ok] [--prefix=<prefix>/]"];
pub const HELP_MISSING_OK: &str = "allow missing objects";
pub const HELP_PREFIX: &str = "write tree object for a subdirectory <prefix>";
pub const ERROR_BUILDING_TREES: &str = "git-write-tree: error building trees";

pub fn unmerged(path: &str) -> String {
    format!("{path}: unmerged")
}

pub fn invalid_object(mode: u32, oid: &str, path: &str) -> String {
    format!("invalid object {mode:o} {oid} for '{path}'")
}

pub fn prefix_not_found(prefix: &str) -> String {
    format!("git-write-tree: prefix {prefix} not found")
}

pub const COMMIT_TREE_USAGE: &[&str] = &[
    "git commit-tree <tree> [(-p <parent>)...]",
    "git commit-tree [(-p <parent>)...] [-S[<keyid>]] [(-m <message>)...]\n                [(-F <file>)...] <tree>",
];
pub const HELP_PARENT: &str = "id of a parent commit object";
pub const HELP_MESSAGE: &str = "commit message";
pub const HELP_FILE: &str = "read commit log message from file";

pub fn not_valid_object_name(name: &str) -> String {
    format!("not a valid object name {name}")
}

pub fn could_not_read_file(path: &str, err: &str) -> String {
    format!("could not read log file '{path}': {err}")
}

pub fn duplicate_parent(oid: &str) -> String {
    format!("duplicate parent {oid} ignored")
}

pub const READ_STDIN_FAILED: &str = "could not read log from standard input";
