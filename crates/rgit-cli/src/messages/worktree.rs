// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git worktree`, as printed by the
//! reference git (CLEANROOM.md: user-visible strings are reproduced
//! verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git worktree add [-f] [--detach] [--checkout] [--lock [--reason <string>]]\n                        [--orphan] [(-b | -B) <new-branch>] <path> [<commit-ish>]",
    "git worktree list [-v | --porcelain [-z]]",
    "git worktree lock [--reason <string>] <worktree>",
    "git worktree move <worktree> <new-path>",
    "git worktree prune [-n] [-v] [--expire <expire>]",
    "git worktree remove [-f] <worktree>",
    "git worktree repair [<path>...]",
    "git worktree unlock <worktree>",
];
pub const USAGE_ADD: &[&str] = &[USAGE[0]];
pub const USAGE_LIST: &[&str] = &[USAGE[1]];
pub const USAGE_LOCK: &[&str] = &[USAGE[2]];
pub const USAGE_MOVE: &[&str] = &[USAGE[3]];
pub const USAGE_PRUNE: &[&str] = &[USAGE[4]];
pub const USAGE_REMOVE: &[&str] = &[USAGE[5]];
pub const USAGE_REPAIR: &[&str] = &[USAGE[6]];
pub const USAGE_UNLOCK: &[&str] = &[USAGE[7]];

pub const HELP_ADD_FORCE: &str = "checkout <branch> even if already checked out in other worktree";
pub const HELP_NEW_BRANCH: &str = "create a new branch";
pub const HELP_RESET_BRANCH: &str = "create or reset a branch";
pub const HELP_ORPHAN: &str = "create unborn branch";
pub const HELP_DETACH: &str = "detach HEAD at named commit";
pub const HELP_CHECKOUT: &str = "populate the new working tree";
pub const HELP_LOCK: &str = "keep the new working tree locked";
pub const HELP_REASON: &str = "reason for locking";
pub const HELP_QUIET: &str = "suppress progress reporting";
pub const HELP_TRACK: &str = "set up tracking mode (see git-branch(1))";
pub const HELP_GUESS_REMOTE: &str =
    "try to match the new branch name with a remote-tracking branch";
pub const HELP_RELATIVE: &str = "use relative paths for worktrees";
pub const HELP_PORCELAIN: &str = "machine-readable output";
pub const HELP_LIST_VERBOSE: &str = "show extended annotations and reasons, if available";
pub const HELP_LIST_EXPIRE: &str =
    "add 'prunable' annotation to missing worktrees older than <time>";
pub const HELP_Z: &str = "terminate records with a NUL character";
pub const HELP_PRUNE_DRY_RUN: &str = "do not remove, show only";
pub const HELP_PRUNE_VERBOSE: &str = "report pruned working trees";
pub const HELP_PRUNE_EXPIRE: &str = "prune missing working trees older than <time>";
pub const HELP_MOVE_FORCE: &str = "force move even if worktree is dirty or locked";
pub const HELP_REMOVE_FORCE: &str = "force removal even if worktree is dirty or locked";
pub const ARGH_EXPIRE: &str = "expiry-date";
pub const ARGH_STRING: &str = "string";
pub const ARGH_BRANCH: &str = "branch";

pub const NEED_SUBCOMMAND: &str = "need a subcommand";

// Prune reasons (`prune -v`, `list -v`).
pub const PRUNE_NOT_DIR: &str = "not a valid directory";
pub const PRUNE_NO_GITDIR: &str = "gitdir file does not exist";
pub const PRUNE_UNREADABLE_GITDIR: &str = "unable to read gitdir file";
pub const PRUNE_INVALID_GITDIR: &str = "invalid gitdir file";
pub const PRUNE_NONEXISTENT: &str = "gitdir file points to non-existent location";
pub const PRUNE_DUPLICATE: &str = "duplicate entry";

pub fn removing(id: &str, reason: &str) -> String {
    format!("Removing worktrees/{id}: {reason}")
}

// add
pub fn preparing_new_branch(name: &str) -> String {
    format!("Preparing worktree (new branch '{name}')")
}

pub fn preparing_reset_branch(name: &str, old: &str) -> String {
    format!("Preparing worktree (resetting branch '{name}'; was at {old})")
}

pub fn preparing_checkout(name: &str) -> String {
    format!("Preparing worktree (checking out '{name}')")
}

pub fn preparing_detached(abbrev: &str) -> String {
    format!("Preparing worktree (detached HEAD {abbrev})")
}

pub const INFER_ORPHAN: &str = "No possible source branch, inferring '--orphan'";
pub const NEED_FETCH: &str = "No local or remote refs exist despite at least one remote\npresent, stopping; use 'add -f' to override or fetch a remote first";
pub const ORPHAN_TRACK: &str = "options '--orphan' and '--track' cannot be used together";
pub const BAD_HEAD: &str = "HEAD points to an invalid (or orphaned) reference.";

pub fn orphan_hint(example: &str) -> String {
    format!(
        "If you meant to create a worktree containing a new unborn branch\n(branch with no commits) for this repository, you can do so\nusing the --orphan flag:\n\n    git worktree add {example}\n"
    )
}

pub const LOCKED_BY_ADD: &str = "added with --lock";
pub const INITIALIZING: &str = "initializing";

