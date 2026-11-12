// SPDX-License-Identifier: MIT

//! Error types.

use crate::{HashKind, ObjectId};

pub mod messages;

/// Errors when parsing ids, prefixes and hash kind names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A character that is not a hex digit.
    #[error("invalid hex digit at position {position}")]
    InvalidHexDigit {
        /// Byte offset of the bad character.
        position: usize,
    },
    /// Input of the wrong length for the requested hash kind.
    #[error("invalid {kind} object id length: expected {expected}, got {actual}")]
    InvalidLength {
        /// The requested kind.
        kind: HashKind,
        /// Expected length (bytes or hex digits, depending on the input).
        expected: usize,
        /// Actual length.
        actual: usize,
    },
    /// Input whose length matches no hash kind.
    #[error("no object id format has length {0}")]
    UnknownLength(usize),
    /// A prefix that is empty or longer than a full id.
    #[error("invalid {kind} object id prefix length {actual}")]
    InvalidPrefixLength {
        /// The requested kind.
        kind: HashKind,
        /// Number of hex digits given.
        actual: usize,
    },
    /// An unknown hash algorithm name.
    #[error("unknown hash algorithm '{0}'")]
    UnknownHashKind(String),
}

/// SHA-1 collision detection found input crafted for a collision attack
/// (such as the SHAttered PDFs). Hashing such input is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}{id}", messages::COLLISION)]
pub struct CollisionError {
    /// The plain (unmitigated) SHA-1 digest of the offending input.
    pub id: ObjectId,
}
