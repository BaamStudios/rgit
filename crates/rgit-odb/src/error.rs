// SPDX-License-Identifier: MIT

//! Error type.

use std::io;
use std::path::PathBuf;

use rgit_hash::{CollisionError, ObjectId};

use crate::messages;

/// Errors of the object database.
///
/// [`Display`](std::fmt::Display) gives the main message as git prints it
/// (without the `error: ` / `fatal: ` prefix); [`Error::git_lines`] gives
/// every line git prints for the problem, in order.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The zlib stream of a loose object is broken before the header was
    /// read.
    #[error("{}", messages::unable_to_unpack_header(.oid))]
    InflateHeader {
        /// The object.
        oid: ObjectId,
        /// zlib's message, e.g. `incorrect header check`.
        zlib: String,
    },
    /// No NUL within the first 32 inflated bytes.
    #[error("{}", messages::header_too_long(.oid))]
    HeaderTooLong {
        /// The object.
        oid: ObjectId,
    },
    /// The header is not `<type> <size>`.
    #[error("{}", messages::unable_to_parse_header(.oid))]
    BadHeader {
        /// The object.
        oid: ObjectId,
    },
    /// The header names an unknown type.
    #[error("{}", messages::INVALID_OBJECT_TYPE)]
    InvalidType {
        /// The object.
        oid: ObjectId,
        /// The type as found.
        found: Vec<u8>,
    },
    /// The object file has zero length.
    #[error("{}", messages::object_file_empty(.path))]
    EmptyFile {
        /// The object.
        oid: ObjectId,
        /// Its file.
        path: PathBuf,
    },
    /// The zlib stream is broken in the object content.
    #[error("{}", messages::inflate_data_error(.zlib))]
    InflateContent {
        /// The object.
        oid: ObjectId,
        /// Its file.
        path: PathBuf,
        /// zlib's message.
        zlib: String,
    },
    /// The stream ends before the size from the header was reached, or holds
    /// more content than the header says.
    #[error("{}", messages::corrupt_loose_object(.oid))]
    SizeMismatch {
        /// The object.
        oid: ObjectId,
        /// Its file.
        path: PathBuf,
        /// Size from the header.
        expected: u64,
        /// Bytes found (at least; reading stops early on excess).
        actual: u64,
    },
    /// The zlib stream ends before its end marker.
    #[error("{}", messages::corrupt_loose_object(.oid))]
    Truncated {
        /// The object.
        oid: ObjectId,
        /// Its file.
        path: PathBuf,
    },
    /// Bytes after the end of the zlib stream.
    #[error("{}", messages::garbage_at_end(.oid))]
    TrailingGarbage {
        /// The object.
        oid: ObjectId,
        /// Its file.
        path: PathBuf,
    },
    /// The content does not hash to the id it is stored under.
    #[error("{}", messages::hash_mismatch(.oid))]
    HashMismatch {
        /// The requested id.
        oid: ObjectId,
        /// The id of the content.
        actual: ObjectId,
        /// Its file.
        path: PathBuf,
    },
    /// SHA-1 collision attack detected while hashing.
    #[error(transparent)]
    Collision(#[from] CollisionError),
    /// A configuration value is invalid (fatal in git).
    #[error("{0}")]
    Config(String),
    /// A copy of an object in a pack cannot be read (reported for each
    /// copy while looking further).
    #[error(transparent)]
    Pack(rgit_pack::Error),
    /// No copy of an object in the packs can be read; the reasons were
    /// reported before as [`Error::Pack`].
    #[error(transparent)]
    PackSummary(rgit_pack::Error),
    /// Some other store reported a problem (for example a pack backend).
    #[error(transparent)]
    Store(Box<dyn std::error::Error + Send + Sync>),
    /// An I/O error.
    #[error("{context}: {source}")]
    Io {
        /// What was being done, e.g. a path.
        context: String,
        /// The error.
        #[source]
        source: io::Error,
    },
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> Error {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// Whether the error means "this copy of the object is damaged". The
    /// database then tries the other stores before giving up.
    pub fn is_corruption(&self) -> bool {
        !matches!(self, Error::Config(_) | Error::Collision(_))
    }

    /// The lines git prints (each to be prefixed with `error: `) when it
    /// skips this broken copy of an object and goes on to another copy:
    /// for a broken zlib stream only `inflate: data stream error (...)`
    /// (`unable to unpack <oid> header` only follows when no copy is
    /// left), otherwise [`Error::git_lines`].
    pub fn skipped_copy_lines(&self) -> Vec<String> {
        match self {
            Error::InflateHeader { zlib, .. } => vec![messages::inflate_data_error(zlib)],
            _ => self.git_lines(),
        }
    }

    /// All lines git prints for this problem (each to be prefixed with
    /// `error: `), in order. For example a broken zlib stream gives
    /// `inflate: data stream error (incorrect header check)` followed by
    /// `unable to unpack <oid> header`.
    pub fn git_lines(&self) -> Vec<String> {
        match self {
            Error::InflateHeader { zlib, .. } => {
                vec![messages::inflate_data_error(zlib), self.to_string()]
            }
            Error::InflateContent { oid, .. } => {
                vec![self.to_string(), messages::corrupt_loose_object(oid)]
            }
            Error::Pack(e) | Error::PackSummary(e) => e.chain(),
            _ => vec![self.to_string()],
        }
    }

    /// The object the error is about, if any.
    pub fn oid(&self) -> Option<&ObjectId> {
        match self {
            Error::InflateHeader { oid, .. }
            | Error::HeaderTooLong { oid }
            | Error::BadHeader { oid }
            | Error::InvalidType { oid, .. }
            | Error::EmptyFile { oid, .. }
            | Error::InflateContent { oid, .. }
            | Error::SizeMismatch { oid, .. }
            | Error::Truncated { oid, .. }
            | Error::TrailingGarbage { oid, .. }
            | Error::HashMismatch { oid, .. } => Some(oid),
            _ => None,
        }
    }
}
