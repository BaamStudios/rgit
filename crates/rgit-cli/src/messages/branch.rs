// SPDX-License-Identifier: MIT
//! Messages of `git branch`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git branch [<options>] [-r | -a] [--merged] [--no-merged]",
    "git branch [<options>] [-f] [--recurse-submodules] <branch-name> [<start-point>]",
    "git branch [<options>] [-l] [<pattern>...]",
    "git branch [<options>] [-r] (-d | -D) <branch-name>...",
    "git branch [<options>] (-m | -M) [<old-branch>] <new-branch>",
    "git branch [<options>] (-c | -C) [<old-branch>] <new-branch>",
    "git branch [<options>] [-r | -a] [--points-at]",
    "git branch [<options>] [-r | -a] [--format]",
];

pub const HELP_REMOTES: &str = "act on remote-tracking branches";
pub const HELP_ALL: &str = "list both remote-tracking and local branches";
pub const HELP_LIST: &str = "list branch names";
pub const HELP_SHOW_CURRENT: &str = "show current branch name";
pub const HELP_DELETE: &str = "delete fully merged branch";
pub const HELP_FORCE_DELETE: &str = "delete branch (even if not merged)";

pub const D_NOT_SUPPORTED: &str = "rgit: 'branch -d' (merged check) is not implemented yet; use -D";
pub const HINT_REF_SYNTAX: &str = "hint: See 'git help check-ref-format'
hint: Disable this message with \"git config set advice.refSyntax false\"
";
pub const BRANCH_NAME_REQUIRED: &str = "branch name required";

pub fn already_exists(name: &str) -> String {
    format!("a branch named '{name}' already exists")
}

pub fn not_valid_branch_name(name: &str) -> String {
    format!("'{name}' is not a valid branch name")
}

pub fn not_valid_object_name(name: &str) -> String {
    format!("not a valid object name: '{name}'")
}

pub fn not_found(name: &str) -> String {
    format!("branch '{name}' not found")
}

pub fn deleted(name: &str, abbrev: &str) -> String {
    format!("Deleted branch {name} (was {abbrev}).")
}

pub fn created_from(start: &str) -> String {
    format!("branch: Created from {start}")
}

pub fn detached_at(abbrev: &str) -> String {
    format!("(HEAD detached at {abbrev})")
}
