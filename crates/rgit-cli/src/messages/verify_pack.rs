// SPDX-License-Identifier: MIT
//! Usage, option help and messages of `git verify-pack`, `git show-index`
//! and `git count-objects` (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

pub const VERIFY_PACK_USAGE: &[&str] =
    &["git verify-pack [-v | --verbose] [-s | --stat-only] [--] <pack>.idx..."];
pub const HELP_VERBOSE: &str = "verbose";
pub const HELP_STAT_ONLY: &str = "show statistics only";
pub const HELP_OBJECT_FORMAT: &str = "specify the hash algorithm to use";

pub fn pack_ok(pack: &str) -> String {
    format!("{pack}: ok")
}

pub fn pack_bad(pack: &str) -> String {
    format!("{pack}: bad")
}

pub const SHOW_INDEX_USAGE: &[&str] =
    &["git show-index [--object-format=<hash-algorithm>] < <pack-idx-file>"];
pub const UNABLE_TO_READ_HEADER: &str = "unable to read header";
pub const UNABLE_TO_READ_INDEX: &str = "unable to read index";
pub const UNKNOWN_INDEX_VERSION: &str = "unknown index version";
pub const INCONSISTENT_64BIT: &str = "inconsistent 64b offset index";
pub const UNKNOWN_HASH: &str = "Unknown hash algorithm";

pub fn unable_to_read_entry(i: u32, n: u32) -> String {
    format!("unable to read entry {i}/{n}")
}

pub fn unable_to_read_sha1(i: u32, n: u32) -> String {
    format!("unable to read sha1 {i}/{n}")
}

pub fn unable_to_read_crc(i: u32, n: u32) -> String {
    format!("unable to read crc {i}/{n}")
}

pub fn unable_to_read_32bit(i: u32, n: u32) -> String {
    format!("unable to read 32b offset {i}/{n}")
}

pub fn unable_to_read_64bit(i: u32) -> String {
    format!("unable to read 64b offset {i}")
}

pub const COUNT_OBJECTS_USAGE: &[&str] = &["git count-objects [-v] [-H | --human-readable]"];
pub const HELP_BE_VERBOSE: &str = "be verbose";
pub const HELP_HUMAN: &str = "print sizes in human readable format";

pub fn count_summary(count: u64, size: &str, human: bool) -> String {
    if human {
        format!("{count} objects, {size}")
    } else {
        format!("{count} objects, {size} kilobytes")
    }
}

pub fn garbage(path: &str) -> String {
    format!("garbage found: {path}")
}
pub const NO_PACK: &str = "no corresponding .pack";
pub const NO_IDX: &str = "no corresponding .idx";
pub const NO_IDX_OR_PACK: &str = "no corresponding .idx or .pack";
