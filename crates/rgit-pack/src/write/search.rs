// SPDX-License-Identifier: MIT

//! Delta search (`Documentation/technical/pack-heuristics.adoc`,
//! `--window` and `--depth` in `Documentation/git-pack-objects.adoc`).
//!
//! Objects are sorted by type, then by the hash of the path they were
//! found under, then by size, largest first. Each object is compared with
//! the objects shortly before it in that order (the window), and stored
//! as a delta against the one that gives the smallest delta. Large objects
//! thus become bases for smaller ones (removing data is cheaper to encode
//! than adding it), and versions of the same file meet in the window.
//!
//! The sorted list is cut into one piece per thread; pieces are searched
//! independently, so the result depends on the thread count.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_hash::ObjectId;

use super::encode::{DeltaIndex, create_delta};
use super::pack::check_id;
use super::{ObjectSource, WriteError};
use crate::{DeltaBaseCache, ObjectKind, Pack};

/// Delta base cache of each search thread.
const THREAD_CACHE: usize = 32 << 20;

/// Reads objects for one search thread: from the packs with a cache of
/// its own (the source's cache is shared and locked), else from the
/// source.
struct Reader<'a, S: ?Sized> {
    source: &'a S,
    packs: &'a [Arc<Pack>],
    cache: DeltaBaseCache,
}

impl<S: ObjectSource + ?Sized> Reader<'_, S> {
    fn read(&mut self, id: &ObjectId) -> Result<Vec<u8>, WriteError> {
        let o = self.read_unchecked(id)?;
        check_id(id, o.kind, &o.data)?;
        Ok(o.data)
    }

    fn read_unchecked(&mut self, id: &ObjectId) -> Result<crate::Object, WriteError> {
        for pack in self.packs {
            match pack.read(id, &mut self.cache) {
                Ok(Some(o)) => return Ok(o),
                Ok(None) => continue,
                // Corrupt here, or a base elsewhere: let the source try.
                Err(_) => break,
            }
        }
        match self.source.read(id) {
            Ok(Some(o)) => Ok(o),
            Ok(None) => Err(WriteError::Missing(*id)),
            Err(e) => Err(WriteError::read(*id, e)),
        }
    }
}

/// Delta search settings (see [`WriteOptions`](super::WriteOptions)).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SearchOptions {
    pub window: usize,
    pub window_memory: u64,
    pub max_depth: u32,
    pub threads: usize,
    pub big_file_threshold: u64,
    pub level: Compression,
    /// Length of an object id: a `REF_DELTA` costs that much more.
    pub id_len: usize,
}

/// One object taking part in the search.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Candidate {
    /// Position in the writer's object list.
    pub pos: usize,
    pub id: ObjectId,
    pub kind: ObjectKind,
    pub name_hash: u32,
    pub size: u64,
    /// The object keeps a delta copied from an existing pack: it is only a
    /// base here, at this depth.
    pub fixed_depth: Option<u32>,
    /// The longest chain of reused deltas that has this object as its base.
    pub below: u32,
    /// Only a base (thin pack bases): never stored in the pack.
    pub base_only: bool,
}

/// A delta found by the search.
#[derive(Debug, Clone)]
pub(crate) struct Found {
    /// Position of the base in the writer's object list.
    pub base: usize,
    /// Length of the delta data before compression.
    pub raw_len: u64,
    /// The zlib-compressed delta.
    pub compressed: Vec<u8>,
}

struct Slot {
    cand: Candidate,
    data: Vec<u8>,
    index: Option<DeltaIndex>,
    depth: u32,
}

impl Slot {
    fn memory(&self) -> u64 {
        (self.data.len() + self.index.as_ref().map_or(0, DeltaIndex::memory)) as u64
    }
}

/// Smallest object worth a delta.
const MIN_SIZE: u64 = 32;

/// Sort `cands` and search deltas. Returns `(position, delta)` pairs.
pub(crate) fn search<S: ObjectSource + Sync + ?Sized>(
    source: &S,
    packs: &[Arc<Pack>],
    mut cands: Vec<Candidate>,
    opts: &SearchOptions,
) -> Result<Vec<(usize, Found)>, WriteError> {
    if opts.window == 0 || opts.max_depth == 0 {
        return Ok(Vec::new());
    }
    cands.retain(|c| c.size <= opts.big_file_threshold);
    // Thin bases come first in their group: they cost nothing to keep and
    // are the only bases the newest objects can have.
    cands.sort_by(|a, b| {
        (
            a.kind.pack_type(),
            a.name_hash,
            !a.base_only,
            std::cmp::Reverse(a.size),
            a.pos,
        )
            .cmp(&(
                b.kind.pack_type(),
                b.name_hash,
                !b.base_only,
                std::cmp::Reverse(b.size),
                b.pos,
            ))
    });
    let pieces = split(&cands, opts.threads.max(1), opts.window);
    let reader = || Reader {
        source,
        packs,
        cache: DeltaBaseCache::new(THREAD_CACHE),
    };
    if pieces.len() <= 1 {
        return search_piece(reader(), &cands, opts);
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = pieces
            .iter()
            .map(|r| {
                let piece = &cands[r.clone()];
                let reader = reader();
                s.spawn(move || search_piece(reader, piece, opts))
            })
            .collect();
        let mut out = Vec::new();
        for h in handles {
            out.extend(h.join().expect("delta search thread")?);
        }
        Ok(out)
    })
}

