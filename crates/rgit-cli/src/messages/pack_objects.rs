// SPDX-License-Identifier: MIT
//! Messages of `pack-objects` ("git": observed from the git binary).

/// git: usage line.
pub const USAGE: &[&str] = &[
    "git pack-objects [-q | --progress | --all-progress] [--all-progress-implied]
                        [--no-reuse-delta] [--delta-base-offset] [--non-empty]
                        [--local] [--incremental] [--window=<n>] [--depth=<n>]
                        [--revs [--unpacked | --all]] [--keep-pack=<pack-name>]
                        [--cruft] [--cruft-expiration=<time>]
                        [--stdout [--filter=<filter-spec>] | <base-name>]
                        [--shallow] [--keep-true-parents] [--[no-]sparse]
                        [--name-hash-version=<n>] [--path-walk] < <object-list>",
];

pub const H_QUIET: &str = "do not show progress meter";
pub const H_PROGRESS: &str = "show progress meter";
pub const H_ALL_PROGRESS: &str = "show progress meter during object writing phase";
pub const H_ALL_PROGRESS_IMPLIED: &str = "similar to --all-progress when progress meter is shown";
pub const H_WINDOW: &str = "limit pack window by objects";
pub const H_DEPTH: &str = "maximum length of delta chain allowed in the resulting pack";
pub const H_REUSE_DELTA: &str = "reuse existing deltas";
pub const H_REUSE_OBJECT: &str = "reuse existing objects";
pub const H_DELTA_BASE_OFFSET: &str = "use OFS_DELTA objects";
pub const H_THREADS: &str = "use threads when searching for best delta matches";
pub const H_NON_EMPTY: &str = "do not create an empty pack output";
pub const H_REVS: &str = "read revision arguments from standard input";
pub const H_ALL: &str = "include objects reachable from any reference";
pub const H_STDOUT: &str = "output pack to stdout";
pub const H_INCLUDE_TAG: &str = "include tag objects that refer to objects to be packed";
pub const H_SPARSE: &str = "use the sparse reachability algorithm";
pub const H_THIN: &str = "create thin packs";
pub const H_SHALLOW: &str = "create packs suitable for shallow fetches";
pub const H_COMPRESSION: &str = "pack compression level";
pub const H_INDEX_VERSION: &str = "write the pack index file in the specified idx format version";
pub const H_MAX_PACK_SIZE: &str = "maximum size of each output pack file";
pub const H_LOCAL: &str = "ignore borrowed objects from alternate object store";
pub const H_INCREMENTAL: &str = "ignore packed objects";
pub const H_WINDOW_MEMORY: &str = "limit pack window by memory in addition to object limit";
pub const H_UNPACKED: &str = "limit the objects to those that are not yet packed";
pub const H_REFLOG: &str = "include objects referred by reflog entries";
pub const H_INDEXED_OBJECTS: &str = "include objects referred to by the index";
pub const H_STDIN_PACKS: &str = "read packs from stdin";
pub const H_KEEP_UNREACHABLE: &str = "keep unreachable objects";
pub const H_PACK_LOOSE_UNREACHABLE: &str = "pack loose unreachable objects";
pub const H_UNPACK_UNREACHABLE: &str = "unpack unreachable objects newer than <time>";
pub const H_CRUFT: &str = "create a cruft pack";
pub const H_CRUFT_EXPIRATION: &str = "expire cruft objects older than <time>";
pub const H_PATH_WALK: &str = "use the path-walk API to walk objects when possible";
pub const H_HONOR_PACK_KEEP: &str = "ignore packs that have companion .keep file";
pub const H_KEEP_PACK: &str = "ignore this pack";
pub const H_KEEP_TRUE_PARENTS: &str = "do not hide commits by grafts";
pub const H_USE_BITMAP_INDEX: &str = "use a bitmap index if available to speed up counting objects";
pub const H_WRITE_BITMAP_INDEX: &str = "write a bitmap index together with the pack index";
pub const H_FILTER: &str = "object filtering";
pub const H_MISSING: &str = "handling for missing objects";
pub const H_EXCLUDE_PROMISOR_OBJECTS: &str = "do not pack objects in promisor packfiles";
pub const H_EXCLUDE_PROMISOR_OBJECTS_BEST_EFFORT: &str = "implies --missing=allow-any";
pub const H_DELTA_ISLANDS: &str = "respect islands during delta compression";
pub const H_URI_PROTOCOL: &str =
    "exclude any configured uploadpack.blobpackfileuri with this protocol";
pub const H_NAME_HASH_VERSION: &str =
    "use the specified name-hash function to group similar objects";

/// git: a line of the object list is not an object id.
pub fn expected_object_id(line: &str) -> String {
    format!("expected object ID, got garbage:\n {line}")
}

/// git: a revision on standard input (`--revs`) cannot be resolved.
pub fn bad_revision(rev: &str) -> String {
    format!("bad revision '{rev}'")
}

/// git: bad `--compression` value.
pub fn bad_compression(level: i64) -> String {
    format!("bad pack compression level {level}")
}

/// git: an unknown option line on standard input (`--revs`).
pub fn not_a_rev(line: &str) -> String {
    format!("not a rev '{line}'")
}

/// git: a full object id on standard input (`--revs`) that does not exist.
pub fn bad_object(hex: &str) -> String {
    format!("bad object {hex}")
}

/// git: `--depth` above the maximum.
pub fn depth_too_large(depth: i32, max: u32) -> String {
    format!("delta chain depth {depth} is too deep, forcing {max}")
}

/// rgit: an option or mode that is not implemented yet.
pub fn not_supported(what: &str) -> String {
    format!("rgit pack-objects: {what} is not supported yet")
}

/// rgit: a revision syntax not implemented yet.
pub fn rev_not_supported(rev: &str) -> String {
    format!("rgit pack-objects: revision syntax '{rev}' is not supported yet")
}
