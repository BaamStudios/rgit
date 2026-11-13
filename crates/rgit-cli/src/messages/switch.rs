// SPDX-License-Identifier: MIT
//! Messages of `git switch` and `git checkout` (Documentation/git-switch.adoc,
//! git-checkout.adoc), reproduced verbatim as the `git` binary prints them.

pub const USAGE: &[&str] = &["git switch [<options>] [<branch>]"];
pub const CHECKOUT_USAGE: &[&str] = &[
    "git checkout [<options>] <branch>",
    "git checkout [<options>] [<branch>] -- <file>...",
];

pub const HELP_CREATE: &str = "create and switch to a new branch";
pub const HELP_FORCE_CREATE: &str = "create/reset and switch to a branch";
pub const HELP_CHECKOUT_B: &str = "create and checkout a new branch";
pub const HELP_CHECKOUT_FORCE_B: &str = "create/reset and checkout a branch";
pub const HELP_REFLOG: &str = "create reflog for new branch";
pub const HELP_GUESS_SWITCH: &str = "second guess 'git switch <no-such-branch>'";
pub const HELP_GUESS_CHECKOUT: &str = "second guess 'git checkout <no-such-branch>' (default)";
pub const HELP_OVERLAY: &str = "use overlay mode (default)";
pub const HELP_DISCARD: &str = "throw away local modifications";
pub const HELP_QUIET: &str = "suppress progress reporting";
pub const HELP_RECURSE: &str = "control recursive updating of submodules";
pub const HELP_PROGRESS: &str = "force progress reporting";
pub const HELP_MERGE: &str = "perform a 3-way merge with the new branch";
pub const HELP_CONFLICT: &str = "conflict style (merge, diff3, or zdiff3)";
pub const HELP_DETACH: &str = "detach HEAD at named commit";
pub const HELP_FORCE: &str = "force checkout (throw away local modifications)";
pub const HELP_ORPHAN: &str = "new unborn branch";
pub const HELP_OVERWRITE_IGNORE: &str = "update ignored files (default)";
pub const HELP_IGNORE_OTHER: &str = "do not check if another worktree is using this branch";
pub const HELP_OURS: &str = "checkout our version for unmerged files";
pub const HELP_THEIRS: &str = "checkout their version for unmerged files";
pub const HELP_SKIP_WORKTREE_BITS: &str = "do not limit pathspecs to sparse entries only";

