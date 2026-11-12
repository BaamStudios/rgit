// SPDX-License-Identifier: MIT
//! The object database of a repository as commands see it: loose objects
//! (`rgit-odb`) and packs (`rgit-pack`), plus abbreviated object names.
//!
//! The pack store adapter lives here until the object database crate
//! wires packs in itself.

use std::sync::{Arc, Mutex};

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{ObjectHeader, ObjectKind};
use rgit_odb::{LooseOptions, ObjectDatabase, ObjectStore, RawObject};
use rgit_pack::{DeltaBaseCache, PackSet};

/// Default `core.deltaBaseCacheLimit` (96 MiB).
const DELTA_CACHE: usize = rgit_pack::DEFAULT_DELTA_BASE_CACHE_LIMIT;

/// Shortest abbreviation git prints (`core.abbrev` minimum).
pub const MIN_ABBREV: usize = 4;
/// Abbreviation length when the object count gives no larger value.
pub const DEFAULT_ABBREV: usize = 7;

fn kind_from_pack(k: rgit_pack::ObjectKind) -> ObjectKind {
    match k {
        rgit_pack::ObjectKind::Commit => ObjectKind::Commit,
        rgit_pack::ObjectKind::Tree => ObjectKind::Tree,
        rgit_pack::ObjectKind::Blob => ObjectKind::Blob,
        rgit_pack::ObjectKind::Tag => ObjectKind::Tag,
    }
}

fn report(e: &rgit_pack::Error) {
    for line in e.chain() {
        eprintln!("error: {line}");
    }
}

/// The packs of an object directory as an [`ObjectStore`].
struct PackStore {
    set: PackSet,
    cache: Mutex<DeltaBaseCache>,
}

impl std::fmt::Debug for PackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackStore").field("set", &self.set).finish()
    }
}

fn store_err(e: rgit_pack::Error) -> rgit_odb::Error {
    rgit_odb::Error::Store(Box::new(e))
}

impl ObjectStore for PackStore {
    fn hash_kind(&self) -> HashKind {
        self.set.hash_kind()
    }

    fn contains(&self, oid: &ObjectId) -> bool {
        self.set.contains(oid)
    }

    fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, rgit_odb::Error> {
        for pack in self.set.packs() {
            let Some(pos) = pack.index().lookup(oid) else {
                continue;
            };
            let info = pack
                .index()
                .offset_at(pos)
                .and_then(|off| pack.object_info(off));
            match info {
                Ok((kind, size)) => {
                    return Ok(Some(ObjectHeader {
                        kind: kind_from_pack(kind),
                        size,
                    }));
                }
                Err(e) => report(&e),
            }
        }
        Ok(self.read(oid)?.map(|o| o.header()))
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, rgit_odb::Error> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        match self.set.read(oid, &mut cache, &mut |e| report(e)) {
            Ok(Some(o)) => Ok(Some(RawObject {
                kind: kind_from_pack(o.kind),
                data: o.data,
            })),
            Ok(None) => Ok(None),
            Err(e) => Err(store_err(e)),
        }
    }

    fn for_each_prefix_match(
        &self,
        prefix: &Prefix,
        f: &mut dyn FnMut(ObjectId),
    ) -> Result<(), rgit_odb::Error> {
        for id in self.set.lookup_prefix(prefix, usize::MAX) {
            f(id);
        }
        Ok(())
    }

    fn for_each_object(&self, f: &mut dyn FnMut(ObjectId)) -> Result<(), rgit_odb::Error> {
        for id in self.set.all_ids() {
            f(id);
        }
        Ok(())
    }
}

/// Open the object database below `objects_dir` with loose objects and
/// packs.
pub fn open(objects_dir: &std::path::Path, hash: HashKind, loose: LooseOptions) -> ObjectDatabase {
    let mut db = ObjectDatabase::open(objects_dir, hash, loose);
    let packs = PackStore {
        set: PackSet::open(objects_dir.join("pack"), hash),
        cache: Mutex::new(DeltaBaseCache::new(DELTA_CACHE)),
    };
    db.insert_store(0, Arc::new(packs));
    db.set_error_sink(|e| {
        for line in e.git_lines() {
            eprintln!("error: {line}");
        }
    });
    db
}

/// Approximate number of objects: all packed objects plus the loose
/// objects in the `objects/` fan-out directories.
fn approximate_count(db: &ObjectDatabase, objects_dir: &std::path::Path) -> u64 {
    let mut n: u64 = 0;
    let packs = PackSet::open(objects_dir.join("pack"), db.hash_kind());
    for p in packs.packs() {
        n += u64::from(p.index().len());
    }
    if let Ok(dirs) = std::fs::read_dir(objects_dir) {
        for d in dirs.flatten() {
            let name = d.file_name();
            let name = name.to_string_lossy();
            if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Ok(files) = std::fs::read_dir(d.path()) {
                    n += files.count() as u64;
                }
            }
        }
    }
    n
}

/// The automatic abbreviation length (`core.abbrev` unset or `auto`): long
/// enough that the expected number of collisions stays small for the
/// number of objects in the repository, at least [`DEFAULT_ABBREV`].
pub fn auto_abbrev(db: &ObjectDatabase, objects_dir: &std::path::Path) -> usize {
    let count = approximate_count(db, objects_dir);
    // Each hex digit holds 4 bits; with n objects about log2(n) * 2 bits
    // keep the expected number of collisions small (birthday bound).
    let bits = 64 - count.leading_zeros() as usize;
    let len = bits.div_ceil(2);
    len.max(DEFAULT_ABBREV)
}

/// The shortest prefix of `id`, at least `min` hex digits, that no other
/// object in the database shares.
pub fn unique_abbrev(db: &ObjectDatabase, id: &ObjectId, min: usize) -> String {
    let hex = id.to_hex();
    let full = hex.len();
    if min >= full {
        return hex;
    }
    let mut len = min.max(MIN_ABBREV);
    while len < full {
        let Ok(prefix) = Prefix::new(*id, len) else {
            break;
        };
        let mut other = false;
        for store in db.stores() {
            let _ = store.for_each_prefix_match(&prefix, &mut |o| {
                if o != *id {
                    other = true;
                }
            });
            if other {
                break;
            }
        }
        if !other {
            break;
        }
        len += 1;
    }
    hex[..len].to_string()
}
