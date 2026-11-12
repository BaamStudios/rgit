// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git ls-files` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git ls-files [<options>] [<file>...]"];

pub const HELP_Z: &str = "separate paths with the NUL character";
pub const HELP_T: &str = "identify the file status with tags";
pub const HELP_V: &str = "use lowercase letters for 'assume unchanged' files";
pub const HELP_F: &str = "use lowercase letters for 'fsmonitor clean' files";
pub const HELP_CACHED: &str = "show cached files in the output (default)";
pub const HELP_DELETED: &str = "show deleted files in the output";
pub const HELP_MODIFIED: &str = "show modified files in the output";
pub const HELP_OTHERS: &str = "show other files in the output";
pub const HELP_IGNORED: &str = "show ignored files in the output";
pub const HELP_STAGE: &str = "show staged contents' object name in the output";
pub const HELP_KILLED: &str = "show files on the filesystem that need to be removed";
pub const HELP_DIRECTORY: &str = "show 'other' directories' names only";
pub const HELP_EOL: &str = "show line endings of files";
pub const HELP_EMPTY_DIRECTORY: &str = "don't show empty directories";
pub const HELP_UNMERGED: &str = "show unmerged files in the output";
pub const HELP_RESOLVE_UNDO: &str = "show resolve-undo information";
pub const HELP_EXCLUDE: &str = "skip files matching pattern";
pub const HELP_EXCLUDE_FROM: &str = "read exclude patterns from <file>";
pub const HELP_EXCLUDE_PER_DIR: &str = "read additional per-directory exclude patterns in <file>";
pub const HELP_EXCLUDE_STANDARD: &str = "add the standard git exclusions";
pub const HELP_FULL_NAME: &str = "make the output relative to the project top directory";
pub const HELP_RECURSE_SUBMODULES: &str = "recurse through submodules";
pub const HELP_ERROR_UNMATCH: &str = "if any <file> is not in the index, treat this as an error";
pub const HELP_WITH_TREE: &str = "pretend that paths removed since <tree-ish> are still present";
pub const HELP_ABBREV: &str = "use <n> digits to display object names";
pub const HELP_DEBUG: &str = "show debugging data";
pub const HELP_DEDUP: &str = "suppress duplicate entries";
pub const HELP_SPARSE: &str = "show sparse directories in the presence of a sparse index";
pub const HELP_FORMAT: &str = "format to use for the output";

pub const FORMAT_EXCLUSIVE: &str =
    "--format cannot be used with -s, -o, -k, -t, --resolve-undo, --deduplicate, --eol";
pub const IGNORED_NEEDS_MODE: &str = "ls-files -i must be used with either -o or -c";
pub const IGNORED_NEEDS_EXCLUDES: &str = "ls-files --ignored needs some exclude pattern";
pub const WITH_TREE_AND_STAGE: &str =
    "options 'ls-files --with-tree' and '-s/-u' cannot be used together";
pub const RECURSE_UNSUPPORTED: &str = "ls-files --recurse-submodules unsupported mode";
pub const FORGOT_ADD: &str = "Did you forget to 'git add'?";

pub fn bad_exclude_file(file: &str) -> String {
    format!("cannot use {file} as an exclude file")
}

pub fn tree_ish_not_found(name: &str) -> String {
    format!("tree-ish {name} not found.")
}

pub fn pathspec_unmatched(spec: &str) -> String {
    format!("pathspec '{spec}' did not match any file(s) known to git")
}

/// rgit wording: an option that is not implemented yet.
pub fn not_supported(what: &str) -> String {
    format!("rgit ls-files: {what} is not supported yet")
}

pub fn no_object_info(id: &str) -> String {
    format!("could not get object info about '{id}'")
}
