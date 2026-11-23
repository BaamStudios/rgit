// SPDX-License-Identifier: MIT

//! Writing a pack from a list of objects, reusing packed data.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_hash::{HashKind, ObjectId};

use super::entry::{TYPE_OFS_DELTA, TYPE_REF_DELTA, push_base_distance, push_entry_header};
use super::output::PackStream;
use super::search::{Candidate, SearchOptions, search};
use super::{ListedObject, ObjectSource, WriteError};
use crate::data::EntryKind;
use crate::index_pack::{IndexedObject, IndexedPack};
use crate::{ObjectKind, Pack, PackData};

/// The longest delta chain allowed in a pack.
pub const MAX_DEPTH: u32 = 4095;

/// How [`write_pack`] writes.
#[derive(Debug, Clone)]
pub struct WriteOptions {
    /// Write deltas as `OFS_DELTA` (base given as a distance back in the
    /// pack) instead of `REF_DELTA` (base given by id). Only when the
    /// reader understands it (`--delta-base-offset`, capability
    /// `ofs-delta`).
    pub ofs_delta: bool,
    /// Copy deltas from existing packs when their base is in the output.
    pub reuse_delta: bool,
    /// Copy compressed whole objects from existing packs.
    pub reuse_object: bool,
    /// zlib level 0–9 for data compressed anew; `None`: zlib's default.
    pub compression: Option<u32>,
    /// Longest delta chain; longer reused chains are cut by storing an
    /// object whole. At most [`MAX_DEPTH`]. 0 disables deltas.
    pub max_depth: u32,
    /// Delta search: how many preceding objects each object is compared
    /// with (`--window`, `pack.window`). 0 searches no new deltas.
    pub window: usize,
    /// Delta search: memory for the window's objects in bytes; 0 means no
    /// limit (`--window-memory`, `pack.windowMemory`).
    pub window_memory: u64,
    /// Threads for the delta search; 0 uses one per CPU (`--threads`,
    /// `pack.threads`).
    pub threads: usize,
    /// Objects larger than this are never deltified
    /// (`core.bigFileThreshold`).
    pub big_file_threshold: u64,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            ofs_delta: false,
            reuse_delta: true,
            reuse_object: true,
            compression: None,
            max_depth: DEFAULT_DEPTH,
            window: DEFAULT_WINDOW,
            window_memory: 0,
            threads: 0,
            big_file_threshold: DEFAULT_BIG_FILE_THRESHOLD,
        }
    }
}

/// Default of `--depth` / `pack.depth`.
pub const DEFAULT_DEPTH: u32 = 50;
/// Default of `--window` / `pack.window`.
pub const DEFAULT_WINDOW: usize = 10;
/// Default of `core.bigFileThreshold` (512 MiB).
pub const DEFAULT_BIG_FILE_THRESHOLD: u64 = 512 << 20;

/// One entry as written, with the values a `.idx` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrittenEntry {
    /// The object.
    pub id: ObjectId,
    /// Its type.
    pub kind: ObjectKind,
    /// Whether it is stored as a delta.
    pub delta: bool,
    /// Offset of its entry in the pack.
    pub offset: u64,
    /// CRC32 of the entry bytes (header and data).
    pub crc32: u32,
}

/// What [`write_pack`] did.
#[derive(Debug, Clone)]
pub struct PackSummary {
    /// The trailing checksum (also the pack's name).
    pub checksum: ObjectId,
    /// Entries in pack order.
    pub entries: Vec<WrittenEntry>,
    /// Entries stored as deltas.
    pub deltas: u32,
    /// Entries copied from an existing pack.
    pub reused: u32,
    /// Deltas copied from an existing pack.
    pub reused_deltas: u32,
    /// Deltas against objects that are not in the pack (thin packs).
    pub thin_deltas: u32,
    /// Size of the pack in bytes, trailer included.
    pub pack_len: u64,
}

impl PackSummary {
    /// The pack as [`index_pack`](crate::index_pack) would describe it,
    /// for writing its `.idx` and `.rev` files.
    pub fn indexed(&self, hash: HashKind) -> IndexedPack {
        IndexedPack::from_parts(
            hash,
            self.entries
                .iter()
                .map(|e| IndexedObject {
                    id: e.id,
                    kind: e.kind,
                    offset: e.offset,
                    crc32: e.crc32,
                    delta: e.delta,
                })
                .collect(),
            self.checksum,
            self.pack_len,
        )
    }
}

