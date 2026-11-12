// SPDX-License-Identifier: MIT

//! A pack: its index plus the lazily opened `.pack` data.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use rgit_hash::{HashKind, ObjectId, hash_object};

use crate::data::{EntryHeader, EntryKind, PackData};
use crate::delta::{apply_delta, delta_sizes};
use crate::{DeltaBaseCache, Error, Object, ObjectKind, PackIndex};

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// Resolver for `REF_DELTA` bases that are not in the pack itself (thin
/// packs). Returns the base object, or `None` if it is not available.
pub type ExternalBase<'a> = &'a mut dyn FnMut(&ObjectId) -> Option<Object>;

/// One pack: `pack-<hash>.idx` and `pack-<hash>.pack`.
///
/// The index is read when the pack is opened. The `.pack` file is opened on
/// first access and checked against the index then (header, object count,
/// trailing checksum); if that fails every read reports the same error.
///
/// `Pack` is `Send + Sync`; reads take `&self` and a per-caller
/// [`DeltaBaseCache`].
pub struct Pack {
    index: PackIndex,
    pack_path: PathBuf,
    data: OnceLock<Result<PackData, Error>>,
    /// (offset, index position), sorted by offset.
    revindex: OnceLock<Vec<(u64, u32)>>,
    uid: u64,
    keep: bool,
    promisor: bool,
}

impl Pack {
    /// Open the pack whose index is `idx_path`; the `.pack` is expected
    /// next to it. `.keep` and `.promisor` files next to it set the flags.
    pub fn open(idx_path: impl AsRef<Path>, kind: HashKind) -> Result<Pack, Error> {
        let index = PackIndex::open(idx_path.as_ref(), kind)?;
        let pack_path = index.pack_path();
        let keep = idx_path.as_ref().with_extension("keep").exists();
        let promisor = idx_path.as_ref().with_extension("promisor").exists();
        Ok(Pack::new(index, pack_path, None, keep, promisor))
    }

    /// A pack held in memory. `name` is the `.pack` path used in messages.
    pub fn from_bytes(
        idx: Vec<u8>,
        pack: Vec<u8>,
        name: impl Into<PathBuf>,
        kind: HashKind,
    ) -> Result<Pack, Error> {
        let pack_path: PathBuf = name.into();
        let index = PackIndex::from_bytes(idx, pack_path.with_extension("idx"), kind)?;
        let data = PackData::from_bytes(pack, &pack_path, kind);
        Ok(Pack::new(index, pack_path, Some(data), false, false))
    }

    fn new(
        index: PackIndex,
        pack_path: PathBuf,
        data: Option<Result<PackData, Error>>,
        keep: bool,
        promisor: bool,
    ) -> Pack {
        let pack = Pack {
            index,
            pack_path,
            data: OnceLock::new(),
            revindex: OnceLock::new(),
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            keep,
            promisor,
        };
        if let Some(d) = data {
            let d = d.and_then(|d| pack.check(d));
            let _ = pack.data.set(d);
        }
        pack
    }

    /// The index.
    pub fn index(&self) -> &PackIndex {
        &self.index
    }

    /// Path of the `.pack` file.
    pub fn path(&self) -> &Path {
        &self.pack_path
    }

    /// Whether a `.keep` file exists for this pack.
    pub fn is_keep(&self) -> bool {
        self.keep
    }

    /// Whether a `.promisor` file exists for this pack.
    pub fn is_promisor(&self) -> bool {
        self.promisor
    }

