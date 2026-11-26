// SPDX-License-Identifier: MIT
//! Messages and usage of `git history` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here; observed with git 2.56.1).

pub const USAGE_DROP: &str = "git history drop <commit> [--dry-run] [--update-refs=(branches|head)] [--empty=(drop|keep|abort)]";
pub const USAGE_FIXUP: &str = "git history fixup <commit> [--dry-run] [--update-refs=(branches|head)] [--reedit-message] [--empty=(drop|keep|abort)]";
pub const USAGE_REWORD: &str =
    "git history reword <commit> [--dry-run] [--update-refs=(branches|head)]";
pub const USAGE_SPLIT: &str =
    "git history split <commit> [--dry-run] [--update-refs=(branches|head)] [--] [<pathspec>...]";
pub const USAGE: &[&str] = &[USAGE_DROP, USAGE_FIXUP, USAGE_REWORD, USAGE_SPLIT];

pub const H_UPDATE_REFS: &str = "control which refs should be updated";
pub const H_UPDATE_REFS_SPLIT: &str = "control ref update behavior";
pub const H_DRY_RUN: &str = "perform a dry-run without updating any refs";
pub const H_REEDIT: &str = "open an editor to modify the commit message";
pub const H_EMPTY_FIXUP: &str = "how to handle commits that become empty";
pub const H_EMPTY_DROP: &str = "how to handle descendants that become empty";
pub const ARGH_UPDATE_REFS: &str = "(branches|head)";
pub const ARGH_EMPTY: &str = "(drop|keep|abort)";

pub const NEED_SUBCOMMAND: &str = "need a subcommand";
pub const BAD_UPDATE_REFS: &str = "update-refs expects one of 'branches' or 'head'";
pub const SINGLE_REVISION: &str = "command expects a single revision";
pub const NEED_COMMITTISH: &str = "command expects a committish";
pub const MERGES_UNSUPPORTED: &str = "replaying merge commits is not supported yet!";
pub const NOT_ANCESTOR: &str =
    "rewritten commit must be an ancestor of HEAD when using --update-refs=head";
pub const FAILED_REPLAY: &str = "failed replaying descendants";
pub const EDITOR_FAILED: &str = "Aborting commit as launching the editor failed.";
pub const EMPTY_MESSAGE: &str = "Aborting commit due to empty commit message.";
pub const FAILED_REWORD: &str = "failed writing reworded commit";
pub const FAILED_FIXUP: &str = "failed writing fixed-up commit";
pub const FAILED_SPLIT_FIRST: &str = "failed writing first commit";
pub const FAILED_SPLIT_SECOND: &str = "failed writing second commit";
pub const DROP_OVERWRITE: &str = "dropping this commit would overwrite local changes; aborting";
pub const FIXUP_BARE: &str = "cannot run fixup in a bare repository";
pub const NOTHING_TO_FIXUP: &str = "nothing to fixup: no staged changes";
pub const FIXUP_CONFLICTS: &str = "fixup would produce conflicts; aborting";
pub const SPLIT_MERGE: &str = "cannot split up merge commit";
pub const SPLIT_EMPTY: &str = "split commit is empty";
pub const SPLIT_ALL: &str = "split commit tree matches original commit";
pub const CHANGES_TO_BE_COMMITTED: &str = "Changes to be committed:";

/// The kind of change named in the editor template.
pub const WHAT_REWORD: &str = "reworded";
pub const WHAT_FIXUP: &str = "fixup";
pub const WHAT_SPLIT: &str = "split-out";

pub fn unknown_subcommand(s: &str) -> String {
    format!("unknown subcommand: `{s}'")
}

pub fn bad_empty(v: &str) -> String {
    format!(
        "unrecognized '--empty=' action '{v}'; valid values are \"drop\", \"keep\", and \"abort\"."
    )
}

pub fn not_found(rev: &str) -> String {
    format!("commit cannot be found: {rev}")
}

pub fn not_a_commit(id: &str, kind: &str) -> String {
    format!("object {id} is a {kind}, not a commit")
}

pub fn drop_root(rev: &str) -> String {
    format!("cannot drop root commit {rev}: it has no parent to replay onto")
}

pub fn drop_merge(rev: &str) -> String {
    format!("cannot drop merge commit: {rev}")
}

pub fn became_empty(id: &str) -> String {
    format!("commit {id} became empty after replay")
}

pub fn fixup_empty(rev: &str) -> String {
    format!("fixup makes commit {rev} empty")
}

pub fn worktree_failed(id: &str) -> String {
    format!("could not update working tree to new commit {id}")
}

/// The first two comment lines of the editor template.
pub fn instructions(what: &str, c: &str) -> String {
    format!(
        "Please enter the commit message for the {what} changes. Lines starting\nwith '{c}' will be ignored, and an empty message aborts the commit.\n"
    )
}

/// The reflog message: `<cmd>: updating <rev>` (`drop: dropping <rev>`).
pub fn reflog(cmd: &str, rev: &str) -> String {
    if cmd == "drop" {
        format!("drop: dropping {rev}")
    } else {
        format!("{cmd}: updating {rev}")
    }
}