pub fn switched_new(name: &str) -> String {
    format!("Switched to a new branch '{name}'")
}
pub fn switched(name: &str) -> String {
    format!("Switched to branch '{name}'")
}
pub fn switched_and_reset(name: &str) -> String {
    format!("Switched to and reset branch '{name}'")
}
pub fn reset_branch(name: &str) -> String {
    format!("Reset branch '{name}'")
}
pub fn already_on(name: &str) -> String {
    format!("Already on '{name}'")
}
pub fn head_now_at(abbrev: &str, subject: &str) -> String {
    format!("HEAD is now at {abbrev} {subject}")
}
pub fn previous_head(abbrev: &str, subject: &str) -> String {
    format!("Previous HEAD position was {abbrev} {subject}")
}
pub fn branch_exists(name: &str) -> String {
    format!("a branch named '{name}' already exists")
}
pub fn invalid_ref(name: &str) -> String {
    format!("invalid reference: {name}")
}
pub fn not_valid_branch_name(name: &str) -> String {
    format!("'{name}' is not a valid branch name")
}
pub fn reflog_moving(from: &str, to: &str) -> String {
    format!("checkout: moving from {from} to {to}")
}
pub fn created_from(start: &str) -> String {
    format!("branch: Created from {start}")
}
pub fn reset_to(start: &str) -> String {
    format!("branch: Reset to {start}")
}
pub fn not_a_commit_for_branch(start: &str, branch: &str) -> String {
    format!("'{start}' is not a commit and a branch '{branch}' cannot be created from it")
}
pub fn branch_expected(kind: &str, name: &str) -> String {
    format!("a branch is expected, got {kind} '{name}'")
}
pub fn detach_takes_no_path(arg: &str) -> String {
    format!("git checkout: --detach does not take a path argument '{arg}'")
}
pub fn pathspec_no_match(spec: &str) -> String {
    format!("pathspec '{spec}' did not match any file(s) known to git")
}
pub fn path_unmerged(path: &str) -> String {
    format!("path '{path}' is unmerged")
}
pub fn updated_paths_from_index(n: usize) -> String {
    if n == 1 {
        "Updated 1 path from the index".to_owned()
    } else {
        format!("Updated {n} paths from the index")
    }
}
pub fn updated_paths_from(n: usize, abbrev: &str) -> String {
    if n == 1 {
        format!("Updated 1 path from {abbrev}")
    } else {
        format!("Updated {n} paths from {abbrev}")
    }
}
pub fn unable_to_read_tree(id: &str) -> String {
    format!("unable to read tree ({id})")
}
pub fn hint_detach_branch_expected() -> &'static str {
    "hint: If you want to detach HEAD at the commit, try again with the --detach option.\n"
}
pub const MISSING_BRANCH_OR_COMMIT: &str = "missing branch or commit argument";
pub const ONE_REFERENCE_EXPECTED: &str = "only one reference expected";
pub const DETACH_WITH_CREATE: &str = "'--detach' cannot be used with '-b/-B/--orphan'";
pub const ORPHAN_WITH_TRACK: &str = "'--orphan' cannot be used with '-t'";
pub const TRACK_WITH_PATHS: &str = "'--track' cannot be used with updating paths";
pub const HINT_PULL_DIVERGED: &str =
    "  (use \"git pull\" if you want to integrate the remote branch with yours)";

/// Printed before `HEAD is now at ...` when `checkout` detaches `HEAD`
/// (`advice.detachedHead`).
pub fn detached_advice(name: &str) -> String {
    format!(
        "Note: switching to '{name}'.\n\
         \n\
         You are in 'detached HEAD' state. You can look around, make experimental\n\
         changes and commit them, and you can discard any commits you make in this\n\
         state without impacting any branches by switching back to a branch.\n\
         \n\
         If you want to create a new branch to retain commits you create, you may\n\
         do so (now or later) by using -c with the switch command. Example:\n\
         \n\
         \x20 git switch -c <new-branch-name>\n\
         \n\
         Or undo this operation with:\n\
         \n\
         \x20 git switch -\n\
         \n\
         Turn off this advice by setting config variable advice.detachedHead to false\n"
    )
}

/// rgit-only: an option that is accepted but not implemented yet.
pub fn not_supported(what: &str) -> String {
    format!("rgit: '{what}' is not implemented yet")
}

/// Leaving a detached `HEAD` whose commits no ref reaches: the warning up
/// to the commit list. `n` is the number of commits.
pub fn leaving_behind(n: usize) -> String {
    let what = if n == 1 { "commit" } else { "commits" };
    format!("Warning: you are leaving {n} {what} behind, not connected to\nany of your branches:\n")
}

/// One line of the [`leaving_behind`] list.
pub fn leaving_behind_commit(abbrev: &str, subject: &str) -> String {
    format!("  {abbrev} {subject}")
}

/// The last line of a shortened [`leaving_behind`] list.
pub fn leaving_behind_more(n: usize) -> String {
    format!(" ... and {n} more.")
}

/// After [`leaving_behind`] (`advice.detachedHead`): how to keep the
/// commits; `n` commits, `abbrev` the old `HEAD`.
pub fn leaving_behind_advice(n: usize, abbrev: &str) -> String {
    let what = if n == 1 { "it" } else { "them" };
    format!(
        "If you want to keep {what} by creating a new branch, this may be a good time\n\
         to do so with:\n\
         \n\
         \x20git branch <new-branch-name> {abbrev}\n"
    )
}
