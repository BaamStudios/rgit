// SPDX-License-Identifier: MIT

//! The interface every object store implements (loose objects, packs,
//! alternates).

use std::fmt;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{Object, ObjectHeader, ObjectKind};

use crate::Error;

/// An object as stored: its type and content (without the header).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawObject {
    /// The type.
    pub kind: ObjectKind,
    /// The content.
    pub data: Vec<u8>,
}

impl RawObject {
    /// The header `<type> <size>`.
    pub fn header(&self) -> ObjectHeader {
        ObjectHeader {
            kind: self.kind,
            size: self.data.len() as u64,
        }
    }

    /// Parse the content (see [`rgit_object::Object::parse`]).
    pub fn parse(&self, hash: HashKind) -> Result<Object, rgit_object::Error> {
        Object::parse(self.kind, hash, &self.data)
    }
}

/// A source of objects.
///
/// [`crate::LooseStore`] implements it; the pack reader (#17) and
/// alternates (#93) plug in through it too. A store only ever returns
/// whole objects: a pack store resolves deltas itself and hands back the
/// object's type and bytes.
///
/// `Ok(None)` means "not in this store". An `Err` means the store has the
/// object but cannot deliver it intact; [`crate::ObjectDatabase`] then
/// reports the error and tries the remaining stores.
pub trait ObjectStore: Send + Sync + fmt::Debug {
    /// The hash kind of the ids in this store.
    fn hash_kind(&self) -> HashKind;

    /// Whether the store has the object (cheap; does not check integrity).
    fn contains(&self, oid: &ObjectId) -> bool;

    /// Type and size of an object, if possible without reading it all.
    fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        Ok(self.read(oid)?.map(|o| o.header()))
    }

    /// The whole object.
    fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error>;

    /// Call `f` for every object id in the store that starts with `prefix`
    /// (in any order; duplicates are allowed).
    fn for_each_prefix_match(
        &self,
        prefix: &Prefix,
        f: &mut dyn FnMut(ObjectId),
    ) -> Result<(), Error>;

    /// Call `f` for every object id in the store.
    fn for_each_object(&self, f: &mut dyn FnMut(ObjectId)) -> Result<(), Error>;

    /// If the store has the object, mark it as recently used (git updates
    /// the file's modification time so that `gc` does not prune it) and
    /// return `true`. Used instead of writing an object that exists.
    fn freshen(&self, oid: &ObjectId) -> bool {
        self.contains(oid)
    }
}
