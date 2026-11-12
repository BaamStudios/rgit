// SPDX-License-Identifier: MIT

//! Indexing a pack stream (`git index-pack`).
//!
//! [`index_pack`] reads a pack from a [`Read`]er in one pass, without
//! seeking: entry headers, zlib streams, CRC32 of every entry and the
//! trailing checksum. With [`Input::Stream`] the bytes are copied to a file
//! while they are parsed (`--stdin`); with [`Input::File`] the pack is read
//! where it is. Base objects are hashed as they arrive.
//!
//! The second pass resolves `OFS_DELTA` and `REF_DELTA` entries from the
//! written file, in parallel: each thread takes a base object and walks the
//! tree of deltas that depend on it, depth first. Each thread keeps the
//! chain of objects from the base to the current delta; when their total
//! size exceeds its share of [`Options::memory_limit`] the oldest are
//! dropped and rebuilt from the pack when needed again, so memory stays
//! bounded for any delta depth. Every delta is resolved exactly once, which
//! also ends delta cycles.
//!
//! With [`Options::fix_thin`], `REF_DELTA` bases missing from the pack are
//! read from an [`ObjectDb`] and appended to the pack (thin pack
//! completion); the header and the trailing checksum are rewritten.
//!
//! The result, [`IndexedPack`], writes the `.idx` (version 1 or 2, with a
//! configurable 64-bit offset threshold) and the `.rev` reverse index,
//! byte for byte as git does. Writing files under their final names is the
//! caller's job.
//!
//! Checks: object ids are compared with existing objects of the same id
//! ([`ObjectDb`], git's "SHA1 COLLISION FOUND"), duplicate objects can be
//! rejected ([`Options::reject_duplicates`]), and every object is handed to
//! an optional [`ObjectHook`] (`--strict`, `--fsck-objects`).

mod error;
pub mod messages;
mod output;
mod parse;
mod resolve;
mod thin;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rgit_hash::{HashKind, ObjectId};

use crate::{Object, ObjectKind};

pub use error::{BadEntry, Error};
pub use output::{DEFAULT_LARGE_OFFSET_LIMIT, IndexVersion};

/// Default for [`Options::memory_limit`] (git's `core.deltaBaseCacheLimit`
/// default).
pub const DEFAULT_MEMORY_LIMIT: usize = 96 << 20;

/// Read access to the objects already in the repository.
///
/// Used for `--fix-thin` (bases of thin packs) and for the collision
/// check. `Ok(None)` means the object does not exist.
pub trait ObjectDb: Sync {
    /// Whether the object exists (cheap check before [`ObjectDb::read`]).
    fn contains(&self, id: &ObjectId) -> bool;
    /// Read an object. `Err` carries a message for a damaged object.
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, String>;
}

/// Called for every object of the pack once its id is known, from several
/// threads at once. An `Err` aborts indexing with that message.
pub trait ObjectHook: Sync {
    /// Look at one object.
    fn object(&self, id: &ObjectId, kind: ObjectKind, data: &[u8]) -> Result<(), String>;
}

/// The phases reported to a [`Progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Reading the entries (`Receiving objects` / `Indexing objects`).
    Reading,
    /// Resolving deltas.
    Resolving,
}

/// Progress reporting.
pub trait Progress: Send {
    /// A phase begins with `total` items.
    fn start(&mut self, phase: Phase, total: u64);
    /// `done` items are finished; `bytes` bytes of input were read so far
    /// (only meaningful while [`Phase::Reading`]).
    fn update(&mut self, done: u64, bytes: u64);
    /// The phase is over; `suffix` replaces git's `done` (thin packs).
    fn finish(&mut self, suffix: Option<String>);
}

/// Where the pack comes from.
pub enum Input<'a> {
    /// An existing pack file, read in place.
    File(&'a Path),
    /// A stream whose bytes are copied to `dest` (empty, opened for
    /// reading and writing) while they are parsed.
    Stream {
        /// The pack data.
        reader: &'a mut dyn Read,
        /// Where the pack is written.
        dest: &'a mut File,
        /// Path of `dest` (to map it for the second pass).
        dest_path: &'a Path,
    },
}

