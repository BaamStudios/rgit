// SPDX-License-Identifier: MIT
//! Messages of `git switch` (Documentation/git-switch.adoc).

pub const USAGE: &[&str] = &["git switch [<options>] [<branch>]"];
pub const HELP_CREATE: &str = "create and switch to a new branch";
pub const HELP_DETACH: &str = "detach HEAD at named commit";
pub const HELP_QUIET: &str = "suppress progress reporting";

pub fn switched_new(name: &str) -> String {
    format!("Switched to a new branch '{name}'")
}
pub fn switched(name: &str) -> String {
    format!("Switched to branch '{name}'")
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
pub const BRANCH_CREATED_FROM_HEAD: &str = "branch: Created from HEAD";
/// rgit-only: the work tree would have to change (#42, #107).
pub const TREE_CHANGE_UNSUPPORTED: &str =
    "switching to a commit with a different tree is not supported yet";
/// rgit-only.
pub const UNBORN_UNSUPPORTED: &str = "switching from an unborn branch is not supported yet";
