// SPDX-License-Identifier: MIT
//! Usage and messages of `git unpack-objects` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

use rgit_hash::ObjectId;

pub const USAGE: &str = "git unpack-objects [-n] [-q] [-r] [--strict]";
pub const EARLY_EOF: &str = "early EOF";
pub const BAD_PACK_FILE: &str = "bad pack file";
pub const FINAL_SHA1: &str = "final sha1 did not match";
pub const UNRESOLVED: &str = "unresolved deltas left after unpacking";
pub const TOO_LARGE: &str = "pack exceeds maximum allowed size";
pub const UNEXPECTED_TYPE: &str = "object of unexpected type";
pub const FSCK_ERROR: &str = "fsck error in packed object";
pub const FSCK_ERROR_PACK: &str = "fsck error in pack objects";

pub fn unknown_version(v: u32) -> String {
    format!("unknown pack file version {v}")
}

pub fn bad_object_type(t: u8) -> String {
    format!("bad object type {t}")
}

pub fn bad(arg: &str) -> String {
    format!("bad {arg}")
}

pub fn fsck_finding(id: &ObjectId, finding: &dyn std::fmt::Display) -> String {
    format!("object {id}: {finding}")
}
