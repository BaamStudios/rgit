// SPDX-License-Identifier: MIT
//! Messages of `git fetch`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git fetch [<options>] [<repository> [<refspec>...]]",
    "git fetch [<options>] <group>",
    "git fetch --multiple [<options>] [(<repository>|<group>)...]",
    "git fetch --all [<options>]",
];

pub const HELP_VERBOSE: &str = "be more verbose";
pub const HELP_QUIET: &str = "be more quiet";
pub const HELP_UPLOAD_PACK: &str = "path to upload pack on remote end";
pub const HELP_FORCE: &str = "force overwrite of local reference";
pub const HELP_TAGS: &str = "fetch all tags and associated objects";
pub const HELP_NO_TAGS: &str = "do not fetch all tags (--no-tags)";
pub const HELP_PRUNE: &str = "prune remote-tracking branches no longer on remote";
pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_APPEND: &str = "append to .git/FETCH_HEAD instead of overwriting";
pub const HELP_WRITE_FETCH_HEAD: &str = "write fetched references to the FETCH_HEAD file";
pub const HELP_KEEP: &str = "keep downloaded pack";
pub const HELP_UPDATE_HEAD_OK: &str = "allow updating of HEAD ref";
pub const HELP_PROGRESS: &str = "force progress reporting";
pub const HELP_SERVER_OPTION: &str = "option to transmit";
pub const HELP_IPV4: &str = "use IPv4 addresses only";
pub const HELP_IPV6: &str = "use IPv6 addresses only";
pub const HELP_AUTO_MAINTENANCE: &str = "run 'maintenance --auto' after fetching";
pub const HELP_SHOW_FORCED: &str = "check for forced-updates on all updated branches";
pub const HELP_WRITE_COMMIT_GRAPH: &str = "write the commit-graph after fetching";

pub const HELP_DEPTH: &str = "deepen history of shallow clone";
pub const HELP_SHALLOW_SINCE: &str = "deepen history of shallow repository based on time";
pub const HELP_SHALLOW_EXCLUDE: &str = "deepen history of shallow clone, excluding ref";
pub const HELP_DEEPEN: &str = "deepen history of shallow clone";
pub const HELP_UNSHALLOW: &str = "convert to a complete repository";
pub const HELP_UPDATE_SHALLOW: &str = "accept refs that update .git/shallow";
pub fn depth_not_positive(depth: &str) -> String {
    format!("depth {depth} is not a positive number")
}
pub fn options_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}
pub const NEGATIVE_DEEPEN: &str = "negative depth in --deepen is not supported";
pub const UNSHALLOW_COMPLETE: &str = "--unshallow on a complete repository does not make sense";
pub const HELP_ALL: &str = "fetch from all remotes";
pub const HELP_SET_UPSTREAM: &str = "set upstream for git pull/fetch";
pub const HELP_ATOMIC: &str = "use atomic transaction to update references";
pub const HELP_MULTIPLE: &str = "fetch from multiple remotes";
pub const HELP_JOBS: &str = "number of submodules fetched in parallel";
pub const HELP_PRUNE_TAGS: &str = "prune local tags no longer on remote and clobber changed tags";
pub const HELP_PORCELAIN: &str = "machine-readable output";
pub const HELP_REFETCH: &str = "re-fetch without negotiating common commits";
pub const HELP_REFMAP: &str = "specify fetch refmap";
pub const HELP_STDIN: &str = "accept refspecs from stdin";

pub const HELP_NEGOTIATION_RESTRICT: &str =
    "report that we have only objects reachable from this object";
pub const HELP_NEGOTIATION_TIP: &str = "alias of --negotiation-restrict";
pub const HELP_NEGOTIATION_INCLUDE: &str = "ensure this ref is always sent as a negotiation have";
pub const REFMAP_WITHOUT_REFSPECS: &str =
    "--refmap option is only meaningful with command-line refspec(s)";

/// `GIT_TRACE` lines of a parallel multiple fetch.
pub fn trace_parallel(tasks: i64) -> String {
    format!("run_processes_parallel: preparing to run up to {tasks} tasks")
}
pub const TRACE_PARALLEL_DONE: &[u8] = b"run_processes_parallel: done";

pub fn object_does_not_exist(oid: &str) -> String {
    format!("the object {oid} does not exist")
}

pub const ALL_NO_ARGUMENTS: &str = "fetch --all does not take a repository argument";
pub const GROUP_WITH_REFSPECS: &str =
    "fetching a group and specifying refspecs does not make sense";
pub const NO_SOURCE_BRANCH_UPSTREAM: &str = "no source branch found;\nyou need to specify exactly one branch with the --set-upstream option";
pub const MULTIPLE_BRANCHES_UPSTREAM: &str =
    "multiple branches detected, incompatible with --set-upstream";

/// The advice after a `followRemoteHEAD=warn` warning.
pub fn remote_head_hint(remote: &str, branch: &str) -> String {
    format!(
        "Run 'git remote set-head {remote} {branch}' to follow the change, or modify
either of the 'remote.{remote}.followRemoteHEAD' or 'fetch.followRemoteHEAD'
configuration variables to handle the situation differently.

Using this specific setting

    git config set remote.{remote}.followRemoteHEAD warn-if-not-{branch}

will suppress the warning until the remote changes HEAD to something else."
    )
}
/// A local ref could not be updated.
pub fn prune_advice(remote: &str) -> String {
    format!(
        "some local refs could not be updated; try running\n 'git remote prune {remote}' to remove any old, conflicting branches"
    )
}
/// Before each remote of a multiple fetch.
pub fn fetching(name: &str) -> String {
    format!("Fetching {name}")
}
/// A remote of a multiple fetch failed.
pub fn could_not_fetch(name: &str) -> String {
    format!("could not fetch {name}")
}
/// A remote of a parallel multiple fetch failed.
pub fn could_not_fetch_code(name: &str, code: i32) -> String {
    format!("could not fetch '{name}' (exit code: {code})")
}
pub fn no_such_remote_or_group(name: &str) -> String {
    format!("no such remote or remote group: {name}")
}
pub fn invalid_output(value: &str) -> String {
    format!("invalid value for 'fetch.output': '{value}'")
}
pub fn missing_value(key: &str) -> String {
    format!("missing value for '{key}'")
}
/// `--set-upstream` on a detached HEAD.
pub fn upstream_detached(branch: &str, remote: &str) -> String {
    format!(
        "could not set upstream of HEAD to '{branch}' from '{remote}' when it does not point to any branch."
    )
}
