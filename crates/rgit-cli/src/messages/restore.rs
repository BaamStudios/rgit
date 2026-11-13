// SPDX-License-Identifier: MIT
//! Messages of `git restore` (Documentation/git-restore.adoc) and of the
//! path modes it shares with `git checkout`, reproduced verbatim as the
//! `git` binary prints them.

pub const USAGE: &[&str] = &["git restore [<options>] [--source=<branch>] <file>..."];

pub const HELP_SOURCE: &str = "which tree-ish to checkout from";
pub const HELP_STAGED: &str = "restore the index";
pub const HELP_WORKTREE: &str = "restore the working tree (default)";
pub const HELP_IGNORE_UNMERGED: &str = "ignore unmerged entries";
pub const HELP_OVERLAY: &str = "use overlay mode";

pub const NO_PATHS: &str = "you must specify path(s) to restore";
pub const OURS_WITH_STAGED: &str = "'--ours' or '--theirs' cannot be used with --staged";
pub const MERGE_WITH_STAGED: &str = "'--merge' or '--conflict' cannot be used with --staged";
pub const FROM_TREE: &str =
    "'--merge', '--ours', or '--theirs' cannot be used when checking out of a tree";
pub const INDEX_MODES: &str = "git checkout: --ours/--theirs, --force and --merge are incompatible when\nchecking out of the index.";
pub const OURS_NEEDS_PATHS: &str = "'--ours/--theirs' needs the paths to check out";
pub const OURS_WITH_SWITCHING: &str = "'--ours/--theirs' cannot be used with switching branches";
pub const OVERLAY_WITH_SWITCHING: &str = "'--[no]-overlay' cannot be used with switching branches";
pub const PATCH_WITH_OVERLAY: &str = "options '-p' and '--overlay' cannot be used together";
pub const FROM_FILE_WITH_DETACH: &str =
    "options '--pathspec-from-file' and '--detach' cannot be used together";

pub fn could_not_resolve(name: &str) -> String {
    format!("could not resolve '{name}'")
}
pub fn unknown_conflict_style(style: &str) -> String {
    format!("unknown conflict style '{style}'")
}
/// `--ours` (`side`: "our") or `--theirs` ("their") for an unmerged path
/// without that stage.
pub fn no_version(path: &str, side: &str) -> String {
    format!("path '{path}' does not have {side} version")
}
