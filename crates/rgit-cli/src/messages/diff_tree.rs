// SPDX-License-Identifier: MIT
//! Messages and option help of `git diff-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git diff-tree [<options>] <tree-ish> <tree-ish>"];

pub fn bad_object(name: &str) -> String {
    format!("ambiguous argument '{name}': unknown revision or path not in the working tree.")
}
