// SPDX-License-Identifier: MIT

//! Writing a pack from a list of objects, reusing packed data.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_hash::{HashKind, ObjectId};

use super::entry::{TYPE_OFS_DELTA, TYPE_REF_DELTA, push_base_distance, push_entry_header};
use super::output::PackStream;
use super::{ListedObject, ObjectSource, WriteError};
use crate::data::EntryKind;
use crate::{Pack, PackData};

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
    /// object whole. At most [`MAX_DEPTH`].
    pub max_depth: u32,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            ofs_delta: false,
            reuse_delta: true,
            reuse_object: true,
            compression: None,
            max_depth: 50,
        }
    }
}

/// One entry as written, with the values a `.idx` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrittenEntry {
    /// The object.
    pub id: ObjectId,
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
}

/// Write the objects in `objects` (in this order, duplicates dropped) as a
/// self-contained version 2 pack to `out`.
///
/// Objects are read from `source`. When an object is found in one of
/// `packs` (searched in order) its compressed data is copied instead of
/// compressing it again; a stored delta is copied when its base is also in
/// the output, otherwise the object is stored whole. Delta bases are
/// always written before their deltas. Copied data is checked against the
/// CRC32 in the pack index (or inflated, for version 1 indexes) and
/// written anew if it does not match.
pub fn write_pack<W: Write, S: ObjectSource + ?Sized>(
    out: W,
    hash: HashKind,
    objects: &[ListedObject],
    source: &S,
    packs: &[Arc<Pack>],
    opts: &WriteOptions,
) -> Result<(PackSummary, W), WriteError> {
    let mut index: HashMap<ObjectId, usize> = HashMap::with_capacity(objects.len());
    let mut ids: Vec<ObjectId> = Vec::with_capacity(objects.len());
    for o in objects {
        if let std::collections::hash_map::Entry::Vacant(e) = index.entry(o.id) {
            e.insert(ids.len());
            ids.push(o.id);
        }
    }
    let count = u32::try_from(ids.len()).map_err(|_| WriteError::TooManyObjects)?;

    let mut reps = choose_representations(&ids, &index, packs, opts);
    let bases: Vec<Option<usize>> = reps
        .iter()
        .map(|r| match r {
            Rep::PackedDelta { base, .. } => Some(*base),
            _ => None,
        })
        .collect();
    for (rep, whole) in reps
        .iter_mut()
        .zip(limit_chains(&bases, opts.max_depth.min(MAX_DEPTH)))
    {
        if whole {
            *rep = Rep::Whole;
        }
    }

    let level = match opts.compression {
        Some(l) => Compression::new(l.min(9)),
        None => Compression::default(),
    };
    let mut stream = PackStream::new(out, hash, count)?;
    let mut offsets: Vec<Option<u64>> = vec![None; ids.len()];
    let mut summary = PackSummary {
        checksum: hash.null(),
        entries: Vec::with_capacity(ids.len()),
        deltas: 0,
        reused: 0,
        reused_deltas: 0,
    };
    let mut todo: Vec<usize> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    for first in 0..ids.len() {
        todo.push(first);
        while let Some(&i) = todo.last() {
            if offsets[i].is_some() {
                todo.pop();
                continue;
            }
            if let Rep::PackedDelta { base, .. } = reps[i] {
                if offsets[base].is_none() {
                    todo.push(base);
                    continue;
                }
            }
            todo.pop();
            let offset = stream.begin_entry();
            buf.clear();
            let written = match &reps[i] {
                Rep::Whole => false,
                Rep::PackedWhole { pack, offset: from } => {
                    copy_whole(pack, *from, &mut buf).unwrap_or(false)
                }
                Rep::PackedDelta {
                    pack,
                    offset: from,
                    base,
                } => {
                    let base_ref = if opts.ofs_delta {
                        BaseRef::Distance(offset - offsets[*base].expect("base written"))
                    } else {
                        BaseRef::Id(ids[*base])
                    };
                    copy_delta(pack, *from, base_ref, &mut buf).unwrap_or(false)
                }
            };
            if written {
                summary.reused += 1;
                if matches!(reps[i], Rep::PackedDelta { .. }) {
                    summary.deltas += 1;
                    summary.reused_deltas += 1;
                }
            } else {
                buf.clear();
                compress_whole(source, &ids[i], level, &mut buf)?;
            }
            stream.put(&buf)?;
            let crc32 = stream.end_entry();
            offsets[i] = Some(offset);
            summary.entries.push(WrittenEntry {
                id: ids[i],
                offset,
                crc32,
            });
        }
    }
    let (checksum, out) = stream.finish()?;
    summary.checksum = checksum;
    Ok((summary, out))
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
                    return Rep::PackedWhole {
                        pack: pack.clone(),
                        offset,
                    };
                }
                if !opts.reuse_delta {
                    return Rep::Whole;
                }
                return match pack.delta_base(offset) {
                    Ok(Some(base)) => match index.get(&base) {
                        Some(&b) => Rep::PackedDelta {
                            pack: pack.clone(),
                            offset,
                            base: b,
                        },
                        None => Rep::Whole,
                    },
                    _ => Rep::Whole,
                };
            }
            Rep::Whole
        })
        .collect()
}

