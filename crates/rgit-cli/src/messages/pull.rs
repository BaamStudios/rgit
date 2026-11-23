// SPDX-License-Identifier: MIT
//! Messages, usage line and option help of `git pull` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git pull [<options>] [<repository> [<refspec>...]]"];

pub const GROUP_MERGING: &str = "Options related to merging";
pub const GROUP_FETCHING: &str = "Options related to fetching";

pub const HELP_RECURSE_SUBMODULES: &str = "control for recursive fetching of submodules";
pub const HELP_REBASE: &str = "incorporate changes by rebasing rather than merging";
pub const HELP_ALL: &str = "fetch from all remotes";
pub const HELP_FORCE: &str = "force overwrite of local branch";
pub const HELP_JOBS: &str = "number of submodules pulled in parallel";
pub const HELP_DEPTH: &str = "deepen history of shallow clone";
pub const HELP_SHALLOW_SINCE: &str = "deepen history of shallow repository based on time";
pub const HELP_SHALLOW_EXCLUDE: &str = "deepen history of shallow clone, excluding ref";
pub const HELP_UNSHALLOW: &str = "convert to a complete repository";
pub const HELP_UPDATE_SHALLOW: &str = "accept refs that update .git/shallow";
pub const HELP_REFMAP: &str = "specify fetch refmap";
pub const HELP_NEGOTIATION_RESTRICT: &str =
    "report that we have only objects reachable from this object";
pub const HELP_NEGOTIATION_TIP: &str = "alias of --negotiation-restrict";
pub const HELP_NEGOTIATION_INCLUDE: &str = "ensure this ref is always sent as a negotiation have";
pub const HELP_SET_UPSTREAM: &str = "set upstream for git pull/fetch";
pub const HELP_SIGNOFF: &str = "add a Signed-off-by trailer";
pub const HELP_EDIT: &str = "edit message before committing";
pub const HELP_FF: &str = "allow fast-forward";
pub const HELP_VERIFY: &str = "control use of pre-merge-commit and commit-msg hooks";
pub const HELP_ALLOW_UNRELATED: &str = "allow merging unrelated histories";

pub fn invalid_option_value(option: &str, value: &str) -> String {
    format!("invalid value for '{option}': '{value}'")
}

/// `error:` before a pull with conflicts in the index.
pub const UNMERGED: &str = "Pulling is not possible because you have unmerged files.";
/// `fatal:` after [`UNMERGED`].
pub const UNRESOLVED: &str = "Exiting because of an unresolved conflict.";
/// `error:` before a pull while a merge is in progress.
pub const MERGE_IN_PROGRESS: &str = "You have not concluded your merge (MERGE_HEAD exists).";
/// Advice `resolveConflict` after [`MERGE_IN_PROGRESS`].
pub const MERGE_IN_PROGRESS_HINT: &str = "Please, commit your changes before merging.";
/// `fatal:` after [`MERGE_IN_PROGRESS`].
pub const UNFINISHED_MERGE: &str = "Exiting because of unfinished merge.";

/// `warning:` when the fetch moved the checked-out branch.
pub fn fetch_updated_head(old: &str) -> String {
    format!(
        "fetch updated the current branch head.\n\
         fast-forwarding your working tree from\n\
         commit {old}."
    )
}

/// `fatal:` when the work tree cannot follow the branch the fetch moved.
pub fn cannot_fast_forward_work_tree(old: &str) -> String {
    format!(
        "Cannot fast-forward your working tree.\n\
         After making sure that you saved anything precious from\n\
         $ git diff {old}\n\
         output, run\n\
         $ git reset --hard\n\
         to recover."
    )
}

/// No `FETCH_HEAD` line is marked for merging although refspecs were given.
pub fn no_candidates(rebase: bool) -> String {
    let first = if rebase {
        "There is no candidate for rebasing against among the refs that you just fetched."
    } else {
        "There are no candidates for merging among the refs that you just fetched."
    };
    format!(
        "{first}\n\
         Generally this means that you provided a wildcard refspec which had no\n\
         matches on the remote end."
    )
}

