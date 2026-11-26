// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git sparse-checkout`, as printed by
//! the reference git (CLEANROOM.md: user-visible strings are reproduced
//! verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git sparse-checkout (init | list | set | add | reapply | disable | check-rules | clean) [<options>]",
];
pub const USAGE_INIT: &[&str] = &["git sparse-checkout init [--cone] [--[no-]sparse-index]"];
pub const USAGE_LIST: &[&str] = &["git sparse-checkout list"];
pub const USAGE_SET: &[&str] = &[
    "git sparse-checkout set [--[no-]cone] [--[no-]sparse-index] [--skip-checks] (--stdin | <patterns>)",
];
pub const USAGE_ADD: &[&str] = &["git sparse-checkout add [--skip-checks] (--stdin | <patterns>)"];
pub const USAGE_REAPPLY: &[&str] =
    &["git sparse-checkout reapply [--[no-]cone] [--[no-]sparse-index]"];
pub const USAGE_DISABLE: &[&str] = &["git sparse-checkout disable"];
pub const USAGE_CHECK_RULES: &[&str] =
    &["git sparse-checkout check-rules [-z] [--skip-checks][--[no-]cone] [--rules-file <file>]"];
pub const USAGE_CLEAN: &[&str] = &["git sparse-checkout clean [-n|--dry-run]"];

pub const HELP_CONE: &str = "initialize the sparse-checkout in cone mode";
pub const HELP_SPARSE_INDEX: &str = "toggle the use of a sparse index";
pub const HELP_SKIP_CHECKS: &str =
    "skip some sanity checks on the given paths that might give false positives";
pub const HELP_STDIN: &str = "read patterns from standard in";
pub const HELP_Z: &str = "terminate input and output files by a NUL character";
pub const HELP_RULES_CONE: &str =
    "when used with --rules-file interpret patterns as cone mode patterns";
pub const HELP_RULES_FILE: &str = "use patterns in <file> instead of the current ones.";
pub const ARGH_FILE: &str = "file";
pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_FORCE: &str = "force";
pub const HELP_VERBOSE: &str = "report each affected file, not just directories";

pub const NOT_SPARSE: &str = "this worktree is not sparse";
pub const NOT_SPARSE_NO_FILE: &str =
    "this worktree is not sparse (sparse-checkout file may not exist)";
pub const NO_SPARSE_TO_ADD: &str = "no sparse-checkout to add to";
pub const MUST_BE_SPARSE_REAPPLY: &str =
    "must be in a sparse-checkout to reapply sparsity patterns";
pub const MUST_BE_SPARSE_CLEAN: &str = "must be in a sparse-checkout to clean directories";
pub const MUST_BE_CONE_CLEAN: &str = "must be in a cone-mode sparse-checkout to clean directories";
pub const REFUSING_TO_CLEAN: &str =
    "for safety, refusing to clean without one of --force or --dry-run";
pub const NOT_CONE_PATTERNS: &str = "existing sparse-checkout patterns do not use cone mode";
pub const TOPLEVEL_IN_NON_CONE: &str = "please run from the toplevel directory in non-cone mode";
pub const SPARSE_INDEX_CONFLICTS: &str =
    "failed to convert index to a sparse index; resolve merge conflicts and try again";

/// `add`, `rm`, `mv`: paths outside the sparse checkout (then the paths,
/// one per line, and [`UPDATE_SPARSE_PATH_HINT`]).
pub const OUTSIDE_SPARSE_HEADER: &str = "The following paths and/or pathspecs matched paths that exist\noutside of your sparse-checkout definition, so will not be\nupdated in the index:";

/// Advice `updateSparsePath`.
pub const UPDATE_SPARSE_PATH_HINT: &str = "If you intend to update such entries, try one of the following:\n* Use the --sparse option.\n* Disable or modify the sparsity rules.";

pub fn not_a_directory(arg: &str) -> String {
    format!(
        "'{arg}' is not a directory; to treat it as a directory anyway, rerun with --skip-checks"
    )
}

pub fn pass_leading_slash(arg: &str) -> String {
    format!(
        "pass a leading slash before paths such as '{arg}' if you want a single file (see NON-CONE PROBLEMS in the git-sparse-checkout manual)."
    )
}

pub fn untracked_outside_cone(dir: &str) -> String {
    format!("directory '{dir}' contains untracked files, but is not in the sparse-checkout cone")
}

pub fn would_remove(path: &str) -> String {
    format!("Would remove {path}")
}

pub fn removing(path: &str) -> String {
    format!("Removing {path}")
}

pub fn failed_to_remove(path: &str) -> String {
    format!("failed to remove directory '{path}'")
}

pub fn unable_to_load(path: &str) -> String {
    format!("unable to load existing sparse-checkout patterns from '{path}'")
}
