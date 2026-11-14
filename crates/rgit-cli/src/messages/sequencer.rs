// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git cherry-pick`, `git revert`
//! and the sequencer behind them (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

pub const CHERRY_PICK_USAGE: &[&str] = &[
    "git cherry-pick [--edit] [-n] [-m <parent-number>] [-s] [-x] [--ff]\n                [-S[<keyid>]] <commit>...",
    "git cherry-pick (--continue | --skip | --abort | --quit)",
];
pub const REVERT_USAGE: &[&str] = &[
    "git revert [--[no-]edit] [-n] [-m <parent-number>] [-s] [-S[<keyid>]] <commit>...",
    "git revert (--continue | --skip | --abort | --quit)",
];

pub const HELP_QUIT: &str = "end revert or cherry-pick sequence";
pub const HELP_CONTINUE: &str = "resume revert or cherry-pick sequence";
pub const HELP_ABORT: &str = "cancel revert or cherry-pick sequence";
pub const HELP_SKIP: &str = "skip current commit and continue";
pub const HELP_CLEANUP: &str = "how to strip spaces and #comments from message";
pub const HELP_NO_COMMIT: &str = "don't automatically commit";
pub const HELP_EDIT: &str = "edit the commit message";
pub const HELP_SIGNOFF: &str = "add a Signed-off-by trailer";
pub const HELP_MAINLINE: &str = "select mainline parent";
pub const HELP_RERERE: &str = "update the index with reused conflict resolution if possible";
pub const HELP_STRATEGY: &str = "merge strategy";
pub const HELP_STRATEGY_OPTION: &str = "option for merge strategy";
pub const HELP_GPG_SIGN: &str = "GPG sign commit";
pub const HELP_X: &str = "append commit name";
pub const HELP_FF: &str = "allow fast-forward";
pub const HELP_ALLOW_EMPTY: &str = "preserve initially empty commits";
pub const HELP_ALLOW_EMPTY_MESSAGE: &str = "allow commits with empty messages";
pub const HELP_KEEP_REDUNDANT: &str = "deprecated: use --empty=keep instead";
pub const HELP_EMPTY: &str = "how to handle commits that become empty";
pub const HELP_REFERENCE: &str = "use the 'reference' format to refer to commits";

pub const MAINLINE_NOT_POSITIVE: &str = "option `mainline' expects a number greater than zero";

pub fn invalid_empty(value: &str) -> String {
    format!("invalid value for '--empty': '{value}'")
}

/// "cherry-pick" or "revert" followed by "failed".
pub fn failed(action: &str) -> String {
    format!("{action} failed")
}

pub fn incompatible(action: &str, a: &str, b: &str) -> String {
    format!("{action}: {a} cannot be used with {b}")
}

pub fn option_not_supported(opt: &str) -> String {
    format!("rgit: {opt} is not supported yet")
}

pub fn strategy_not_supported(name: &str) -> String {
    format!("rgit: the merge strategy '{name}' is not supported yet")
}

pub const EMPTY_SET: &str = "empty commit set passed";

pub fn cannot_pick_object(name: &str, action: &str, kind: &str) -> String {
    format!("{name}: can't {action} a {kind}")
}

pub const NOTHING_IN_PROGRESS: &str = "no cherry-pick or revert in progress";
pub const NO_CHERRY_PICK: &str = "no cherry-pick in progress";
pub const NO_REVERT: &str = "no revert in progress";
pub const NOTHING_TO_SKIP: &str = "there is nothing to skip";

pub fn nothing_to_skip_hint(action: &str) -> String {
    format!("have you committed already?\ntry \"git {action} --continue\"")
}

pub fn already_in_progress(action: &str) -> String {
    format!("{action} is already in progress")
}

pub fn already_in_progress_hint(action: &str, skip: bool) -> String {
    if skip {
        format!("try \"git {action} (--continue | --skip | --abort | --quit)\"")
    } else {
        format!("try \"git {action} (--continue | --abort | --quit)\"")
    }
}

