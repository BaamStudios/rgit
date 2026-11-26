// SPDX-License-Identifier: MIT
//! User-visible messages of the commit-graph code, verbatim like git's
//! (observed from the reference binary).

pub const TOO_SMALL: &str = "commit-graph file is too small";
pub const FANOUT_MISSING: &str = "commit-graph required OID fanout chunk missing or corrupted";
pub const LOOKUP_MISSING: &str = "commit-graph required OID lookup chunk missing or corrupted";
pub const CDAT_MISSING: &str = "commit-graph required commit data chunk missing or corrupted";
pub const FANOUT_WRONG_SIZE: &str = "commit-graph oid fanout chunk is wrong size";
pub const FANOUT_ORDER: &str = "commit-graph fanout values out of order";
pub const LOOKUP_WRONG_SIZE: &str = "commit-graph OID lookup chunk is the wrong size";
pub const CDAT_WRONG_SIZE: &str = "commit-graph commit data chunk is wrong size";
pub const GENERATIONS_WRONG_SIZE: &str = "commit-graph generations chunk is wrong size";
pub const EXTRA_EDGES_OOB: &str = "commit-graph extra-edges pointer out of bounds";
pub const BASE_TOO_SMALL: &str = "commit-graph base graphs chunk is too small";
pub const OVERFLOW_TOO_SMALL: &str = "commit-graph overflow generation data is too small";
pub const BLOOM_INDEX_TOO_SMALL: &str = "commit-graph changed-path index chunk is too small";
pub const CHAIN_TOO_SMALL: &str = "commit-graph chain file too small";
pub const CHAIN_MISMATCH: &str = "commit-graph chain does not match";
pub const CHAIN_INCOMPLETE: &str = "unable to find all commit-graph files";
pub const CHAIN_NOT_LOADED: &str = "one or more commit-graph chain files could not be loaded";
pub const BAD_CHECKSUM: &str = "the commit-graph file has incorrect checksum and is likely corrupt";
pub const VERIFYING: &str = "Verifying commits in commit graph";
pub const LOADING_KNOWN: &str = "Loading known commits in commit graph";
pub const EXPANDING_REACHABLE: &str = "Expanding reachable commits in commit graph";
pub const CLEARING_MARKS: &str = "Clearing commit marks in commit graph";
pub const FINDING_EXTRA_EDGES: &str = "Finding extra edges in commit graph";
pub const COMPUTING_LEVELS: &str = "Computing commit graph topological levels";
pub const COMPUTING_GENERATIONS: &str = "Computing commit graph generation numbers";
pub const COMPUTING_FILTERS: &str = "Computing commit changed paths Bloom filters";

pub fn writing_passes(passes: usize) -> String {
    format!("Writing out commit graph in {passes} passes")
}

pub fn signature_mismatch(found: u32) -> String {
    format!(
        "commit-graph signature {found:X} does not match signature {:X}",
        crate::SIGNATURE
    )
}

pub fn version_mismatch(found: u8) -> String {
    format!("commit-graph version {found:X} does not match version 1")
}

pub fn hash_version_mismatch(found: u8, want: u8) -> String {
    format!("commit-graph hash version {found:X} does not match version {want:X}")
}

pub fn too_small_for_chunks(n: u8) -> String {
    format!("commit-graph file is too small to hold {n} chunks")
}

pub fn final_chunk_nonzero(id: u32) -> String {
    format!("final chunk has non-zero id {id:x}")
}

pub fn improper_chunk_offset(a: u64, b: u64) -> String {
    format!("improper chunk offset(s) {a:x} and {b:x}")
}

pub fn duplicate_chunk(id: u32) -> String {
    format!("duplicate chunk ID {id:x} found")
}

pub fn invalid_parent_position(pos: u32) -> String {
    format!("invalid parent position {pos}")
}

pub fn chain_line_not_hash(line: &str) -> String {
    format!("invalid commit-graph chain: line '{line}' not a hash")
}

pub fn could_not_open_chain(path: &str, err: &str) -> String {
    format!("could not open commit-graph chain '{path}': {err}")
}

pub fn could_not_open_graph(path: &str, err: &str) -> String {
    format!("Could not open commit-graph '{path}': {err}")
}

pub fn bloom_data_too_small(len: u64) -> String {
    format!("ignoring too-small changed-path chunk ({len} < 12) in commit-graph file")
}

pub fn bloom_offset_out_of_range(offset: u32, pos: u32, path: &str) -> String {
    format!(
        "ignoring out-of-range offset ({offset}) for changed-path filter at pos {pos} of {path}"
    )
}

pub fn bloom_offsets_decreasing(a: u32, b: u32, pa: u32, pb: u32, path: &str) -> String {
    format!(
        "ignoring decreasing changed-path index offsets ({a} > {b}) for positions {pa} and {pb} of {path}"
    )
}

pub fn bloom_layer_disabled(hash: &str) -> String {
    format!("disabling Bloom filters for commit-graph layer '{hash}' due to incompatible settings")
}

// verify

pub fn incorrect_oid_order(a: &str, b: &str) -> String {
    format!("commit-graph has incorrect OID order: {a} then {b}")
}

pub fn incorrect_fanout(i: usize, stored: u32, want: u32) -> String {
    format!("commit-graph has incorrect fanout value: fanout[{i}] = {stored} != {want}")
}

pub fn failed_to_parse(id: &str) -> String {
    format!("failed to parse commit {id} from object database for commit-graph")
}

pub fn failed_to_parse_from_graph(id: &str) -> String {
    format!("failed to parse commit {id} from commit-graph")
}

pub fn root_tree_mismatch(id: &str, graph: &str, odb: &str) -> String {
    format!("root tree OID for commit {id} in commit-graph is {graph} != {odb}")
}

pub fn parent_mismatch(id: &str, graph: &str, odb: &str) -> String {
    format!("commit-graph parent for {id} is {graph} != {odb}")
}

pub fn parents_too_long(id: &str) -> String {
    format!("commit-graph parent list for commit {id} is too long")
}

pub fn parents_too_short(id: &str) -> String {
    format!("commit-graph parent list for commit {id} terminates early")
}

pub fn generation_too_small(id: &str, generation: u64, want: u64) -> String {
    format!("commit-graph generation for commit {id} is {generation} < {want}")
}

pub fn date_mismatch(id: &str, graph: u64, odb: u64) -> String {
    format!("commit date for commit {id} in commit-graph is {graph} != {odb}")
}

pub fn mixed_generations(a: &str, b: &str) -> String {
    format!("commit-graph has both zero and non-zero generations (e.g., commits '{a}' and '{b}')")
}

pub fn could_not_read(id: &str) -> String {
    format!("Could not read {id}")
}
