// SPDX-License-Identifier: MIT

//! Where the pack writer gets objects from.
//!
//! The object database (`rgit-odb`) is not a dependency of this crate, so
//! the writer asks a small trait. [`PackSetSource`] implements it for the
//! packs of a repository; callers add loose objects and alternates by
//! implementing the trait for their own store.

use std::cell::RefCell;
use std::sync::Arc;

use rgit_hash::ObjectId;

use super::SourceError;
use crate::{DEFAULT_DELTA_BASE_CACHE_LIMIT, DeltaBaseCache, Object, ObjectKind, Pack, PackSet};

/// Read access to the objects that may be packed.
pub trait ObjectSource {
    /// Read `id`. `Ok(None)`: the object does not exist.
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, SourceError>;

    /// The type of `id` without needing its content. The default reads the
    /// whole object.
    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        Ok(self.read(id)?.map(|o| o.kind))
    }
}

impl<T: ObjectSource + ?Sized> ObjectSource for &T {
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, SourceError> {
        (**self).read(id)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        (**self).kind(id)
    }
}

/// An [`ObjectSource`] over a [`PackSet`], with its own delta base cache.
pub struct PackSetSource<'a> {
    set: &'a PackSet,
    cache: RefCell<DeltaBaseCache>,
}

impl<'a> PackSetSource<'a> {
    /// Read from `set`.
    pub fn new(set: &'a PackSet) -> PackSetSource<'a> {
        PackSetSource {
            set,
            cache: RefCell::new(DeltaBaseCache::new(DEFAULT_DELTA_BASE_CACHE_LIMIT)),
        }
    }

    /// The pack set.
    pub fn set(&self) -> &'a PackSet {
        self.set
    }

    /// The packs, in lookup order (for [`write_pack`](super::write_pack)'s
    /// reuse of packed data).
    pub fn packs(&self) -> Vec<Arc<Pack>> {
        self.set.packs()
    }
}

impl ObjectSource for PackSetSource<'_> {
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, SourceError> {
        let mut cache = self.cache.borrow_mut();
        Ok(self.set.read(id, &mut cache, &mut |_| {})?)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        for pack in self.set.packs() {
            let Some(pos) = pack.index().lookup(id) else {
                continue;
            };
            let info = pack
                .index()
                .offset_at(pos)
                .and_then(|offset| pack.object_info(offset));
            if let Ok((kind, _)) = info {
                return Ok(Some(kind));
            }
        }
        self.read(id).map(|o| o.map(|o| o.kind))
    }
}
