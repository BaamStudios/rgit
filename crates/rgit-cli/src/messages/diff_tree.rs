// SPDX-License-Identifier: MIT
//! Messages and option help of `git diff-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git diff-tree [<options>] <tree-ish> <tree-ish>"];

/// `error:` for a `--stdin` tree line without a second tree.
pub const NEED_TWO_TREES: &str = "Need exactly two trees, separated by a space";

/// `error:` for a `--stdin` line naming another kind of object.
pub fn not_commit_or_tree(hex: &str, kind: &str) -> String {
    format!("Object {hex} is a {kind}, not a commit or tree")
}

pub fn bad_object(name: &str) -> String {
    format!("ambiguous argument '{name}': unknown revision or path not in the working tree.")
}
