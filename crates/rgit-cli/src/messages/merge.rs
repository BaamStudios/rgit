// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git merge` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git merge [<options>] [<commit>...]",
    "git merge --abort",
    "git merge --continue",
];

pub const HELP_N: &str = "do not show a diffstat at the end of the merge";
pub const HELP_STAT: &str = "show a diffstat at the end of the merge";
pub const HELP_SUMMARY: &str = "(synonym to --stat)";
pub const HELP_COMPACT_SUMMARY: &str = "show a compact-summary at the end of the merge";
pub const HELP_LOG: &str = "add (at most <n>) entries from shortlog to merge commit message";
pub const HELP_SQUASH: &str = "create a single commit instead of doing a merge";
pub const HELP_COMMIT: &str = "perform a commit if the merge succeeds (default)";
pub const HELP_EDIT: &str = "edit message before committing";
pub const HELP_CLEANUP: &str = "how to strip spaces and #comments from message";
pub const HELP_FF: &str = "allow fast-forward (default)";
pub const HELP_FF_ONLY: &str = "abort if fast-forward is not possible";
pub const HELP_RERERE: &str = "update the index with reused conflict resolution if possible";
pub const HELP_VERIFY_SIGNATURES: &str = "verify that the named commit has a valid GPG signature";
pub const HELP_STRATEGY: &str = "merge strategy to use";
pub const HELP_STRATEGY_OPTION: &str = "option for selected merge strategy";
pub const HELP_MESSAGE: &str = "merge commit message (for a non-fast-forward merge)";
pub const HELP_FILE: &str = "read message from file";
pub const HELP_INTO_NAME: &str = "use <name> instead of the real target";
pub const HELP_VERBOSE: &str = "be more verbose";
pub const HELP_QUIET: &str = "be more quiet";
pub const HELP_ABORT: &str = "abort the current in-progress merge";
pub const HELP_QUIT: &str = "--abort but leave index and working tree alone";
pub const HELP_CONTINUE: &str = "continue the current in-progress merge";
pub const HELP_ALLOW_UNRELATED: &str = "allow merging unrelated histories";
pub const HELP_PROGRESS: &str = "force progress reporting";
pub const HELP_GPG_SIGN: &str = "GPG sign commit";
pub const HELP_AUTOSTASH: &str = "automatically stash/stash pop before and after";
pub const HELP_OVERWRITE_IGNORE: &str = "update ignored files (default)";
pub const HELP_SIGNOFF: &str = "add a Signed-off-by trailer";
pub const HELP_NO_VERIFY: &str = "bypass pre-merge-commit and commit-msg hooks";
pub const HELP_VERIFY: &str = "opposite of --no-verify";

pub fn not_something_we_can_merge(name: &str) -> String {
    format!("merge: {name} - not something we can merge")
}

pub const DID_YOU_MEAN_ONE: &str = "Did you mean this?";
pub const DID_YOU_MEAN_MANY: &str = "Did you mean one of these?";

pub fn could_not_open_for_reading(path: &str, err: &str) -> String {
    format!("could not open '{path}' for reading: {err}")
}

pub fn expects_no_arguments(option: &str) -> String {
    format!("{option} expects no arguments")
}

/// git puts the period inside the quotes here.
pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}.' cannot be used together")
}

pub const LOG_NOT_A_NUMBER: &str = "option `log' expects a numerical value";

pub const ALREADY_UP_TO_DATE: &str = "Already up to date.";
pub const FAST_FORWARD: &str = "Fast-forward";
pub const SQUASH_NOT_UPDATING: &str = "Squash commit -- not updating HEAD";
pub const AUTOMATIC_FAILED: &str =
    "Automatic merge failed; fix conflicts and then commit the result.";
pub const WENT_WELL: &str = "Automatic merge went well; stopped before committing as requested";
pub const NOT_COMMITTING: &str = "Not committing merge; use 'git commit' to complete the merge.";
pub const EMPTY_MESSAGE: &str = "Empty commit message.";

pub fn updating(from: &str, to: &str) -> String {
    format!("Updating {from}..{to}")
}

pub fn made_by(strategy: &str) -> String {
    format!("Merge made by the '{strategy}' strategy.")
}

pub fn strategy_failed(strategy: &str) -> String {
    format!("Merge with strategy {strategy} failed.")
}

