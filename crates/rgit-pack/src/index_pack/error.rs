// SPDX-License-Identifier: MIT

//! Errors of [`index_pack`](super::index_pack). `Display` is git's text
//! (without the `fatal: ` prefix); [`Error::errors_before`] gives the
//! `error:` lines git prints first.

use std::fmt;
use std::io;
use std::path::PathBuf;

use rgit_hash::ObjectId;

use super::messages as m;

/// What is wrong with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BadEntry {
    /// Type 0 or 5.
    UnknownType(u8),
    /// zlib stopped with this result code; `detail` is zlib's message for
    /// data errors.
    Inflate {
        /// zlib's result code (`-3` data error, `-5` more data than the
        /// header says, `1` less data than the header says).
        code: i32,
        /// zlib's message, if any.
        detail: Option<String>,
    },
    /// An `OFS_DELTA` base offset is not the start of an earlier entry.
    DeltaBaseOutOfBound,
    /// A delta does not apply; `note` is the `error:` line git prints first.
    BadDelta {
        /// `delta replay has gone wild` or `unexpected delta opcode 0`.
        note: Option<&'static str>,
    },
    /// The entry header's size does not fit in 64 bits.
    BadHeader,
}

impl fmt::Display for BadEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BadEntry::UnknownType(t) => m::unknown_type(f, *t),
            BadEntry::Inflate { code, .. } => m::inflate_returned(f, *code),
            BadEntry::DeltaBaseOutOfBound => f.write_str(m::DELTA_BASE_OUT_OF_BOUND),
            BadEntry::BadDelta { .. } => f.write_str(m::FAILED_TO_APPLY_DELTA),
            BadEntry::BadHeader => f.write_str(m::BAD_HEADER),
        }
    }
}

/// Why indexing failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Opening or mapping a file failed.
    Open {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
    /// Reading the input failed.
    Read(io::Error),
    /// Writing the pack failed.
    Write(io::Error),
    /// The input ended inside the pack.
    EarlyEof,
    /// The input does not start with `PACK`.
    SignatureMismatch,
    /// Unsupported pack version.
    Version(u32),
    /// The trailing checksum is wrong.
    ChecksumMismatch,
    /// Data after the trailing checksum.
    JunkAtEnd,
    /// A bad entry at `offset`.
    BadObject {
        /// Offset of the entry.
        offset: u64,
        /// The problem.
        what: BadEntry,
    },
    /// `n` deltas could not be resolved.
    Unresolved(u64),
    /// An object occurs twice (with [`super::Options::reject_duplicates`]).
    Duplicate(ObjectId),
    /// An object exists locally with different content.
    Collision(ObjectId),
    /// The pack is larger than [`super::Options::max_input_size`].
    TooLarge(u64),
    /// A SHA-1 collision attack was detected while hashing the entry at
    /// this offset.
    CollisionAttack(u64),
    /// An [`ObjectHook`](super::ObjectHook) or the object database
    /// reported a problem; the text is theirs.
    Hook(String),
    /// Index version 1 cannot hold an offset.
    OffsetTooLarge,
}

impl Error {
    /// The `error:` lines git prints before the fatal message.
    pub fn errors_before(&self) -> Vec<String> {
        match self {
            Error::BadObject {
                what: BadEntry::Inflate {
                    detail: Some(d), ..
                },
                ..
            } => vec![m::inflate_error(d)],
            Error::BadObject {
                what: BadEntry::BadDelta { note: Some(n) },
                ..
            } => vec![(*n).to_string()],
            _ => Vec::new(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open { path, source } => m::cannot_open(f, path, source),
            Error::Read(e) => m::read_error(f, e),
            Error::Write(e) => m::write_error(f, e),
            Error::EarlyEof => f.write_str(m::EARLY_EOF),
            Error::SignatureMismatch => f.write_str(m::SIGNATURE_MISMATCH),
            Error::Version(v) => m::version_unsupported(f, *v),
            Error::ChecksumMismatch => f.write_str(m::CHECKSUM_MISMATCH),
            Error::JunkAtEnd => f.write_str(m::JUNK_AT_END),
            Error::BadObject { offset, what } => m::bad_object(f, *offset, what),
            Error::Unresolved(n) => m::unresolved(f, *n),
            Error::Duplicate(id) => m::duplicate(f, id),
            Error::Collision(id) => m::collision(f, id),
            Error::TooLarge(n) => m::too_large(f, &m::humanise_bytes(*n)),
            Error::CollisionAttack(offset) => m::collision_attack(f, *offset),
            Error::Hook(msg) => f.write_str(msg),
            Error::OffsetTooLarge => f.write_str(m::OFFSET_TOO_LARGE_V1),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Open { source, .. } | Error::Read(source) | Error::Write(source) => Some(source),
            _ => None,
        }
    }
}