/// Settings for [`index_pack`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Hash algorithm of the pack.
    pub hash: HashKind,
    /// Threads for delta resolution; 0 picks a default.
    pub threads: usize,
    /// Fail when the pack is larger than this many bytes.
    pub max_input_size: Option<u64>,
    /// Complete thin packs from the [`ObjectDb`] (needs [`Input::Stream`]).
    pub fix_thin: bool,
    /// Fail when an object occurs twice.
    pub reject_duplicates: bool,
    /// The pack header was already read by the caller (`--pack_header`):
    /// version and object count. The stream then starts with the first
    /// entry; the header is still written to the destination.
    pub pack_header: Option<(u32, u32)>,
    /// zlib level for objects appended by `fix_thin` (-1 = default).
    pub compression_level: i32,
    /// Memory for delta bases kept during resolution, split between
    /// threads.
    pub memory_limit: usize,
    /// With [`Input::Stream`]: data after the pack is not an error; the
    /// bytes already read past the trailer are returned in
    /// [`IndexedPack::trailing_data`] and the stream is not read further
    /// (it may belong to a protocol that continues). Otherwise the input
    /// is read to its end and any data after the pack is an error.
    pub allow_trailing_data: bool,
}

impl Options {
    /// Defaults for `hash`.
    pub fn new(hash: HashKind) -> Options {
        Options {
            hash,
            threads: 0,
            max_input_size: None,
            fix_thin: false,
            reject_duplicates: false,
            pack_header: None,
            compression_level: -1,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            allow_trailing_data: false,
        }
    }
}

/// One object of an indexed pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexedObject {
    /// The object id.
    pub id: ObjectId,
    /// The object type (for deltas: the type of the delta chain's base).
    pub kind: ObjectKind,
    /// Offset of the entry in the pack.
    pub offset: u64,
    /// CRC32 of the whole entry (header and compressed data).
    pub crc32: u32,
    /// Whether the entry is stored as a delta.
    pub delta: bool,
}

/// A fully indexed pack: every object's id, offset and CRC32.
#[derive(Debug, Clone)]
pub struct IndexedPack {
    hash: HashKind,
    /// In pack order.
    objects: Vec<IndexedObject>,
    checksum: ObjectId,
    local_objects: u64,
    deltas: u64,
    pack_len: u64,
    trailing: Vec<u8>,
}

impl IndexedPack {
    /// Hash algorithm.
    pub fn hash_kind(&self) -> HashKind {
        self.hash
    }

    /// The objects in pack order.
    pub fn objects(&self) -> &[IndexedObject] {
        &self.objects
    }

    /// The pack's trailing checksum, which names the pack
    /// (`pack-<checksum>.pack`).
    pub fn checksum(&self) -> ObjectId {
        self.checksum
    }

    /// Objects appended from the repository to complete a thin pack.
    pub fn local_objects(&self) -> u64 {
        self.local_objects
    }

    /// Number of entries stored as deltas.
    pub fn deltas(&self) -> u64 {
        self.deltas
    }

    /// Size of the final pack file in bytes.
    pub fn pack_len(&self) -> u64 {
        self.pack_len
    }

    /// Bytes that followed the pack in the stream (see
    /// [`Options::allow_trailing_data`]).
    pub fn trailing_data(&self) -> &[u8] {
        &self.trailing
    }
}

/// Per-entry facts collected by the first pass.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub offset: u64,
    /// Start of the zlib stream.
    pub data_offset: u64,
    /// Inflated size (object or delta data).
    pub size: u64,
    pub crc32: u32,
    pub base: Base,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Base {
    /// A whole object.
    None(ObjectKind),
    /// `OFS_DELTA` against the entry with this index.
    Ofs(u32),
    /// `REF_DELTA` against this id.
    Ref(ObjectId),
}

impl Entry {
    pub fn is_delta(&self) -> bool {
        !matches!(self.base, Base::None(_))
    }
}

/// The resolved id and type of each entry.
pub(crate) type Slots = Vec<OnceLock<(ObjectId, ObjectKind)>>;

