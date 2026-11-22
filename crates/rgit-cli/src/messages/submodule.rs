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
