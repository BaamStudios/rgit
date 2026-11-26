// SPDX-License-Identifier: MIT
//! Errors of reading and writing notes trees.

use rgit_hash::ObjectId;

/// Reading or writing a notes tree failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The object database failed.
    #[error(transparent)]
    Odb(#[from] rgit_odb::Error),
    /// A tree of the notes tree is missing or not a tree.
    #[error("unable to read tree {}", .0.to_hex())]
    BadTree(ObjectId),
    /// A note blob is missing.
    #[error("unable to read note {}", .0.to_hex())]
    BadNote(ObjectId),
}
