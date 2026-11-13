// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git commit` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git commit [-a | --interactive | --patch] [-s] [-v] [-u[<mode>]] [--amend]\n                  [--dry-run] [(-c | -C | --squash) <commit> | --fixup [(amend|reword):]<commit>]\n                  [-F <file> | -m <msg>] [--reset-author] [--allow-empty]\n                  [--allow-empty-message] [--no-verify] [-e] [--author=<author>]\n                  [--date=<date>] [--cleanup=<mode>] [--[no-]status]\n                  [-i | -o] [--pathspec-from-file=<file> [--pathspec-file-nul]]\n                  [(--trailer <token>[(=|:)<value>])...] [-S[<keyid>]]\n                  [--] [<pathspec>...]",
];

pub const HELP_QUIET: &str = "suppress summary after successful commit";
pub const GROUP_MESSAGE: &str = "Commit message options";
pub const HELP_FILE: &str = "read message from file";
pub const HELP_AUTHOR: &str = "override author for commit";
pub const HELP_DATE: &str = "override date for commit";
pub const HELP_MESSAGE: &str = "commit message";
pub const HELP_RESET_AUTHOR: &str = "the commit is authored by me now (used with -C/-c/--amend)";
pub const HELP_EDIT: &str = "force edit of commit";
pub const HELP_CLEANUP: &str = "how to strip spaces and #comments from message";
pub const HELP_STATUS: &str = "include status in commit message template";
pub const GROUP_CONTENTS: &str = "Commit contents options";
pub const HELP_ALL: &str = "commit all changed files";
pub const HELP_INCLUDE: &str = "add specified files to index for commit";
pub const HELP_ONLY: &str = "commit only specified files";
pub const HELP_NO_VERIFY: &str = "bypass pre-commit and commit-msg hooks";
pub const HELP_VERIFY: &str = "opposite of --no-verify";
pub const HELP_AMEND: &str = "amend previous commit";
pub const HELP_NO_POST_REWRITE: &str = "bypass post-rewrite hook";
pub const HELP_UNTRACKED: &str =
    "show untracked files, optional modes: all, normal, no. (Default: all)";
pub const HELP_ALLOW_EMPTY: &str = "ok to record an empty change";
pub const HELP_ALLOW_EMPTY_MESSAGE: &str = "ok to record a change with an empty message";

pub const NOTHING_TO_AMEND: &str = "You have nothing to amend.";
pub const M_AND_F: &str = "options '-m' and '-F' cannot be used together";
pub const RESET_AUTHOR_ONLY_WITH: &str = "--reset-author can be used only with -C, -c or --amend.";
pub const RESET_AUTHOR_AND_AUTHOR: &str =
    "options '--reset-author' and '--author' cannot be used together";
pub const INCLUDE_AND_ONLY: &str = "options '-i/--include' and '-o/--only' cannot be used together";
pub const INCLUDE_AND_ALL: &str = "options '-i/--include' and '-a/--all' cannot be used together";
pub const ONLY_AND_ALL: &str = "options '-o/--only' and '-a/--all' cannot be used together";
pub const NO_PATHS: &str = "No paths with --include/--only does not make sense.";

pub fn paths_with_all(first: &str) -> String {
    format!("paths '{first} ...' with -a does not make sense")
}

pub fn invalid_cleanup(mode: &str) -> String {
    format!("Invalid cleanup mode {mode}")
}

pub fn invalid_untracked(mode: &str) -> String {
    format!("Invalid untracked files mode '{mode}'")
}

pub fn no_existing_author(author: &str) -> String {
    format!("--author '{author}' is not 'Name <email>' and matches no existing author")
}

pub fn invalid_date(date: &str) -> String {
    format!("invalid date format: {date}")
}

pub fn could_not_read_log(path: &str, err: &str) -> String {
    format!("could not read log file '{path}': {err}")
}

pub const COULD_NOT_READ_STDIN: &str = "could not read log from standard input";
pub const NO_COMMENT_CHAR: &str =
    "unable to select a comment character that is not used\nin the current commit message";
pub const EMPTY_MESSAGE: &str = "Aborting commit due to empty commit message.";
pub const SUPPLY_MESSAGE: &str = "Please supply the message using either -m or -F option.";

pub const AMEND_EMPTY: &str = "You asked to amend the most recent commit, but doing so would make
it empty. You can repeat your command with --allow-empty, or you can
remove the commit entirely with \"git reset HEAD^\".";

pub const UNMERGED: &str = "Committing is not possible because you have unmerged files.";
/// Advice `resolveConflict` (shown without the "Disable" line).
pub const UNMERGED_HINT: &str = "\
Fix them up in the work tree, and then use 'git add/rm <file>'
as appropriate to mark resolution and make a commit.";
pub const UNRESOLVED: &str = "Exiting because of an unresolved conflict.";

pub fn instructions_strip(c: &str) -> String {
    format!(
        "Please enter the commit message for your changes. Lines starting\nwith '{c}' will be ignored, and an empty message aborts the commit.\n"
    )
}

pub fn instructions_keep(c: &str) -> String {
    format!(
        "Please enter the commit message for your changes. Lines starting\nwith '{c}' will be kept; you may remove them yourself if you want to.\nAn empty message aborts the commit.\n"
    )
}

pub const SCISSORS_HELP: &str =
    "Do not modify or remove the line above.\nEverything below it will be ignored.\n";

pub fn template_author(ident: &str) -> String {
    format!("Author:    {ident}")
}

pub fn template_date(date: &str) -> String {
    format!("Date:      {date}")
}

pub fn summary_author(ident: &str) -> String {
    format!(" Author: {ident}")
}

pub fn summary_date(date: &str) -> String {
    format!(" Date: {date}")
}

pub const ROOT_COMMIT: &str = " (root-commit)";
pub const DETACHED_HEAD: &str = "detached HEAD";

pub fn files_changed(files: usize, ins: usize, del: usize) -> String {
    let mut s = format!(" {files} file{} changed", if files == 1 { "" } else { "s" });
    if ins > 0 || del == 0 {
        s.push_str(&format!(
            ", {ins} insertion{}(+)",
            if ins == 1 { "" } else { "s" }
        ));
    }
    if del > 0 || ins == 0 {
        s.push_str(&format!(
            ", {del} deletion{}(-)",
            if del == 1 { "" } else { "s" }
        ));
    }
    s
}

pub fn reflog_message(kind: &str, first_line: &str) -> String {
    format!("{kind}: {first_line}")
}

pub const REFLOG_COMMIT: &str = "commit";
pub const REFLOG_INITIAL: &str = "commit (initial)";
pub const REFLOG_AMEND: &str = "commit (amend)";
pub const REFLOG_MERGE: &str = "commit (merge)";

pub const AMEND_DURING_MERGE: &str = "You are in the middle of a merge -- cannot amend.";
pub const PARTIAL_DURING_MERGE: &str = "cannot do a partial commit during a merge.";
pub const MERGE_NOTICE: &str = "
It looks like you may be committing a merge.
If this is not correct, please run
\tgit update-ref -d MERGE_HEAD
and try again.
";

// Messages of rgit itself.

/// Decision on #45: hooks are not run in M2; never skip them silently.
pub fn hook_not_run(name: &str) -> String {
    format!("warning: rgit does not run hooks yet; the '{name}' hook was not run")
}

pub const SEQUENCER_NOT_SUPPORTED: &str =
    "rgit: commit: concluding a cherry-pick or revert is not supported yet";
