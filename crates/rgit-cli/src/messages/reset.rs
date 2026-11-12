// SPDX-License-Identifier: MIT
//! Messages of `git reset`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git reset [--mixed | --soft | --hard | --merge | --keep] [-q] [<commit>]",
    "git reset [-q] [<tree-ish>] [--] <pathspec>...",
    "git reset [-q] [--pathspec-from-file [--pathspec-file-nul]] [<tree-ish>]",
    "git reset --patch [<tree-ish>] [--] [<pathspec>...]",
];

pub const HELP_QUIET: &str = "be quiet, only report errors";
pub const HELP_SOFT: &str = "reset only HEAD";
pub const HELP_MIXED: &str = "reset HEAD and index";

pub const SOFT_WITH_PATHS: &str = "Cannot do soft reset with paths.";
pub const MODE_NOT_SUPPORTED: &str = "rgit: only 'git reset --soft' is implemented so far";

pub fn ambiguous(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

pub fn not_a_commit(arg: &str) -> String {
    format!("Could not parse object '{arg}'.")
}

pub fn wrong_kind(oid: &str, kind: &str) -> String {
    format!("error: object {oid} is a {kind}, not a commit")
}

pub fn failed_to_resolve(arg: &str) -> String {
    format!("Failed to resolve '{arg}' as a valid revision.")
}

pub fn moving_to(arg: &str) -> String {
    format!("reset: moving to {arg}")
}

pub const UPDATING_ORIG_HEAD: &str = "updating ORIG_HEAD";
