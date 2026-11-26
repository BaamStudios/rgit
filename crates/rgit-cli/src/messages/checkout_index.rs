// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git checkout-index` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here). The
//! per-path messages live in `rgit_unpack::messages`.

pub const USAGE: &[&str] = &["git checkout-index [<options>] [--] [<file>...]"];

pub const HELP_ALL: &str = "check out all files in the index";
pub const HELP_IGNORE_SKIP: &str = "do not skip files with skip-worktree set";
pub const HELP_FORCE: &str = "force overwrite of existing files";
pub const HELP_QUIET: &str = "no warning for existing files and files not in index";
pub const HELP_NO_CREATE: &str = "don't checkout new files";
pub const HELP_CREATE: &str = "opposite of --no-create";
pub const HELP_INDEX: &str = "update stat information in the index file";
pub const HELP_Z: &str = "paths are separated with NUL character";
pub const HELP_STDIN: &str = "read list of paths from the standard input";
pub const HELP_TEMP: &str = "write the content to temporary files";
pub const HELP_PREFIX: &str = "when creating files, prepend <string>";
pub const HELP_STAGE: &str = "copy out the files from named stage";

pub const MIX_ALL: &str = "git checkout-index: don't mix '--all' and explicit filenames";
pub const MIX_STDIN: &str = "git checkout-index: don't mix '--stdin' and explicit filenames";

pub const BAD_STAGE: &str = "stage should be between 1 and 3 or all";
pub const ALL_AND_NO_TEMP: &str = "options '--stage=all' and '--no-temp' cannot be used together";

/// `git checkout-index: <path> does not exist at stage <n>`.
pub fn not_at_stage(path: &str, stage: u8) -> String {
    format!("git checkout-index: {path} does not exist at stage {stage}")
}

/// `unable to read sha1 file of <path> (<oid>)`.
pub fn unable_to_read(path: &str, oid: &str) -> String {
    format!("unable to read sha1 file of {path} ({oid})")
}

/// `cannot create temporary submodule <path>` (`--temp` on a gitlink).
pub fn temporary_submodule(path: &str) -> String {
    format!("cannot create temporary submodule {path}")
}

/// A path whose entry is marked skip-worktree.
pub fn skip_worktree_enabled(path: &str) -> String {
    format!(
        "git checkout-index: {path} has skip-worktree enabled; use '--ignore-skip-worktree-bits' to checkout"
    )
}

/// A sparse directory entry of a sparse index.
pub fn sparse_directory(path: &str) -> String {
    format!("git checkout-index: {path} is a sparse directory")
}
