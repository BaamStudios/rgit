// SPDX-License-Identifier: MIT

//! User-visible texts of `index-pack`.
//!
//! Texts reproduce git's wording verbatim (observed from the git binary,
//! v2.56), because scripts and tests depend on them, except those marked
//! "rgit", whose git wording was not observable. The caller prefixes them
//! with `fatal: `, `error: ` or `warning: `.

use std::fmt;
use std::io;
use std::path::Path;

use rgit_hash::ObjectId;

/// The input ended before the pack did.
pub const EARLY_EOF: &str = "early EOF";

/// The input does not start with `PACK`.
pub const SIGNATURE_MISMATCH: &str = "pack signature mismatch";

/// Pack version other than 2 or 3.
pub fn version_unsupported(f: &mut fmt::Formatter<'_>, version: u32) -> fmt::Result {
    write!(f, "pack version {version} unsupported")
}

/// The trailing checksum does not match the data.
pub const CHECKSUM_MISMATCH: &str = "pack is corrupted (SHA1 mismatch)";

/// Bytes follow the trailing checksum.
pub const JUNK_AT_END: &str = "pack has junk at the end";

/// Prefix of every per-entry problem.
pub fn bad_object(
    f: &mut fmt::Formatter<'_>,
    offset: u64,
    detail: &dyn fmt::Display,
) -> fmt::Result {
    write!(f, "pack has bad object at offset {offset}: {detail}")
}

/// Entry type 0 or 5.
pub fn unknown_type(f: &mut fmt::Formatter<'_>, ty: u8) -> fmt::Result {
    write!(f, "unknown object type {ty}")
}

/// zlib result code that ended inflating an entry.
pub fn inflate_returned(f: &mut fmt::Formatter<'_>, code: i32) -> fmt::Result {
    write!(f, "inflate returned {code}")
}

/// zlib's own error text, printed as `error:` before the fatal message.
pub fn inflate_error(detail: &str) -> String {
    format!("inflate: data stream error ({detail})")
}

/// An `OFS_DELTA` base offset that is not the start of an earlier entry.
pub const DELTA_BASE_OUT_OF_BOUND: &str = "delta base offset is out of bound";

/// A delta could not be applied to its base.
pub const FAILED_TO_APPLY_DELTA: &str = "failed to apply delta";

/// Printed as `error:` when delta instructions run outside their bounds.
pub const DELTA_GONE_WILD: &str = "delta replay has gone wild";

/// Printed as `error:` for the reserved delta opcode.
pub const DELTA_OPCODE_0: &str = "unexpected delta opcode 0";

/// Deltas whose base never became available.
pub fn unresolved(f: &mut fmt::Formatter<'_>, n: u64) -> fmt::Result {
    if n == 1 {
        write!(f, "pack has {n} unresolved delta")
    } else {
        write!(f, "pack has {n} unresolved deltas")
    }
}

/// `--strict` / `--verify`: an object occurs more than once.
pub fn duplicate(f: &mut fmt::Formatter<'_>, id: &ObjectId) -> fmt::Result {
    write!(f, "The same object {id} appears twice in the pack")
}

/// An object exists locally with the same id and different content.
pub fn collision(f: &mut fmt::Formatter<'_>, id: &ObjectId) -> fmt::Result {
    write!(f, "SHA1 COLLISION FOUND WITH {id} !")
}

/// rgit: the size in an entry header does not fit in 64 bits.
pub const BAD_HEADER: &str = "bad object header";

/// The pack file cannot be opened.
pub fn cannot_open(f: &mut fmt::Formatter<'_>, path: &Path, e: &io::Error) -> fmt::Result {
    write!(
        f,
        "could not open '{}' for reading: {}",
        path.display(),
        strerror(e)
    )
}

/// rgit: reading the input failed.
pub fn read_error(f: &mut fmt::Formatter<'_>, e: &io::Error) -> fmt::Result {
    write!(f, "read error on input: {}", strerror(e))
}

/// rgit: writing the pack failed.
pub fn write_error(f: &mut fmt::Formatter<'_>, e: &io::Error) -> fmt::Result {
    write!(f, "unable to write pack file: {}", strerror(e))
}

/// The C library's text for an I/O error, without Rust's ` (os error N)`.
pub fn strerror(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// A byte count the way git prints sizes: `20 bytes`, `1.95 KiB`,
/// `2.86 MiB`, `1.00 GiB`.
pub fn humanise_bytes(n: u64) -> String {
    humanise(n, "")
}

/// A transfer rate in bytes per second, like [`humanise_bytes`] with `/s`.
pub fn humanise_rate(n: u64) -> String {
    humanise(n, "/s")
}

fn humanise(n: u64, suffix: &str) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if n >= GIB {
        format!("{:.2} GiB{suffix}", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.2} MiB{suffix}", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.2} KiB{suffix}", n as f64 / KIB as f64)
    } else if n == 1 {
        format!("1 byte{suffix}")
    } else {
        format!("{n} bytes{suffix}")
    }
}

