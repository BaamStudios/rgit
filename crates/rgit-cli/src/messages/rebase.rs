// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git rebase` (merge back end)
//! and its status banner (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git rebase [-i] [options] [--exec <cmd>] [--onto <newbase> | --keep-base] [<upstream> [<branch>]]",
    "git rebase [-i] [options] [--exec <cmd>] [--onto <newbase>] --root [<branch>]",
    "git rebase --continue | --abort | --skip | --edit-todo",
];

pub const HELP_ONTO: &str = "rebase onto given branch instead of upstream";
pub const HELP_KEEP_BASE: &str = "use the merge-base of upstream and branch as the current base";
pub const HELP_NO_VERIFY: &str = "allow pre-rebase hook to run";
pub const HELP_VERIFY: &str = "opposite of --no-verify";
pub const HELP_QUIET: &str = "be quiet. implies --no-stat";
pub const HELP_VERBOSE: &str = "display a diffstat of what changed upstream";
pub const HELP_NO_STAT: &str = "do not show diffstat of what changed upstream";
pub const HELP_STAT: &str = "opposite of --no-stat";
pub const HELP_TRAILER: &str = "add custom trailer(s)";
pub const HELP_SIGNOFF: &str = "add a Signed-off-by trailer to each commit";
pub const HELP_CDATE_IS_ADATE: &str = "make committer date match author date";
pub const HELP_RESET_AUTHOR_DATE: &str = "ignore author date and use current date";
pub const HELP_IGNORE_DATE: &str = "synonym of --reset-author-date";
pub const HELP_C: &str = "passed to 'git apply'";
pub const HELP_IGNORE_WHITESPACE: &str = "ignore changes in whitespace";
pub const HELP_WHITESPACE: &str = "passed to 'git apply'";
pub const HELP_FORCE_REBASE: &str = "cherry-pick all commits, even if unchanged";
pub const HELP_FF: &str = "opposite of --no-ff";
pub const HELP_CONTINUE: &str = "continue";
pub const HELP_SKIP: &str = "skip current patch and continue";
pub const HELP_ABORT: &str = "abort and check out the original branch";
pub const HELP_QUIT: &str = "abort but keep HEAD where it is";
pub const HELP_EDIT_TODO: &str = "edit the todo list during an interactive rebase";
pub const HELP_SHOW_CURRENT_PATCH: &str = "show the patch file being applied or merged";
pub const HELP_APPLY: &str = "use apply strategies to rebase";
pub const HELP_MERGE: &str = "use merging strategies to rebase";
pub const HELP_INTERACTIVE: &str = "let the user edit the list of commits to rebase";
pub const HELP_PRESERVE_MERGES: &str =
    "(REMOVED) was: try to recreate merges instead of ignoring them";
pub const HELP_RERERE: &str = "update the index with reused conflict resolution if possible";
pub const HELP_KEEP_EMPTY: &str = "keep commits which start empty";
pub const HELP_ALLOW_EMPTY_MESSAGE: &str = "allow rebasing commits with empty messages";
pub const HELP_EMPTY: &str = "how to handle commits that become empty";
pub const HELP_AUTOSQUASH: &str = "move commits that begin with squash!/fixup! under -i";
pub const HELP_UPDATE_REFS: &str = "update branches that point to commits that are being rebased";
pub const HELP_GPG_SIGN: &str = "GPG-sign commits";
pub const HELP_AUTOSTASH: &str = "automatically stash/stash pop before and after";
pub const HELP_EXEC: &str = "add exec lines after each commit of the editable list";
pub const HELP_REBASE_MERGES: &str = "try to rebase merges instead of skipping them";
pub const HELP_FORK_POINT: &str = "use 'merge-base --fork-point' to refine upstream";
pub const HELP_STRATEGY: &str = "use the given merge strategy";
pub const HELP_STRATEGY_OPTION: &str = "pass the argument through to the merge strategy";
pub const HELP_ROOT: &str = "rebase all reachable commits up to the root(s)";
pub const HELP_RESCHEDULE: &str = "automatically re-schedule any `exec` that fails";
pub const HELP_REAPPLY: &str = "apply all changes, even those already present upstream";

