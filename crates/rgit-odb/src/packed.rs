// SPDX-License-Identifier: MIT

//! [`PackStore`]: the packs of an object directory as an [`ObjectStore`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectHeader;
use rgit_pack::{DEFAULT_DELTA_BASE_CACHE_LIMIT, DeltaBaseCache, PackSet};

use crate::stream::ObjectReader;

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

    /// [`PackStore::open`] with the multi-pack index of the directory
    /// ([`PackSet::open_with_midx`]).
    pub fn open_with_midx(
        pack_dir: impl Into<PathBuf>,
        hash: HashKind,
        errors: &mut Vec<String>,
    ) -> Result<PackStore, Error> {
        let set = PackSet::open_with_midx(pack_dir, hash, errors)
            .map_err(|e| Error::MultiPackIndex(e.0))?;
        Ok(PackStore::from_set(Arc::new(set)))
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

    /// A copy in a cruft pack does not count (see [`PackSet::freshen`]).
    fn freshen(&self, oid: &ObjectId) -> bool {
        self.set.freshen(oid)
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
        check_pack_alloc(&self.set, oid)?;
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

    fn stream(&self, oid: &ObjectId) -> Result<Option<ObjectReader>, Error> {
        if !self.contains(oid) {
            return Ok(None);
        }
        match stream_from_packs(&self.set, oid) {
            Some(r) => Ok(Some(r)),
            None => Ok(self.read(oid)?.map(ObjectReader::from_raw)),
        }
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

/// Stream `oid` from the first pack that stores it as a whole object
/// (see [`rgit_pack::Pack::stream_at`]). `None` when no pack does: the
/// object is a delta, missing or unreadable, and has to be read whole.
pub fn stream_from_packs(set: &PackSet, oid: &ObjectId) -> Option<ObjectReader> {
    for pack in set.packs() {
        let Some(pos) = pack.index().lookup(oid) else {
            continue;
        };
        let Ok(offset) = pack.index().offset_at(pos) else {
            continue;
        };
        match pack.stream_at(offset) {
            Ok(Some((kind, size, reader))) => {
                return Some(ObjectReader::new(
                    ObjectHeader { kind, size },
                    Box::new(reader),
                ));
            }
            Ok(None) => return None,
            Err(_) => continue,
        }
    }
    None
}

/// With `GIT_ALLOC_LIMIT` set: fail before reading a packed object whose
/// content would need a larger buffer.
pub fn check_pack_alloc(set: &PackSet, oid: &ObjectId) -> Result<(), Error> {
    if rgit_object::alloc::limit().is_none() {
        return Ok(());
    }
    for pack in set.packs() {
        let Some(pos) = pack.index().lookup(oid) else {
            continue;
        };
        let info = pack
            .index()
            .offset_at(pos)
            .and_then(|off| pack.object_info(off));
        if let Ok((_, size)) = info {
            rgit_object::alloc::check_object(size)?;
            return Ok(());
        }
    }
    Ok(())
}
