// SPDX-License-Identifier: MIT
//! Messages of `git branch`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git branch [<options>] [-r | -a] [--merged] [--no-merged]",
    "git branch [<options>] [-f] [--recurse-submodules] <branch-name> [<start-point>]",
    "git branch [<options>] [-l] [<pattern>...]",
    "git branch [<options>] [-r] (-d | -D) <branch-name>...",
    "git branch [<options>] (-m | -M) [<old-branch>] <new-branch>",
    "git branch [<options>] (-c | -C) [<old-branch>] <new-branch>",
    "git branch [<options>] [-r | -a] [--points-at]",
    "git branch [<options>] [-r | -a] [--format]",
];

pub const GROUP_GENERIC: &str = "Generic options";
pub const GROUP_ACTIONS: &str = "Specific git-branch actions:";
pub const HELP_VERBOSE: &str = "show hash and subject, give twice for upstream branch";
pub const HELP_QUIET: &str = "suppress informational messages";
pub const HELP_SET_UPSTREAM: &str = "change the upstream info";
pub const HELP_UNSET_UPSTREAM: &str = "unset the upstream info";
pub const HELP_ABBREV: &str = "use <n> digits to display object names";
pub const HELP_MOVE: &str = "move/rename a branch and its reflog";
pub const HELP_FORCE_MOVE: &str = "move/rename a branch, even if target exists";
pub const HELP_COPY: &str = "copy a branch and its reflog";
pub const HELP_FORCE_COPY: &str = "copy a branch, even if target exists";
pub const HELP_REMOTES: &str = "act on remote-tracking branches";
pub const HELP_ALL: &str = "list both remote-tracking and local branches";
pub const HELP_LIST: &str = "list branch names";
pub const HELP_SHOW_CURRENT: &str = "show current branch name";
pub const HELP_FORCE: &str = "force creation, move/rename, deletion";
pub const HELP_DELETE: &str = "delete fully merged branch";
pub const HELP_FORCE_DELETE: &str = "delete branch (even if not merged)";

/// Advice `refSyntax`.
pub const HINT_REF_SYNTAX: &str = "See 'git help check-ref-format'";
pub const BRANCH_NAME_REQUIRED: &str = "branch name required";

pub fn already_exists(name: &str) -> String {
    format!("a branch named '{name}' already exists")
}

pub fn not_valid_branch_name(name: &str) -> String {
    format!("'{name}' is not a valid branch name")
}

pub fn not_valid_object_name(name: &str) -> String {
    format!("not a valid object name: '{name}'")
}

pub fn not_found(name: &str) -> String {
    format!("branch '{name}' not found")
}

pub fn cannot_delete_checked_out(name: &str, path: &str) -> String {
    format!("cannot delete branch '{name}' used by worktree at '{path}'")
}
/// The branch a bisection in this worktree started from.
pub fn cannot_delete_for_bisect(name: &str, path: &str) -> String {
    format!("cannot delete branch '{name}' used by worktree at '{path}' for bisect")
}

pub fn deleted(name: &str, abbrev: &str) -> String {
    format!("Deleted branch {name} (was {abbrev}).")
}

pub fn reset_to(start: &str) -> String {
    format!("branch: Reset to {start}")
}

pub fn created_from(start: &str) -> String {
    format!("branch: Created from {start}")
}

pub fn detached_at(name: &str) -> String {
    format!("(HEAD detached at {name})")
}

pub fn detached_from(name: &str) -> String {
    format!("(HEAD detached from {name})")
}

pub const GONE: &str = "gone";
pub const RENAME_DETACHED: &str = "cannot rename the current branch while not on any";
pub const COPY_DETACHED: &str = "cannot copy the current branch while not on any";
pub const TOO_MANY_RENAME: &str = "too many arguments for a rename operation";
pub const TOO_MANY_COPY: &str = "too many branches for a copy operation";
pub const TOO_MANY_SET_UPSTREAM: &str = "too many arguments to set new upstream";
pub const TOO_MANY_UNSET_UPSTREAM: &str = "too many arguments to unset upstream";
pub const UNSET_UPSTREAM_DETACHED: &str =
    "could not unset upstream of HEAD when it does not point to any branch";

pub fn ahead(n: usize) -> String {
    format!("ahead {n}")
}

pub fn behind(n: usize) -> String {
    format!("behind {n}")
}

pub fn ahead_behind(a: usize, b: usize) -> String {
    format!("ahead {a}, behind {b}")
}

pub fn remote_not_found(name: &str) -> String {
    format!("remote-tracking branch '{name}' not found")
}

