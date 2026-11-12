// SPDX-License-Identifier: MIT

//! User-visible message texts.
//!
//! Texts marked "git" reproduce git's wording verbatim (observed from the git
//! binary), because scripts and tests depend on them. Texts marked "rgit" are
//! our own where git's exact wording is not known or not observable; they may
//! be adjusted once a conformance test shows git's text.
//!
//! The CLI prefixes them with `error: ` or `fatal: `.

use std::fmt;
use std::path::Path;

use rgit_hash::ObjectId;

/// git: `.idx` shorter than its fixed parts.
pub fn index_too_small(f: &mut fmt::Formatter<'_>, idx: &Path) -> fmt::Result {
    write!(f, "index file {} is too small", idx.display())
}

/// git: unknown `.idx` version.
pub fn index_version(f: &mut fmt::Formatter<'_>, idx: &Path, version: u32) -> fmt::Result {
    write!(
        f,
        "index file {} is version {version} and is not supported by this binary (try upgrading GIT to a newer version)",
        idx.display()
    )
}

/// git: fan-out table is not monotonic.
pub fn index_non_monotonic(f: &mut fmt::Formatter<'_>, idx: &Path) -> fmt::Result {
    write!(f, "non-monotonic index {}", idx.display())
}

/// git: `.idx` size does not match its object count.
pub fn index_wrong_size(f: &mut fmt::Formatter<'_>, idx: &Path, version: u32) -> fmt::Result {
    write!(f, "wrong index v{version} file size in {}", idx.display())
}

/// git: `.pack` does not start with `PACK`.
pub fn not_a_pack(f: &mut fmt::Formatter<'_>, pack: &Path) -> fmt::Result {
    write!(f, "file {} is not a GIT packfile", pack.display())
}

/// git: unsupported `.pack` version.
pub fn pack_version(f: &mut fmt::Formatter<'_>, pack: &Path, version: u32) -> fmt::Result {
    write!(
        f,
        "packfile {} is version {version} and not supported (try upgrading GIT to a newer version)",
        pack.display()
    )
}

/// git: object count in the pack header differs from the index.
pub fn count_mismatch(
    f: &mut fmt::Formatter<'_>,
    pack: &Path,
    pack_count: u32,
    index_count: u32,
) -> fmt::Result {
    write!(
        f,
        "packfile {} claims to have {pack_count} objects while index indicates {index_count} objects",
        pack.display()
    )
}

/// git: the pack trailer differs from the checksum recorded in the index.
pub fn does_not_match_index(f: &mut fmt::Formatter<'_>, pack: &Path) -> fmt::Result {
    write!(f, "packfile {} does not match index", pack.display())
}

/// git: an offset from the index points past the pack data.
pub const OFFSET_BEYOND_PACK: &str = "offset beyond end of packfile (truncated pack?)";

/// git: a 64-bit offset index points past the large offset table.
pub fn offset_beyond_index(f: &mut fmt::Formatter<'_>, pack: &Path) -> fmt::Result {
    write!(
        f,
        "offset beyond end of pack index for {} (truncated index?)",
        pack.display()
    )
}

/// git: undecodable entry header (size overflow, truncated).
pub const BAD_OBJECT_HEADER: &str = "bad object header";

/// git: entry type 0 or 5.
pub fn unknown_type(f: &mut fmt::Formatter<'_>, ty: u8, offset: u64, pack: &Path) -> fmt::Result {
    write!(
        f,
        "unknown object type {ty} at offset {offset} in {}",
        pack.display()
    )
}

/// git: zlib reported an error; `detail` is zlib's own message.
pub fn inflate(f: &mut fmt::Formatter<'_>, detail: &str) -> fmt::Result {
    write!(f, "inflate: data stream error ({detail})")
}

/// rgit: the zlib stream ends before the end of the pack data.
pub const INFLATE_TRUNCATED: &str = "inflate: unexpected end of compressed data";

/// rgit: the zlib stream inflates to a different size than the header says.
pub fn inflate_size(
    f: &mut fmt::Formatter<'_>,
    offset: u64,
    expected: u64,
    actual: u64,
) -> fmt::Result {
    write!(
        f,
        "object at offset {offset} inflates to {actual} bytes, header says {expected}"
    )
}

/// git: an `OFS_DELTA` base offset is not the start of an object.
pub const BAD_REVINDEX_OFFSET: &str = "bad offset for revindex";

/// rgit: an `OFS_DELTA` base offset is not before the delta.
pub fn delta_base_offset(f: &mut fmt::Formatter<'_>, offset: u64, pack: &Path) -> fmt::Result {
    write!(
        f,
        "delta base offset out of bound for object at offset {offset} in {}",
        pack.display()
    )
}

/// git: the base of a delta could not be read.
pub fn delta_base(
    f: &mut fmt::Formatter<'_>,
    base: &ObjectId,
    offset: u64,
    pack: &Path,
) -> fmt::Result {
    write!(
        f,
        "failed to read delta base object {base} at offset {offset} from {}",
        pack.display()
    )
}

/// git: the leading sizes of a delta could not be read (header-only read).
pub const DELTA_UNPACK_INITIAL: &str = "delta data unpack-initial failed";

/// rgit: a delta could not be applied.
pub fn bad_delta(f: &mut fmt::Formatter<'_>, offset: u64, reason: &str) -> fmt::Result {
    write!(f, "corrupt delta at offset {offset}: {reason}")
}

/// rgit: a `REF_DELTA` base is not available.
pub fn missing_base(f: &mut fmt::Formatter<'_>, base: &ObjectId, offset: u64) -> fmt::Result {
    write!(
        f,
        "delta base {base} of object at offset {offset} is not available"
    )
}

/// rgit: a `REF_DELTA` chain refers back to itself.
pub fn delta_loop(f: &mut fmt::Formatter<'_>, offset: u64, pack: &Path) -> fmt::Result {
    write!(
        f,
        "delta chain loop at offset {offset} in {}",
        pack.display()
    )
}

/// git: every copy of a packed object failed to read.
pub fn corrupt(f: &mut fmt::Formatter<'_>, id: &ObjectId, pack: &Path) -> fmt::Result {
    write!(
        f,
        "packed object {id} (stored in {}) is corrupt",
        pack.display()
    )
}

/// rgit: a file checksum does not match its content.
pub fn checksum(f: &mut fmt::Formatter<'_>, path: &Path) -> fmt::Result {
    write!(f, "{}: checksum mismatch", path.display())
}

/// rgit: CRC32 from the index does not match the packed entry.
pub fn crc_mismatch(f: &mut fmt::Formatter<'_>, id: &ObjectId, pack: &Path) -> fmt::Result {
    write!(f, "CRC mismatch for object {id} in {}", pack.display())
}

/// rgit: an object hashes to a different id than the index says.
pub fn hash_mismatch(f: &mut fmt::Formatter<'_>, id: &ObjectId, pack: &Path) -> fmt::Result {
    write!(
        f,
        "object {id} in {} hashes to a different id",
        pack.display()
    )
}
