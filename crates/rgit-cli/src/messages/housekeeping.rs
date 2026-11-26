// SPDX-License-Identifier: MIT
//! Messages of `git repack`, `git prune`, `git prune-packed` and the
//! reachability walk they share, as printed by the reference git
//! (CLEANROOM.md: user-visible strings are reproduced verbatim and
//! collected here).

// ------------------------------------------------------------ shared

pub fn bad_object(name: &str) -> String {
    format!("bad object {name}")
}

pub fn unable_to_read(hex: &str) -> String {
    format!("unable to read {hex}")
}

pub fn unable_to_read_tree(hex: &str) -> String {
    format!("unable to read tree ({hex})")
}

pub fn could_not_read(hex: &str) -> String {
    format!("Could not read {hex}")
}

pub fn failed_to_traverse(hex: &str) -> String {
    format!("Failed to traverse parents of commit {hex}")
}

pub fn unable_to_parse(name: &str) -> String {
    format!("unable to parse object: {name}")
}

pub fn bad_compression(level: i64) -> String {
    format!("bad pack compression level {level}")
}

pub fn cannot_write(path: &str, err: &str) -> String {
    format!("unable to write {path}: {err}")
}

pub fn malformed_expiration(v: &str) -> String {
    format!("malformed expiration date '{v}'")
}

pub fn not_supported(what: &str) -> String {
    format!("{what} is not supported by rgit yet")
}

// ---------------------------------------------------------------- gc

pub const GC_USAGE: &[&str] = &["git gc [<options>]"];
pub const GC_H_QUIET: &str = "suppress progress reporting";
pub const GC_H_PRUNE: &str = "prune unreferenced objects";
pub const GC_ARGH_PRUNE: &str = "date";
pub const GC_H_CRUFT: &str = "pack unreferenced objects separately";
pub const GC_H_MAX_CRUFT: &str = "with --cruft, limit the size of new cruft packs";
pub const GC_H_AGGRESSIVE: &str = "be more thorough (increased runtime)";
pub const GC_H_AUTO: &str = "enable auto-gc mode";
pub const GC_H_DETACH: &str = "perform garbage collection in the background";
pub const GC_H_FORCE: &str = "force running gc even if there may be another gc running";
pub const GC_H_KEEP_LARGEST: &str = "repack all other packs except the largest pack";
pub const GC_H_EXPIRE_TO: &str = "pack prefix to store a pack containing pruned objects";
pub const GC_H_SKIP_FOREGROUND: &str = "skip maintenance tasks typically done in the foreground";
pub const GC_AUTO_BACKGROUND: &str =
    "Auto packing the repository in background for optimum performance.";
pub const GC_AUTO_FOREGROUND: &str = "Auto packing the repository for optimum performance.";
pub const GC_SEE_HELP: &str = "See \"git help gc\" for manual housekeeping.";
pub const GC_TOO_MANY_LOOSE: &str =
    "There are too many unreachable loose objects; run 'git prune' to remove them.";

pub fn gc_running(host: &str, pid: u32) -> String {
    format!("gc is already running on machine '{host}' pid {pid} (use --force if not)")
}

pub fn gc_last_run(log_path: &str, log: &str) -> String {
    format!(
        "The last gc run reported the following. Please correct the root cause\nand remove {log_path}\nAutomatic cleanup will not be performed until the file is removed.\n\n{log}"
    )
}

pub fn invalid_config(key: &str, value: &str) -> String {
    format!("Invalid {key}: '{value}'")
}

pub fn failed_to_run(cmd: &str) -> String {
    format!("failed to run {cmd}")
}

pub fn hook_failed(cmd: &str) -> String {
    format!("unable to enumerate additional recent objects: '{cmd}' failed")
}

pub fn hook_bad_line(line: &str) -> String {
    format!("invalid extra cruft tip: '{line}'")
}

// ------------------------------------------------------------- prune

pub const PRUNE_USAGE: &[&str] =
    &["git prune [-n] [-v] [--progress] [--expire <time>] [--] [<head>...]"];
pub const PRUNE_H_DRY_RUN: &str = "do not remove, show only";
pub const PRUNE_H_VERBOSE: &str = "report pruned objects";
pub const PRUNE_H_PROGRESS: &str = "show progress";
pub const PRUNE_H_EXPIRE: &str = "expire objects older than <time>";
pub const PRUNE_ARGH_EXPIRE: &str = "expiry-date";
pub const PRUNE_H_EXCLUDE_PROMISOR: &str = "limit traversal to objects outside promisor packfiles";
pub const PRUNE_PRECIOUS: &str = "cannot prune in a precious-objects repo";

pub fn unrecognized_argument(arg: &str) -> String {
    format!("unrecognized argument: {arg}")
}

