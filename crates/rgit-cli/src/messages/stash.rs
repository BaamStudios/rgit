// SPDX-License-Identifier: MIT
//! Messages of `git stash`, as printed by the reference git.

pub const USAGE_LIST: &str = "git stash list [<log-options>]";
pub const USAGE_SHOW: &str =
    "git stash show [-u | --include-untracked | --only-untracked] [<diff-options>] [<stash>]";
pub const USAGE_DROP: &str = "git stash drop [-q | --quiet] [<stash>]";
pub const USAGE_POP: &str = "git stash pop [--index] [-q | --quiet] [<stash>]";
pub const USAGE_APPLY: &str = "git stash apply [--index] [-q | --quiet] [--label-ours=<label>] [--label-theirs=<label>] [--label-base=<label>] [<stash>]";
pub const USAGE_BRANCH: &str = "git stash branch <branchname> [<stash>]";
pub const USAGE_PUSH: &str = "git stash [push] [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]\n\
     \x20                [-u | --include-untracked] [-a | --all] [(-m | --message) <message>]\n\
     \x20                [--pathspec-from-file=<file> [--pathspec-file-nul]]\n\
     \x20                [--] [<pathspec>...]";
pub const USAGE_SAVE: &str = "git stash save [-p | --patch] [-S | --staged] [-k | --[no-]keep-index] [-q | --quiet]\n\
     \x20                [-u | --include-untracked] [-a | --all] [<message>]";
pub const USAGE_CLEAR: &str = "git stash clear";
pub const USAGE_CREATE: &str = "git stash create [<message>]";
pub const USAGE_STORE: &str =
    "git stash store [(-m | --message) <message>] [-q | --quiet] <commit>";
pub const USAGE_EXPORT: &str = "git stash export (--print | --to-ref <ref>) [<stash>...]";
pub const USAGE_IMPORT: &str = "git stash import <commit>";

pub const HELP_KEEP_INDEX: &str = "keep index";
pub const HELP_STAGED: &str = "stash staged changes only";
pub const HELP_PATCH: &str = "stash in patch mode";
pub const HELP_AUTO_ADVANCE: &str =
    "auto advance to the next file when selecting hunks interactively";
pub const HELP_UNIFIED: &str = "generate diffs with <n> lines context";
pub const HELP_INTER_HUNK: &str =
    "show context between diff hunks up to the specified number of lines";
pub const HELP_QUIET_MODE: &str = "quiet mode";
pub const HELP_INCLUDE_UNTRACKED: &str = "include untracked files in stash";
pub const HELP_ALL: &str = "include ignore files";
pub const HELP_MESSAGE: &str = "stash message";
pub const HELP_PATHSPEC_FROM_FILE: &str = "read pathspec from file";
pub const HELP_PATHSPEC_FILE_NUL: &str =
    "with --pathspec-from-file, pathspec elements are separated with NUL character";
pub const HELP_SHOW_UNTRACKED: &str = "include untracked files in the stash";
pub const HELP_ONLY_UNTRACKED: &str = "only show untracked files in the stash";
pub const HELP_QUIET: &str = "be quiet, only report errors";
pub const HELP_QUIET_STORE: &str = "be quiet";
pub const HELP_INDEX: &str = "attempt to recreate the index";
pub const HELP_LABEL_OURS: &str = "label for the upstream side in conflict markers";
pub const HELP_LABEL_THEIRS: &str = "label for the stashed side in conflict markers";
pub const HELP_LABEL_BASE: &str = "label for the base in diff3 conflict markers";

pub const NO_INITIAL_COMMIT: &str = "You do not have the initial commit yet";
pub const NO_LOCAL_CHANGES: &str = "No local changes to save";
pub const NO_STAGED_CHANGES: &str = "No staged changes";
pub const NO_ENTRIES: &str = "No stash entries found.";
pub const NO_BRANCH_NAME: &str = "No branch name specified";
pub const STAGED_WITH_UNTRACKED: &str =
    "Can't use --staged and --include-untracked or --all at the same time";
