// SPDX-License-Identifier: MIT
//! Messages and usage of `git commit-graph` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE_VERIFY: &str =
    "git commit-graph verify [--object-dir <dir>] [--shallow] [--[no-]progress]";
pub const USAGE_WRITE: &str = "git commit-graph write [--object-dir <dir>] [--append]\n[--split[=<strategy>]] [--reachable | --stdin-packs | --stdin-commits]\n[--changed-paths] [--[no-]max-new-filters <n>] [--[no-]progress]\n<split-options>";

pub const H_OBJECT_DIR: &str = "the object directory to store the graph";
pub const H_SHALLOW: &str = "if the commit-graph is split, only verify the tip file";
pub const H_PROGRESS: &str = "force progress reporting";
pub const H_REACHABLE: &str = "start walk at all refs";
pub const H_STDIN_PACKS: &str = "scan pack-indexes listed by stdin for commits";
pub const H_STDIN_COMMITS: &str = "start walk at commits listed by stdin";
pub const H_APPEND: &str = "include all commits already in the commit-graph file";
pub const H_CHANGED_PATHS: &str = "enable computation for changed paths";
pub const H_SPLIT: &str = "allow writing an incremental commit-graph file";
pub const H_MAX_COMMITS: &str = "maximum number of commits in a non-base split commit-graph";
pub const H_SIZE_MULTIPLE: &str = "maximum ratio between two levels of a split commit-graph";
pub const H_EXPIRE_TIME: &str = "only expire files older than a given date-time";
pub const H_MAX_NEW_FILTERS: &str = "maximum number of changed-path Bloom filters to compute";

pub const AT_MOST_ONE_SOURCE: &str =
    "use at most one of --reachable, --stdin-commits, or --stdin-packs";
pub const DISABLED: &str = "attempting to write a commit-graph, but 'core.commitGraph' is disabled";
pub const MAX_NEW_FILTERS_NUMBER: &str = "option `max-new-filters' expects a numerical value";
pub const COLLECTING_REFS: &str = "Collecting referenced commits";
pub const COLLECTING_INPUT: &str = "Collecting commits from input";

pub fn unrecognized_split(arg: &str) -> String {
    format!("unrecognized --split argument, {arg}")
}

pub fn no_object_dir(dir: &str) -> String {
    format!("could not find object directory matching {dir}")
}

pub fn non_hex(line: &str) -> String {
    format!("unexpected non-hex object ID: {line}")
}

pub fn invalid_object(hex: &str) -> String {
    format!("invalid object: {hex}")
}

pub fn error_adding_pack(path: &str) -> String {
    format!("error adding pack {path}")
}

pub fn finding_commits(packs: usize) -> String {
    format!("Finding commits for commit graph in {packs} packs")
}

pub fn unsupported_changed_paths_version(v: i64) -> String {
    format!(
        "attempting to write a commit-graph, but 'commitGraph.changedPathsVersion' ({v}) is not supported"
    )
}

pub fn malformed_expiration(v: &str) -> String {
    format!("malformed expiration date '{v}'")
}

pub fn could_not_read(hex: &str) -> String {
    format!("Could not read {hex}")
}

pub fn exists_in_graph_only(hex: &str) -> String {
    format!("commit {hex} exists in commit-graph but not in the object database")
}
