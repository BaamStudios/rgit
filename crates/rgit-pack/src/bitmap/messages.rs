// SPDX-License-Identifier: MIT

//! git's messages about bitmap files (verbatim, determined from the git
//! binary).

pub const TOO_SMALL: &str = "corrupted bitmap index (too small)";
pub const WRONG_HEADER: &str = "corrupted bitmap index file (wrong header)";
pub const NO_FULL_DAG: &str =
    "unsupported options for bitmap index file (Git requires BITMAP_OPT_FULL_DAG)";
pub const SHORT_HASH_CACHE: &str = "corrupted bitmap index file (too short to fit hash cache)";
pub const SHORT_LOOKUP_TABLE: &str = "corrupted bitmap index file (too short to fit lookup table)";
pub const FAILED_TO_LOAD: &str = "failed to load bitmap index (corrupted?)";
pub const CORRUPTED_PACK_INDEX: &str = "corrupted bitmap pack index";
pub const LOOKUP_OUT_OF_INDEX: &str = "corrupt bitmap lookup table: triplet position out of index";
pub const MIDX_NO_REVINDEX: &str = "could not open MIDX reverse index";

pub const EWAH_EOF_BIT_SIZE: &str = "eof before bit size";
pub const EWAH_EOF_LENGTH: &str = "eof before length";
pub const EWAH_EOF_RLW: &str = "eof before rlw";

pub fn unsupported_version(v: u16) -> String {
    format!("unsupported version '{v}' for bitmap index file")
}

pub fn ewah_corrupt(what: &str) -> String {
    format!("corrupt ewah bitmap: {what}")
}

pub fn ewah_eof_data(short: usize) -> String {
    format!("eof in data ({short} bytes short)")
}

pub fn commit_out_of_range(pos: u32) -> String {
    ewah_corrupt(&format!("commit index {pos} out of range"))
}

pub fn duplicate_entry(hex: &str) -> String {
    format!("duplicate entry in bitmap index: '{hex}'")
}
