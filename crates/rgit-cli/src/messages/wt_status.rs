// SPDX-License-Identifier: MIT
//! Lines of the long status format that `commit` prints (and puts into
//! the message template). Reproduced verbatim (CLEANROOM.md).

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
pub const UNTRACKED_NOT_LISTED: &str = "Untracked files not listed";
pub const UNTRACKED_NOT_LISTED_HINT: &str =
    "Untracked files not listed (use -u option to show untracked files)";
pub const INITIAL_COMMIT: &str = "Initial commit";

pub fn up_to_date(up: &str) -> String {
    format!("Your branch is up to date with '{up}'.")
}
fn commits(n: usize) -> &'static str {
    if n == 1 { "commit" } else { "commits" }
}
pub fn ahead(up: &str, n: usize) -> String {
    format!("Your branch is ahead of '{up}' by {n} {}.", commits(n))
}
pub fn behind(up: &str, n: usize) -> String {
    format!(
        "Your branch is behind '{up}' by {n} {}, and can be fast-forwarded.",
        commits(n)
    )
}
pub fn diverged(up: &str, a: usize, b: usize) -> [String; 2] {
    [
        format!("Your branch and '{up}' have diverged,"),
        format!("and have {a} and {b} different commits each, respectively."),
    ]
}
pub fn gone(up: &str) -> String {
    format!("Your branch is based on '{up}', but the upstream is gone.")
}
pub const HINT_PUSH: &str = "  (use \"git push\" to publish your local commits)";
pub const HINT_PULL_FF: &str = "  (use \"git pull\" to update your local branch)";
pub const HINT_UNSET_UPSTREAM: &str = "  (use \"git branch --unset-upstream\" to fixup)";

pub const STAGED_HEADER: &str = "Changes to be committed:";
pub const HINT_UNSTAGE_INITIAL: &str = "  (use \"git rm --cached <file>...\" to unstage)";
pub const HINT_UNSTAGE: &str = "  (use \"git restore --staged <file>...\" to unstage)";
pub const HINT_UNSTAGE_AMEND: &str =
    "  (use \"git restore --source=HEAD^1 --staged <file>...\" to unstage)";
pub const UNSTAGED_HEADER: &str = "Changes not staged for commit:";
pub const HINT_ADD: &str = "  (use \"git add <file>...\" to update what will be committed)";
pub const HINT_ADD_RM: &str = "  (use \"git add/rm <file>...\" to update what will be committed)";
pub const HINT_RESTORE: &str =
    "  (use \"git restore <file>...\" to discard changes in working directory)";
pub const UNTRACKED_HEADER: &str = "Untracked files:";
pub const HINT_INCLUDE: &str = "  (use \"git add <file>...\" to include in what will be committed)";

pub const LABEL_NEW: &str = "new file:";
pub const LABEL_MODIFIED: &str = "modified:";
pub const LABEL_DELETED: &str = "deleted:";
pub const LABEL_RENAMED: &str = "renamed:";
pub const LABEL_TYPECHANGE: &str = "typechange:";

pub const NO_CHANGES: &str = "No changes";
pub const NOTHING_ADDED_HINT: &str =
    "no changes added to commit (use \"git add\" and/or \"git commit -a\")";
pub const NOTHING_ADDED: &str = "no changes added to commit";
pub const UNTRACKED_ONLY_HINT: &str =
    "nothing added to commit but untracked files present (use \"git add\" to track)";
pub const UNTRACKED_ONLY: &str = "nothing added to commit but untracked files present";
pub const NOTHING_INITIAL_HINT: &str =
    "nothing to commit (create/copy files and use \"git add\" to track)";
pub const NOTHING_NO_UNTRACKED_HINT: &str = "nothing to commit (use -u to show untracked files)";
pub const NOTHING: &str = "nothing to commit";
pub const NOTHING_CLEAN: &str = "nothing to commit, working tree clean";

pub const STILL_MERGING: &str = "All conflicts fixed but you are still merging.";
pub const HINT_CONCLUDE_MERGE: &str = "  (use \"git commit\" to conclude merge)";

pub fn currently_cherry_picking(commit: &str) -> String {
    format!("You are currently cherry-picking commit {commit}.")
}
pub fn currently_reverting(commit: &str) -> String {
    format!("You are currently reverting commit {commit}.")
}
pub const CHERRY_PICK_IN_PROGRESS: &str = "Cherry-pick currently in progress.";
pub const REVERT_IN_PROGRESS: &str = "Revert currently in progress.";
pub fn hint_fix_and_continue(action: &str) -> String {
    format!("  (fix conflicts and run \"git {action} --continue\")")
}
pub fn hint_all_fixed_continue(action: &str) -> String {
    format!("  (all conflicts fixed: run \"git {action} --continue\")")
}
pub fn hint_run_continue(action: &str) -> String {
    format!("  (run \"git {action} --continue\" to continue)")
}
pub fn hint_skip(action: &str) -> String {
    format!("  (use \"git {action} --skip\" to skip this patch)")
}
pub fn hint_abort_operation(action: &str) -> String {
    format!("  (use \"git {action} --abort\" to cancel the {action} operation)")
}