/// How one object is stored.
#[derive(Clone)]
enum Rep {
    /// Read the object and compress it.
    Whole,
    /// Copy the compressed data of a whole object from a pack.
    PackedWhole { pack: Arc<Pack>, offset: u64 },
    /// Copy a delta from a pack; its base is entry `base` of the output.
    PackedDelta {
        pack: Arc<Pack>,
        offset: u64,
        base: usize,
    },
    /// A delta found by the search against entry `base` (an entry of the
    /// output, or a thin base after them).
    NewDelta {
        base: usize,
        raw_len: u64,
        compressed: Vec<u8>,
    },
}

impl Rep {
    fn base(&self) -> Option<usize> {
        match self {
            Rep::PackedDelta { base, .. } | Rep::NewDelta { base, .. } => Some(*base),
            _ => None,
        }
    }
}

/// Write the objects in `objects` (in this order, duplicates dropped) as a
/// self-contained version 2 pack to `out`.
///
/// Objects are read from `source`. When an object is found in one of
/// `packs` (searched in order) its compressed data is copied instead of
/// compressing it again; a stored delta is copied when its base is also in
/// the output. Objects without a reusable delta go through the delta
/// search ([`WriteOptions::window`]). Delta bases are always written
/// before their deltas. Copied data is checked against the CRC32 in the
/// pack index (or inflated, for version 1 indexes) and written anew if it
/// does not match.
pub fn write_pack<W: Write, S: ObjectSource + Sync + ?Sized>(
    out: W,
    hash: HashKind,
    objects: &[ListedObject],
    source: &S,
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
) -> Result<(PackSummary, W), WriteError> {
    write_thin_pack(out, hash, objects, &[], source, packs, opts)
}

/// Like [`write_pack`], but objects may also be stored as deltas against
/// `bases`: objects the reader is known to have (`--thin`). Such deltas
/// are written as `REF_DELTA`; the bases themselves are not written, so
/// the reader must complete the pack (`index-pack --fix-thin`). Bases that
/// are also in `objects`, or cannot be read, are ignored.
pub fn write_thin_pack<W: Write, S: ObjectSource + Sync + ?Sized>(
    out: W,
    hash: HashKind,
    objects: &[ListedObject],
    bases: &[ListedObject],
    source: &S,
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
) -> Result<(PackSummary, W), WriteError> {
    let mut written = write_impl(out, None, hash, objects, bases, source, packs, opts)?;
    Ok(written.pop().expect("one pack"))
}

/// Write the objects as several self-contained packs of at most
/// `max_size` bytes each (`--max-pack-size`, `pack.packSizeLimit`); a
/// single object larger than that gets a pack of its own. Each pack goes
/// to a new file from `open`. Deltas never cross packs: an object whose
/// base went into an earlier pack is stored whole.
pub fn write_pack_files<S: ObjectSource + Sync + ?Sized>(
    open: &mut dyn FnMut() -> std::io::Result<std::fs::File>,
    hash: HashKind,
    objects: &[ListedObject],
    source: &S,
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
    max_size: u64,
) -> Result<Vec<(PackSummary, std::fs::File)>, WriteError> {
    let first = open()?;
    let mut written = write_impl(
        first,
        Some((open, max_size)),
        hash,
        objects,
        &[],
        source,
        packs,
        opts,
    )?;
    for (summary, file) in &mut written {
        summary.checksum = fix_header(file, hash, summary.entries.len() as u32)?;
    }
    Ok(written)
}

/// Set the object count of a pack file written with another count, and
/// write its trailer anew. Returns the new checksum.
fn fix_header(
    file: &mut std::fs::File,
    hash: HashKind,
    count: u32,
) -> Result<ObjectId, WriteError> {
    use std::io::{Read, Seek, SeekFrom};
    let len = file.seek(SeekFrom::End(0))?;
    let body = len - hash.byte_len() as u64;
    file.seek(SeekFrom::Start(8))?;
    file.write_all(&count.to_be_bytes())?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = rgit_hash::Hasher::new(hash);
    let mut left = body;
    let mut chunk = vec![0u8; 1 << 16];
    while left > 0 {
        let n = (left as usize).min(chunk.len());
        file.read_exact(&mut chunk[..n])?;
        hasher.update(&chunk[..n]);
        left -= n as u64;
    }
    let id = hasher.finalize().map_err(|_| WriteError::Collision)?;
    file.write_all(id.as_bytes())?;
    file.flush()?;
    Ok(id)
}