pub const ABORT_UNBORN: &str = "cannot abort from a branch yet to be born";
pub const MOVED_HEAD: &str = "You seem to have moved HEAD. Not rewinding, check your HEAD!";

pub fn could_not_apply(abbrev: &str, subject: &str) -> String {
    format!("could not apply {abbrev}... {subject}")
}

pub fn could_not_revert(abbrev: &str, subject: &str) -> String {
    format!("could not revert {abbrev}... {subject}")
}

/// The advice after a conflict that stopped the sequence.
pub fn conflict_advice(action: &str) -> String {
    format!(
        "After resolving the conflicts, mark them with\n\
         \"git add/rm <pathspec>\", then run\n\
         \"git {action} --continue\".\n\
         You can instead skip this commit with \"git {action} --skip\".\n\
         To abort and get back to the state before \"git {action}\",\n\
         run \"git {action} --abort\"."
    )
}

/// The advice after a conflict with `--no-commit`.
pub const CONFLICT_ADVICE_NO_COMMIT: &str = "\
after resolving the conflicts, mark the corrected paths
with 'git add <paths>' or 'git rm <paths>'";

pub fn local_changes(action: &str) -> String {
    format!("your local changes would be overwritten by {action}.")
}

pub const LOCAL_CHANGES_HINT: &str = "commit your changes or stash them to proceed.";

pub const CHERRY_PICK_UNMERGED: &str =
    "Cherry-picking is not possible because you have unmerged files.";
pub const REVERT_UNMERGED: &str = "Reverting is not possible because you have unmerged files.";

pub fn merge_without_mainline(id: &str) -> String {
    format!("commit {id} is a merge but no -m option was given.")
}

pub fn no_such_parent(id: &str, n: usize) -> String {
    format!("commit {id} does not have parent {n}")
}

pub fn cannot_get_message(abbrev: &str) -> String {
    format!("cannot get commit message for {abbrev}")
}

pub fn invalid_command(word: &str) -> String {
    format!("invalid command '{word}'")
}

pub fn invalid_line(n: usize, line: &str) -> String {
    format!("invalid line {n}: {line}")
}

pub fn could_not_parse(word: &str) -> String {
    format!("could not parse '{word}'")
}

pub fn unusable_todo(path: &str) -> String {
    format!("unusable instruction sheet: '{path}'")
}

pub const PICK_DURING_REVERT: &str = "cannot cherry-pick during a revert.";
pub const REVERT_DURING_PICK: &str = "cannot revert during a cherry-pick.";

pub fn could_not_read(path: &str) -> String {
    format!("could not read '{path}'.")
}

/// The reflog message of a commit made by the sequencer.
pub fn reflog(action: &str, subject: &str) -> String {
    format!("{action}: {subject}")
}

pub const FAST_FORWARD: &str = "fast-forward";

/// The label of a missing parent (a root commit).
pub const EMPTY_TREE_LABEL: &str = "(empty tree)";

/// The labels of the conflict markers.
pub fn parent_of(label: &str) -> String {
    format!("parent of {label}")
}

/// `-x`.
pub fn cherry_picked_from(id: &str) -> String {
    format!("(cherry picked from commit {id})")
}

pub fn revert_subject(subject: &[u8]) -> Vec<u8> {
    [b"Revert \"".as_slice(), subject, b"\""].concat()
}

/// Replaces "Revert " at the start of the subject of a revert of a revert.
pub const REAPPLY: &str = "Reapply ";

pub fn this_reverts(id: &str) -> String {
    format!("This reverts commit {id}")
}

pub fn reversing_changes(parent: &str) -> String {
    format!(", reversing\nchanges made to {parent}")
}

/// The first line of a `--reference` revert, after the comment string.
pub const SAY_WHY: &str = " *** SAY WHY WE ARE REVERTING ON THE TITLE LINE ***";

pub fn dropping(abbrev: &str, subject: &str) -> String {
    format!("dropping {abbrev} {subject} -- patch contents already upstream")
}

pub const EMPTY_MESSAGE: &str = "Aborting commit due to empty commit message.";
