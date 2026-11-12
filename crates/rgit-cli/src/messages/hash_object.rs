// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git hash-object` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub use super::objects::*;

pub const HASH_OBJECT_USAGE: &[&str] = &[
    "git hash-object [-t <type>] [-w] [--path=<file> | --no-filters]\n                       [--stdin [--literally]] [--] <file>...",
    "git hash-object [-t <type>] [-w] --stdin-paths [--no-filters]",
];
pub const HASH_OBJECT_HELP_T: &str = "object type";
pub const HASH_OBJECT_HELP_W: &str = "write the object into the object database";
pub const HASH_OBJECT_HELP_STDIN: &str = "read the object from stdin";
pub const HASH_OBJECT_HELP_STDIN_PATHS: &str = "read file names from stdin";
pub const HASH_OBJECT_HELP_NO_FILTERS: &str = "store file as is without filters";
pub const HASH_OBJECT_HELP_LITERALLY: &str =
    "just hash any random garbage to create corrupt objects for debugging Git";
pub const HASH_OBJECT_HELP_PATH: &str = "process file as it were from this path";
pub const MULTIPLE_STDIN: &str = "Multiple --stdin arguments are not supported";
pub const STDIN_PATHS_WITH_STDIN: &str = "Can't use --stdin-paths with --stdin";
pub const STDIN_PATHS_WITH_FILES: &str = "Can't specify files with --stdin-paths";
pub const STDIN_PATHS_WITH_PATH: &str = "Can't use --stdin-paths with --path";
pub const PATH_WITH_NO_FILTERS: &str = "Can't use --path with --no-filters";
pub const REFUSING_MALFORMED: &str = "refusing to create malformed object";

pub fn object_fails_fsck(finding: &str) -> String {
    format!("object fails fsck: {finding}")
}

pub fn could_not_open_for_reading(path: &str, err: &str) -> String {
    format!("could not open '{path}' for reading: {err}")
}

pub fn unable_to_hash(path: &str) -> String {
    format!("Unable to hash {path}")
}

pub const LINE_BADLY_QUOTED: &str = "line is badly quoted";