pub const COULD_NOT_WRITE_INDEX: &str = "could not write index";
pub const CANNOT_SAVE_INDEX: &str = "Cannot save the current index state";
pub const CANNOT_SAVE_UNTRACKED: &str = "Cannot save the untracked files";
pub const CANNOT_SAVE_WORKTREE: &str = "Cannot save the current worktree state";
pub const CANNOT_RECORD_WORKTREE: &str = "Cannot record working tree state";
pub const CANNOT_SAVE_STATUS: &str = "Cannot save the current status";
pub const CANNOT_REMOVE_WORKTREE: &str = "Cannot remove worktree changes";
pub const STORE_NEEDS_ONE: &str = "\"git stash store\" requires one <commit> argument";
pub const STORE_DEFAULT_MSG: &str = "Created via \"git stash store\".";
pub const CLEAR_WITH_ARGS: &str = "git stash clear with arguments is unimplemented";
pub const FORGET_ADD: &str = "Did you forget to 'git add'?";
pub const IN_MIDDLE_OF_MERGE: &str = "Cannot apply a stash in the middle of a merge";
pub const CONFLICTS_IN_INDEX: &str = "Conflicts in index. Try without --index.";
pub const COULD_NOT_RESTORE_UNTRACKED: &str = "could not restore untracked files from stash";
pub const ALREADY_UP_TO_DATE: &str = "Already up to date.";
pub const ENTRY_KEPT: &str = "The stash entry is kept in case you need it again.";
pub const LABEL_OURS: &str = "Updated upstream";
pub const LABEL_THEIRS: &str = "Stashed changes";
pub const LABEL_BASE: &str = "Stash base";
pub const NO_BRANCH: &str = "(no branch)";
/// The identity of stash commits when none is configured.
pub const FALLBACK_NAME: &str = "git stash";
pub const FALLBACK_EMAIL: &str = "git@stash";

pub fn saved(msg: &str) -> String {
    format!("Saved working directory and index state {msg}")
}

pub fn wip_on(base: &str) -> String {
    format!("WIP on {base}")
}

pub fn on_branch(branch: &str, msg: &str) -> String {
    format!("On {branch}: {msg}")
}

pub fn index_on(base: &str) -> String {
    format!("index on {base}\n")
}

pub fn untracked_on(base: &str) -> String {
    format!("untracked files on {base}\n")
}

pub fn no_assumed_push(token: &str) -> String {
    format!(
        "subcommand wasn't specified; 'push' can't be assumed due to unexpected token '{token}'"
    )
}

pub fn pathspec_no_match(spec: &str) -> String {
    format!("pathspec '{spec}' did not match any file(s) known to git")
}

pub fn not_valid_reference(rev: &str) -> String {
    format!("{rev} is not a valid reference")
}

pub fn not_stash_like(rev: &str) -> String {
    format!("'{rev}' is not a stash-like commit")
}

pub fn not_stash_reference(rev: &str) -> String {
    format!("'{rev}' is not a stash reference")
}

pub fn too_many_revisions(list: &str) -> String {
    format!("Too many revisions specified:{list}")
}

pub fn dropped(rev: &str, oid: &str) -> String {
    format!("Dropped {rev} ({oid})")
}

pub fn could_not_drop(rev: &str) -> String {
    format!("{rev}: Could not drop stash entry")
}

pub fn not_a_reflog(rev: &str) -> String {
    format!("not a reflog: {rev}")
}

pub fn unrecognized_argument(arg: &str) -> String {
    format!("unrecognized argument: {arg}")
}

pub fn needs_merge(path: &str) -> String {
    format!("{path}: needs merge")
}

pub fn duplicate_untracked(path: &str) -> String {
    format!("worktree and untracked commit have duplicate entries: {path}")
}

pub fn not_supported(what: &str) -> String {
    format!("rgit: 'git stash {what}' is not implemented yet")
}

pub fn entry_not_uptodate(path: &str) -> String {
    format!("Entry '{path}' not uptodate. Cannot merge.")
}

pub const HELP_EXPORT_PRINT: &str = "print the object ID instead of writing it to a ref";
pub const HELP_EXPORT_TO_REF: &str = "save the data to the given ref";
pub const EXPORT_PRINT_OR_REF: &str = "exactly one of --print and --to-ref is required";
pub const REVISION_REQUIRED: &str = "a revision is required";

pub fn no_stash_entry(rev: &str) -> String {
    format!("unable to find stash entry {rev}")
}

pub fn not_a_stash_commit(rev: &str) -> String {
    format!("{rev} does not look like a stash commit")
}

pub fn not_a_valid_revision(rev: &str) -> String {
    format!("not a valid revision: {rev}")
}

pub fn not_valid_export(oid: &str) -> String {
    format!("{oid} is not a valid exported stash commit")
}

pub fn invalid_root(oid: &str) -> String {
    format!("found root commit {oid} with invalid data")
}

pub fn without_prefix(oid: &str) -> String {
    format!("found stash commit {oid} without expected prefix")
}