    /// Whether the index lists `id`.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.index.lookup(id).is_some()
    }

    /// Process-unique number of this pack (the delta base cache key).
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// The `.pack` data, opened and checked against the index on first use.
    pub fn data(&self) -> Result<&PackData, Error> {
        self.data
            .get_or_init(|| {
                PackData::open(&self.pack_path, self.index.hash_kind()).and_then(|d| self.check(d))
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    fn check(&self, d: PackData) -> Result<PackData, Error> {
        if d.num_objects() != self.index.len() {
            return Err(Error::CountMismatch {
                path: self.pack_path.clone(),
                pack_count: d.num_objects(),
                index_count: self.index.len(),
            });
        }
        if d.checksum() != self.index.pack_checksum() {
            return Err(Error::DoesNotMatchIndex {
                path: self.pack_path.clone(),
            });
        }
        Ok(d)
    }

    /// (offset, index position) pairs sorted by offset; entries whose
    /// offset cannot be read are left out.
    fn revindex(&self) -> &[(u64, u32)] {
        self.revindex.get_or_init(|| {
            let mut v: Vec<(u64, u32)> = (0..self.index.len())
                .filter_map(|p| self.index.offset_at(p).ok().map(|o| (o, p)))
                .collect();
            v.sort_unstable();
            v
        })
    }

    /// Id of the object whose entry starts at `offset`.
    pub fn id_at_offset(&self, offset: u64) -> Option<ObjectId> {
        let r = self.revindex();
        r.binary_search_by_key(&offset, |e| e.0)
            .ok()
            .map(|i| self.index.id_at(r[i].1))
    }

    /// Offset where the entry at `offset` ends (the next entry or the
    /// trailer).
    pub fn entry_end(&self, offset: u64) -> Result<u64, Error> {
        let data = self.data()?;
        let r = self.revindex();
        let i = r
            .binary_search_by_key(&offset, |e| e.0)
            .map_err(|_| Error::BadRevindexOffset)?;
        Ok(r.get(i + 1).map_or(data.entries_end(), |e| e.0))
    }

    /// Read object `id`, or `None` if the pack does not list it.
    pub fn read(&self, id: &ObjectId, cache: &mut DeltaBaseCache) -> Result<Option<Object>, Error> {
        let Some(pos) = self.index.lookup(id) else {
            return Ok(None);
        };
        let offset = self.index.offset_at(pos)?;
        self.read_at(offset, cache, &mut |_| None).map(Some)
    }

    /// Read the object whose entry starts at `offset`, resolving delta
    /// chains of any depth without recursion. `REF_DELTA` bases that are
    /// not in this pack are requested from `external`.
    pub fn read_at(
        &self,
        offset: u64,
        cache: &mut DeltaBaseCache,
        external: ExternalBase<'_>,
    ) -> Result<Object, Error> {
        let data = self.data()?;
        if let Some((kind, bytes)) = cache.get(self.uid, offset) {
            return Ok(Object {
                kind,
                data: bytes.to_vec(),
            });
        }
        // Walk down to a whole object (or a cached base), remembering the
        // deltas on the way.
        let mut chain: Vec<EntryHeader> = Vec::new();
        let mut cur = offset;
        let mut cur_id: Option<ObjectId> = None;
        let max_depth = self.index.len() as usize + 1;
        let (kind, mut base, from_cache): (ObjectKind, Arc<[u8]>, bool) = loop {
            if !chain.is_empty() {
                if let Some((k, b)) = cache.get(self.uid, cur) {
                    break (k, b, true);
                }
            }
            let h = data
                .entry_header(cur)
                .map_err(|e| self.wrap_base(e, &chain, cur, cur_id))?;
            match h.kind {
                EntryKind::Base(k) => {
                    let (bytes, _) = data
                        .inflate(&h)
                        .map_err(|e| self.wrap_base(e, &chain, cur, cur_id))?;
                    break (k, bytes.into(), false);
                }
                EntryKind::OfsDelta { base_offset } => {
                    let base_id = self
                        .id_at_offset(base_offset)
                        .ok_or(Error::BadRevindexOffset)?;
                    chain.push(h);
                    cur = base_offset;
                    cur_id = Some(base_id);
                }
                EntryKind::RefDelta { base: base_id } => {
                    chain.push(h);
                    match self.index.lookup(&base_id) {
                        Some(pos) => {
                            cur = self.index.offset_at(pos)?;
                            cur_id = Some(base_id);
                        }
                        None => match external(&base_id) {
                            Some(obj) => break (obj.kind, obj.data.into(), true),
                            None => {
                                return Err(Error::MissingBase {
                                    base: base_id,
                                    offset: h.offset,
                                });
                            }
                        },
                    }
                }
            }
            if chain.len() > max_depth {
                return Err(Error::DeltaLoop {
                    offset: cur,
                    path: self.pack_path.clone(),
                });
            }
        };
        if chain.is_empty() {
            return Ok(Object {
                kind,
                data: base.to_vec(),
            });
        }
        if !from_cache {
            cache.insert(self.uid, cur, kind, base.clone());
        }
        // Apply the deltas from the innermost outwards. Entry `i` (i > 0)
        // is itself the base of entry `i - 1`.
        for i in (0..chain.len()).rev() {
            let h = &chain[i];
            let applied = data.inflate(h).and_then(|(delta, _)| {
                apply_delta(&base, &delta).map_err(|reason| Error::BadDelta {
                    offset: h.offset,
                    reason,
                })
            });
            if i == 0 {
                return applied.map(|data| Object { kind, data });
            }
            let result = applied.map_err(|e| Error::DeltaBase {
                base: self
                    .id_at_offset(h.offset)
                    .unwrap_or(self.index.hash_kind().null()),
                offset: h.offset,
                path: self.pack_path.clone(),
                source: Box::new(e),
            })?;
            base = result.into();
            cache.insert(self.uid, h.offset, kind, base.clone());
        }
        unreachable!("chain is not empty")
    }

    /// Wrap an error from reading the entry at `cur`: when `cur` is a
    /// delta base (the chain is not empty), name the base object.
    fn wrap_base(
        &self,
        e: Error,
        chain: &[EntryHeader],
        cur: u64,
        cur_id: Option<ObjectId>,
    ) -> Error {
        if chain.is_empty() {
            return e;
        }
        Error::DeltaBase {
            base: cur_id.unwrap_or(self.index.hash_kind().null()),
            offset: cur,
            path: self.pack_path.clone(),
            source: Box::new(e),
        }
    }

    /// Type and size of the object at `offset` without reconstructing it:
    /// the type comes from the end of the delta chain, the size from the
    /// header of the delta.
    pub fn object_info(&self, offset: u64) -> Result<(ObjectKind, u64), Error> {
        let data = self.data()?;
        let top = data.entry_header(offset)?;
        let size = match top.kind {
            EntryKind::Base(k) => return Ok((k, top.size)),
            _ => {
                // The two leading sizes take at most 2 * 10 bytes.
                let head = data
                    .inflate_prefix(&top, 20)
                    .map_err(|_| Error::DeltaUnpackInitial)?;
                delta_sizes(&head).ok_or(Error::DeltaUnpackInitial)?.1
            }
        };
        let mut h = top;
        let max_depth = self.index.len() as usize + 1;
        for _ in 0..=max_depth {
            let next = match h.kind {
                EntryKind::Base(k) => return Ok((k, size)),
                EntryKind::OfsDelta { base_offset } => base_offset,
                EntryKind::RefDelta { base } => match self.index.lookup(&base) {
                    Some(pos) => self.index.offset_at(pos)?,
                    None => {
                        return Err(Error::MissingBase {
                            base,
                            offset: h.offset,
                        });
                    }
                },
            };
            h = data.entry_header(next)?;
        }
        Err(Error::DeltaLoop {
            offset,
            path: self.pack_path.clone(),
        })
    }

    /// The delta base of the object at `offset`, if it is stored as a delta.
    pub fn delta_base(&self, offset: u64) -> Result<Option<ObjectId>, Error> {
        let h = self.data()?.entry_header(offset)?;
        match h.kind {
            EntryKind::Base(_) => Ok(None),
            EntryKind::RefDelta { base } => Ok(Some(base)),
            EntryKind::OfsDelta { base_offset } => self
                .id_at_offset(base_offset)
                .map(Some)
                .ok_or(Error::BadRevindexOffset),
        }
    }

    /// Check everything: both trailing checksums, the CRC32 of every entry
    /// (index v2), and that every object hashes to its id.
    pub fn verify(&self, cache: &mut DeltaBaseCache) -> Result<(), Error> {
        let data = self.data()?;
        self.index.verify_checksum()?;
        data.verify_checksum()?;
        if self.revindex().len() != self.index.len() as usize {
            return Err(Error::OffsetBeyondIndex {
                path: self.pack_path.clone(),
            });
        }
        let kind = self.index.hash_kind();
        for i in 0..self.revindex().len() {
            let (offset, pos) = self.revindex()[i];
            let id = self.index.id_at(pos);
            let end = self.entry_end(offset)?;
            if offset < crate::data::PACK_HEADER_LEN || end > data.entries_end() || end <= offset {
                return Err(Error::OffsetBeyondPack);
            }
            if let Some(crc) = self.index.crc32_at(pos) {
                let actual = crc32fast::hash(&data.bytes()[offset as usize..end as usize]);
                if actual != crc {
                    return Err(Error::CrcMismatch {
                        id,
                        path: self.pack_path.clone(),
                    });
                }
            }
            let obj = self.read_at(offset, cache, &mut |_| None)?;
            match hash_object(kind, obj.kind.name(), &obj.data) {
                Ok(h) if h == id => {}
                _ => {
                    return Err(Error::HashMismatch {
                        id,
                        path: self.pack_path.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for Pack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pack")
            .field("path", &self.pack_path)
            .field("objects", &self.index.len())
            .field("keep", &self.keep)
            .field("promisor", &self.promisor)
            .finish()
    }
}
