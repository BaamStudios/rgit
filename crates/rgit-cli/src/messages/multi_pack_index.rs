// SPDX-License-Identifier: MIT
//! Messages of `git multi-pack-index` (verbatim from git 2.56).

pub const USAGE_WRITE: &str = "git multi-pack-index [<options>] write [--preferred-pack=<pack>]\n\t  [--[no-]bitmap] [--[no-]incremental] [--[no-]stdin-packs]\n\t  [--refs-snapshot=<path>] [--[no-]write-chain-file]\n\t  [--base=<checksum>]";
pub const USAGE_COMPACT: &str = "git multi-pack-index [<options>] compact [--[no-]incremental]\n\t  [--[no-]bitmap] [--base=<checksum>] [--[no-]write-chain-file]\n\t  <from> <to>";
pub const USAGE_VERIFY: &str = "git multi-pack-index [<options>] verify";
pub const USAGE_EXPIRE: &str = "git multi-pack-index [<options>] expire";
pub const USAGE_REPACK: &str = "git multi-pack-index [<options>] repack [--batch-size=<size>]";

pub const H_OBJECT_DIR: &str = "object directory containing set of packfile and pack-index pairs";
pub const H_PROGRESS: &str = "force progress reporting";
pub const H_PREFERRED: &str = "pack for reuse when computing a multi-pack bitmap";
pub const H_BITMAP: &str = "write multi-pack bitmap";
pub const H_BASE: &str = "base MIDX for incremental writes";
pub const H_INCREMENTAL: &str = "write a new incremental MIDX";
pub const H_CHAIN_FILE: &str = "write the multi-pack-index chain file";
pub const H_STDIN_PACKS: &str = "write multi-pack index containing only given indexes";
pub const H_REFS_SNAPSHOT: &str = "refs snapshot for selecting bitmap commits";
pub const H_BATCH_SIZE: &str =
    "during repack, collect pack-files of smaller size into a batch that is larger than this size";

pub const NO_CHAIN_WITHOUT_INCREMENTAL: &str =
    "cannot use --no-write-chain-file without --incremental";
pub const BASE_WITHOUT_NO_CHAIN: &str = "cannot use --base without --no-write-chain-file";
pub const EXPIRE_INCREMENTAL: &str = "cannot expire packs from an incremental multi-pack-index";
pub const REPACK_INCREMENTAL: &str = "cannot repack an incremental multi-pack-index";

pub fn not_an_alternate(dir: &str) -> String {
    format!("object directory is not an alternate of the current repository: '{dir}'")
}

pub fn could_not_find_midx(name: &str) -> String {
    format!("could not find MIDX: {name}")
}

pub const PROGRESS_COUNTING_REFERENCED: &str = "Counting referenced objects";
pub const PROGRESS_FINDING_UNREFERENCED: &str = "Finding and deleting unreferenced packfiles";

pub const COMPACT_V1: &str = "cannot perform MIDX compaction with v1 format";
pub const COMPACT_UNIQUE: &str = "MIDX compaction endpoints must be unique";

pub fn not_an_ancestor(from: &str, to: &str) -> String {
    format!("MIDX {from} must be an ancestor of {to}")
}

pub fn could_not_find_base_midx(name: &str) -> String {
    format!("could not find base MIDX: {name}")
}