pub fn invalid_empty(value: &str) -> String {
    format!("unrecognized empty type '{value}'; valid values are \"drop\", \"keep\", and \"stop\".")
}

pub const NO_REBASE_IN_PROGRESS: &str = "no rebase in progress";

/// Starting a rebase while one is in progress (`dir` is the state
/// directory as git names it).
pub fn already_in_progress(name: &str, dir: &str) -> String {
    format!(
        "It seems that there is already a {name} directory, and\n\
         I wonder if you are in the middle of another rebase.  If that is the\n\
         case, please try\n\
         \tgit rebase (--continue | --abort | --skip)\n\
         If that is not the case, please\n\
         \trm -fr \"{dir}\"\n\
         and run me again.  I am stopping in case you still have something\n\
         valuable there.\n"
    )
}

pub const UNSTAGED_CHANGES: &str = "cannot rebase: You have unstaged changes.";
pub const UNCOMMITTED_CHANGES: &str = "cannot rebase: Your index contains uncommitted changes.";
pub const COMMIT_OR_STASH: &str = "Please commit or stash them.";

/// `git rebase` without an upstream on a branch without one configured.
pub fn no_tracking(branch: &str) -> String {
    format!(
        "There is no tracking information for the current branch.\n\
         Please specify which branch you want to rebase against.\n\
         See git-rebase(1) for details.\n\
         \n    git rebase '<branch>'\n\
         \n\
         If you wish to set tracking information for this branch you can do so with:\n\
         \n    git branch --set-upstream-to=<remote>/<branch> {branch}\n"
    )
}

/// `git rebase` without an upstream on a detached `HEAD`.
pub const NOT_ON_BRANCH: &str = "You are not currently on a branch.\n\
Please specify which branch you want to rebase against.\n\
See git-rebase(1) for details.\n\
\n    git rebase '<branch>'\n";

pub fn invalid_upstream(name: &str) -> String {
    format!("invalid upstream '{name}'")
}

pub fn no_such_branch(name: &str) -> String {
    format!("no such branch/commit '{name}'")
}

pub fn invalid_onto(name: &str) -> String {
    format!("Does not point to a valid commit '{name}'")
}

pub fn need_one_merge_base(name: &str) -> String {
    format!("'{name}': need exactly one merge base")
}

pub fn need_one_merge_base_with_branch(name: &str) -> String {
    format!("'{name}': need exactly one merge base with branch")
}

pub const COULD_NOT_DETACH: &str = "could not detach HEAD";

pub const UNTRACKED_OVERWRITTEN_BY_RESET: &str =
    "The following untracked working tree files would be overwritten by reset:";
pub const MOVE_OR_REMOVE_BEFORE_RESET: &str = "Please move or remove them before you reset.";

pub fn could_not_move_back(to: &str) -> String {
    format!("could not move back to {to}")
}

pub const COULD_NOT_DISCARD: &str = "could not discard worktree changes";

pub const HOOK_REFUSED: &str = "The pre-rebase hook refused to rebase.";

pub fn up_to_date(branch: Option<&str>, forced: bool) -> String {
    let mut s = match branch {
        Some(b) => format!("Current branch {b} is up to date"),
        None => "HEAD is up to date".to_owned(),
    };
    s.push_str(if forced { ", rebase forced." } else { "." });
    s
}

pub fn changes_from(from: &str, to: &str) -> String {
    format!("Changes from {from} to {to}:")
}

/// `-v` when the branch and the new base share no history.
pub fn changes_to(to: &str) -> String {
    format!("Changes to {to}:")
}

pub fn progress(n: usize, total: usize) -> String {
    format!("Rebasing ({n}/{total})")
}

pub fn success(head_name: &str) -> String {
    format!("Successfully rebased and updated {head_name}.")
}

pub const DETACHED_HEAD: &str = "detached HEAD";

pub fn skipped_cherry_pick(abbrev: &str) -> String {
    format!("skipped previously applied commit {abbrev}")
}

pub const SKIPPED_CHERRY_PICK_HINT: &str = "use --reapply-cherry-picks to include skipped commits";

