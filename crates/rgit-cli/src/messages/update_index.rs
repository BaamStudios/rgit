// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git update-index` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git update-index [<options>] [--] [<file>...]"];

pub const HELP_Q: &str = "continue refresh even when index needs update";
pub const HELP_IGNORE_SUBMODULES: &str = "refresh: ignore submodules";
pub const HELP_ADD: &str = "do not ignore new files";
pub const HELP_REPLACE: &str = "let files replace directories and vice-versa";
pub const HELP_REMOVE: &str = "notice files missing from worktree";
pub const HELP_UNMERGED: &str = "refresh even if index contains unmerged entries";
pub const HELP_REFRESH: &str = "refresh stat information";
pub const HELP_REALLY_REFRESH: &str = "like --refresh, but ignore assume-unchanged setting";
pub const HELP_CACHEINFO: &str = "add the specified entry to the index";
pub const HELP_CHMOD: &str = "override the executable bit of the listed files";
pub const HELP_ASSUME_UNCHANGED: &str = "mark files as \"not changing\"";
pub const HELP_NO_ASSUME_UNCHANGED: &str = "clear assumed-unchanged bit";
pub const HELP_SKIP_WORKTREE: &str = "mark files as \"index-only\"";
pub const HELP_NO_SKIP_WORKTREE: &str = "clear skip-worktree bit";
pub const HELP_IGNORE_SKIP_WORKTREE: &str = "do not touch index-only entries";
pub const HELP_INFO_ONLY: &str = "add to index only; do not add content to object database";
pub const HELP_FORCE_REMOVE: &str = "remove named paths even if present in worktree";
pub const HELP_Z: &str = "with --stdin: input lines are terminated by null bytes";
pub const HELP_STDIN: &str = "read list of paths to be updated from standard input";
pub const HELP_INDEX_INFO: &str = "add entries from standard input to the index";
pub const HELP_UNRESOLVE: &str = "repopulate stages #2 and #3 for the listed paths";
pub const HELP_AGAIN: &str = "only update entries that differ from HEAD";
pub const HELP_IGNORE_MISSING: &str = "ignore files missing from worktree";
pub const HELP_VERBOSE: &str = "report actions to standard output";
pub const HELP_CLEAR_RESOLVE_UNDO: &str = "(for porcelains) forget saved unresolved conflicts";
pub const HELP_INDEX_VERSION: &str = "write index in this format";
pub const HELP_SHOW_INDEX_VERSION: &str = "report on-disk index format version";
pub const HELP_SPLIT_INDEX: &str = "enable or disable split index";
pub const HELP_UNTRACKED_CACHE: &str = "enable/disable untracked cache";
pub const HELP_TEST_UNTRACKED_CACHE: &str = "test if the filesystem supports untracked cache";
pub const HELP_FORCE_UNTRACKED_CACHE: &str =
    "enable untracked cache without testing the filesystem";
pub const HELP_FORCE_WRITE: &str = "write out the index even if is not flagged as changed";
pub const HELP_FSMONITOR: &str = "enable or disable file system monitor";
pub const HELP_FSMONITOR_VALID: &str = "mark files as fsmonitor valid";
pub const HELP_NO_FSMONITOR_VALID: &str = "clear fsmonitor valid bit";

pub const CACHEINFO_ARGS: &str = "option 'cacheinfo' expects <mode>,<sha1>,<path>";
pub const CHMOD_ARG: &str = "option 'chmod' expects \"+x\" or \"-x\"";
pub const WRITE_FAILED: &str = "Unable to write new index file";

pub fn must_be_last(opt: &str) -> String {
    format!("option '{opt}' must be the last argument")
}

pub fn verbose_add(path: &str) -> String {
    format!("add '{path}'")
}

pub fn verbose_remove(path: &str) -> String {
    format!("remove '{path}'")
}

pub fn verbose_chmod(flip: char, path: &str) -> String {
    format!("chmod {flip}x '{path}'")
}

pub fn index_version_changed(old: u32, new: u32) -> String {
    format!("index-version: was {old}, set to {new}")
}

pub fn index_version_range(v: i64) -> String {
    format!("index-version {v} not in range: 2..4")
}

pub fn ignoring_path(path: &str) -> String {
    format!("Ignoring path {path}")
}

pub fn unable_to_process(path: &str) -> String {
    format!("Unable to process path {path}")
}

pub fn unable_to_mark(path: &str) -> String {
    format!("Unable to mark file {path}")
}

pub fn missing_add(path: &str) -> String {
    format!("{path}: cannot add to the index - missing --add option?")
}

pub fn missing_remove(path: &str) -> String {
    format!("{path}: does not exist and --remove not passed")
}

pub fn is_directory_tracked(path: &str) -> String {
    format!("{path}: is a directory - add individual files instead")
}

pub fn is_directory(path: &str) -> String {
    format!("{path}: is a directory - add files inside instead")
}

pub fn beyond_symlink(path: &str) -> String {
    format!("'{path}' is beyond a symbolic link")
}

pub fn file_and_directory(path: &str) -> String {
    format!("'{path}' appears as both a file and as a directory")
}

pub fn cacheinfo_cannot_add(path: &str) -> String {
    format!("git update-index: --cacheinfo cannot add {path}")
}

pub fn invalid_path(path: &str) -> String {
    format!("Invalid path '{path}'")
}

pub fn cannot_chmod(flip: char, path: &str) -> String {
    format!("git update-index: cannot chmod {flip}x '{path}'")
}

pub fn null_sha1(path: &str) -> String {
    format!("cache entry has null sha1: {path}")
}

pub fn malformed_index_info(line: &str) -> String {
    format!("malformed index info {line}")
}

pub fn needs_update(path: &str) -> String {
    format!("{path}: needs update")
}

pub fn needs_merge(path: &str) -> String {
    format!("{path}: needs merge")
}

pub fn outside_repository(path: &str, root: &str) -> String {
    format!("'{path}' is outside repository at '{root}'")
}

pub fn badly_quoted(line: &str) -> String {
    format!("line is badly quoted: {line}")
}

/// rgit wording: an option that is not implemented yet.
pub fn not_supported(what: &str) -> String {
    format!("rgit update-index: {what} is not supported yet")
}