/// Cut the sorted list into about `threads` pieces, at changes of type or
/// path hash where possible.
fn split(cands: &[Candidate], threads: usize, window: usize) -> Vec<std::ops::Range<usize>> {
    let n = cands.len();
    let pieces = threads.min(n / (window * 4).max(64)).max(1);
    let mut out = Vec::with_capacity(pieces);
    let mut start = 0;
    for p in 1..pieces {
        let mut cut = (n * p / pieces).max(start);
        let limit = (cut + n / pieces / 2).min(n);
        while cut > start
            && cut < limit
            && (cands[cut].kind, cands[cut].name_hash)
                == (cands[cut - 1].kind, cands[cut - 1].name_hash)
        {
            cut += 1;
        }
        if cut > start && cut < n {
            out.push(start..cut);
            start = cut;
        }
    }
    out.push(start..n);
    out
}

fn search_piece<S: ObjectSource + ?Sized>(
    mut reader: Reader<'_, S>,
    piece: &[Candidate],
    opts: &SearchOptions,
) -> Result<Vec<(usize, Found)>, WriteError> {
    let mut out = Vec::new();
    let mut window: VecDeque<Slot> = VecDeque::with_capacity(opts.window + 1);
    for cand in piece {
        let data = match reader.read(&cand.id) {
            Ok(d) => d,
            // Objects that are only bases (thin bases, and objects that keep
            // a reused delta) are simply not used when they cannot be read.
            Err(_) if cand.base_only || cand.fixed_depth.is_some() => continue,
            Err(e) => return Err(e),
        };
        let mut depth = cand.fixed_depth.unwrap_or(0);
        if cand.fixed_depth.is_none() && !cand.base_only && data.len() as u64 >= MIN_SIZE {
            if let Some((slot, delta)) = best_delta(&mut window, cand, &data, opts) {
                depth = window[slot].depth + 1;
                let base = window[slot].cand.pos;
                let mut z = ZlibEncoder::new(Vec::with_capacity(delta.len() / 2 + 16), opts.level);
                z.write_all(&delta)?;
                out.push((
                    cand.pos,
                    Found {
                        base,
                        raw_len: delta.len() as u64,
                        compressed: z.finish()?,
                    },
                ));
            }
        }
        // An object at the end of a chain can be no one's base: it does not
        // take a place in the window.
        if depth >= opts.max_depth {
            continue;
        }
        let slot = Slot {
            cand: *cand,
            data,
            index: None,
            depth,
        };
        window.push_back(slot);
        while window.len() > opts.window {
            window.pop_front();
        }
        if opts.window_memory > 0 {
            let mut memory: u64 = window.iter().map(Slot::memory).sum();
            while memory > opts.window_memory && window.len() > 1 {
                let old = window.pop_front().expect("not empty");
                memory -= old.memory();
            }
        }
    }
    Ok(out)
}

/// The window slot giving the smallest delta for `target`, with the delta.
fn best_delta(
    window: &mut VecDeque<Slot>,
    cand: &Candidate,
    target: &[u8],
    opts: &SearchOptions,
) -> Option<(usize, Vec<u8>)> {
    let size = target.len();
    // A delta must save at least half of the object (less the cost of
    // naming the base) to be worth the time it takes to apply.
    let limit = (size / 2).saturating_sub(opts.id_len);
    let mut best: Option<(usize, Vec<u8>)> = None;
    // Deltas are compared by their length weighted with the depth of the
    // base: `len * max_depth / (max_depth - base_depth)`. A deep base must
    // give a proportionally smaller delta, because reading the result
    // costs every delta below it and the chain cannot grow much further;
    // shallow bases keep chains short and branching.
    let mut best_cost = usize::MAX;
    let m = opts.max_depth as usize;
    for slot in (0..window.len()).rev() {
        let w = &mut window[slot];
        if w.cand.kind != cand.kind || w.depth + 1 + cand.below > opts.max_depth {
            continue;
        }
        let depth_left = (opts.max_depth - w.depth) as usize;
        let mut max = limit * depth_left / m;
        if best_cost != usize::MAX {
            max = max.min((best_cost * depth_left / m).saturating_sub(1));
        }
        let base_len = w.data.len();
        // The delta contains at least the size difference as literals.
        if max == 0 || size.saturating_sub(base_len) >= max || base_len < size / 32 {
            continue;
        }
        let index = w.index.get_or_insert_with(|| DeltaIndex::new(&w.data));
        if let Some(d) = create_delta(&w.data, index, target, max) {
            best_cost = d.len() * m / depth_left;
            best = Some((slot, d));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn cand(pos: usize, name_hash: u32) -> Candidate {
        Candidate {
            pos,
            id: HashKind::Sha1.null(),
            kind: ObjectKind::Blob,
            name_hash,
            size: 100,
            fixed_depth: None,
            below: 0,
            base_only: false,
        }
    }

    #[test]
    fn split_keeps_groups_together() {
        let cands: Vec<Candidate> = (0..1000).map(|i| cand(i, (i / 300) as u32)).collect();
        let pieces = split(&cands, 4, 10);
        assert_eq!(pieces.first().unwrap().start, 0);
        assert_eq!(pieces.last().unwrap().end, 1000);
        for w in pieces.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
        assert!(pieces.len() > 1);
        // Small inputs stay in one piece.
        assert_eq!(split(&cands[..50], 8, 10).len(), 1);
    }
}
