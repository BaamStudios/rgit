// SPDX-License-Identifier: MIT

//! User-visible message texts reproduced from git for compatibility.
//!
//! Collected here so they are reviewable in one place (see `CLEANROOM.md`).
//! They were taken from the observable output of the reference `git` binary
//! (`git cat-file`, `git fsck`, `git hash-object`) on hand-corrupted objects.

use std::path::Path;

use rgit_hash::ObjectId;

/// `inflate: data stream error (<zlib message>)`.
pub fn inflate_data_error(zlib_msg: &str) -> String {
    format!("inflate: data stream error ({zlib_msg})")
}

/// `unable to unpack <oid> header`.
pub fn unable_to_unpack_header(oid: &ObjectId) -> String {
    format!("unable to unpack {oid} header")
}

/// `header for <oid> too long, exceeds 32 bytes`.
pub fn header_too_long(oid: &ObjectId) -> String {
    format!("header for {oid} too long, exceeds 32 bytes")
}

/// `unable to parse <oid> header`.
pub fn unable_to_parse_header(oid: &ObjectId) -> String {
    format!("unable to parse {oid} header")
}

/// `invalid object type` (git dies with this when a loose header names an
/// unknown type).
pub const INVALID_OBJECT_TYPE: &str = "invalid object type";

/// `object file <path> is empty`.
pub fn object_file_empty(path: &Path) -> String {
    format!("object file {} is empty", path.display())
}

/// `loose object <oid> (stored in <path>) is corrupt`.
pub fn loose_object_corrupt(oid: &ObjectId, path: &Path) -> String {
    format!(
        "loose object {oid} (stored in {}) is corrupt",
        path.display()
    )
}

/// `corrupt loose object '<oid>'` (the stream ends before the content).
pub fn corrupt_loose_object(oid: &ObjectId) -> String {
    format!("corrupt loose object '{oid}'")
}

/// `garbage at end of loose object '<oid>'`.
pub fn garbage_at_end(oid: &ObjectId) -> String {
    format!("garbage at end of loose object '{oid}'")
}

/// `hash mismatch <oid>`.
pub fn hash_mismatch(oid: &ObjectId) -> String {
    format!("hash mismatch {oid}")
}

/// `<real oid>: hash-path mismatch, found at: <path>`.
pub fn hash_path_mismatch(real: &ObjectId, path: &Path) -> String {
    format!("{real}: hash-path mismatch, found at: {}", path.display())
}

/// `bad zlib compression level <n>`.
pub fn bad_compression_level(level: i64) -> String {
    format!("bad zlib compression level {level}")
}

/// `ignoring unknown core.fsync component '<name>'`.
pub fn unknown_fsync_component(name: &str) -> String {
    format!("ignoring unknown core.fsync component '{name}'")
}

/// `ignoring unknown core.fsyncMethod value '<value>'`.
pub fn unknown_fsync_method(value: &str) -> String {
    format!("ignoring unknown core.fsyncMethod value '{value}'")
}