/// Where the next pack goes when the current one is full, and the size
/// limit.
type Split<'a, W> = Option<(&'a mut dyn FnMut() -> std::io::Result<W>, u64)>;

#[allow(clippy::too_many_arguments)]
fn write_impl<W: Write, S: ObjectSource + Sync + ?Sized>(
    out: W,
    mut split: Split<'_, W>,
    hash: HashKind,
    objects: &[ListedObject],
    bases: &[ListedObject],
    source: &S,
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
) -> Result<Vec<(PackSummary, W)>, WriteError> {
    let mut index: HashMap<ObjectId, usize> = HashMap::with_capacity(objects.len());
    let mut listed: Vec<ListedObject> = Vec::with_capacity(objects.len());
    for o in objects {
        if let std::collections::hash_map::Entry::Vacant(e) = index.entry(o.id) {
            e.insert(listed.len());
            listed.push(*o);
        }
    }
    let count = u32::try_from(listed.len()).map_err(|_| WriteError::TooManyObjects)?;
    let ids: Vec<ObjectId> = listed.iter().map(|o| o.id).collect();
    let max_depth = opts.max_depth.min(MAX_DEPTH);

    let mut reps = choose_representations(&ids, &index, packs, opts);
    let fixed: Vec<Option<usize>> = reps.iter().map(Rep::base).collect();
    let (whole, depths) = limit_chains(&fixed, max_depth);
    for (rep, whole) in reps.iter_mut().zip(whole) {
        if whole {
            *rep = Rep::Whole;
        }
    }

    let level = match opts.compression {
        Some(l) => Compression::new(l.min(9)),
        None => Compression::default(),
    };

    // Thin bases get positions after the pack's own objects.
    let mut thin: Vec<ListedObject> = Vec::new();
    if opts.window > 0 && max_depth > 0 {
        let mut seen: HashSet<ObjectId> = HashSet::new();
        for b in bases {
            if !index.contains_key(&b.id) && seen.insert(b.id) {
                thin.push(*b);
            }
        }
        // How far reused deltas hang below each object: a new delta for it
        // must leave room for them within the depth limit.
        let mut below = vec![0u32; reps.len()];
        let mut by_depth: Vec<usize> = (0..reps.len())
            .filter(|&i| matches!(reps[i], Rep::PackedDelta { .. }))
            .collect();
        by_depth.sort_unstable_by_key(|&i| std::cmp::Reverse(depths[i]));
        for i in by_depth {
            if let Some(b) = reps[i].base() {
                below[b] = below[b].max(below[i] + 1);
            }
        }
        let mut cands = Vec::with_capacity(listed.len() + thin.len());
        for (pos, o) in listed.iter().chain(thin.iter()).enumerate() {
            let base_only = pos >= listed.len();
            let Some(size) = object_size(source, &o.id, base_only)? else {
                continue;
            };
            let fixed_depth = match reps.get(pos) {
                Some(Rep::PackedDelta { .. }) => Some(depths[pos]),
                _ => None,
            };
            cands.push(Candidate {
                pos,
                id: o.id,
                kind: o.kind,
                name_hash: o.name_hash,
                size,
                fixed_depth,
                below: below.get(pos).copied().unwrap_or(0),
                base_only,
            });
        }
        let search_opts = SearchOptions {
            window: opts.window,
            window_memory: opts.window_memory,
            max_depth,
            threads: effective_threads(opts.threads),
            big_file_threshold: opts.big_file_threshold,
            level,
            id_len: hash.byte_len(),
        };
        for (pos, found) in search(source, packs, cands, &search_opts)? {
            reps[pos] = Rep::NewDelta {
                base: found.base,
                raw_len: found.raw_len,
                compressed: found.compressed,
            };
        }
        // New deltas may lengthen reused chains or, across search
        // threads, close a cycle through them.
        let all: Vec<Option<usize>> = reps
            .iter()
            .map(Rep::base)
            .chain(std::iter::repeat_n(None, thin.len()))
            .collect();
        let (whole, _) = limit_chains(&all, max_depth);
        for (rep, whole) in reps.iter_mut().zip(whole) {
            if whole {
                *rep = Rep::Whole;
            }
        }
    }

    let mut stream = PackStream::new(out, hash, count)?;
    let mut offsets: Vec<Option<u64>> = vec![None; ids.len()];
    // Which output pack each written object went to.
    let mut pack_of: Vec<u32> = vec![0; ids.len()];
    let mut current = 0u32;
    let mut done: Vec<(PackSummary, W)> = Vec::new();
    let new_summary = || PackSummary {
        checksum: hash.null(),
        entries: Vec::new(),
        deltas: 0,
        reused: 0,
        reused_deltas: 0,
        thin_deltas: 0,
        pack_len: 0,
    };
    let mut summary = new_summary();
    let mut todo: Vec<usize> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    for first in 0..ids.len() {
        todo.push(first);
        while let Some(&i) = todo.last() {
            if offsets[i].is_some() {
                todo.pop();
                continue;
            }
            if let Some(base) = reps[i].base() {
                if base < ids.len() && offsets[base].is_none() {
                    todo.push(base);
                    continue;
                }
            }
            todo.pop();
            let mut offset = stream.begin_entry();
            buf.clear();
            // A base in an earlier output pack cannot be used.
            let base_here = |base: usize, current: u32, pack_of: &[u32]| {
                base >= ids.len() || pack_of[base] == current
            };
            let usable = reps[i]
                .base()
                .is_none_or(|b| base_here(b, current, &pack_of));
            let base_ref = |base: usize| {
                if base >= ids.len() {
                    BaseRef::Id(thin[base - ids.len()].id)
                } else if opts.ofs_delta {
                    BaseRef::Distance(offset - offsets[base].expect("base written"))
                } else {
                    BaseRef::Id(ids[base])
                }
            };
            let mut delta = false;
            let mut reused = false;
            match &reps[i] {
                _ if !usable => {}
                Rep::Whole => {}
                Rep::PackedWhole { pack, offset: from } => {
                    reused = copy_whole(pack, *from, &mut buf).is_some();
                }
                Rep::PackedDelta {
                    pack,
                    offset: from,
                    base,
                } => {
                    reused = copy_delta(pack, *from, base_ref(*base), &mut buf).is_some();
                    delta = reused;
                    if reused {
                        summary.reused_deltas += 1;
                    }
                }
                Rep::NewDelta {
                    base,
                    raw_len,
                    compressed,
                } => {
                    push_delta_header(&mut buf, base_ref(*base), *raw_len);
                    buf.extend_from_slice(compressed);
                    if *base >= ids.len() {
                        summary.thin_deltas += 1;
                    }
                    delta = true;
                }
            }
            if !reused && !delta {
                buf.clear();
                compress_whole(source, &ids[i], level, &mut buf)?;
            }
            if let Some((next, limit)) = split.as_mut() {
                let end = stream.offset() + buf.len() as u64 + hash.byte_len() as u64;
                if end > *limit && !summary.entries.is_empty() {
                    summary.pack_len = stream.offset() + hash.byte_len() as u64;
                    let (checksum, w) = stream.finish_partial()?;
                    summary.checksum = checksum;
                    done.push((std::mem::replace(&mut summary, new_summary()), w));
                    stream = PackStream::new(next()?, hash, count)?;
                    current += 1;
                    offset = stream.begin_entry();
                    if delta {
                        // Its base stayed in the previous pack.
                        if matches!(reps[i], Rep::PackedDelta { .. }) {
                            summary.reused_deltas = summary.reused_deltas.saturating_sub(1);
                        }
                        delta = false;
                        reused = false;
                        buf.clear();
                        compress_whole(source, &ids[i], level, &mut buf)?;
                    }
                }
            }
            summary.reused += u32::from(reused);
            summary.deltas += u32::from(delta);
            stream.put(&buf)?;
            let crc32 = stream.end_entry();
            offsets[i] = Some(offset);
            pack_of[i] = current;
            summary.entries.push(WrittenEntry {
                id: ids[i],
                kind: listed[i].kind,
                delta,
                offset,
                crc32,
            });
        }
    }
    summary.pack_len = stream.offset() + hash.byte_len() as u64;
    let (checksum, out) = if split.is_some() {
        stream.finish_partial()?
    } else {
        stream.finish()?
    };
    summary.checksum = checksum;
    done.push((summary, out));
    Ok(done)
}