/// The advice after a conflict.
pub const CONFLICT_ADVICE: &str = "Resolve all conflicts manually, mark them as resolved with\n\
\"git add/rm <conflicted_files>\", then run \"git rebase --continue\".\n\
You can instead skip this commit: run \"git rebase --skip\".\n\
To abort and get back to the state before \"git rebase\", run \"git rebase --abort\".";

/// The last line when a pick stopped (`rest` is the rest of its todo
/// line after the commit).
pub fn could_not_apply(abbrev: &str, rest: &str) -> String {
    format!("Could not apply {abbrev}... {rest}")
}

pub fn needs_merge(path: &str) -> String {
    format!("{path}: needs merge")
}

pub const MUST_EDIT_CONFLICTS: &str =
    "You must edit all merge conflicts and then\nmark them as resolved using git add";

pub fn created_autostash(abbrev: &str) -> String {
    format!("Created autostash: {abbrev}")
}

pub const APPLIED_AUTOSTASH: &str = "Applied autostash.";
pub const AUTOSTASH_CONFLICTS: &str = "Applying autostash resulted in conflicts.\n\
Your changes are safe in the stash.\n\
You can run \"git stash pop\" or \"git stash drop\" at any time.\n";
/// The autostash was applied with conflicts, which are left in the index
/// and work tree; the stash is kept too.
pub const AUTOSTASH_APPLIED_WITH_CONFLICTS: &str = "Your local changes are stashed, however \
applying them\nresulted in conflicts.  You can either resolve the conflicts\nand then discard \
the stash with \"git stash drop\", or, if you\ndo not want to resolve them now, run \"git reset \
--hard\" and\napply the local changes later by running \"git stash pop\".";
pub const AUTOSTASH_KEPT: &str = "Autostash exists; creating a new stash entry.\n\
Your changes are safe in the stash.\n\
You can run \"git stash pop\" or \"git stash drop\" at any time.\n";
pub const EMPTY_ASK_DEPRECATED: &str = "--empty=ask is deprecated; use '--empty=stop' instead.";
pub const REFLOG_COMMIT_REBASE: &str = "commit (rebase)";
/// The hint after a todo command that could not run at all.
pub fn rescheduled(line: &str) -> String {
    format!(
        "Could not execute the todo command\n\
         \n    {line}\n\
         \n\
         It has been rescheduled; To edit the command before continuing, please\n\
         edit the todo list first:\n\
         \n    git rebase --edit-todo\n    git rebase --continue\n"
    )
}

pub const AUTOSTASH_MESSAGE: &str = "autostash";

pub const EMPTY_SKIP: &str = "Otherwise, please use 'git rebase --skip'";

/// Reflog messages: `<action> (<what>): <rest>`.
pub fn reflog(action: &str, what: &str, rest: &str) -> String {
    format!("{action} ({what}): {rest}")
}

pub fn checkout(name: &str) -> String {
    format!("checkout {name}")
}

pub fn finish_onto(head_name: &str, onto: &str) -> String {
    format!("{head_name} onto {onto}")
}

pub fn returning_to(name: &str) -> String {
    format!("returning to {name}")
}

pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn not_supported(what: &str) -> String {
    format!("rgit: rebase: {what} is not supported yet")
}

// The status banner.

pub fn in_progress_onto(onto: &str) -> String {
    format!("interactive rebase in progress; onto {onto}")
}

/// The first status line during `rebase --apply`.
pub fn apply_in_progress_onto(onto: &str) -> String {
    format!("rebase in progress; onto {onto}")
}

pub fn last_done(n: usize) -> String {
    if n == 1 {
        format!("Last command done ({n} command done):")
    } else {
        format!("Last commands done ({n} commands done):")
    }
}

pub const NO_COMMANDS_DONE: &str = "No commands done.";
pub const NO_COMMANDS_REMAINING: &str = "No commands remaining.";

pub fn see_more(path: &str) -> String {
    format!("  (see more in file {path})")
}

pub fn next_to_do(n: usize) -> String {
    if n == 1 {
        format!("Next command to do ({n} remaining command):")
    } else {
        format!("Next commands to do ({n} remaining commands):")
    }
}

pub const HINT_EDIT_TODO: &str = "  (use \"git rebase --edit-todo\" to view and edit)";

