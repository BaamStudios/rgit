// SPDX-License-Identifier: MIT

//! The interface every object store implements (loose objects, packs,
//! alternates).

use std::fmt;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectHeader;

use crate::Error;

pub use rgit_object::RawObject;

/// A source of objects.
///
/// [`crate::LooseStore`] implements it; the pack reader (#17) and
/// alternates (#93) plug in through it too. A store only ever returns
/// whole objects: a pack store resolves deltas itself and hands back the
/// object's type and bytes.
///
/// `Ok(None)` means "not in this store". An `Err` means the store has the
/// object but cannot deliver it intact; [`crate::ObjectDatabase`] then
/// tries the remaining stores (see there for how errors are reported).
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

    /// [`read`](ObjectStore::read), passing problems with single copies
    /// that the store skipped on its own (a store holding several copies,
    /// such as a set of packs) to `report`. The default calls `read`.
    fn read_reporting(
        &self,
        oid: &ObjectId,
        report: &mut dyn FnMut(&Error),
    ) -> Result<Option<RawObject>, Error> {
        let _ = report;
        self.read(oid)
    }

    /// [`header`](ObjectStore::header) with skipped copies passed to
    /// `report` (see [`read_reporting`](ObjectStore::read_reporting)).
    fn header_reporting(
        &self,
        oid: &ObjectId,
        report: &mut dyn FnMut(&Error),
    ) -> Result<Option<ObjectHeader>, Error> {
        let _ = report;
        self.header(oid)
    }

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
