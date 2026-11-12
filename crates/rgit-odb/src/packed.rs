// SPDX-License-Identifier: MIT

//! [`PackStore`]: the packs of an object directory as an [`ObjectStore`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectHeader;
use rgit_pack::{DEFAULT_DELTA_BASE_CACHE_LIMIT, DeltaBaseCache, PackSet};

use crate::Error;
use crate::store::{ObjectStore, RawObject};

/// All packs of one `objects/pack` directory ([`rgit_pack::PackSet`]) with
/// a shared delta base cache.
///
/// Corrupt copies inside the packs are passed to the `report` callback of
/// [`ObjectStore::read_reporting`] (so [`crate::ObjectDatabase`] prints
/// them like git) before the next copy is tried.
pub struct PackStore {
    set: Arc<PackSet>,
    cache: Mutex<DeltaBaseCache>,
}

impl std::fmt::Debug for PackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackStore").field("set", &self.set).finish()
    }
}

impl PackStore {
    /// The packs in `pack_dir` (usually `<objects>/pack`); a missing
    /// directory gives an empty store.
    pub fn open(pack_dir: impl Into<PathBuf>, hash: HashKind) -> PackStore {
        PackStore::from_set(Arc::new(PackSet::open(pack_dir, hash)))
    }

    /// A store over an existing pack set.
    pub fn from_set(set: Arc<PackSet>) -> PackStore {
        PackStore {
            set,
            cache: Mutex::new(DeltaBaseCache::new(DEFAULT_DELTA_BASE_CACHE_LIMIT)),
        }
    }

    /// Use a delta base cache of `limit` bytes (`core.deltaBaseCacheLimit`).
    pub fn set_cache_limit(&self, limit: usize) {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) = DeltaBaseCache::new(limit);
    }

    /// The underlying pack set (e.g. for reusing deltas when writing packs).
    pub fn set(&self) -> &Arc<PackSet> {
        &self.set
    }

    fn header_once(&self, oid: &ObjectId) -> Option<ObjectHeader> {
        for pack in self.set.packs() {
            let Some(pos) = pack.index().lookup(oid) else {
                continue;
            };
            let info = pack
                .index()
                .offset_at(pos)
                .and_then(|off| pack.object_info(off));
            if let Ok((kind, size)) = info {
                return Some(ObjectHeader { kind, size });
            }
        }
        None
    }
}

impl ObjectStore for PackStore {
    fn hash_kind(&self) -> HashKind {
        self.set.hash_kind()
    }

    fn contains(&self, oid: &ObjectId) -> bool {
        self.set.contains(oid) || (self.set.rescan_if_changed() && self.set.contains(oid))
    }

    fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        self.header_reporting(oid, &mut |_| {})
    }

    fn header_reporting(
        &self,
        oid: &ObjectId,
        report: &mut dyn FnMut(&Error),
    ) -> Result<Option<ObjectHeader>, Error> {
        if !self.contains(oid) {
            return Ok(None);
        }
        if let Some(h) = self.header_once(oid) {
            return Ok(Some(h));
        }
        // Fall back to reading the object, which reports what is broken.
        Ok(self.read_reporting(oid, report)?.map(|o| o.header()))
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        self.read_reporting(oid, &mut |_| {})
    }

    fn read_reporting(
        &self,
        oid: &ObjectId,
        report: &mut dyn FnMut(&Error),
    ) -> Result<Option<RawObject>, Error> {
        self.read_with_bases(oid, report, &mut |_| None)
    }

    fn read_with_bases(
        &self,
        oid: &ObjectId,
        report: &mut dyn FnMut(&Error),
        bases: &mut dyn FnMut(&ObjectId) -> Option<RawObject>,
    ) -> Result<Option<RawObject>, Error> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        self.set
            .read_with_fallback(
                oid,
                &mut cache,
                &mut |e| report(&Error::Pack(e.clone())),
                bases,
            )
            .map_err(Error::PackSummary)
    }

    fn for_each_prefix_match(
        &self,
        prefix: &Prefix,
        f: &mut dyn FnMut(ObjectId),
    ) -> Result<(), Error> {
        self.set.rescan_if_changed();
        for id in self.set.lookup_prefix(prefix, usize::MAX) {
            f(id);
        }
        Ok(())
    }

    fn for_each_object(&self, f: &mut dyn FnMut(ObjectId)) -> Result<(), Error> {
        self.set.rescan_if_changed();
        for id in self.set.all_ids() {
            f(id);
        }
        Ok(())
    }
}