pub fn currently_rebasing(branch: Option<&str>, onto: &str) -> String {
    match branch {
        Some(b) => format!("You are currently rebasing branch '{b}' on '{onto}'."),
        None => "You are currently rebasing.".to_owned(),
    }
}

pub const HINT_FIX_CONFLICTS: &str = "  (fix conflicts and then run \"git rebase --continue\")";
pub const HINT_SKIP: &str = "  (use \"git rebase --skip\" to skip this patch)";
pub const HINT_ABORT: &str = "  (use \"git rebase --abort\" to check out the original branch)";
pub const HINT_ALL_FIXED: &str = "  (all conflicts fixed: run \"git rebase --continue\")";

pub fn editing_a_commit(branch: Option<&str>, onto: &str) -> String {
    match branch {
        Some(b) => {
            format!("You are currently editing a commit while rebasing branch '{b}' on '{onto}'.")
        }
        None => "You are currently editing a commit during a rebase.".to_owned(),
    }
}

pub fn splitting_a_commit(branch: Option<&str>, onto: &str) -> String {
    match branch {
        Some(b) => {
            format!("You are currently splitting a commit while rebasing branch '{b}' on '{onto}'.")
        }
        None => "You are currently splitting a commit during a rebase.".to_owned(),
    }
}

pub const HINT_AMEND: &str = "  (use \"git commit --amend\" to amend the current commit)";
pub const HINT_CONTINUE_SATISFIED: &str =
    "  (use \"git rebase --continue\" once you are satisfied with your changes)";
pub const HINT_CONTINUE_CLEAN: &str =
    "  (Once your working directory is clean, run \"git rebase --continue\")";

// Interactive rebase (#155).

/// `--continue` after `edit` when the stopped commit was replaced.
pub const UNCOMMITTED_AFTER_EDIT: &str = "\nYou have uncommitted changes in your working tree. Please, commit them\n\
first and then run 'git rebase --continue' again.";

/// `--continue` with staged changes after `break` or `exec`.
pub const STAGED_CHANGES: &str = "you have staged changes in your working tree\n\
If these changes are meant to be squashed into the previous commit, run:\n\
\n  git commit --amend \n\
\nIf they are meant to go into a new commit, run:\n\
\n  git commit \n\
\nIn both cases, once you're done, continue with:\n\
\n  git rebase --continue\n";

/// A `fixup`/`squash` first in the list.
pub fn meld_first(cmd: &str) -> String {
    format!("cannot '{cmd}' without a previous commit")
}

pub const EDIT_TODO_NOT_INTERACTIVE: &str =
    "The --edit-todo action can only be used during interactive rebase.";

pub const EMPTY_EXEC: &str = "empty exec command";
pub const EXEC_NEWLINE: &str = "exec commands cannot contain newlines";

pub fn does_not_accept_arguments(cmd: &str, rest: &str) -> String {
    format!("{cmd} does not accept arguments: '{rest}'")
}

pub fn missing_arguments(cmd: &str) -> String {
    format!("missing arguments for {cmd}")
}

pub const NOTHING_TO_DO: &str = "nothing to do";

pub const FIX_WITH_EDIT_TODO: &str = "You can fix this with 'git rebase --edit-todo' and then run 'git rebase --continue'.\n\
Or you can abort the rebase with 'git rebase --abort'.";

pub const PLEASE_FIX_WITH_EDIT_TODO: &str = "please fix this using 'git rebase --edit-todo'.";

/// The first line of the help below the generated todo list.
pub fn todo_header(range: &str, onto: &str, n: usize) -> String {
    let what = if n == 1 { "command" } else { "commands" };
    format!("Rebase {range} onto {onto} ({n} {what})")
}

