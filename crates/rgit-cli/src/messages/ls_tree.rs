// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git ls-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git ls-tree [<options>] <tree-ish> [<path>...]"];

pub const HELP_D: &str = "only show trees";
pub const HELP_R: &str = "recurse into subtrees";
pub const HELP_T: &str = "show trees when recursing";
pub const HELP_Z: &str = "terminate entries with NUL byte";
pub const HELP_LONG: &str = "include object size";
pub const HELP_NAME_ONLY: &str = "list only filenames";
pub const HELP_OBJECT_ONLY: &str = "list only objects";
pub const HELP_FULL_NAME: &str = "use full path names";
pub const HELP_FULL_TREE: &str =
    "list entire tree; not just current directory (implies --full-name)";
pub const HELP_FORMAT: &str = "format to use for the output";
pub const HELP_ABBREV: &str = "use <n> digits to display object names";

pub const FORMAT_EXCLUSIVE: &str = "--format can't be combined with other format-altering options";
pub const NOT_A_TREE: &str = "not a tree object";

pub fn not_valid_object_name(name: &str) -> String {
    format!("Not a valid object name {name}")
}

pub fn could_not_read(id: &str) -> String {
    format!("Could not read {id}")
}
