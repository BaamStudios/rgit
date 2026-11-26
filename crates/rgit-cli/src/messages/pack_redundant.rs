// SPDX-License-Identifier: MIT
//! Messages and usage of `git pack-redundant` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &str =
    "usage: git pack-redundant [--verbose] [--alt-odb] (--all | <pack-filename>...)\n";

pub const ZERO_PACKS: &str = "Zero packs found!";

/// `Bad object ID on stdin: <line>`; the line keeps its line feed.
pub fn bad_object_id(line: &str) -> String {
    format!("Bad object ID on stdin: {line}")
}

pub fn bad_pack_filename(name: &str) -> String {
    format!("Bad pack filename: {name}")
}

pub fn not_in_packed_git(name: &str) -> String {
    format!("Filename {name} not found in packed_git")
}

pub fn alt_packs(n: usize) -> String {
    format!("There are {n} packs available in alt-odbs.\n")
}

pub const SMALLEST_SET: &str = "The smallest (bytewise) set of packs is:\n";

pub fn duplicates(count: usize, kb: u64) -> String {
    format!("containing {count} duplicate objects with a total size of {kb}kb.\n")
}

pub fn unique_considered(n: usize) -> String {
    format!("A total of {n} unique objects were considered.\n")
}

pub const REDUNDANT_HEADER: &str = "Redundant packs (with indexes):\n";

pub fn redundant_total(mb: u64) -> String {
    format!("{mb}MB of redundant packs in total.\n")
}