/// The size of an object for the delta search; `None` for a thin base
/// that cannot be read (it is then simply not used).
fn object_size<S: ObjectSource + ?Sized>(
    source: &S,
    id: &ObjectId,
    optional: bool,
) -> Result<Option<u64>, WriteError> {
    match source.header(id) {
        Ok(Some((_, size))) => Ok(Some(size)),
        Ok(None) if optional => Ok(None),
        Ok(None) => Err(WriteError::Missing(*id)),
        Err(_) if optional => Ok(None),
        Err(e) => Err(WriteError::read(*id, e)),
    }
}

fn effective_threads(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Pick a stored representation for every object.
fn choose_representations(
    ids: &[ObjectId],
    index: &HashMap<ObjectId, usize>,
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
) -> Vec<Rep> {
    ids.iter()
        .map(|id| {
            if !opts.reuse_object {
                return Rep::Whole;
            }
            // A delta whose base is in the output beats a whole copy, in
            // whichever pack it is: packs completed with `--fix-thin` hold
            // the bases of their deltas whole, while an older pack has the
            // same object as a delta.
            let mut whole = None;
            for pack in packs {
                let Some(pos) = pack.index().lookup(id) else {
                    continue;
                };
                let Ok(offset) = pack.index().offset_at(pos) else {
                    continue;
                };
                let Ok(header) = pack.data().and_then(|d| d.entry_header(offset)) else {
                    continue;
                };
                if let EntryKind::Base(_) = header.kind {
                    whole.get_or_insert_with(|| Rep::PackedWhole {
                        pack: pack.clone(),
                        offset,
                    });
                    continue;
                }
                if !opts.reuse_delta {
                    continue;
                }
                if let Ok(Some(base)) = pack.delta_base(offset) {
                    if let Some(&b) = index.get(&base) {
                        return Rep::PackedDelta {
                            pack: pack.clone(),
                            offset,
                            base: b,
                        };
                    }
                }
            }
            whole.unwrap_or(Rep::Whole)
        })
        .collect()
}

/// Find the deltas to store whole instead: one per cycle (possible when
/// the chosen copies come from different packs), and those that would end
/// up deeper than `max_depth` in a chain. `bases[i]` is the base of entry
/// `i` if it is a delta. Also returns the depth of every entry.
fn limit_chains(bases: &[Option<usize>], max_depth: u32) -> (Vec<bool>, Vec<u32>) {
    const NEW: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;
    let n = bases.len();
    let mut whole = vec![false; n];
    let mut state = vec![NEW; n];
    let mut depth = vec![0u32; n];
    let mut path = Vec::new();
    let base_of = |i: usize, whole: &[bool]| if whole[i] { None } else { bases[i] };
    for start in 0..n {
        // Follow the chain down to a finished entry, a whole object or a
        // cycle.
        let mut cur = start;
        while state[cur] != DONE {
            if state[cur] == ON_PATH {
                // Back at an entry of this path: a cycle. Store it whole.
                whole[cur] = true;
                depth[cur] = 0;
                state[cur] = DONE;
                break;
            }
            state[cur] = ON_PATH;
            path.push(cur);
            match base_of(cur, &whole) {
                Some(b) => cur = b,
                None => break,
            }
        }
        // Unwind from the base end, so every base is finished first.
        while let Some(i) = path.pop() {
            if state[i] == DONE {
                continue;
            }
            depth[i] = match base_of(i, &whole) {
                Some(b) => depth[b] + 1,
                None => 0,
            };
            if depth[i] > max_depth {
                whole[i] = true;
                depth[i] = 0;
            }
            state[i] = DONE;
        }
    }
    (whole, depth)
}

enum BaseRef {
    Distance(u64),
    Id(ObjectId),
}

/// The compressed bytes of the entry at `offset`, after checking them.
/// Returns the header and the data range.
fn checked_entry(pack: &Pack, offset: u64) -> Option<(&PackData, crate::EntryHeader, u64)> {
    let data = pack.data().ok()?;
    let header = data.entry_header(offset).ok()?;
    let end = pack.entry_end(offset).ok()?;
    let pos = pack.index().lookup(&pack.id_at_offset(offset)?)?;
    match pack.index().crc32_at(pos) {
        Some(expected) => {
            let raw = data.bytes().get(offset as usize..end as usize)?;
            if crc32fast::hash(raw) != expected {
                return None;
            }
        }
        None => {
            let (_, used) = data.inflate(&header).ok()?;
            if header.data_offset + used != end {
                return None;
            }
        }
    }
    Some((data, header, end))
}

fn copy_whole(pack: &Pack, offset: u64, buf: &mut Vec<u8>) -> Option<bool> {
    let (data, header, end) = checked_entry(pack, offset)?;
    let EntryKind::Base(kind) = header.kind else {
        return None;
    };
    push_entry_header(buf, kind.pack_type(), header.size);
    buf.extend_from_slice(
        data.bytes()
            .get(header.data_offset as usize..end as usize)?,
    );
    Some(true)
}

fn push_delta_header(buf: &mut Vec<u8>, base: BaseRef, size: u64) {
    match base {
        BaseRef::Distance(d) => {
            push_entry_header(buf, TYPE_OFS_DELTA, size);
            push_base_distance(buf, d);
        }
        BaseRef::Id(id) => {
            push_entry_header(buf, TYPE_REF_DELTA, size);
            buf.extend_from_slice(id.as_bytes());
        }
    }
}

fn copy_delta(pack: &Pack, offset: u64, base: BaseRef, buf: &mut Vec<u8>) -> Option<bool> {
    let (data, header, end) = checked_entry(pack, offset)?;
    if let EntryKind::Base(_) = header.kind {
        return None;
    }
    push_delta_header(buf, base, header.size);
    buf.extend_from_slice(
        data.bytes()
            .get(header.data_offset as usize..end as usize)?,
    );
    Some(true)
}

/// Objects are checked against their id before they are written anew or
/// used for a delta, so that corrupt data is not passed on.
pub(crate) fn check_id(id: &ObjectId, kind: ObjectKind, data: &[u8]) -> Result<(), WriteError> {
    match rgit_hash::hash_object(id.kind(), kind.name(), data) {
        Ok(actual) if actual == *id => Ok(()),
        _ => Err(WriteError::Mismatch(*id)),
    }
}

fn compress_whole<S: ObjectSource + ?Sized>(
    source: &S,
    id: &ObjectId,
    level: Compression,
    buf: &mut Vec<u8>,
) -> Result<(), WriteError> {
    let object = match source.read(id) {
        Ok(Some(o)) => o,
        Ok(None) => return Err(WriteError::Missing(*id)),
        Err(e) => return Err(WriteError::read(*id, e)),
    };
    check_id(id, object.kind, &object.data)?;
    push_entry_header(buf, object.kind.pack_type(), object.data.len() as u64);
    let mut z = ZlibEncoder::new(std::mem::take(buf), level);
    z.write_all(&object.data)?;
    *buf = z.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::limit_chains;

    #[test]
    fn chains_without_problems_stay() {
        let bases = [None, Some(0), Some(1), Some(0)];
        assert_eq!(limit_chains(&bases, 50).0, [false; 4]);
    }

    #[test]
    fn cycles_are_broken_once() {
        // 0 -> 1 -> 2 -> 0, and 3 -> 1.
        let bases = [Some(1), Some(2), Some(0), Some(1)];
        let whole = limit_chains(&bases, 50).0;
        assert_eq!(whole.iter().filter(|w| **w).count(), 1);
        // Self-reference.
        assert_eq!(limit_chains(&[Some(0)], 50).0, [true]);
    }

    #[test]
    fn long_chains_are_cut() {
        // 0 <- 1 <- 2 <- ... <- 9
        let bases: Vec<Option<usize>> = (0..10usize).map(|i| i.checked_sub(1)).collect();
        let whole = limit_chains(&bases, 3).0;
        // Depths 0 1 2 3 | 0 1 2 3 | 0 1
        let expect: Vec<bool> = (0..10).map(|i| i == 4 || i == 8).collect();
        assert_eq!(whole, expect);
        // Starting in the middle of the chain gives the same result.
        let mut rev = bases.clone();
        rev.reverse();
        let rev: Vec<Option<usize>> = rev.into_iter().map(|b| b.map(|b| 9 - b)).collect();
        let whole = limit_chains(&rev, 3).0;
        let expect: Vec<bool> = (0..10).map(|i| 9 - i == 4 || 9 - i == 8).collect();
        assert_eq!(whole, expect);
    }
}
