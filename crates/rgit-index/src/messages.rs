// SPDX-License-Identifier: MIT

//! User-visible message texts.
//!
//! Texts marked "git" reproduce git's wording verbatim for compatibility
//! (see `CLEANROOM.md`); they were taken from the behaviour of the `git`
//! binary on crafted index files. The others are rgit's own.

/// git: printed as `fatal: index file corrupt` after an `error:` line.
pub const INDEX_FILE_CORRUPT: &str = "index file corrupt";

/// git: `error: bad signature 0x<8 hex digits>` (the first four bytes read
/// in host byte order).
pub fn bad_signature(raw: u32) -> String {
    format!("bad signature 0x{raw:08x}")
}

/// git: `error: bad index version <n>`.
pub fn bad_index_version(version: u32) -> String {
    format!("bad index version {version}")
}

/// git: `error: bad index file sha1 signature` (also used for SHA-256).
pub const BAD_CHECKSUM: &str = "bad index file sha1 signature";

/// git: `error: index uses <sig> extension, which we do not understand`.
pub fn unknown_required_extension(sig: &[u8; 4]) -> String {
    format!(
        "index uses {} extension, which we do not understand",
        String::from_utf8_lossy(sig)
    )
}

/// git: printed on stderr (without prefix) for each ignored unknown optional
/// extension: `ignoring <sig> extension`.
pub fn ignoring_extension(sig: &[u8; 4]) -> String {
    format!("ignoring {} extension", String::from_utf8_lossy(sig))
}

/// git: `fatal: <path>: index file smaller than expected`.
pub fn smaller_than_expected(path: &str) -> String {
    format!("{path}: index file smaller than expected")
}

/// git: `fatal: unknown index entry format 0x<8 hex digits>` (extended
/// flags shifted into the upper 16 bits).
pub fn unknown_entry_format(flags: u32) -> String {
    format!("unknown index entry format 0x{flags:08x}")
}

/// git: `fatal: unordered stage entries in index`.
pub const UNORDERED_ENTRIES: &str = "unordered stage entries in index";

/// git: `fatal: multiple stage entries for merged file '<path>'`.
pub fn multiple_stage_entries(path: &str) -> String {
    format!("multiple stage entries for merged file '{path}'")
}

/// git: `fatal: unordered stage entries for '<path>'`.
pub fn unordered_stage_entries(path: &str) -> String {
    format!("unordered stage entries for '{path}'")
}

/// git: `error: '<path>' appears as both a file and as a directory`.
pub fn file_directory_conflict(path: &str) -> String {
    format!("'{path}' appears as both a file and as a directory")
}

/// git: `error: Invalid path '<path>'`.
pub fn invalid_path(path: &str) -> String {
    format!("Invalid path '{path}'")
}

/// rgit: a split index (`link` extension) cannot be read yet (#116).
pub const SPLIT_INDEX_UNSUPPORTED: &str =
    "index uses the split index (link extension), which rgit does not support yet";

/// rgit: a sparse index (`sdir` extension) cannot be read yet (#119).
pub const SPARSE_INDEX_UNSUPPORTED: &str =
    "index is a sparse index (sdir extension), which rgit does not support yet";

/// rgit: generic structural corruption that git does not name separately.
pub fn corrupt_detail(detail: &str) -> String {
    format!("index file corrupt: {detail}")
}