/// Find the deltas to store whole instead: one per cycle (possible when
/// the chosen copies come from different packs), and those that would end
/// up deeper than `max_depth` in a chain. `bases[i]` is the base of entry
/// `i` if it is a delta.
fn limit_chains(bases: &[Option<usize>], max_depth: u32) -> Vec<bool> {
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
    whole
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

fn copy_delta(pack: &Pack, offset: u64, base: BaseRef, buf: &mut Vec<u8>) -> Option<bool> {
    let (data, header, end) = checked_entry(pack, offset)?;
    if let EntryKind::Base(_) = header.kind {
        return None;
    }
    match base {
        BaseRef::Distance(d) => {
            push_entry_header(buf, TYPE_OFS_DELTA, header.size);
            push_base_distance(buf, d);
        }
        BaseRef::Id(id) => {
            push_entry_header(buf, TYPE_REF_DELTA, header.size);
            buf.extend_from_slice(id.as_bytes());
        }
    }
    buf.extend_from_slice(
        data.bytes()
            .get(header.data_offset as usize..end as usize)?,
    );
    Some(true)
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
        assert_eq!(limit_chains(&bases, 50), [false; 4]);
    }

    #[test]
    fn cycles_are_broken_once() {
        // 0 -> 1 -> 2 -> 0, and 3 -> 1.
        let bases = [Some(1), Some(2), Some(0), Some(1)];
        let whole = limit_chains(&bases, 50);
        assert_eq!(whole.iter().filter(|w| **w).count(), 1);
        // Self-reference.
        assert_eq!(limit_chains(&[Some(0)], 50), [true]);
    }

    #[test]
    fn long_chains_are_cut() {
        // 0 <- 1 <- 2 <- ... <- 9
        let bases: Vec<Option<usize>> = (0..10usize).map(|i| i.checked_sub(1)).collect();
        let whole = limit_chains(&bases, 3);
        // Depths 0 1 2 3 | 0 1 2 3 | 0 1
        let expect: Vec<bool> = (0..10).map(|i| i == 4 || i == 8).collect();
        assert_eq!(whole, expect);
        // Starting in the middle of the chain gives the same result.
        let mut rev = bases.clone();
        rev.reverse();
        let rev: Vec<Option<usize>> = rev.into_iter().map(|b| b.map(|b| 9 - b)).collect();
        let whole = limit_chains(&rev, 3);
        let expect: Vec<bool> = (0..10).map(|i| 9 - i == 4 || 9 - i == 8).collect();
        assert_eq!(whole, expect);
    }
}