pub fn already_exists(path: &str) -> String {
    format!("'{path}' already exists")
}

pub fn invalid_reference(name: &str) -> String {
    format!("invalid reference: {name}")
}

pub fn already_used_by(branch: &str, path: &str) -> String {
    format!("'{branch}' is already used by worktree at '{path}'")
}

pub fn missing_locked(path: &str, cmd: &str) -> String {
    format!(
        "'{path}' is a missing but locked worktree;\nuse '{cmd} -f -f' to override, or 'unlock' and 'prune' or 'remove' to clear"
    )
}

pub fn missing_registered(path: &str, cmd: &str) -> String {
    format!(
        "'{path}' is a missing but already registered worktree;\nuse '{cmd} -f' to override, or 'prune' or 'remove' to clear"
    )
}

pub fn could_not_create_dir(path: &str, err: &str) -> String {
    format!("could not create directory of '{path}': {err}")
}

pub const BRANCH_DETACH_EXCLUSIVE: &str =
    "options '-b', '-B', and '--detach' cannot be used together";
pub const ORPHAN_DETACH: &str = "options '--orphan' and '--detach' cannot be used together";
pub const ORPHAN_NO_CHECKOUT: &str =
    "options '--orphan' and '--no-checkout' cannot be used together";
pub const ORPHAN_COMMITISH: &str = "option '--orphan' and commit-ish cannot be used together";
pub const REASON_NEEDS_LOCK: &str = "the option '--reason' requires '--lock'";
pub const Z_NEEDS_PORCELAIN: &str = "the option '-z' requires '--porcelain'";
pub const VERBOSE_PORCELAIN: &str = "options '--verbose' and '--porcelain' cannot be used together";

// lock / unlock / move / remove
pub fn not_a_working_tree(arg: &str) -> String {
    format!("'{arg}' is not a working tree")
}

pub const MAIN_LOCK: &str = "The main working tree cannot be locked or unlocked";

pub fn already_locked(arg: &str) -> String {
    format!("'{arg}' is already locked")
}

pub fn already_locked_reason(arg: &str, reason: &str) -> String {
    format!("'{arg}' is already locked, reason: {reason}")
}

pub fn not_locked(arg: &str) -> String {
    format!("'{arg}' is not locked")
}

pub fn is_main(arg: &str) -> String {
    format!("'{arg}' is a main working tree")
}

pub const CONTAINS_SUBMODULES: &str =
    "working trees containing submodules cannot be moved or removed";

pub fn cannot_locked(cmd: &str, reason: &str) -> String {
    if reason.is_empty() {
        format!(
            "cannot {cmd} a locked working tree;\nuse '{cmd} -f -f' to override or unlock first"
        )
    } else {
        format!(
            "cannot {cmd} a locked working tree, lock reason: {reason}\nuse '{cmd} -f -f' to override or unlock first"
        )
    }
}

pub fn target_exists(path: &str) -> String {
    format!("target '{path}' already exists")
}

pub fn failed_to_move(from: &str, to: &str, err: &str) -> String {
    format!("failed to move '{from}' to '{to}': {err}")
}

pub fn dirty(arg: &str) -> String {
    format!("'{arg}' contains modified or untracked files, use --force to delete it")
}

pub fn failed_to_delete(path: &str, err: &str) -> String {
    format!("failed to delete '{path}': {err}")
}

pub fn status_failed(path: &str) -> String {
    format!("failed to run 'git status' on '{path}'")
}

// repair
pub fn repair_line(what: &str, path: &str) -> String {
    format!("repair: {what}: {path}")
}

pub fn repair_error(what: &str, path: &str) -> String {
    format!("{what}: {path}")
}

pub const REPAIR_GITDIR_INCORRECT: &str = "gitdir incorrect";
pub const REPAIR_DOTGIT_BROKEN: &str = ".git file broken";
pub const REPAIR_DOTGIT_MISMATCH: &str = ".git file absolute/relative path mismatch";
pub const REPAIR_DOTGIT_INCORRECT: &str = ".git file incorrect";
pub const REPAIR_NOT_VALID_PATH: &str = "not a valid path";
pub const REPAIR_NOT_A_FILE: &str = ".git is not a file";
pub const REPAIR_NOT_A_DIR: &str = "not a directory";
pub const REPAIR_NOT_A_REPO: &str = ".git file does not reference a repository";
pub const REPAIR_GITDIR_UNREADABLE: &str = "gitdir unreadable";
pub const REPAIR_GITDIR_MISMATCH: &str = "gitdir absolute/relative path mismatch";

/// `checkoutAmbiguousRemoteBranchName` for `worktree add`.
pub fn hint_ambiguous_remote(name: &str, remotes: &[String], path: &str) -> String {
    let mut s = format!("Branch name '{name}' appears in multiple remotes:\n");
    for r in remotes {
        s.push_str(&format!("  {r}\n"));
    }
    s.push_str(&format!(
        "If you meant to create a worktree from a remote tracking branch on\n\
         <remote>, you can do so by:\n\
         \n\
         \x20   git worktree add -b {name} {path} <remote>/{name}\n\
         \n\
         If you'd like to always prefer some remote, e.g. 'origin',\n\
         consider setting checkout.defaultRemote=origin in your config."
    ));
    s
}
