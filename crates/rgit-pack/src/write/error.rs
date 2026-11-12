// SPDX-License-Identifier: MIT

//! Errors from enumerating objects and writing packs.

use std::fmt;
use std::io;
use std::sync::Arc;

use rgit_hash::ObjectId;

use super::messages as m;
use crate::ObjectKind;

/// The error type of an [`ObjectSource`](super::ObjectSource).
pub type SourceError = Box<dyn std::error::Error + Send + Sync>;

/// Errors from [`write_pack`](super::write_pack) and
/// [`enumerate`](super::enumerate).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum WriteError {
    /// Writing the output failed.
    Io(Arc<io::Error>),
    /// An object to pack, or one reachable from a tip, does not exist.
    Missing(ObjectId),
    /// An object exists but could not be read.
    Read {
        /// The object.
        id: ObjectId,
        /// Why reading failed.
        detail: String,
    },
    /// An object needed for the walk has another type than expected (for
    /// example the tree of a commit is a blob).
    WrongType {
        /// The object.
        id: ObjectId,
        /// The type found.
        found: ObjectKind,
        /// The type expected.
        expected: ObjectKind,
    },
    /// A commit, tree or tag could not be parsed during the walk.
    Corrupt {
        /// The object.
        id: ObjectId,
        /// Its type.
        kind: ObjectKind,
    },
    /// More objects than a pack header can count.
    TooManyObjects,
    /// The trailing checksum hit a SHA-1 collision attack pattern.
    Collision,
}

impl WriteError {
    pub(crate) fn read(id: ObjectId, e: impl fmt::Display) -> WriteError {
        WriteError::Read {
            id,
            detail: e.to_string(),
        }
    }
}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(Arc::new(e))
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Io(e) => m::write_failed(f, e),
            WriteError::Missing(id) => m::unable_to_read(f, id),
            WriteError::Read { id, detail } => m::read_failed(f, id, detail),
            WriteError::WrongType {
                id,
                found,
                expected,
            } => m::wrong_object_type(f, id, found.as_str(), expected.as_str()),
            WriteError::Corrupt { id, kind } => m::bad_object_of_kind(f, id, kind.as_str()),
            WriteError::TooManyObjects => m::too_many_objects(f),
            WriteError::Collision => m::trailer_collision(f),
        }
    }
}

impl std::error::Error for WriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WriteError::Io(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}
