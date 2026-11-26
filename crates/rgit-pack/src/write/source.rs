// SPDX-License-Identifier: MIT

//! Where the pack writer gets objects from.
//!
//! The object database (`rgit-odb`) is not a dependency of this crate, so
//! the writer asks a small trait. [`PackSetSource`] implements it for the
//! packs of a repository; callers add loose objects and alternates by
//! implementing the trait for their own store.

use std::sync::{Arc, Mutex};

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

    /// Whether the commit `id` is shallow (its parents are not walked).
    /// The default: no commit is.
    fn is_shallow(&self, id: &ObjectId) -> bool {
        let _ = id;
        false
    }

    /// The type and size of `id` (for ordering the delta search). The
    /// default reads the whole object.
    fn header(&self, id: &ObjectId) -> Result<Option<(ObjectKind, u64)>, SourceError> {
        Ok(self.read(id)?.map(|o| (o.kind, o.data.len() as u64)))
    }

    /// The content of `id` as a stream, for objects above
    /// `core.bigFileThreshold` that are written without being held in
    /// memory. `Ok(None)` when the source cannot stream it (the object is
    /// then read whole). The default never streams.
    fn stream(&self, id: &ObjectId) -> Result<Option<SourceStream<'_>>, SourceError> {
        let _ = id;
        Ok(None)
    }
}

/// An object's type, size and content reader (see [`ObjectSource::stream`]).
pub type SourceStream<'a> = (ObjectKind, u64, Box<dyn std::io::Read + 'a>);

impl<T: ObjectSource + ?Sized> ObjectSource for &T {
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, SourceError> {
        (**self).read(id)
    }

    fn stream(&self, id: &ObjectId) -> Result<Option<SourceStream<'_>>, SourceError> {
        (**self).stream(id)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        (**self).kind(id)
    }

    fn is_shallow(&self, id: &ObjectId) -> bool {
        (**self).is_shallow(id)
    }

    fn header(&self, id: &ObjectId) -> Result<Option<(ObjectKind, u64)>, SourceError> {
        (**self).header(id)
    }
}

/// `inner` with the commits of `shallow` (a repository's `shallow`
/// file) treated as having no parents.
pub struct ShallowSource<'a, S: ObjectSource + ?Sized> {
    /// The objects.
    pub inner: &'a S,
    /// The shallow commits.
    pub shallow: std::collections::HashSet<ObjectId>,
}

impl<S: ObjectSource + ?Sized> ObjectSource for ShallowSource<'_, S> {
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, SourceError> {
        self.inner.read(id)
    }

    fn stream(&self, id: &ObjectId) -> Result<Option<SourceStream<'_>>, SourceError> {
        self.inner.stream(id)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        self.inner.kind(id)
    }

    fn header(&self, id: &ObjectId) -> Result<Option<(ObjectKind, u64)>, SourceError> {
        self.inner.header(id)
    }

    fn is_shallow(&self, id: &ObjectId) -> bool {
        self.shallow.contains(id)
    }
}

/// An [`ObjectSource`] over a [`PackSet`], with its own delta base cache.
pub struct PackSetSource<'a> {
    set: &'a PackSet,
    cache: Mutex<DeltaBaseCache>,
}

impl<'a> PackSetSource<'a> {
    /// Read from `set`.
    pub fn new(set: &'a PackSet) -> PackSetSource<'a> {
        PackSetSource {
            set,
            cache: Mutex::new(DeltaBaseCache::new(DEFAULT_DELTA_BASE_CACHE_LIMIT)),
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
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.set.read(id, &mut cache, &mut |_| {})?)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        Ok(self.header(id)?.map(|(kind, _)| kind))
    }

    fn header(&self, id: &ObjectId) -> Result<Option<(ObjectKind, u64)>, SourceError> {
        for pack in self.set.packs() {
            let Some(pos) = pack.index().lookup(id) else {
                continue;
            };
            let info = pack
                .index()
                .offset_at(pos)
                .and_then(|offset| pack.object_info(offset));
            if let Ok(info) = info {
                return Ok(Some(info));
            }
        }
        self.read(id)
            .map(|o| o.map(|o| (o.kind, o.data.len() as u64)))
    }
}
