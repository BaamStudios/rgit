// SPDX-License-Identifier: MIT

//! Error types.

use std::io;
use std::path::PathBuf;

use crate::messages;

/// Failure to parse an index file.
///
/// `Display` gives a short description; [`Error::report`] gives the lines
/// git prints for the same problem (with their `error:`/`fatal:` prefixes).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The file is shorter than a header plus trailer.
    #[error("index file smaller than expected")]
    TooSmall,
    /// The file does not start with `DIRC`. The value is the first four
    /// bytes in host byte order, as git prints it.
    #[error("{}", messages::bad_signature(*.0))]
    BadSignature(u32),
    /// The version is not 2, 3 or 4.
    #[error("{}", messages::bad_index_version(*.0))]
    BadVersion(u32),
    /// The trailing checksum does not match the content.
    #[error("{}", messages::BAD_CHECKSUM)]
    BadChecksum,
    /// A required (lowercase) extension that rgit does not know.
    #[error("{}", messages::unknown_required_extension(.0))]
    UnknownExtension([u8; 4]),
    /// The index is split (`link` extension); not supported yet.
    #[error("{}", messages::SPLIT_INDEX_UNSUPPORTED)]
    SplitIndex,
    /// The index is sparse (`sdir` extension); not supported yet.
    #[error("{}", messages::SPARSE_INDEX_UNSUPPORTED)]
    SparseIndex,
    /// An entry with unknown extended flag bits. The value is the extended
    /// flags shifted into the upper 16 bits, as git prints it.
    #[error("{}", messages::unknown_entry_format(*.0))]
    UnknownEntryFormat(u32),
    /// Structural corruption (truncated entry, bad extension size, ...).
    #[error("{}", messages::corrupt_detail(.0))]
    Corrupt(&'static str),
    /// Reading the index file failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The index file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

impl Error {
    /// The lines git prints for this error, prefixes included. `path` is
    /// the index file as git names it (used by [`Error::TooSmall`]).
    pub fn report(&self, path: &str) -> Vec<String> {
        let corrupt = format!("fatal: {}", messages::INDEX_FILE_CORRUPT);
        match self {
            Error::TooSmall => vec![format!("fatal: {}", messages::smaller_than_expected(path))],
            Error::BadSignature(_)
            | Error::BadVersion(_)
            | Error::BadChecksum
            | Error::UnknownExtension(_) => vec![format!("error: {self}"), corrupt],
            Error::SplitIndex | Error::SparseIndex | Error::UnknownEntryFormat(_) => {
                vec![format!("fatal: {self}")]
            }
            Error::Corrupt(_) => vec![corrupt],
            Error::Io { .. } => vec![format!("fatal: {self}")],
        }
    }
}

/// The entries are not in index order (see [`crate::Index::verify_order`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrderError {
    /// Two paths are out of order.
    #[error("{}", messages::UNORDERED_ENTRIES)]
    Unordered,
    /// A stage-0 entry is followed by another entry for the same path.
    #[error("{}", messages::multiple_stage_entries(&String::from_utf8_lossy(.0)))]
    MultipleStages(Vec<u8>),
    /// The stages of one path are out of order.
    #[error("{}", messages::unordered_stage_entries(&String::from_utf8_lossy(.0)))]
    UnorderedStages(Vec<u8>),
}

/// An entry could not be added (see [`crate::Index::add`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddError {
    /// The path is not acceptable in the index.
    #[error("{}", messages::invalid_path(&String::from_utf8_lossy(.0)))]
    InvalidPath(Vec<u8>),
    /// The path is a file where a directory exists, or below a file.
    #[error("{}", messages::file_directory_conflict(&String::from_utf8_lossy(.0)))]
    FileDirectoryConflict(Vec<u8>),
}
