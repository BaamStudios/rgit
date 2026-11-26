// SPDX-License-Identifier: MIT
//! Messages of the legacy merge plumbing: `merge-index`,
//! `merge-one-file`, `unpack-file` and the strategy entry points
//! `merge-ours`, `merge-resolve`, `merge-octopus` and `merge-subtree`
//! (CLEANROOM.md: user-visible strings are reproduced verbatim, taken from
//! the behaviour of the `git` binary, and collected here).

pub const MERGE_INDEX_USAGE: &str =
    "usage: git merge-index [-o] [-q] <merge-program> (-a | [--] [<filename>...])";
pub fn merge_index_not_in_cache(path: &str) -> String {
    format!("git merge-index: {path} not in the cache")
}
pub fn merge_index_unknown_option(opt: &str) -> String {
    format!("git merge-index: unknown option {opt}")
}
/// `fatal: cannot exec '<program>': <reason>` (a program given with a
/// directory part that cannot be started).
pub fn cannot_exec(program: &str, reason: &str) -> String {
    format!("cannot exec '{program}': {reason}")
}
/// `error: cannot run <program>: <reason>` (a program not found in `$PATH`).
pub fn cannot_run(program: &str, reason: &str) -> String {
    format!("cannot run {program}: {reason}")
}

pub const UNPACK_FILE_USAGE: &str = "usage: git unpack-file <blob>";
pub fn not_a_valid_object(name: &str) -> String {
    format!("Not a valid object name {name}")
}
pub fn unable_to_read_blob(id: &str) -> String {
    format!("unable to read blob object {id}")
}

/// The usage of `git merge-one-file`, printed on standard output.
pub const MERGE_ONE_FILE_USAGE: &str = "usage: git merge-one-file <orig blob> <our blob> <their blob> <path> <orig mode> <our mode> <their mode>\n\
\n\
usage: git merge-one-file <orig blob> <our blob> <their blob> <path> <orig mode> <our mode> <their mode>\n\
\n\
Blob ids and modes should be empty for missing files.";
pub const NEED_WORK_TREE: &str = "fatal: this operation must be run in a work tree";
/// The second line `merge-one-file` prints without a work tree (the
/// variable is not expanded, observed).
pub const CANNOT_CHDIR_TOPLEVEL: &str = "Cannot chdir to $cdup, the toplevel of the working tree";
pub fn removing(path: &str) -> String {
    format!("Removing {path}")
}
pub fn adding(path: &str) -> String {
    format!("Adding {path}")
}
pub fn deleted_but_permissions(path: &str) -> String {
    format!(
        "ERROR: File {path} deleted on one branch but had its\nERROR: permissions changed on the other."
    )
}
pub fn untracked_overwritten(path: &str) -> String {
    format!("ERROR: untracked {path} is overwritten by the merge.")
}
pub fn added_identically_permissions(path: &str, ours: &str, theirs: &str) -> String {
    format!(
        "ERROR: File {path} added identically in both branches,\nERROR: but permissions conflict {ours}->{theirs}."
    )
}
pub fn submodule_conflict(path: &str) -> String {
    format!("ERROR: {path}: Not merging conflicting submodule changes.")
}

pub const MERGE_OURS_USAGE: &str = "usage: git merge-ours <base>... -- HEAD <remote>...";
pub const MERGE_SUBTREE_USAGE: &str = "usage: git merge-subtree <base>... -- <head> <remote> ...";
pub const MERGE_RECURSIVE_OURS_USAGE: &str =
    "usage: git merge-recursive-ours <base>... -- <head> <remote> ...";
pub const MERGE_RECURSIVE_THEIRS_USAGE: &str =
    "usage: git merge-recursive-theirs <base>... -- <head> <remote> ...";
/// The usage of the script strategies (the trailing space is git's).
pub fn script_usage(name: &str) -> String {
    format!("usage: git {name} ")
}
pub const SIMPLE_MERGE_FAILED_OCTOPUS: &str = "Simple merge did not work, trying automatic merge.";
