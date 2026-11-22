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
pub const USAGE_EXPIRE: &str = "\
git reflog expire [--expire=<time>] [--expire-unreachable=<time>]
                         [--rewrite] [--updateref] [--stale-fix]
                         [--dry-run | -n] [--verbose] [--all [--single-worktree] | <refs>...]";
pub const HELP_DRY_RUN: &str = "do not actually prune any entries";
pub const HELP_REWRITE: &str =
    "rewrite the old SHA1 with the new SHA1 of the entry that now precedes it";
pub const HELP_UPDATEREF: &str = "update the reference to the value of the top reflog entry";
pub const HELP_VERBOSE: &str = "print extra information on screen";
pub const HELP_EXPIRE: &str = "prune entries older than the specified time";
pub const HELP_EXPIRE_UNREACHABLE: &str =
    "prune entries older than <time> that are not reachable from the current tip of the branch";
pub const HELP_STALE_FIX: &str = "prune any reflog entries that point to broken commits";
pub const HELP_ALL: &str = "process the reflogs of all references";
pub const HELP_SINGLE_WORKTREE: &str =
    "limits processing to reflogs from the current worktree only";
pub const ARGH_TIMESTAMP: &str = "timestamp";

/// `reflog expire --verbose`: what happens to an entry.
pub const KEEP: &str = "keep";
pub const PRUNE: &str = "prune";
pub const WOULD_PRUNE: &str = "would prune";

pub fn invalid_timestamp_option(value: &str, option: &str) -> String {
    format!("invalid timestamp '{value}' given to '--{option}'")
}

pub fn reflog_not_found(name: &str) -> String {
    format!("reflog could not be found: '{name}'")
}
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

pub const USAGE_DELETE: &str = "\
git reflog delete [--rewrite] [--updateref]
                         [--dry-run | -n] [--verbose] <ref>@{<specifier>}...";
pub const USAGE_DROP: &str = "git reflog drop [--all [--single-worktree] | <refs>...]";
pub const USAGE_WRITE: &str = "git reflog write <ref> <old-oid> <new-oid> <message>";
pub const HELP_DROP_ALL: &str = "drop the reflogs of all references";
pub const HELP_DROP_SINGLE_WORKTREE: &str = "drop reflogs from the current worktree only";
pub const NO_REFLOG_TO_DELETE: &str = "no reflog specified to delete";
pub const REFS_WITH_ALL: &str = "references specified along with --all";

pub fn not_a_reflog(arg: &str) -> String {
    format!("not a reflog: {arg}")
}

pub fn no_reflog_for(arg: &str) -> String {
    format!("no reflog for '{arg}'")
}

pub fn invalid_reference_name(name: &str) -> String {
    format!("invalid reference name: {name}")
}

/// `which` is `old` or `new`.
pub fn invalid_object_id(which: &str, hex: &str) -> String {
    format!("invalid {which} object ID: '{hex}'")
}

pub fn object_does_not_exist(which: &str, hex: &str) -> String {
    format!("{which} object '{hex}' does not exist")
}

/// `reflog expire --stale-fix --verbose`, before the walk.
pub const MARKING_REACHABLE: &str = "Marking reachable objects...";