/// The help below the todo list, without the comment characters.
pub const TODO_HELP: &str = "\n\
Commands:\n\
p, pick <commit> = use commit\n\
r, reword <commit> = use commit, but edit the commit message\n\
e, edit <commit> = use commit, but stop for amending\n\
s, squash <commit> = use commit, but meld into previous commit\n\
f, fixup [-C | -c] <commit> = like \"squash\" but keep only the previous\n                   \
commit's log message, unless -C is used, in which case\n                   \
keep only this commit's message; -c is same as -C but\n                   \
opens the editor\n\
x, exec <command> = run command (the rest of the line) using shell\n\
b, break = stop here (continue rebase later with 'git rebase --continue')\n\
d, drop <commit> = remove commit\n\
l, label <label> = label current HEAD with a name\n\
t, reset <label> = reset HEAD to a label\n\
m, merge [-C <commit> | -c <commit>] <label> [# <oneline>]\n        \
create a merge commit using the original merge commit's\n        \
message (or the oneline, if no original merge commit was\n        \
specified); use -c <commit> to reword the commit message\n\
u, update-ref <ref> = track a placeholder for the <ref> to be updated\n                      \
to this position in the new commits. The <ref> is\n                      \
updated at the end of the rebase\n\
\n\
These lines can be re-ordered; they are executed from top to bottom.\n";

pub const TODO_HELP_LOST: &str = "\nIf you remove a line here THAT COMMIT WILL BE LOST.\n";
pub const TODO_HELP_DROP: &str =
    "\nDo not remove any line. Use 'drop' explicitly to remove a commit.\n";
pub const TODO_HELP_ABORT: &str =
    "\nHowever, if you remove everything, the rebase will be aborted.\n\n";
pub const TODO_HELP_EDIT_TODO: &str = "\nYou are editing the todo file of an ongoing interactive rebase.\n\
To continue rebase after editing, run:\n    git rebase --continue\n\n";

pub const DROPPED_WARNING: &str =
    "Warning: some commits may have been dropped accidentally.\nDropped commits (newer to older):";
pub const DROPPED_HINT: &str = "To avoid this message, use \"drop\" to explicitly remove a commit.\n\
\n\
Use 'git config rebase.missingCommitsCheck' to change the level of warnings.\n\
The possible behaviours are: ignore, warn, error.\n";

pub fn unrecognized_check_level(value: &str) -> String {
    format!("unrecognized setting {value} for option rebase.missingCommitsCheck. Ignoring.")
}

/// `edit`: the stop.
pub fn stopped_at_edit(abbrev: &str, rest: &str) -> String {
    format!("Stopped at {abbrev}...  {rest}")
}

/// `sign` is the quoted `-S` option of the rebase, or empty.
pub fn edit_advice(sign: &str) -> String {
    format!(
        "You can amend the commit now, with\n\
\n  git commit --amend {sign}\n\
\nOnce you are satisfied with your changes, run\n\
\n  git rebase --continue\n"
    )
}

/// `break`: the stop.
pub fn stopped_at(abbrev: &str, subject: &str) -> String {
    format!("Stopped at {abbrev} ({subject})")
}

pub fn executing(cmd: &str) -> String {
    format!("Executing: {cmd}")
}

pub fn execution_failed(cmd: &str) -> String {
    format!("execution failed: {cmd}")
}

pub const EXEC_FAILED_ADVICE: &str = "You can fix the problem, and then run\n\
\n  git rebase --continue\n\n";

pub fn execution_left_changes(cmd: &str) -> String {
    format!(
        "execution succeeded: {cmd}\nbut left changes to the index and/or the working tree.\n\
         Commit or stash your changes, and then run\n\
         \n  git rebase --continue\n\n"
    )
}

pub fn cannot_run(cmd: &str, reason: &str) -> String {
    format!("cannot run {cmd}: {reason}")
}

// The message of a squash/fixup chain.

pub fn combination(n: usize) -> String {
    format!("This is a combination of {n} commits.")
}

pub const FIRST_MESSAGE: &str = "This is the 1st commit message:";
pub const FIRST_SKIPPED: &str = "The 1st commit message will be skipped:";

pub fn nth_message(n: usize) -> String {
    format!("This is the commit message #{n}:")
}

pub fn nth_skipped(n: usize) -> String {
    format!("The commit message #{n} will be skipped:")
}

// --rebase-merges.

pub fn reset_label(label: &str) -> String {
    format!("'{label}'")
}

pub fn could_not_resolve(name: &str) -> String {
    format!("could not resolve '{name}'")
}

pub fn not_a_commit(oid: &str, kind: &str) -> String {
    format!("object {oid} is a {kind}, not a commit")
}

