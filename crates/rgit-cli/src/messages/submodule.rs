// SPDX-License-Identifier: MIT
//! Messages about submodules shown by the core commands (status, diff)
//! (CLEANROOM.md: reproduced verbatim, collected here).

pub fn bad_ignore_argument(value: &str) -> String {
    format!("bad --ignore-submodules argument: {value}")
}

pub fn bad_submodule_format(value: &str) -> String {
    format!("failed to parse --submodule option parameter: '{value}'")
}

pub fn unknown_diff_submodule(value: &str) -> String {
    format!("Unknown value for 'diff.submodule' config variable: '{value}'")
}

pub const HINT_SUBMODULE_CONTENT: &str =
    "commit or discard the untracked or modified content in submodules";

pub const NEW_COMMITS: &str = "new commits";
pub const MODIFIED_CONTENT: &str = "modified content";
pub const UNTRACKED_CONTENT: &str = "untracked content";

pub const HELP_IGNORE_SUBMODULES: &str =
    "ignore changes to submodules, optional when: all, dirty, untracked. (Default: all)";

pub const CONTAINS_UNTRACKED: &str = "contains untracked content";
pub const CONTAINS_MODIFIED: &str = "contains modified content";

pub fn new_submodule(a: &str, b: &str) -> String {
    format!("{a}...{b} (new submodule)")
}

pub fn submodule_deleted(a: &str, b: &str) -> String {
    format!("{a}...{b} (submodule deleted)")
}

/// A commit of the submodule summary that the submodule lacks.
pub fn summary_missing_commit(path: &str, id: &str) -> String {
    format!("Warn: {path} doesn't contain commit {id}")
}

pub fn commits_not_present(a: &str, b: &str) -> String {
    format!("{a}...{b} (commits not present)")
}

pub fn no_name_for(path: &str) -> String {
    format!("could not lookup name for submodule '{path}'")
}

pub fn in_unpopulated(path: &str) -> String {
    format!("in unpopulated submodule '{path}'")
}

pub fn skipping_ignored(path: &str) -> String {
    format!(
        "Skipping submodule due to ignore=all: {path}\nUse --force if you really want to add the submodule."
    )
}

pub fn pathspec_in_submodule(spec: &str, path: &str) -> String {
    format!("Pathspec '{spec}' is in submodule '{path}'")
}

pub fn could_not_migrate(from: &str, to: &str, reason: &str) -> String {
    format!("could not migrate git directory from '{from}' to '{to}': {reason}")
}

pub fn migrating(path: &str, from: &str, to: &str) -> String {
    format!("Migrating git directory of '{path}' from\n'{from}' to\n'{to}'")
}

pub fn not_a_git_repository(path: &str) -> String {
    format!("'{path}' not recognized as a git repository")
}

pub fn could_not_access(path: &str) -> String {
    format!("Could not access submodule '{path}'")
}

pub fn gitfile_not_repo(path: &str) -> String {
    format!("gitfile does not point to a valid repository: {path}")
}
