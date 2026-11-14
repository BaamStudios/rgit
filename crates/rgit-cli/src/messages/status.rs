// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git status` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git status [<options>] [--] [<pathspec>...]"];

pub const HELP_SHORT: &str = "show status concisely";
pub const HELP_BRANCH: &str = "show branch information";
pub const HELP_AHEAD_BEHIND: &str = "compute full ahead/behind values";
pub const HELP_LONG: &str = "show status in long format (default)";
pub const HELP_PORCELAIN: &str = "machine-readable output";
pub const HELP_Z: &str = "terminate entries with NUL";
pub const HELP_UNTRACKED: &str =
    "show untracked files, optional modes: all, normal, no. (Default: all)";

pub const HELP_IGNORED: &str =
    "show ignored files, optional modes: traditional, matching, no. (Default: traditional)";

pub fn invalid_ignored(mode: &str) -> String {
    format!("Invalid ignored mode '{mode}'")
}

pub const IGNORED_UNTRACKED_COMBINATION: &str =
    "Unsupported combination of ignored and untracked-files arguments";

pub fn invalid_untracked(mode: &str) -> String {
    format!("Invalid untracked files mode '{mode}'")
}

pub const LONG_AND_Z: &str = "options '--long' and '-z' cannot be used together";

pub fn unsupported_porcelain(v: &str) -> String {
    format!("unsupported porcelain version '{v}'")
}

// Long format. Hint texts are printed inside "  (" ... ")" when advice is
// enabled.

pub fn on_branch(b: &str) -> String {
    format!("On branch {b}")
}
pub fn detached_at(name: &str) -> String {
    format!("HEAD detached at {name}")
}
pub fn detached_from(name: &str) -> String {
    format!("HEAD detached from {name}")
}
pub const NOT_ON_BRANCH: &str = "Not currently on any branch.";
pub const NO_COMMITS_YET: &str = "No commits yet";

pub const LABEL_NEW: &str = "new file:";
pub const LABEL_MODIFIED: &str = "modified:";
pub const LABEL_DELETED: &str = "deleted:";
pub const LABEL_TYPECHANGE: &str = "typechange:";
pub const LABEL_BOTH_DELETED: &str = "both deleted:";
pub const LABEL_ADDED_BY_US: &str = "added by us:";
pub const LABEL_DELETED_BY_THEM: &str = "deleted by them:";
pub const LABEL_ADDED_BY_THEM: &str = "added by them:";
pub const LABEL_DELETED_BY_US: &str = "deleted by us:";
pub const LABEL_BOTH_ADDED: &str = "both added:";
pub const LABEL_BOTH_MODIFIED: &str = "both modified:";

pub const ALL_CONFLICTS_FIXED: &str = "All conflicts fixed but you are still merging.";
pub const HINT_CONCLUDE_MERGE: &str = "use \"git commit\" to conclude merge";
pub const UNMERGED_PATHS: &str = "You have unmerged paths.";
pub const HINT_FIX_CONFLICTS: &str = "fix conflicts and run \"git commit\"";
pub const HINT_MERGE_ABORT: &str = "use \"git merge --abort\" to abort the merge";

pub const STAGED_HEADER: &str = "Changes to be committed:";
pub const HINT_UNSTAGE_INITIAL: &str = "use \"git rm --cached <file>...\" to unstage";
pub const HINT_UNSTAGE: &str = "use \"git restore --staged <file>...\" to unstage";
pub const UNMERGED_HEADER: &str = "Unmerged paths:";
pub const HINT_MARK_RESOLUTION: &str = "use \"git add <file>...\" to mark resolution";
pub const HINT_MARK_RESOLUTION_RM: &str =
    "use \"git add/rm <file>...\" as appropriate to mark resolution";
pub const UNSTAGED_HEADER: &str = "Changes not staged for commit:";
pub const HINT_ADD_RM: &str = "use \"git add/rm <file>...\" to update what will be committed";
pub const HINT_ADD: &str = "use \"git add <file>...\" to update what will be committed";
pub const HINT_RESTORE: &str =
    "use \"git restore <file>...\" to discard changes in working directory";
pub const UNTRACKED_HEADER: &str = "Untracked files:";
pub const IGNORED_HEADER: &str = "Ignored files:";
pub const HINT_INCLUDE_IGNORED: &str =
    "use \"git add -f <file>...\" to include in what will be committed";
pub const HINT_INCLUDE: &str = "use \"git add <file>...\" to include in what will be committed";

// Closing line: the text and the hint appended to it when advice is enabled.
pub const UNTRACKED_NOT_LISTED: (&str, &str) = (
    "Untracked files not listed",
    " (use -u option to show untracked files)",
);
pub const NOTHING_ADDED: (&str, &str) = (
    "no changes added to commit",
    " (use \"git add\" and/or \"git commit -a\")",
);
pub const UNTRACKED_ONLY: (&str, &str) = (
    "nothing added to commit but untracked files present",
    " (use \"git add\" to track)",
);
pub const NOTHING_INITIAL: (&str, &str) = (
    "nothing to commit",
    " (create/copy files and use \"git add\" to track)",
);
pub const NOTHING_NO_UNTRACKED: (&str, &str) =
    ("nothing to commit", " (use -u to show untracked files)");
pub const NOTHING_CLEAN: (&str, &str) = ("nothing to commit, working tree clean", "");

// Upstream tracking lines.
fn commits(n: usize) -> &'static str {
    if n == 1 { "commit" } else { "commits" }
}
pub fn upstream_gone(up: &str) -> String {
    format!("Your branch is based on '{up}', but the upstream is gone.")
}
pub const HINT_UNSET_UPSTREAM: &str = "use \"git branch --unset-upstream\" to fixup";
pub fn up_to_date(up: &str) -> String {
    format!("Your branch is up to date with '{up}'.")
}
pub fn ahead(up: &str, n: usize) -> String {
    format!("Your branch is ahead of '{up}' by {n} {}.", commits(n))
}
pub const HINT_PUSH: &str = "use \"git push\" to publish your local commits";
pub fn behind(up: &str, n: usize) -> String {
    format!(
        "Your branch is behind '{up}' by {n} {}, and can be fast-forwarded.",
        commits(n)
    )
}
pub const HINT_PULL_FF: &str = "use \"git pull\" to update your local branch";
pub fn diverged(up: &str, a: usize, b: usize) -> String {
    format!(
        "Your branch and '{up}' have diverged,\n\
         and have {a} and {b} different commits each, respectively."
    )
}
pub const HINT_PULL_MERGE: &str =
    "use \"git pull\" if you want to integrate the remote branch with yours";
pub fn different(up: &str) -> String {
    format!("Your branch and '{up}' refer to different commits.")
}
pub const HINT_AHEAD_BEHIND: &str = "use \"git status --ahead-behind\" for details";
