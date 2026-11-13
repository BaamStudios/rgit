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
pub const HELP_DELETE: &str = "delete fully merged branch";
pub const HELP_FORCE_DELETE: &str = "delete branch (even if not merged)";

pub const HINT_REF_SYNTAX: &str = "hint: See 'git help check-ref-format'
hint: Disable this message with \"git config set advice.refSyntax false\"
";
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

pub fn deleted(name: &str, abbrev: &str) -> String {
    format!("Deleted branch {name} (was {abbrev}).")
}

pub fn created_from(start: &str) -> String {
    format!("branch: Created from {start}")
}

pub fn detached_at(abbrev: &str) -> String {
    format!("(HEAD detached at {abbrev})")
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
pub const HINT_SET_UPSTREAM_FAILURE: &str = "hint:
hint: If you are planning on basing your work on an upstream
hint: branch that already exists at the remote, you may need to
hint: run \"git fetch\" to retrieve it.
hint:
hint: If you are planning to push out a new local branch that
hint: will track its remote counterpart, you may want to use
hint: \"git push -u\" to set the upstream config as you push.
hint: Disable this message with \"git config set advice.setUpstreamFailure false\"
";

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

pub fn hint_force_delete(name: &str) -> String {
    format!(
        "hint: If you are sure you want to delete it, run 'git branch -D {name}'\n\
         hint: Disable this message with \"git config set advice.forceDeleteBranch false\"\n"
    )
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

pub fn not_a_branch(name: &str) -> String {
    format!("cannot set up tracking information; starting point '{name}' is not a branch")
}

pub fn upstream_does_not_exist(name: &str) -> String {
    format!("the requested upstream branch '{name}' does not exist")
}

pub fn own_upstream(name: &str) -> String {
    format!("not setting branch '{name}' as its own upstream")
}

pub fn set_up_to_track(branch: &str, upstream: &str, rebase: bool) -> String {
    if rebase {
        format!("branch '{branch}' set up to track '{upstream}' by rebasing.")
    } else {
        format!("branch '{branch}' set up to track '{upstream}'.")
    }
}

pub fn no_upstream_information(name: &str) -> String {
    format!("branch '{name}' has no upstream information")
}