pub fn deleted_remote(name: &str, abbrev: &str) -> String {
    format!("Deleted remote-tracking branch {name} (was {abbrev}).")
}

pub fn not_fully_merged(name: &str) -> String {
    format!("the branch '{name}' is not fully merged")
}

/// Advice `forceDeleteBranch`.
pub fn hint_force_delete(name: &str) -> String {
    format!("If you are sure you want to delete it, run 'git branch -D {name}'")
}

pub fn merged_to_upstream_not_head(name: &str, upstream: &str) -> String {
    format!(
        "deleting branch '{name}' that has been merged to\n         '{upstream}', but not yet merged to HEAD"
    )
}

pub fn merged_to_head_not_upstream(name: &str, upstream: &str) -> String {
    format!(
        "not deleting branch '{name}' that is not yet merged to\n         '{upstream}', even though it is merged to HEAD"
    )
}

pub fn no_branch_named(name: &str) -> String {
    format!("no branch named '{name}'")
}

pub fn no_commit_on_branch(name: &str) -> String {
    format!("no commit on branch '{name}' yet")
}

pub fn cannot_force_update_checked_out(name: &str, path: &str) -> String {
    format!("cannot force update the branch '{name}' used by worktree at '{path}'")
}

pub fn renamed_reflog(old: &str, new: &str) -> String {
    format!("Branch: renamed {old} to {new}")
}

pub fn copied_reflog(old: &str, new: &str) -> String {
    format!("Branch: copied {old} to {new}")
}

pub fn set_upstream_detached(upstream: &str) -> String {
    format!("could not set upstream of HEAD to {upstream} when it does not point to any branch")
}

pub fn branch_does_not_exist(name: &str) -> String {
    format!("branch '{name}' does not exist")
}

pub fn no_upstream_information(name: &str) -> String {
    format!("branch '{name}' has no upstream information")
}

pub fn being_rebased(refname: &str, path: &str) -> String {
    format!("branch {refname} is being rebased at {path}")
}

pub fn being_bisected(refname: &str, path: &str) -> String {
    format!("branch {refname} is being bisected at {path}")
}

pub fn reflog_is_symlink(refname: &str) -> String {
    format!("reflog for {refname} is a symlink")
}

pub const RENAME_FAILED: &str = "branch rename failed";
pub const COPY_FAILED: &str = "branch copy failed";

pub use crate::messages::tag::{HELP_FORMAT, HELP_IGNORE_CASE, HELP_OMIT_EMPTY, HELP_SORT};
pub const HELP_CONTAINS: &str = "print only branches that contain the commit";
pub const HELP_NO_CONTAINS: &str = "print only branches that don't contain the commit";
pub const HELP_MERGED: &str = "print only branches that are merged";
pub const HELP_NO_MERGED: &str = "print only branches that are not merged";
pub const HELP_POINTS_AT: &str = "print only branches of the object";
pub const HELP_CREATE_REFLOG: &str = "create the branch's reflog";
pub const HELP_EDIT_DESCRIPTION: &str = "edit the description for the branch";
pub const DESCRIPTION_DETACHED: &str = "cannot give description to detached HEAD";
pub const DESCRIPTION_MANY: &str = "cannot edit description of more than one branch";

/// The comment lines of `EDIT_DESCRIPTION` (without the comment string).
pub fn description_template(name: &str, comment: &str) -> String {
    format!(
        "Please edit the description for the branch\n  {name}\nLines starting with '{comment}' will be stripped.\n"
    )
}

pub const ALL_REMOTES_NO_NAME: &str = "the -a, and -r, options to 'git branch' do not take a branch name.\nDid you mean to use: -a|-r --list <pattern>?";

pub fn rebasing(what: &str) -> String {
    format!("(no branch, rebasing {what})")
}

pub fn bisect_started(on: &str) -> String {
    format!("(no branch, bisect started on {on})")
}

pub use crate::messages::tag::HELP_COLUMN;

/// A linked worktree whose `HEAD` could not follow a renamed branch.
pub fn head_of_worktree_not_updated(path: &str) -> String {
    format!("HEAD of working tree {path} is not updated")
}

pub const SET_UPSTREAM_SINGLE_ARGUMENT: &str =
    "--set-upstream-to takes a single <remote>/<branch> argument";

pub fn hint_set_upstream_slip(upstream: &str) -> String {
    format!("Did you mean to use: git branch --set-upstream-to={upstream}?")
}

pub fn renamed_head_not_updated(new: &str) -> String {
    format!("branch renamed to {new}, but HEAD is not updated")
}