/// A remote other than the branch's remote was named without a branch.
pub fn not_default_remote(remote: &str) -> String {
    format!(
        "You asked to pull from the remote '{remote}', but did not specify\n\
         a branch. Because this is not the default configured remote\n\
         for your current branch, you must specify a branch on the command line."
    )
}

fn which_branch(rebase: bool) -> &'static str {
    if rebase {
        "Please specify which branch you want to rebase against."
    } else {
        "Please specify which branch you want to merge with."
    }
}

/// `HEAD` is detached and nothing was named.
pub fn not_on_a_branch(rebase: bool) -> String {
    format!(
        "You are not currently on a branch.\n\
         {}\n\
         See git-pull(1) for details.\n\
         \n    git pull <remote> <branch>\n",
        which_branch(rebase)
    )
}

/// The current branch has no `branch.<name>.merge`.
pub fn no_tracking_information(rebase: bool, remote: &str, branch: &str) -> String {
    format!(
        "There is no tracking information for the current branch.\n\
         {}\n\
         See git-pull(1) for details.\n\
         \n    git pull <remote> <branch>\n\n\
         If you wish to set tracking information for this branch you can do so with:\n\
         \n    git branch --set-upstream-to={remote}/<branch> {branch}\n",
        which_branch(rebase)
    )
}

/// The configured upstream was not among the fetched refs.
pub fn upstream_not_fetched(merge: &str) -> String {
    format!(
        "Your configuration specifies to merge with the ref '{merge}'\n\
         from the remote, but no such ref was fetched."
    )
}

/// Advice (always shown) before [`NEED_RECONCILE`].
pub const DIVERGENT_HINT: &str = "\
You have divergent branches and need to specify how to reconcile them.
You can do so by running one of the following commands sometime before
your next pull:

  git config pull.rebase false  # merge
  git config pull.rebase true   # rebase
  git config pull.ff only       # fast-forward only

You can replace \"git config\" with \"git config --global\" to set a default
preference for all repositories. You can also pass --rebase, --no-rebase,
or --ff-only on the command line to override the configured default per
invocation.";
pub const NEED_RECONCILE: &str = "Need to specify how to reconcile divergent branches.";

pub const MULTIPLE_INTO_EMPTY: &str = "Cannot merge multiple branches into empty head.";
pub const REBASE_MULTIPLE: &str = "Cannot rebase onto multiple branches.";
pub const FF_MULTIPLE: &str = "Cannot fast-forward to multiple branches.";

/// `fatal:` for a rebasing pull into an unborn branch with staged files.
pub const UNBORN_WITH_STAGED: &str = "Updating an unborn branch with changes added to the index.";

/// `error:` lines when a rebasing pull finds local changes.
pub fn cannot_rebase(what: &str) -> String {
    format!("cannot pull with rebase: {what}")
}
pub const UNSTAGED: &str = "You have unstaged changes.";
pub const STAGED: &str = "Your index contains uncommitted changes.";
pub const ADDITIONALLY_STAGED: &str = "additionally, your index contains uncommitted changes.";
pub const COMMIT_OR_STASH: &str = "Please commit or stash them.";

/// Reflog message of a pull into an unborn branch.
pub const REFLOG_INITIAL: &str = "initial pull";

/// rgit's own message: `--rebase=interactive` and `--rebase=merges` need
/// parts of rebase rgit does not have yet (#155).
pub fn rebase_mode_not_supported(mode: &str) -> String {
    format!(
        "pull --rebase={mode} is not supported by rgit yet unless it is a fast-forward \
         (rgit #155); use --rebase or --no-rebase instead"
    )
}

/// `pull --rebase --verify-signatures`.
pub const IGNORING_VERIFY_SIGNATURES: &str = "ignoring --verify-signatures for rebase";
