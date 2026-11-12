// SPDX-License-Identifier: MIT

//! A bounded cache of delta base objects.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::ObjectKind;

/// Default byte limit, the same as git's `core.deltaBaseCacheLimit` default.
pub const DEFAULT_DELTA_BASE_CACHE_LIMIT: usize = 96 * 1024 * 1024;

type Key = (u64, u64);

struct Entry {
    kind: ObjectKind,
    data: Arc<[u8]>,
    tick: u64,
}

/// Least-recently-used cache of reconstructed delta bases, keyed by pack and
/// offset, limited by the total size of the cached objects.
///
/// Reading objects that share bases (all versions of one file, for example)
/// then inflates and applies each base only once. The cache is owned by the
/// caller and passed to every read; use one per thread.
pub struct DeltaBaseCache {
    limit: usize,
    used: usize,
    tick: u64,
    map: HashMap<Key, Entry>,
    lru: BTreeMap<u64, Key>,
}

impl Default for DeltaBaseCache {
    fn default() -> Self {
        DeltaBaseCache::new(DEFAULT_DELTA_BASE_CACHE_LIMIT)
    }
}

impl DeltaBaseCache {
    /// A cache holding at most `limit` bytes of object data (0 disables it).
    pub fn new(limit: usize) -> DeltaBaseCache {
        DeltaBaseCache {
            limit,
            used: 0,
            tick: 0,
            map: HashMap::new(),
            lru: BTreeMap::new(),
        }
    }

    /// Bytes currently held.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Number of cached objects.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.map.clear();
        self.lru.clear();
        self.used = 0;
    }

    pub(crate) fn get(&mut self, pack: u64, offset: u64) -> Option<(ObjectKind, Arc<[u8]>)> {
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(&(pack, offset))?;
        self.lru.remove(&e.tick);
        e.tick = tick;
        self.lru.insert(tick, (pack, offset));
        Some((e.kind, e.data.clone()))
    }

    pub(crate) fn insert(&mut self, pack: u64, offset: u64, kind: ObjectKind, data: Arc<[u8]>) {
        let size = data.len();
        if size > self.limit / 2 || self.map.contains_key(&(pack, offset)) {
            return;
        }
        while self.used + size > self.limit {
            let Some((_, key)) = self.lru.pop_first() else {
                break;
            };
            if let Some(old) = self.map.remove(&key) {
                self.used -= old.data.len();
            }
        }
        self.tick += 1;
        self.lru.insert(self.tick, (pack, offset));
        self.map.insert(
            (pack, offset),
            Entry {
                kind,
                data,
                tick: self.tick,
            },
        );
        self.used += size;
    }
}

impl std::fmt::Debug for DeltaBaseCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeltaBaseCache")
            .field("limit", &self.limit)
            .field("used", &self.used)
            .field("entries", &self.map.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let mut c = DeltaBaseCache::new(10);
        let d = |n: usize| -> Arc<[u8]> { vec![0u8; n].into() };
        c.insert(1, 1, ObjectKind::Blob, d(4));
        c.insert(1, 2, ObjectKind::Blob, d(4));
        assert!(c.get(1, 1).is_some());
        c.insert(1, 3, ObjectKind::Blob, d(4));
        assert!(c.get(1, 2).is_none());
        assert!(c.get(1, 1).is_some());
        assert!(c.get(1, 3).is_some());
        assert_eq!(c.used(), 8);
        // Too large for the cache.
        c.insert(1, 4, ObjectKind::Blob, d(6));
        assert!(c.get(1, 4).is_none());
    }
}
