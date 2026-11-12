// SPDX-License-Identifier: MIT

//! Error type. `Display` gives git's message text (see [`crate::messages`]).

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rgit_hash::ObjectId;

use crate::messages as m;

/// Errors from reading packs and pack indexes.
///
/// Corrupt files are ordinary input: every problem found in a file is
/// reported as an error, never as a panic. The error is `Clone` so a pack
/// that failed to open can report the same error on every access.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Error {
    /// Opening, mapping or reading a file failed.
    Io {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: Arc<io::Error>,
    },
    /// The `.idx` is shorter than its fixed parts.
    IndexTooSmall {
        /// The `.idx` file.
        path: PathBuf,
    },
    /// The `.idx` has a version other than 1 or 2.
    IndexVersion {
        /// The `.idx` file.
        path: PathBuf,
        /// The version found.
        version: u32,
    },
    /// The fan-out table decreases somewhere.
    IndexNonMonotonic {
        /// The `.idx` file.
        path: PathBuf,
    },
    /// The `.idx` size does not fit its object count.
    IndexWrongSize {
        /// The `.idx` file.
        path: PathBuf,
        /// Index version (1 or 2).
        version: u32,
    },
    /// The `.pack` does not start with `PACK` or is too short.
    NotAPack {
        /// The `.pack` file.
        path: PathBuf,
    },
    /// The `.pack` has a version other than 2 or 3.
    PackVersion {
        /// The `.pack` file.
        path: PathBuf,
        /// The version found.
        version: u32,
    },
    /// Pack header and index disagree on the number of objects.
    CountMismatch {
        /// The `.pack` file.
        path: PathBuf,
        /// Count in the pack header.
        pack_count: u32,
        /// Count in the index.
        index_count: u32,
    },
    /// The pack trailer is not the checksum recorded in the index.
    DoesNotMatchIndex {
        /// The `.pack` file.
        path: PathBuf,
    },
    /// An offset points outside the pack data.
    OffsetBeyondPack,
    /// A large-offset reference points outside the index's 64-bit table.
    OffsetBeyondIndex {
        /// The `.pack` file the index belongs to.
        path: PathBuf,
    },
    /// An entry header cannot be decoded.
    BadObjectHeader,
    /// An entry has type 0 or 5.
    UnknownType {
        /// The type number.
        ty: u8,
        /// Entry offset.
        offset: u64,
        /// The `.pack` file.
        path: PathBuf,
    },
    /// zlib reported an error.
    Inflate {
        /// zlib's message.
        detail: String,
    },
    /// The zlib stream ends before the pack data does.
    InflateTruncated,
    /// The inflated size differs from the size in the entry header.
    InflateSize {
        /// Entry offset.
        offset: u64,
        /// Size from the header.
        expected: u64,
        /// Size produced (may be cut off at `expected + 1`).
        actual: u64,
    },
    /// An `OFS_DELTA` base offset is not the start of an object.
    BadRevindexOffset,
    /// An `OFS_DELTA` base offset is not before the delta entry.
    DeltaBaseOffset {
        /// Offset of the delta entry.
        offset: u64,
        /// The `.pack` file.
        path: PathBuf,
    },
    /// Reading the base of a delta failed.
    DeltaBase {
        /// Id of the base object.
        base: ObjectId,
        /// Offset of the base entry.
        offset: u64,
        /// The `.pack` file.
        path: PathBuf,
        /// Why it failed.
        source: Box<Error>,
    },
    /// The leading sizes of a delta could not be read.
    DeltaUnpackInitial,
    /// A delta does not apply to its base.
    BadDelta {
        /// Offset of the delta entry.
        offset: u64,
        /// What is wrong.
        reason: &'static str,
    },
    /// A `REF_DELTA` base is neither in the pack nor available elsewhere.
    MissingBase {
        /// The base id.
        base: ObjectId,
        /// Offset of the delta entry.
        offset: u64,
    },
    /// `REF_DELTA` entries refer to each other in a loop.
    DeltaLoop {
        /// Offset where the loop was noticed.
        offset: u64,
        /// The `.pack` file.
        path: PathBuf,
    },
    /// Every copy of the object failed to read.
    Corrupt {
        /// The object.
        id: ObjectId,
        /// The pack of the first failing copy.
        path: PathBuf,
    },
    /// A file's trailing checksum does not match its content.
    Checksum {
        /// The file.
        path: PathBuf,
    },
    /// An entry does not match the CRC32 recorded in the index.
    CrcMismatch {
        /// The object.
        id: ObjectId,
        /// The `.pack` file.
        path: PathBuf,
    },
    /// An object does not hash to the id recorded in the index.
    HashMismatch {
        /// The object.
        id: ObjectId,
        /// The `.pack` file.
        path: PathBuf,
    },
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Error {
        Error::Io {
            path: path.into(),
            source: Arc::new(source),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::IndexTooSmall { path } => m::index_too_small(f, path),
            Error::IndexVersion { path, version } => m::index_version(f, path, *version),
            Error::IndexNonMonotonic { path } => m::index_non_monotonic(f, path),
            Error::IndexWrongSize { path, version } => m::index_wrong_size(f, path, *version),
            Error::NotAPack { path } => m::not_a_pack(f, path),
            Error::PackVersion { path, version } => m::pack_version(f, path, *version),
            Error::CountMismatch {
                path,
                pack_count,
                index_count,
            } => m::count_mismatch(f, path, *pack_count, *index_count),
            Error::DoesNotMatchIndex { path } => m::does_not_match_index(f, path),
            Error::OffsetBeyondPack => f.write_str(m::OFFSET_BEYOND_PACK),
            Error::OffsetBeyondIndex { path } => m::offset_beyond_index(f, path),
            Error::BadObjectHeader => f.write_str(m::BAD_OBJECT_HEADER),
            Error::UnknownType { ty, offset, path } => m::unknown_type(f, *ty, *offset, path),
            Error::Inflate { detail } => m::inflate(f, detail),
            Error::InflateTruncated => f.write_str(m::INFLATE_TRUNCATED),
            Error::InflateSize {
                offset,
                expected,
                actual,
            } => m::inflate_size(f, *offset, *expected, *actual),
            Error::BadRevindexOffset => f.write_str(m::BAD_REVINDEX_OFFSET),
            Error::DeltaBaseOffset { offset, path } => m::delta_base_offset(f, *offset, path),
            Error::DeltaBase {
                base, offset, path, ..
            } => m::delta_base(f, base, *offset, path),
            Error::DeltaUnpackInitial => f.write_str(m::DELTA_UNPACK_INITIAL),
            Error::BadDelta { offset, reason } => m::bad_delta(f, *offset, reason),
            Error::MissingBase { base, offset } => m::missing_base(f, base, *offset),
            Error::DeltaLoop { offset, path } => m::delta_loop(f, *offset, path),
            Error::Corrupt { id, path } => m::corrupt(f, id, path),
            Error::Checksum { path } => m::checksum(f, path),
            Error::CrcMismatch { id, path } => m::crc_mismatch(f, id, path),
            Error::HashMismatch { id, path } => m::hash_mismatch(f, id, path),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(&**source),
            Error::DeltaBase { source, .. } => Some(&**source),
            _ => None,
        }
    }
}

impl Error {
    /// The messages of this error and its causes, outermost last, the order
    /// in which git prints them (the cause is reported first).
    pub fn chain(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(self);
        while let Some(e) = cur {
            out.push(e.to_string());
            cur = match e {
                Error::DeltaBase { source, .. } => Some(source),
                _ => None,
            };
        }
        out.reverse();
        out
    }
}
