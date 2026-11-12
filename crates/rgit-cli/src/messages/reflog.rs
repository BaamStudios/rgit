// SPDX-License-Identifier: MIT
//! Messages and usage lines of `git reflog` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

/// `git reflog -h`.
pub const USAGE: &str = "\
usage: git reflog [show] [<log-options>] [<ref>]
   or: git reflog list
   or: git reflog exists <ref>
   or: git reflog write <ref> <old-oid> <new-oid> <message>
   or: git reflog delete [--rewrite] [--updateref]
                         [--dry-run | -n] [--verbose] <ref>@{<specifier>}...
   or: git reflog drop [--all [--single-worktree] | <refs>...]
   or: git reflog expire [--expire=<time>] [--expire-unreachable=<time>]
                         [--rewrite] [--updateref] [--stale-fix]
                         [--dry-run | -n] [--verbose] [--all [--single-worktree] | <refs>...]

";

pub const USAGE_SHOW: &str = "usage: git reflog [show] [<log-options>] [<ref>]\n\n";
pub const USAGE_LIST: &str = "git reflog list";
pub const USAGE_EXISTS: &str = "git reflog exists <ref>";

pub fn list_no_arguments(arg: &str) -> String {
    format!("list does not accept arguments: '{arg}'")
}

pub fn invalid_ref_format(name: &str) -> String {
    format!("invalid ref format: {name}")
}

pub fn ambiguous_argument(arg: &str) -> String {
    format!(
        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

pub fn no_commits_yet(branch: &str) -> String {
    format!("your current branch '{branch}' does not have any commits yet")
}

/// rgit's own text for what is not implemented yet.
pub fn not_supported(what: &str) -> String {
    format!("rgit reflog: {what} is not supported yet")
}
