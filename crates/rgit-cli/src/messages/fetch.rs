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