pub fn removing_stale(path: &str) -> String {
    format!("Removing stale temporary file {path}")
}

// ------------------------------------------------------ prune-packed

pub const PRUNE_PACKED_USAGE: &[&str] = &["git prune-packed [-n | --dry-run] [-q | --quiet]"];
pub const PRUNE_PACKED_H_DRY_RUN: &str = "dry run";
pub const PRUNE_PACKED_H_QUIET: &str = "be quiet";
pub const TOO_MANY_ARGUMENTS: &str = "too many arguments";

// ------------------------------------------------------------ repack

pub const REPACK_USAGE: &[&str] = &[
    "git repack [-a] [-A] [-d] [-f] [-F] [-l] [-n] [-q] [-b] [-m]\n       [--window=<n>] [--depth=<n>] [--threads=<n>] [--keep-pack=<pack-name>]\n       [--write-midx[=<mode>]] [--name-hash-version=<n>] [--path-walk]\n       [--filter=<filter-spec>] [--drop-filtered [--dry-run]]",
];
pub const REPACK_H_A: &str = "pack everything in a single pack";
pub const REPACK_H_BIG_A: &str = "same as -a, and turn unreachable objects loose";
pub const REPACK_H_CRUFT: &str = "same as -a, pack unreachable cruft objects separately";
pub const REPACK_H_CRUFT_EXPIRATION: &str = "with --cruft, expire objects older than this";
pub const REPACK_H_COMBINE_CRUFT: &str = "with --cruft, only repack cruft packs smaller than this";
pub const REPACK_H_MAX_CRUFT: &str = "with --cruft, limit the size of new cruft packs";
pub const REPACK_H_D: &str = "remove redundant packs, and run git-prune-packed";
pub const REPACK_H_F: &str = "pass --no-reuse-delta to git-pack-objects";
pub const REPACK_H_BIG_F: &str = "pass --no-reuse-object to git-pack-objects";
pub const REPACK_H_NAME_HASH: &str =
    "specify the name hash version to use for grouping similar objects by path";
pub const REPACK_H_PATH_WALK: &str = "pass --path-walk to git-pack-objects";
pub const REPACK_H_N: &str = "do not run git-update-server-info";
pub const REPACK_H_QUIET: &str = "be quiet";
pub const REPACK_H_LOCAL: &str = "pass --local to git-pack-objects";
pub const REPACK_H_BITMAP: &str = "write bitmap index";
pub const REPACK_H_ISLANDS: &str = "pass --delta-islands to git-pack-objects";
pub const REPACK_H_UNPACK_UNREACHABLE: &str = "with -A, do not loosen objects older than this";
pub const REPACK_H_KEEP_UNREACHABLE: &str = "with -a, repack unreachable objects";
pub const REPACK_H_WINDOW: &str = "size of the window used for delta compression";
pub const REPACK_H_WINDOW_MEMORY: &str =
    "same as the above, but limit memory size instead of entries count";
pub const REPACK_H_DEPTH: &str = "limits the maximum delta depth";
pub const REPACK_H_THREADS: &str = "limits the maximum number of threads";
pub const REPACK_H_MAX_PACK_SIZE: &str = "maximum size of each packfile";
pub const REPACK_H_FILTER: &str = "object filtering";
pub const REPACK_H_PACK_KEPT: &str = "repack objects in packs marked with .keep";
pub const REPACK_H_KEEP_PACK: &str = "do not repack this pack";
pub const REPACK_H_GEOMETRIC: &str = "find a geometric progression with factor <N>";
pub const REPACK_H_WRITE_MIDX: &str = "write a multi-pack index of the resulting packs";
pub const REPACK_H_EXPIRE_TO: &str = "pack prefix to store a pack containing pruned objects";
pub const REPACK_H_FILTER_TO: &str = "pack prefix to store a pack containing filtered out objects";
pub const REPACK_H_DROP_FILTERED: &str = "delete filtered out objects (requires --filter)";
pub const REPACK_H_DRY_RUN: &str = "only show which objects would be dropped";
pub const REPACK_NOTHING_NEW: &str = "Nothing new to pack.";
pub const REPACK_PRECIOUS: &str = "cannot delete packs in a precious-objects repo";
pub const REPACK_KEEP_AND_CRUFT: &str =
    "options '-k/--keep-unreachable' and '--cruft' cannot be used together";

/// `repack -b` cannot write a bitmap: the pack lacks an object.
pub fn bitmap_no_closure(id: &str) -> String {
    format!(
        "Failed to write bitmap index. Packfile doesn't have full closure (object {id} is missing)"
    )
}

pub const FAILED_TO_WRITE_BITMAP: &str = "failed to write bitmap index";
