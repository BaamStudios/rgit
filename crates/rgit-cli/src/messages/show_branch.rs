// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git show-branch` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git show-branch [-a | --all] [-r | --remotes] [--topo-order | --date-order]\n\
     [--current] [--color[=<when>] | --no-color] [--sparse]\n\
     [--more=<n> | --list | --independent | --merge-base]\n\
     [--no-name | --sha1-name] [--topics]\n\
     [(<rev> | <glob>)...]",
    "git show-branch (-g | --reflog)[=<n>[,<base>]] [--list] [<ref>]",
];

pub const HELP_ALL: &str = "show remote-tracking and local branches";
pub const HELP_REMOTES: &str = "show remote-tracking branches";
pub const HELP_COLOR: &str = "color '*!+-' corresponding to the branch";
pub const HELP_MORE: &str = "show <n> more commits after the common ancestor";
pub const HELP_LIST: &str = "synonym to more=-1";
pub const HELP_NO_NAME: &str = "suppress naming strings";
pub const HELP_CURRENT: &str = "include the current branch";
pub const HELP_SHA1_NAME: &str = "name commits with their object names";
pub const HELP_MERGE_BASE: &str = "show possible merge bases";
pub const HELP_INDEPENDENT: &str = "show refs unreachable from any other ref";
pub const HELP_TOPO_ORDER: &str = "show commits in topological order";
pub const HELP_TOPICS: &str = "show only commits not on the first branch";
pub const HELP_SPARSE: &str = "show merges reachable from only one tip";
pub const HELP_DATE_ORDER: &str = "topologically sort, maintaining date order where possible";
pub const HELP_REFLOG: &str = "show <n> most recent ref-log entries starting at base";
pub const ARGH_REFLOG: &str = "<n>[,<base>]";

/// Printed on stderr (without a prefix) when there is nothing to show.
pub const NO_REVS: &str = "No revs to be shown.";

pub const REFLOG_INCOMPATIBLE: &str =
    "options '--reflog' and '--all/--remotes/--independent/--merge-base' cannot be used together";
pub const REFLOG_CURRENT: &str = "options '--reflog' and '--current' cannot be used together";
pub const REFLOG_ONE_BRANCH: &str = "--reflog option needs one branch name";
pub const HEAD_NOT_VALID: &str = "no branches given, and HEAD is not valid";

pub fn bad_sha1_reference(name: &str) -> String {
    format!("bad sha1 reference {name}")
}

pub fn no_matching_refs(pattern: &str) -> String {
    format!("no matching refs with {pattern}")
}

pub fn ignoring_ref(name: &str, max: usize) -> String {
    format!("ignoring {name}; cannot handle more than {max} refs")
}

pub fn no_such_ref(name: &str) -> String {
    format!("no such ref {name}")
}

pub fn only_entries(max: usize) -> String {
    format!("only {max} entries can be shown at one time.")
}

pub fn unrecognized_reflog_param(value: &str) -> String {
    format!("unrecognized reflog param '{value}'")
}