/// Staged changes found before a merge (`error: `, paths follow indented
/// by two spaces).
pub const LOCAL_CHANGES: &str =
    "Your local changes to the following files would be overwritten by merge:";

pub const NOT_POSSIBLE_FF: &str = "Not possible to fast-forward, aborting.";
/// Advice `diverging`.
pub const DIVERGING_HINT: &str = "\
Diverging branches can't be fast-forwarded, you need to either:

\tgit merge --no-ff

or:

\tgit rebase
";

pub const NO_MERGE_TO_ABORT: &str = "There is no merge to abort (MERGE_HEAD missing).";
pub const NO_MERGE_IN_PROGRESS: &str = "There is no merge in progress (MERGE_HEAD missing).";
pub const NOT_CONCLUDED: &str = "You have not concluded your merge (MERGE_HEAD exists).
Please, commit your changes before you merge.";
pub const NOT_CONCLUDED_CHERRY_PICK: &str =
    "You have not concluded your cherry-pick (CHERRY_PICK_HEAD exists).
Please, commit your changes before you merge.";
pub const UNMERGED: &str = "Merging is not possible because you have unmerged files.";

pub const NO_CURRENT_BRANCH: &str = "No current branch.";
pub const NO_REMOTE: &str = "No remote for the current branch.";
pub const NO_DEFAULT_UPSTREAM: &str = "No default upstream defined for the current branch.";
pub const NO_COMMIT_SPECIFIED: &str = "No commit specified and merge.defaultToUpstream not set.";

pub fn no_remote_tracking_branch(merge: &str, remote: &str) -> String {
    format!("No remote-tracking branch for {merge} from {remote}")
}

pub fn unknown_strategy(name: &str) -> String {
    format!(
        "Could not find merge strategy '{name}'.\n\
         Available strategies are: octopus ours recursive resolve subtree."
    )
}

pub const UNRELATED_HISTORIES: &str = "refusing to merge unrelated histories";
pub const ONLY_ONE_INTO_EMPTY: &str = "Can merge only exactly one commit into empty head";
pub const SQUASH_INTO_EMPTY: &str = "Squash commit into empty head not supported yet";
pub const READ_TREE_FAILED: &str = "read-tree failed";

/// Reflog message of a merge into an unborn branch.
pub const REFLOG_INITIAL: &str = "initial pull";

pub fn reflog(action: &str, what: &str) -> String {
    format!("{action}: {what}")
}

/// The template below the message when the merge commit is edited.
pub fn edit_template(c: &str) -> String {
    format!(
        "{c} Please enter a commit message to explain why this merge is necessary,\n\
         {c} especially if it merges an updated upstream into a topic branch.\n\
         {c}\n\
         {c} Lines starting with '{c}' will be ignored, and an empty message aborts\n\
         {c} the commit.\n"
    )
}

pub const SQUASHED_HEADER: &str = "Squashed commit of the following:";
pub const CONFLICTS: &str = "Conflicts:";

pub const KIND_BRANCH: &str = "branch";
pub const KIND_TAG: &str = "tag";
pub const KIND_REMOTE: &str = "remote-tracking branch";
pub const KIND_COMMIT: &str = "commit";
pub const KIND_BRANCHES: &str = "branches";
pub const KIND_TAGS: &str = "tags";
pub const KIND_REMOTES: &str = "remote-tracking branches";
pub const KIND_COMMITS: &str = "commits";
pub const EARLY_PART: &str = "(early part)";

pub fn commits_count(n: usize) -> String {
    format!(" ({n} commits)")
}

// rgit's own messages for what is not implemented yet.
pub fn strategy_not_supported(name: &str) -> String {
    format!("rgit merge: strategy '{name}' is not supported yet (#153)")
}
pub fn octopus_trying(name: &str) -> String {
    format!("Trying simple merge with {name}\n")
}
pub fn octopus_fast_forward(name: &str) -> String {
    format!("Fast-forwarding to: {name}\n")
}
pub fn octopus_up_to_date(name: &str) -> String {
    format!("Already up to date with {name}\n")
}
pub const OCTOPUS_STOP: &str = "Should not be doing an octopus.\n";
pub const MERGE_PROGRAM_FAILED: &str = "merge program failed";

pub fn option_not_supported(name: &str) -> String {
    format!("rgit merge: {name} is not supported yet")
}
