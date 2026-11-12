// SPDX-License-Identifier: MIT

//! Error type.

use crate::messages;

/// Errors from parsing objects.
///
/// Malformed input never panics; it yields one of these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// An object header that is not `<type> <decimal size>\0`.
    #[error("bad object header")]
    BadHeader,
    /// A type name that is not `blob`, `tree`, `commit` or `tag`.
    #[error("invalid object type \"{}\"", String::from_utf8_lossy(.0))]
    UnknownKind(Vec<u8>),
    /// The content is shorter or longer than the header says.
    #[error("object size mismatch: header says {expected}, content has {actual}")]
    SizeMismatch {
        /// Size from the header.
        expected: u64,
        /// Actual content length.
        actual: u64,
    },
    /// A tree entry runs past the end of the data.
    #[error("{}", messages::tree::TOO_SHORT)]
    TreeTooShort,
    /// A tree entry mode is not an octal number followed by a space.
    #[error("{}", messages::tree::MALFORMED_MODE)]
    TreeMalformedMode,
    /// A tree entry has an empty name.
    #[error("{}", messages::tree::EMPTY_FILENAME)]
    TreeEmptyFilename,
    /// The last header line of a commit or tag has no LF.
    #[error("{}", messages::fsck::UNTERMINATED_HEADER)]
    UnterminatedHeader,
    /// A required header is missing or not in its place.
    #[error("missing '{0}' header")]
    MissingHeader(&'static str),
    /// A header that should hold an object id does not.
    #[error("invalid '{0}' line format - bad sha1")]
    BadObjectId(&'static str),
}