pub fn unable_to_parse(name: &str) -> String {
    format!("unable to parse '{name}'")
}

pub fn merge_branches(labels: &str) -> String {
    format!("Merge branches '{labels}'")
}

pub fn merge_branch(label: &str) -> String {
    format!("Merge branch '{label}'")
}

pub fn invalid_label(label: &str) -> String {
    format!("'{label}' is not a valid label")
}

pub fn invalid_refname(name: &str) -> String {
    format!("'{name}' is not a valid refname")
}

pub fn update_ref_needs_full_name(name: &str) -> String {
    format!("update-ref requires a fully qualified refname e.g. refs/heads/{name}")
}

/// A todo command that does not take a merge commit.
pub fn no_merge_commits(cmd: &str) -> String {
    format!("'{cmd}' does not accept merge commits")
}

pub const PICK_MERGE_HINT: &str = "'pick' does not take a merge commit. If you wanted to
replay the merge, use 'merge -C' on the commit.";

pub const REWORD_MERGE_HINT: &str = "'reword' does not take a merge commit. If you wanted to
replay the merge and reword the commit message, use
'merge -c' on the commit";

pub const EDIT_MERGE_HINT: &str = "'edit' does not take a merge commit. If you wanted to
replay the merge, use 'merge -C' on the commit, and then
'break' to give the control back to you so that you can
do 'git commit --amend && git rebase --continue'.";

pub const CANNOT_SQUASH_MERGE: &str = "cannot squash merge commit into another commit";

/// `--update-refs`: a branch checked out in another worktree.
pub fn ref_checked_out_at(name: &str, path: &str) -> String {
    format!("Ref {name} checked out at '{path}'")
}

pub fn update_ref_failed(name: &str, why: &str) -> String {
    format!("update_ref failed for ref '{name}': {why}")
}

pub const UPDATED_REFS: &str = "Updated the following refs with --update-refs:";
pub const FAILED_REFS: &str = "Failed to update the following refs with --update-refs:";
pub const REWRITTEN_DURING_REBASE: &str = "rewritten during rebase";

pub const APPLY_AND_MERGE: &str = "apply options and merge options cannot be used together";

pub const C_NOT_A_NUMBER: &str = "switch `C' expects a numerical value";

pub const COULD_NOT_COMMIT_STAGED: &str = "could not commit staged changes.";

pub fn unknown_author_variable(key: &str) -> String {
    format!("unknown variable '{key}'")
}

pub fn author_variable_twice(key: &str) -> String {
    format!("'{key}' already given")
}

pub fn missing_author_variable(key: &str) -> String {
    format!("missing '{key}'")
}

pub fn could_not_write(path: &str, err: &str) -> String {
    format!("could not open '{path}' for writing: {err}")
}

pub fn fast_forwarded(branch: &str, onto: &str) -> String {
    format!("Fast-forwarded {branch} to {onto}.")
}

pub fn invalid_whitespace(value: &str) -> String {
    format!("Invalid whitespace option: '{value}'")
}

pub fn apply_incompatible_config(key: &str, option: &str) -> String {
    format!("apply options are incompatible with {key}.  Consider adding {option}")
}

pub const EMPTY_TRAILER: &str = "empty --trailer argument";

pub fn trailer_missing_key(trailer: &str) -> String {
    format!("invalid trailer '{trailer}': missing key before separator")
}

pub fn requires_merge_backend(what: &str) -> String {
    format!("{what} requires the merge backend")
}

/// `rebase --apply`, before `am` replays the commits.
pub const REWINDING: &str = "First, rewinding head to replay your work on top of it...";

/// `--resolvemsg` of the `am` that `rebase --apply` runs.
pub const APPLY_RESOLVE_MSG: &str = "Resolve all conflicts manually, mark them as resolved with\n\
\"git add/rm <conflicted_files>\", then run \"git rebase --continue\".\n\
You can instead skip this commit: run \"git rebase --skip\".\n\
To abort and get back to the state before \"git rebase\", run \"git rebase --abort\".";

pub fn invalid_rebase_merges(mode: &str) -> String {
    format!("Unknown rebase-merges mode: {mode}")
}
