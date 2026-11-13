// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git read-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git read-tree [(-m [--trivial] [--aggressive] | --reset | --prefix=<prefix>)\n              [-u | -i]] [--index-output=<file>] [--no-sparse-checkout]\n              (--empty | <tree-ish1> [<tree-ish2> [<tree-ish3>]])",
];

pub const HELP_INDEX_OUTPUT: &str = "write resulting index to <file>";
pub const HELP_EMPTY: &str = "only empty the index";
pub const HELP_VERBOSE: &str = "be verbose";
pub const GROUP_MERGING: &str = "Merging";
pub const HELP_MERGE: &str = "perform a merge in addition to a read";
pub const HELP_TRIVIAL: &str = "3-way merge if no file level merging required";
pub const HELP_AGGRESSIVE: &str = "3-way merge in presence of adds and removes";
pub const HELP_RESET: &str = "same as -m, but discard unmerged entries";
pub const HELP_PREFIX: &str = "read the tree into the index under <subdirectory>/";
pub const HELP_UPDATE: &str = "update working tree with merge result";
pub const HELP_EXCLUDE: &str = "allow explicitly ignored files to be overwritten";
pub const HELP_INDEX_ONLY: &str = "don't check the working tree after merging";
pub const HELP_DRY_RUN: &str = "don't update the index or the work tree";
pub const HELP_NO_SPARSE: &str = "skip applying sparse checkout filter";
pub const HELP_SPARSE: &str = "opposite of --no-sparse-checkout";
pub const HELP_DEBUG_UNPACK: &str = "debug unpack-trees";
pub const HELP_RECURSE: &str = "control recursive updating of submodules";
pub const HELP_QUIET: &str = "suppress feedback messages";

pub fn not_valid_object_name(name: &str) -> String {
    format!("Not a valid object name {name}")
}

pub const EMPTY_DEPRECATED: &str =
    "read-tree: emptying the index with no arguments is deprecated; use --empty";
pub const WHICH_ONE: &str = "Which one? -m, --reset, or --prefix?";
pub const EXCLUDE_NEEDS_U: &str = "--exclude-per-directory is meaningless unless -u";
pub const EXCLUDE_GITIGNORE: &str = "--exclude-per-directory argument must be .gitignore";
pub const TOO_MANY_TREES: &str = "I cannot read more than 8 trees";
pub const U_AND_I: &str = "-u and -i at the same time makes no sense";
pub const U_MEANINGLESS: &str = "-u is meaningless without -m, --reset, or --prefix";
pub const I_MEANINGLESS: &str = "-i is meaningless without -m, --reset, or --prefix";
pub const NEED_TREE: &str = "you must specify at least one tree to merge";
pub const EMPTY_CONTRADICTS: &str = "passing trees as arguments contradicts --empty";
pub const UNABLE_TO_WRITE_INDEX: &str = "unable to write new index file";

/// rgit's own: features of read-tree that are not implemented yet.
pub const THREE_WAY_UNSUPPORTED: &str = "rgit: read-tree with three trees is not implemented yet";
pub const PREFIX_UNSUPPORTED: &str = "rgit: read-tree --prefix is not implemented yet";
pub const OVERLAY_UNSUPPORTED: &str =
    "rgit: read-tree with several trees and without -m is not implemented yet";

/// rgit's own: the work tree was written without eol/filter conversion.
pub const ATTRIBUTES_NOT_APPLIED: &str =
    "rgit: eol/filter attributes are not applied yet; files were checked out unconverted";
