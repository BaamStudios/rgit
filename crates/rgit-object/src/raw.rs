// SPDX-License-Identifier: MIT

//! An object as stored: type plus unparsed content.

use rgit_hash::HashKind;

use crate::{Error, Object, ObjectHeader, ObjectKind};

/// An object as stored in a repository: its type and content (without the
/// `<type> <size>\0` header), not parsed. Object stores (loose objects,
/// packs) hand out objects in this form.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RawObject {
    /// The type.
    pub kind: ObjectKind,
    /// The content.
    pub data: Vec<u8>,
}

impl RawObject {
    /// An object of type `kind` with content `data`.
    pub fn new(kind: ObjectKind, data: impl Into<Vec<u8>>) -> RawObject {
        RawObject {
            kind,
            data: data.into(),
        }
    }

    /// The header `<type> <size>`.
    pub fn header(&self) -> ObjectHeader {
        ObjectHeader {
            kind: self.kind,
            size: self.data.len() as u64,
        }
    }

    /// Parse the content (see [`Object::parse`]).
    pub fn parse(&self, hash: HashKind) -> Result<Object, Error> {
        Object::parse(self.kind, hash, &self.data)
    }
}

impl From<Object> for RawObject {
    fn from(o: Object) -> RawObject {
        RawObject {
            kind: o.kind(),
            data: o.serialize(),
        }
    }
}
