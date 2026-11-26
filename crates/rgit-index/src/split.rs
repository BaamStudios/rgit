// SPDX-License-Identifier: MIT

//! Split index (`link` extension, gitformat-index(5) "Split index"): most
//! entries live in a shared index file `sharedindex.<hash>`; the index
//! itself holds the entries that replace shared ones, the ones it adds,
//! and bitmaps of the replaced and the deleted shared entries.

use rgit_hash::ObjectId;

use crate::entry::Entry;
use crate::{Error, Index, WriteOptions, ewah};

/// Whether and how the index is written split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SplitMode {
    /// Split if the index was read split (`core.splitIndex` unset).
    #[default]
    Keep,
    /// Write a whole index (`core.splitIndex=false`, `--no-split-index`).
    Off,
    /// Write split, with a new shared index if the index is not split yet
    /// (`core.splitIndex=true`).
    On,
    /// Write split with a new shared index holding all entries
    /// (`--split-index`).
    Renew,
}

/// Split index settings for writing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitOptions {
    pub mode: SplitMode,
    /// `splitIndex.maxPercentChange`: a new shared index is written when
    /// the split index would hold more than this percentage of all
    /// entries (0: always, 100: never).
    pub max_percent_change: u32,
    /// `splitIndex.sharedIndexExpire`: other shared index files last
    /// modified before this time (seconds since the epoch) are removed when
    /// a new one is written; 0 never removes, `u64::MAX` removes all.
    pub expire_before: u64,
}

impl Default for SplitOptions {
    fn default() -> SplitOptions {
        SplitOptions {
            mode: SplitMode::Keep,
            max_percent_change: 20,
            expire_before: 0,
        }
    }
}

/// The shared index an index was read with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Split {
    /// Checksum (and file name) of the shared index; null when there is
    /// none.
    pub base_id: ObjectId,
    /// Its entries.
    pub base: Vec<Entry>,
}

/// The raw `link` extension.
pub(crate) struct Link {
    pub base_id: ObjectId,
    pub delete: Vec<usize>,
    pub replace: Vec<usize>,
}

impl Link {
    pub fn parse(data: &[u8], kind: rgit_hash::HashKind) -> Option<Link> {
        let n = kind.byte_len();
        let base_id = ObjectId::from_bytes(kind, data.get(..n)?).ok()?;
        let mut pos = n;
        if pos == data.len() {
            // No bitmaps: nothing deleted or replaced.
            return Some(Link {
                base_id,
                delete: Vec::new(),
                replace: Vec::new(),
            });
        }
        let (delete, used) = ewah::decode(&data[pos..])?;
        pos += used;
        let (replace, used) = ewah::decode(data.get(pos..)?)?;
        pos += used;
        (pos == data.len()).then_some(Link {
            base_id,
            delete,
            replace,
        })
    }
}

/// Merges the entries of a split index (`own`, as read) with its shared
/// index (`base`) into the full list.
pub(crate) fn merge(own: Vec<Entry>, base: Vec<Entry>, link: &Link) -> Result<Vec<Entry>, Error> {
    let corrupt = || Error::Corrupt("split index");
    if link.replace.len() > own.len() {
        return Err(corrupt());
    }
    let mut own = own.into_iter();
    let mut replaced: Vec<Option<Entry>> = vec![None; base.len()];
    for &pos in &link.replace {
        let mut e = own.next().ok_or_else(corrupt)?;
        let b = base.get(pos).ok_or_else(corrupt)?;
        if e.path.is_empty() {
            e.path.clone_from(&b.path);
        }
        replaced[pos] = Some(e);
    }
    let mut deleted = vec![false; base.len()];
    for &pos in &link.delete {
        *deleted.get_mut(pos).ok_or_else(corrupt)? = true;
    }
    let mut kept: Vec<Entry> = Vec::with_capacity(base.len());
    for (i, b) in base.into_iter().enumerate() {
        if deleted[i] {
            continue;
        }
        let mut e = replaced[i].take().unwrap_or(b);
        e.base = u32::try_from(i + 1).map_err(|_| corrupt())?;
        kept.push(e);
    }
    let added: Vec<Entry> = own.collect();
    if added.is_empty() {
        return Ok(kept);
    }
    // Both lists are sorted; merge them.
    let mut out = Vec::with_capacity(kept.len() + added.len());
    let (mut a, mut b) = (kept.into_iter().peekable(), added.into_iter().peekable());
    loop {
        let take_a = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) => x.cmp_key(&y.path, y.stage).is_lt(),
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        out.push(if take_a { a.next() } else { b.next() }.expect("peeked"));
    }
    Ok(out)
}

/// How an index is written.
pub(crate) enum Plan {
    /// A whole index.
    Whole,
    /// Split, against the shared index it was read with.
    Reuse,
    /// Split, with a new shared index of all entries.
    NewShared,
}

pub(crate) fn plan(index: &Index, opts: &WriteOptions) -> Plan {
    let split = index.split.as_ref();
    match opts.split.mode {
        SplitMode::Off => Plan::Whole,
        SplitMode::Renew => Plan::NewShared,
        SplitMode::Keep if split.is_none() => Plan::Whole,
        SplitMode::On if split.is_none() => Plan::NewShared,
        SplitMode::Keep | SplitMode::On => {
            let split = split.expect("checked");
            if split.base_id.is_null() || opts.split.max_percent_change == 0 {
                return Plan::NewShared;
            }
            let changed = index
                .entries
                .iter()
                .filter(|e| e.base == 0 || split.base.get(e.base as usize - 1) != Some(*e))
                .count();
            if changed * 100 > opts.split.max_percent_change as usize * index.entries.len() {
                Plan::NewShared
            } else {
                Plan::Reuse
            }
        }
    }
}

/// The entries and `link` data of a split index against `split`.
pub(crate) fn own_entries<'a>(
    index: &'a Index,
    split: &Split,
) -> (Vec<(&'a Entry, bool)>, Vec<u8>) {
    let mut used = vec![false; split.base.len()];
    let mut replaced: Vec<(usize, &Entry)> = Vec::new();
    let mut added: Vec<&Entry> = Vec::new();
    for e in &index.entries {
        let pos = e.base as usize;
        match pos.checked_sub(1).filter(|&p| p < used.len() && !used[p]) {
            Some(p) => {
                used[p] = true;
                if split.base[p] != *e {
                    replaced.push((p, e));
                }
            }
            None => added.push(e),
        }
    }
    replaced.sort_by_key(|r| r.0);
    let deleted: Vec<usize> = (0..used.len()).filter(|&p| !used[p]).collect();
    let mut link = split.base_id.as_bytes().to_vec();
    ewah::encode(&deleted, &mut link);
    let positions: Vec<usize> = replaced.iter().map(|r| r.0).collect();
    ewah::encode(&positions, &mut link);
    // Replacements are written without their names.
    let mut own: Vec<(&Entry, bool)> = replaced.into_iter().map(|(_, e)| (e, true)).collect();
    own.extend(added.into_iter().map(|e| (e, false)));
    (own, link)
}