/// `--max-input-size`; `size` is already humanised (`20 bytes`, `1.95 KiB`).
pub fn too_large(f: &mut fmt::Formatter<'_>, size: &str) -> fmt::Result {
    write!(f, "pack exceeds maximum allowed size ({size})")
}

/// `--fix-thin` progress suffix.
pub fn completed_with(n: u64) -> String {
    if n == 1 {
        format!("completed with {n} local object")
    } else {
        format!("completed with {n} local objects")
    }
}

/// Progress title of the first phase when reading a file.
pub const PROGRESS_INDEXING: &str = "Indexing objects";
/// Progress title of the first phase with `--stdin`.
pub const PROGRESS_RECEIVING: &str = "Receiving objects";
/// Progress title of the second phase.
pub const PROGRESS_RESOLVING: &str = "Resolving deltas";

// Command line (`rgit index-pack`).

/// The usage line (after `usage: `).
pub const USAGE: &str = "git index-pack [-v] [-o <index-file>] [--keep | --keep=<msg>] [--[no-]rev-index] [--verify] [--strict[=<msg-id>=<severity>...]] [--fsck-objects[=<msg-id>=<severity>...]] (<pack-file> | --stdin [--fix-thin] [<pack-file>])";

/// The pack name has no `.pack` suffix and no `-o` was given.
pub fn not_a_pack_name(name: &str) -> String {
    format!("packfile name '{name}' does not end with '.pack'")
}

/// `--fix-thin` without `--stdin`.
pub const FIX_THIN_NEEDS_STDIN: &str = "the option '--fix-thin' requires '--stdin'";

/// `--stdin` outside a repository.
pub const STDIN_NEEDS_REPO: &str = "--stdin requires a git repository";

/// `--verify --stdin` without a pack name.
pub const VERIFY_NEEDS_NAME: &str = "--verify with no packfile name given";

/// `--object-format` with `--stdin`.
pub const OBJECT_FORMAT_WITH_STDIN: &str =
    "options '--object-format' and '--stdin' cannot be used together";

/// An option value that does not parse (`arg` is the whole argument).
pub fn bad_arg(arg: &str) -> String {
    format!("bad {arg}")
}

/// `--object-format` with an unknown name.
pub fn unknown_hash(name: &str) -> String {
    format!("unknown hash algorithm '{name}'")
}

/// The output file exists or cannot be created.
pub fn cannot_create(path: &Path, e: &io::Error) -> String {
    format!("unable to create '{}': {}", path.display(), strerror(e))
}

/// rgit: renaming a finished file into place failed.
pub fn cannot_rename(path: &Path, e: &io::Error) -> String {
    format!(
        "unable to rename temporary file to '{}': {}",
        path.display(),
        strerror(e)
    )
}

/// rgit: writing the `.keep` file failed.
pub fn cannot_write_keep(path: &Path, e: &io::Error) -> String {
    format!(
        "cannot write keep file '{}': {}",
        path.display(),
        strerror(e)
    )
}

/// `--verify`: no pack next to the index (`name` is the index name).
pub fn cannot_open_pack(name: &str) -> String {
    format!("Cannot open existing pack file '{name}'")
}

/// `--verify` without an index to compare with.
pub fn cannot_open_index(path: &Path) -> String {
    format!(
        "Cannot open existing pack idx file for '{}'",
        path.display()
    )
}

/// `--verify`: the regenerated index differs from the existing one.
pub fn index_differs(path: &Path) -> String {
    format!("sha1 file '{}' validation error", path.display())
}

/// `--fsck-objects` outside a repository with `.gitmodules` or
/// `.gitattributes` blobs that are not in the pack.
pub const QUEUED_OUTSIDE_REPO: &str = "cannot perform queued object checks outside of a repository";

/// An object failed an fsck check at error level.
pub const FSCK_ERROR: &str = "fsck error in packed object";

/// One fsck finding, after `error: ` or `warning: `.
pub fn fsck_finding(id: &ObjectId, finding: &dyn fmt::Display) -> String {
    format!("object {id}: {finding}")
}

/// `--strict` / `--check-self-contained-and-connected`: a link points to
/// an object that is neither in the pack nor (for `--strict`) local.
pub fn missing_link(id: &ObjectId) -> String {
    format!("did not receive expected object {id}")
}

/// rgit: writing an index version 1 for offsets that do not fit.
pub const OFFSET_TOO_LARGE_V1: &str = "pack too large for current definition of off_t";

/// rgit: a hash computation reported a SHA-1 collision attack.
pub fn collision_attack(f: &mut fmt::Formatter<'_>, offset: u64) -> fmt::Result {
    write!(
        f,
        "pack has bad object at offset {offset}: SHA-1 collision attack detected"
    )
}