/// Index a pack. See the module documentation.
///
/// `odb` enables the collision check and is required for
/// [`Options::fix_thin`]; `hook` sees every object.
pub fn index_pack(
    input: Input<'_>,
    opts: &Options,
    odb: Option<&dyn ObjectDb>,
    hook: Option<&dyn ObjectHook>,
    mut progress: Option<&mut (dyn Progress + 'static)>,
) -> Result<IndexedPack, Error> {
    let checker = Checker { odb, hook };
    let (pass1, data_path, dest): (parse::Pass1, PathBuf, Option<&mut File>) = match input {
        Input::File(path) => {
            let mut file = File::open(path).map_err(|e| Error::Open {
                path: path.to_path_buf(),
                source: e,
            })?;
            let p = parse::read_pack(
                &mut file,
                None,
                opts,
                &checker,
                progress.as_deref_mut(),
                true,
            )?;
            (p, path.to_path_buf(), None)
        }
        Input::Stream {
            reader,
            dest,
            dest_path,
        } => {
            let p = parse::read_pack(
                reader,
                Some(&mut *dest),
                opts,
                &checker,
                progress.as_deref_mut(),
                false,
            )?;
            (p, dest_path.to_path_buf(), Some(dest))
        }
    };
    let parse::Pass1 {
        mut entries,
        mut slots,
        mut checksum,
        entries_end,
        version,
        trailing,
    } = pass1;
    let deltas = entries.iter().filter(|e| e.is_delta()).count() as u64;

    let data = crate::mmap::FileData::open(&data_path).map_err(|e| Error::Open {
        path: data_path.clone(),
        source: e,
    })?;
    let pack = &data.as_slice()[..entries_end as usize];

    let mut graph = resolve::Graph::new(&entries);
    let mut resolved = 0u64;
    if deltas > 0 {
        if let Some(p) = progress.as_deref_mut() {
            p.start(Phase::Resolving, deltas);
            p.update(0, 0);
        }
        resolved = resolve::resolve_all(
            pack,
            &entries,
            &slots,
            &graph,
            opts,
            &checker,
            progress.as_deref_mut(),
        )?;
    }

    let mut local_objects = 0;
    let mut pack_len = entries_end + opts.hash.byte_len() as u64;
    if resolved < deltas && opts.fix_thin {
        if let (Some(db), Some(dest)) = (odb, dest) {
            let out = thin::complete(
                thin::Thin {
                    pack,
                    dest,
                    entries: &mut entries,
                    slots: &mut slots,
                    graph: &mut graph,
                    entries_end,
                    version,
                },
                db,
                opts,
                &checker,
                &mut resolved,
                deltas,
                progress.as_deref_mut(),
            )?;
            local_objects = out.appended;
            if out.appended > 0 {
                checksum = out.checksum;
                pack_len = out.pack_len;
            }
        }
    }
    if deltas > 0 {
        if let Some(p) = progress {
            let suffix = (local_objects > 0).then(|| messages::completed_with(local_objects));
            p.finish(suffix);
        }
    }
    if resolved < deltas {
        return Err(Error::Unresolved(deltas - resolved));
    }

    let objects: Vec<IndexedObject> = entries
        .iter()
        .zip(slots.iter())
        .map(|(e, s)| {
            let (id, kind) = *s.get().expect("every entry is resolved");
            IndexedObject {
                id,
                kind,
                offset: e.offset,
                crc32: e.crc32,
                delta: e.is_delta(),
            }
        })
        .collect();
    let out = IndexedPack {
        hash: opts.hash,
        objects,
        checksum,
        local_objects,
        deltas,
        pack_len,
        trailing,
    };
    if opts.reject_duplicates {
        out.check_duplicates()?;
    }
    Ok(out)
}

impl IndexedPack {
    /// Fail with [`Error::Duplicate`] if an object occurs twice.
    pub fn check_duplicates(&self) -> Result<(), Error> {
        let mut ids: Vec<ObjectId> = self.objects.iter().map(|o| o.id).collect();
        ids.sort_unstable();
        match ids.windows(2).find(|w| w[0] == w[1]) {
            Some(w) => Err(Error::Duplicate(w[0])),
            None => Ok(()),
        }
    }
}

/// The checks every object goes through.
pub(crate) struct Checker<'a> {
    odb: Option<&'a dyn ObjectDb>,
    hook: Option<&'a dyn ObjectHook>,
}

impl Checker<'_> {
    pub fn check(&self, id: &ObjectId, kind: ObjectKind, data: &[u8]) -> Result<(), Error> {
        if let Some(db) = self.odb {
            if db.contains(id) {
                match db.read(id) {
                    Ok(Some(o)) if o.kind == kind && o.data == data => {}
                    Ok(Some(_)) => return Err(Error::Collision(*id)),
                    Ok(None) => {}
                    Err(msg) => return Err(Error::Hook(msg)),
                }
            }
        }
        if let Some(h) = self.hook {
            h.object(id, kind, data).map_err(Error::Hook)?;
        }
        Ok(())
    }
}

/// Number of threads for `requested` (0: available parallelism, at most 3,
/// as documented for `git index-pack --threads=0`).
pub fn effective_threads(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(3)
}

#[cfg(test)]
mod tests;
