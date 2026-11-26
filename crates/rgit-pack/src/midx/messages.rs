// SPDX-License-Identifier: MIT

//! User-visible texts of the multi-pack-index code, verbatim as git prints
//! them (observed from the git binary and t5319). The caller adds the
//! `error: `, `warning: ` or `fatal: ` prefix.

use rgit_hash::ObjectId;

pub fn signature_mismatch(found: u32, expected: u32) -> String {
    format!("multi-pack-index signature 0x{found:08x} does not match signature 0x{expected:08x}")
}

pub fn version_unknown(version: u8) -> String {
    format!("multi-pack-index version {version} not recognized")
}

pub fn hash_version_mismatch(found: u8, expected: u8) -> String {
    format!("multi-pack-index hash version {found} does not match version {expected}")
}

pub fn too_small(path: &str) -> String {
    format!("multi-pack-index file {path} is too small")
}

pub const TERMINATING_EARLY: &str = "terminating chunk id appears earlier than expected";

pub fn final_chunk_nonzero(id: u32) -> String {
    format!("final chunk has non-zero id {id:x}")
}

pub fn chunk_unaligned(id: u32) -> String {
    format!("chunk id {id:x} not 4-byte aligned")
}

pub fn improper_offsets(a: u64, b: u64) -> String {
    format!("improper chunk offset(s) {a:x} and {b:x}")
}

pub fn duplicate_chunk(id: u32) -> String {
    format!("duplicate chunk ID {id:x} found")
}

pub const PNAM_MISSING: &str = "multi-pack-index required pack-name chunk missing or corrupted";
pub const OIDF_MISSING: &str = "multi-pack-index required OID fanout chunk missing or corrupted";
pub const OIDL_MISSING: &str = "multi-pack-index required OID lookup chunk missing or corrupted";
pub const OOFF_MISSING: &str =
    "multi-pack-index required object offsets chunk missing or corrupted";
pub const OIDF_SIZE: &str = "multi-pack-index OID fanout is of the wrong size";
pub const OIDL_SIZE: &str = "multi-pack-index OID lookup chunk is the wrong size";
pub const OOFF_SIZE: &str = "multi-pack-index object offset chunk is the wrong size";
pub const RIDX_SIZE: &str = "multi-pack-index reverse-index chunk is the wrong size";
pub const PNAM_SHORT: &str = "multi-pack-index pack-name chunk is too short";
pub const LARGE_OFFSET_OOB: &str = "multi-pack-index large offset out of bounds";

pub fn fanout_order(i: usize, a: u32, b: u32) -> String {
    format!(
        "oid fanout out of order: fanout[{i}] = {a:x} > {b:x} = fanout[{}]",
        i + 1
    )
}

pub fn pack_names_order(a: &str, b: &str) -> String {
    format!("multi-pack-index pack names out of order: '{a}' before '{b}'")
}

pub fn bad_pack_int_id(id: u32, total: u32) -> String {
    format!("bad pack-int-id: {id} ({total} total packs)")
}

// verify (printed without a prefix, like git's report lines)

pub const INCORRECT_CHECKSUM: &str = "incorrect checksum";
pub const NO_OID: &str = "the midx contains no oid";

pub fn failed_to_load_pack(pos: u32) -> String {
    format!("failed to load pack in position {pos}")
}

pub fn oid_lookup_order(i: u32, a: &ObjectId, b: &ObjectId) -> String {
    format!(
        "oid lookup out of order: oid[{i}] = {a} >= {b} = oid[{}]",
        i + 1
    )
}

pub fn failed_pack_entry(i: u32, id: &ObjectId) -> String {
    format!("failed to load pack entry for oid[{i}] = {id}")
}

pub fn incorrect_offset(i: u32, id: &ObjectId, midx: u64, pack: u64) -> String {
    format!("incorrect object offset for oid[{i}] = {id}: {midx:x} != {pack:x}")
}

pub const EXISTS_BUT_UNPARSABLE: &str = "multi-pack-index file exists, but failed to parse";

pub const PROGRESS_REFERENCED_PACKS: &str = "Looking for referenced packfiles";
pub const PROGRESS_OID_ORDER: &str = "Verifying OID order in multi-pack-index";
pub const PROGRESS_SORTING: &str = "Sorting objects by packfile";
pub const PROGRESS_OFFSETS: &str = "Verifying object offsets";

// write

pub const PROGRESS_ADDING: &str = "Adding packfiles to multi-pack-index";
pub const NO_PACKS: &str = "no pack files to index.";

pub fn unknown_preferred(name: &str) -> String {
    format!("unknown preferred pack: '{name}'")
}

pub fn preferred_empty(path: &str) -> String {
    format!("cannot select preferred pack {path} with no objects")
}

pub fn could_not_load_pack(pos: u32) -> String {
    format!("could not load pack {pos}")
}

pub fn failed_to_add_packfile(path: &str) -> String {
    format!("failed to add packfile '{path}'")
}

pub const IGNORING_CHECKSUM_MISMATCH: &str =
    "ignoring existing multi-pack-index; checksum mismatch";

pub fn packfile_index_unavailable(path: &str) -> String {
    format!("packfile {path} index unavailable")
}
