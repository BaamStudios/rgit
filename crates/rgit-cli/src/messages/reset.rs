// SPDX-License-Identifier: MIT
//! Messages of `git reset`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git reset [--mixed | --soft | --hard | --merge | --keep] [-q] [<commit>]",
    "git reset [-q] [<tree-ish>] [--] <pathspec>...",
    "git reset [-q] [--pathspec-from-file [--pathspec-file-nul]] [<tree-ish>]",
    "git reset --patch [<tree-ish>] [--] [<pathspec>...]",
];

pub const HELP_QUIET: &str = "be quiet, only report errors";
pub const HELP_NO_REFRESH: &str = "skip refreshing the index after reset";
pub const HELP_MIXED: &str = "reset HEAD and index";
pub const HELP_SOFT: &str = "reset only HEAD";
pub const HELP_HARD: &str = "reset HEAD, index and working tree";
pub const HELP_MERGE: &str = "reset HEAD, index and working tree";
pub const HELP_KEEP: &str = "reset HEAD but keep local changes";
pub const HELP_RECURSE_SUBMODULES: &str = "control recursive updating of submodules";
pub const HELP_PATCH: &str = "select hunks interactively";
pub const HELP_AUTO_ADVANCE: &str =
    "auto advance to the next file when selecting hunks interactively";
pub const HELP_UNIFIED: &str = "generate diffs with <n> lines context";
pub const HELP_INTER_HUNK: &str =
    "show context between diff hunks up to the specified number of lines";
pub const HELP_INTENT_TO_ADD: &str = "record only the fact that removed paths will be added later";
pub const HELP_PATHSPEC_FROM_FILE: &str = "read pathspec from file";
pub const HELP_PATHSPEC_FILE_NUL: &str =
    "with --pathspec-from-file, pathspec elements are separated with NUL character";

pub const UNSTAGED_HEADER: &str = "Unstaged changes after reset:";
pub const MIXED_WITH_PATHS: &str =
    "--mixed with paths is deprecated; use 'git reset -- <paths>' instead.";
pub const SOFT_IN_MERGE: &str = "Cannot do a soft reset in the middle of a merge.";
pub const N_REQUIRES_MIXED: &str = "the option '-N' requires '--mixed'";
pub const PATCH_WITH_MODE: &str =
    "options '--patch' and '--{hard,mixed,soft}' cannot be used together";
pub const FROM_FILE_WITH_PATCH: &str =
    "options '--pathspec-from-file' and '--patch' cannot be used together";
pub const FROM_FILE_WITH_ARGS: &str =
    "'--pathspec-from-file' and pathspec arguments cannot be used together";
pub const FILE_NUL_WITHOUT_FROM_FILE: &str =
    "the option '--pathspec-file-nul' requires '--pathspec-from-file'";

pub fn not_supported(what: &str) -> String {
    format!("rgit: 'git reset {what}' is not implemented yet")
}

pub fn ambiguous(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

pub fn both_revision_and_filename(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': both revision and filename\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

pub fn not_a_commit(arg: &str) -> String {
    format!("Could not parse object '{arg}'.")
}

pub fn wrong_kind(oid: &str, kind: &str) -> String {
    format!("error: object {oid} is a {kind}, not a commit")
}

pub fn failed_to_resolve(arg: &str) -> String {
    format!("Failed to resolve '{arg}' as a valid revision.")
}

pub fn failed_to_resolve_tree(arg: &str) -> String {
    format!("Failed to resolve '{arg}' as a valid tree.")
}

pub fn with_paths(mode: &str) -> String {
    format!("Cannot do {mode} reset with paths.")
}

pub fn bare(mode: &str) -> String {
    format!("{mode} reset is not allowed in a bare repository")
}

pub fn could_not_open(path: &str, err: &str) -> String {
    format!("could not open '{path}' for reading: {err}")
}

pub fn moving_to(arg: &str) -> String {
    format!("reset: moving to {arg}")
}

pub const UPDATING_ORIG_HEAD: &str = "updating ORIG_HEAD";

pub fn could_not_reset_index(rev: &str) -> String {
    format!("Could not reset index file to revision '{rev}'.")
}
