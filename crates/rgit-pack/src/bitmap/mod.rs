// SPDX-License-Identifier: MIT

//! Reachability bitmaps (`.bitmap` files) for a pack or a multi-pack index.
//!
//! Format: `Documentation/technical/bitmap-format.adoc`. Bit `n` of every
//! bitmap stands for the `n`-th object in pack order (offset order) of the
//! pack, or in pseudo-pack order of the MIDX (see
//! `Documentation/gitformat-pack.adoc`, "multi-pack-index reverse
//! indexes"). The file has four type bitmaps (commits, trees, blobs, tags)
//! and one bitmap per selected commit, each possibly XORed with an earlier
//! one; optionally a name-hash cache and a lookup table.
//!
//! [`BitmapIndex`] reads such a file; [`write`] builds one.

pub mod ewah;
pub mod messages;
pub mod write;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rgit_hash::{HashKind, ObjectId};

pub use ewah::Bitmap;

use crate::midx::MultiPackIndex;
use crate::mmap::FileData;
use crate::{ObjectKind, Pack};
use messages as m;

/// `BITM`.
pub const SIGNATURE: &[u8; 4] = b"BITM";
/// Every bitmap file has this flag: all objects reachable from a bitmapped
/// object are in the pack (or MIDX).
pub const FLAG_FULL_DAG: u16 = 0x1;
/// A name-hash cache follows the bitmaps.
pub const FLAG_HASH_CACHE: u16 = 0x4;
/// A commit lookup table precedes the name-hash cache.
pub const FLAG_LOOKUP_TABLE: u16 = 0x10;
/// Pseudo-merge bitmaps precede the lookup table.
pub const FLAG_PSEUDO_MERGES: u16 = 0x20;

/// The largest XOR offset of an entry.
pub const MAX_XOR_OFFSET: usize = 160;

/// A problem with a bitmap file; `Display` is git's message (printed as
/// `error: <message>`). Commands then go on without the bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitmapError(pub Vec<String>);

impl BitmapError {
    fn one(msg: impl Into<String>) -> BitmapError {
        BitmapError(vec![msg.into()])
    }
}

impl std::fmt::Display for BitmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("\nerror: "))
    }
}

impl std::error::Error for BitmapError {}

/// Where the objects of the bitmap live and in which order.
enum Order {
    /// One pack; `by_bit[n]` is the index position of the `n`-th object in
    /// pack order.
    Pack {
        pack: Arc<Pack>,
        /// (offset, index position) sorted by offset.
        by_bit: Vec<(u64, u32)>,
        /// Index position -> bit.
        bit_of: Vec<u32>,
    },
    /// A MIDX; `by_bit[n]` is the MIDX position of the `n`-th object in
    /// pseudo-pack order.
    Midx {
        midx: Box<MultiPackIndex>,
        /// The packs by pack-int-id (`None`: not loaded).
        packs: Vec<Option<Arc<Pack>>>,
        by_bit: Vec<u32>,
        bit_of: Vec<u32>,
        /// Where the order came from: `"midx"` (the `RIDX` chunk) or
        /// `"rev"` (the `.rev` file).
        source: &'static str,
    },
}

/// One commit bitmap entry in the file.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Index (or MIDX) position of the commit.
    pos: u32,
    /// Entry this one is XORed with (absolute index into `entries`).
    xor_with: Option<usize>,
    flags: u8,
    /// Offset of the EWAH data in the file.
    ewah: usize,
}

/// A loaded `.bitmap` file.
pub struct BitmapIndex {
    data: FileData,
    path: PathBuf,
    kind: HashKind,
    flags: u16,
    order: Order,
    types: [Bitmap; 4],
    entries: Vec<Entry>,
    by_commit: HashMap<ObjectId, usize>,
    resolved: Vec<OnceLock<Result<Arc<Bitmap>, BitmapError>>>,
    hash_cache: Option<usize>,
}

impl std::fmt::Debug for BitmapIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitmapIndex")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// Path of the `.bitmap` of the pack whose index is `idx`.
pub fn pack_bitmap_path(idx: &Path) -> PathBuf {
    idx.with_extension("bitmap")
}

/// Path of the `.bitmap` of the MIDX with `checksum` in `pack_dir`.
pub fn midx_bitmap_path(pack_dir: &Path, checksum: &ObjectId) -> PathBuf {
    pack_dir.join(format!("multi-pack-index-{}.bitmap", checksum.to_hex()))
}

/// Path of the `.rev` of the MIDX with `checksum` in `pack_dir`.
pub fn midx_rev_path(pack_dir: &Path, checksum: &ObjectId) -> PathBuf {
    pack_dir.join(format!("multi-pack-index-{}.rev", checksum.to_hex()))
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

/// Decode the EWAH bitmap at `at`, with git's messages for truncation.
fn read_ewah(data: &[u8], at: usize, end: usize) -> Result<(Bitmap, usize), String> {
    let avail = end.saturating_sub(at);
    let b = &data[at.min(end)..end];
    if avail < 4 {
        return Err(m::ewah_corrupt(m::EWAH_EOF_BIT_SIZE));
    }
    if avail < 8 {
        return Err(m::ewah_corrupt(m::EWAH_EOF_LENGTH));
    }
    let words = be32(b, 4) as usize;
    let need = words.saturating_mul(8);
    if avail - 8 < need {
        return Err(m::ewah_corrupt(&m::ewah_eof_data(need - (avail - 8))));
    }
    if avail - 8 - need < 4 {
        return Err(m::ewah_corrupt(m::EWAH_EOF_RLW));
    }
    ewah::decode(b)
        .ok_or_else(|| m::ewah_corrupt(m::EWAH_EOF_RLW))
        .map(|(bm, used)| (bm, at + used))
}

impl BitmapIndex {
    /// Open the bitmap of `pack`. `Ok(None)`: the pack has none.
    pub fn open_pack(pack: Arc<Pack>) -> Result<Option<BitmapIndex>, BitmapError> {
        let path = pack_bitmap_path(pack.index().path());
        let Ok(data) = FileData::open(&path) else {
            return Ok(None);
        };
        let idx = pack.index();
        let mut by_bit: Vec<(u64, u32)> = Vec::with_capacity(idx.len() as usize);
        for p in 0..idx.len() {
            let off = idx
                .offset_at(p)
                .map_err(|e| BitmapError::one(e.to_string()))?;
            by_bit.push((off, p));
        }
        by_bit.sort_unstable();
        let mut bit_of = vec![0u32; by_bit.len()];
        for (b, &(_, p)) in by_bit.iter().enumerate() {
            bit_of[p as usize] = b as u32;
        }
        let kind = idx.hash_kind();
        let order = Order::Pack {
            pack,
            by_bit,
            bit_of,
        };
        BitmapIndex::parse(data, path, kind, order).map(Some)
    }

    /// Open the bitmap of the MIDX `midx` of `pack_dir`; `packs` are the
    /// opened packs of the directory. The pseudo-pack order comes from the
    /// MIDX's `RIDX` chunk (unless `read_ridx` is false) or its `.rev`
    /// file. `Ok(None)`: there is no bitmap.
    pub fn open_midx(
        pack_dir: &Path,
        midx: MultiPackIndex,
        packs: &[Arc<Pack>],
        read_ridx: bool,
    ) -> Result<Option<BitmapIndex>, BitmapError> {
        let sum = midx.checksum();
        let path = midx_bitmap_path(pack_dir, &sum);
        let Ok(data) = FileData::open(&path) else {
            return Ok(None);
        };
        let (rev, source) = match midx.reverse_index().filter(|_| read_ridx) {
            Some(r) => (r, "midx"),
            None => match read_midx_rev(&midx_rev_path(pack_dir, &sum), &midx) {
                Some(r) => (r, "rev"),
                None => return Err(BitmapError::one(m::MIDX_NO_REVINDEX)),
            },
        };
        let n = midx.len() as usize;
        if rev.len() != n || rev.iter().any(|&p| p as usize >= n) {
            return Err(BitmapError::one(m::MIDX_NO_REVINDEX));
        }
        let mut bit_of = vec![0u32; n];
        for (b, &p) in rev.iter().enumerate() {
            bit_of[p as usize] = b as u32;
        }
        let pack_list: Vec<Option<Arc<Pack>>> = midx
            .pack_names()
            .iter()
            .map(|name| {
                let idx = pack_dir.join(name);
                packs.iter().find(|p| p.index().path() == idx).cloned()
            })
            .collect();
        let kind = midx.hash_kind();
        let order = Order::Midx {
            midx: Box::new(midx),
            packs: pack_list,
            by_bit: rev,
            bit_of,
            source,
        };
        BitmapIndex::parse(data, path, kind, order).map(Some)
    }

    fn parse(
        data: FileData,
        path: PathBuf,
        kind: HashKind,
        order: Order,
    ) -> Result<BitmapIndex, BitmapError> {
        let h = kind.byte_len();
        let b = data.as_slice();
        let header = 12 + h;
        if b.len() < header + h {
            return Err(BitmapError::one(m::TOO_SMALL));
        }
        if &b[..4] != SIGNATURE {
            return Err(BitmapError::one(m::WRONG_HEADER));
        }
        let version = be16(b, 4);
        if version != 1 {
            return Err(BitmapError::one(m::unsupported_version(version)));
        }
        let flags = be16(b, 6);
        if flags & FLAG_FULL_DAG == 0 {
            return Err(BitmapError::one(m::NO_FULL_DAG));
        }
        let nentries = be32(b, 8) as usize;
        let num_objects = match &order {
            Order::Pack { by_bit, .. } => by_bit.len(),
            Order::Midx { by_bit, .. } => by_bit.len(),
        };
        let mut end = b.len() - h;
        let mut hash_cache = None;
        if flags & FLAG_HASH_CACHE != 0 {
            let need = num_objects * 4;
            if end < header + need {
                return Err(BitmapError::one(m::SHORT_HASH_CACHE));
            }
            end -= need;
            hash_cache = Some(end);
        }
        let mut table = None;
        if flags & FLAG_LOOKUP_TABLE != 0 {
            let need = nentries * 16;
            if end < header + need {
                return Err(BitmapError::one(m::SHORT_LOOKUP_TABLE));
            }
            end -= need;
            table = Some(end);
        }
        let failed = |e: String| BitmapError(vec![e, m::FAILED_TO_LOAD.into()]);
        let mut at = header;
        let mut types: [Bitmap; 4] = Default::default();
        for t in &mut types {
            let (bm, next) = read_ewah(b, at, end).map_err(failed)?;
            *t = bm;
            at = next;
        }
        let mut index = BitmapIndex {
            data: FileData::Owned(Vec::new()),
            path,
            kind,
            flags,
            order,
            types,
            entries: Vec::with_capacity(nentries),
            by_commit: HashMap::with_capacity(nentries),
            resolved: Vec::new(),
            hash_cache,
        };
        match table {
            None => {
                for i in 0..nentries {
                    if at + 6 > end {
                        return Err(failed(m::ewah_corrupt(m::EWAH_EOF_BIT_SIZE)));
                    }
                    let pos = be32(b, at);
                    let xor = usize::from(b[at + 4]);
                    let fl = b[at + 5];
                    let ewah_at = at + 6;
                    let (_, next) = read_ewah(b, ewah_at, end).map_err(failed)?;
                    at = next;
                    if pos as usize >= num_objects {
                        return Err(BitmapError::one(m::commit_out_of_range(pos)));
                    }
                    if xor > MAX_XOR_OFFSET || xor > i {
                        return Err(BitmapError::one(m::CORRUPTED_PACK_INDEX));
                    }
                    let id = index.id_at_index_pos(pos);
                    if index.by_commit.insert(id, i).is_some() {
                        return Err(BitmapError::one(m::duplicate_entry(&id.to_hex())));
                    }
                    index.entries.push(Entry {
                        pos,
                        xor_with: (xor > 0).then(|| i - xor),
                        flags: fl,
                        ewah: ewah_at,
                    });
                }
            }
            Some(t) => {
                // Triplets sorted by commit position: position, offset of
                // the entry, row of the XOR base.
                let mut rows: Vec<(u32, usize, u32)> = Vec::with_capacity(nentries);
                for i in 0..nentries {
                    let p = t + i * 16;
                    rows.push((be32(b, p), be64(b, p + 4) as usize, be32(b, p + 12)));
                }
                for (i, &(pos, off, xor_row)) in rows.iter().enumerate() {
                    if pos as usize >= num_objects || off + 6 > end {
                        return Err(BitmapError::one(m::LOOKUP_OUT_OF_INDEX));
                    }
                    if xor_row != u32::MAX && xor_row as usize >= nentries {
                        return Err(BitmapError::one(m::LOOKUP_OUT_OF_INDEX));
                    }
                    let id = index.id_at_index_pos(pos);
                    if index.by_commit.insert(id, i).is_some() {
                        return Err(BitmapError::one(m::duplicate_entry(&id.to_hex())));
                    }
                    index.entries.push(Entry {
                        pos,
                        xor_with: (xor_row != u32::MAX).then_some(xor_row as usize),
                        flags: b[off + 5],
                        ewah: off + 6,
                    });
                }
                // XOR chains must end.
                for i in 0..nentries {
                    let mut cur = i;
                    let mut steps = 0;
                    while let Some(x) = index.entries[cur].xor_with {
                        cur = x;
                        steps += 1;
                        if steps > nentries {
                            return Err(BitmapError::one(m::CORRUPTED_PACK_INDEX));
                        }
                    }
                }
            }
        }
        index.resolved = (0..index.entries.len()).map(|_| OnceLock::new()).collect();
        index.data = data;
        Ok(index)
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hash algorithm.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// The header flags.
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// Whether this is a MIDX bitmap.
    pub fn is_midx(&self) -> bool {
        matches!(self.order, Order::Midx { .. })
    }

    /// Number of objects (bits).
    pub fn num_objects(&self) -> usize {
        match &self.order {
            Order::Pack { by_bit, .. } => by_bit.len(),
            Order::Midx { by_bit, .. } => by_bit.len(),
        }
    }

    fn id_at_index_pos(&self, pos: u32) -> ObjectId {
        match &self.order {
            Order::Pack { pack, .. } => pack.index().id_at(pos),
            Order::Midx { midx, .. } => midx.id_at(pos),
        }
    }

    /// Id of the object at bit `bit` (`bit < num_objects()`).
    pub fn id_at(&self, bit: usize) -> ObjectId {
        match &self.order {
            Order::Pack { pack, by_bit, .. } => pack.index().id_at(by_bit[bit].1),
            Order::Midx { midx, by_bit, .. } => midx.id_at(by_bit[bit]),
        }
    }

    /// Bit of `id`, if the pack (MIDX) has it.
    pub fn position(&self, id: &ObjectId) -> Option<usize> {
        match &self.order {
            Order::Pack { pack, bit_of, .. } => pack
                .index()
                .lookup(id)
                .map(|p| bit_of[p as usize] as usize),
            Order::Midx { midx, bit_of, .. } => midx.lookup(id).map(|p| bit_of[p as usize] as usize),
        }
    }

    /// Index (or MIDX) position of the object at `bit`.
    pub fn index_pos(&self, bit: usize) -> u32 {
        match &self.order {
            Order::Pack { by_bit, .. } => by_bit[bit].1,
            Order::Midx { by_bit, .. } => by_bit[bit],
        }
    }

    /// The type bitmap of `kind` (commits, trees, blobs, tags).
    pub fn type_bitmap(&self, kind: ObjectKind) -> &Bitmap {
        &self.types[type_slot(kind)]
    }

    /// The type of the object at `bit`, by the type bitmaps.
    pub fn kind_at(&self, bit: usize) -> Option<ObjectKind> {
        [
            ObjectKind::Commit,
            ObjectKind::Tree,
            ObjectKind::Blob,
            ObjectKind::Tag,
        ]
        .into_iter()
        .find(|k| self.types[type_slot(*k)].get(bit))
    }

    /// The commits that have a bitmap, in file order.
    pub fn commits(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.entries.iter().map(|e| self.id_at_index_pos(e.pos))
    }

    /// Number of commit bitmaps.
    pub fn num_entries(&self) -> usize {
        self.entries.len()
    }

    /// Whether `commit` has a bitmap.
    pub fn has_commit(&self, commit: &ObjectId) -> bool {
        self.by_commit.contains_key(commit)
    }

    /// The flags byte of the entry of `commit`.
    pub fn entry_flags(&self, commit: &ObjectId) -> Option<u8> {
        self.by_commit.get(commit).map(|&i| self.entries[i].flags)
    }

    /// The bitmap of the objects reachable from `commit`, if it has one.
    pub fn commit_bitmap(&self, commit: &ObjectId) -> Result<Option<Arc<Bitmap>>, BitmapError> {
        match self.by_commit.get(commit) {
            None => Ok(None),
            Some(&i) => self.entry_bitmap(i).map(Some),
        }
    }

    fn entry_bitmap(&self, i: usize) -> Result<Arc<Bitmap>, BitmapError> {
        if let Some(r) = self.resolved[i].get() {
            return r.clone();
        }
        // Resolve the XOR chain from its base up, iteratively.
        let mut chain = vec![i];
        let mut cur = i;
        while let Some(x) = self.entries[cur].xor_with {
            if self.resolved[x].get().is_some() {
                break;
            }
            chain.push(x);
            cur = x;
        }
        for &e in chain.iter().rev() {
            if self.resolved[e].get().is_some() {
                continue;
            }
            let r = self.decode_entry(e);
            let _ = self.resolved[e].set(r);
        }
        self.resolved[i].get().expect("resolved").clone()
    }

    fn decode_entry(&self, e: usize) -> Result<Arc<Bitmap>, BitmapError> {
        let b = self.data.as_slice();
        let (mut bm, _) = read_ewah(b, self.entries[e].ewah, b.len() - self.kind.byte_len())
            .map_err(BitmapError::one)?;
        if let Some(x) = self.entries[e].xor_with {
            let base = self.resolved[x].get().expect("base resolved").clone()?;
            bm.xor(&base);
        }
        Ok(Arc::new(bm))
    }

    /// The name hash of the object at `bit` from the hash cache.
    pub fn name_hash(&self, bit: usize) -> Option<u32> {
        let at = self.hash_cache?;
        let pos = self.index_pos(bit) as usize;
        Some(be32(self.data.as_slice(), at + 4 * pos))
    }

    /// Whether the file has a name-hash cache.
    pub fn has_hash_cache(&self) -> bool {
        self.hash_cache.is_some()
    }

    /// The pack holding the object at `bit` and its offset there.
    pub fn location(&self, bit: usize) -> Option<(Arc<Pack>, u64)> {
        match &self.order {
            Order::Pack { pack, by_bit, .. } => Some((pack.clone(), by_bit[bit].0)),
            Order::Midx {
                midx, packs, by_bit, ..
            } => {
                let pos = by_bit[bit];
                let p = midx.pack_id(pos).ok()?;
                let off = midx.offset(pos).ok()?;
                Some((packs.get(p as usize)?.clone()?, off))
            }
        }
    }

    /// The bytes the object at `bit` takes in its pack.
    pub fn disk_size(&self, bit: usize) -> u64 {
        match &self.order {
            Order::Pack { pack, by_bit, .. } => {
                let start = by_bit[bit].0;
                let end = match by_bit.get(bit + 1) {
                    Some(n) => n.0,
                    None => pack.data().map(|d| d.entries_end()).unwrap_or(start),
                };
                end - start
            }
            Order::Midx { .. } => self
                .location(bit)
                .and_then(|(p, off)| p.entry_end(off).ok().map(|e| e - off))
                .unwrap_or(0),
        }
    }

    /// The pack of a single-pack bitmap.
    pub fn pack(&self) -> Option<&Arc<Pack>> {
        match &self.order {
            Order::Pack { pack, .. } => Some(pack),
            Order::Midx { .. } => None,
        }
    }

    /// Where the pseudo-pack order of a MIDX bitmap came from (`"midx"`
    /// or `"rev"`).
    pub fn revindex_source(&self) -> Option<&'static str> {
        match &self.order {
            Order::Midx { source, .. } => Some(source),
            Order::Pack { .. } => None,
        }
    }

    /// The MIDX of a multi-pack bitmap.
    pub fn midx(&self) -> Option<&MultiPackIndex> {
        match &self.order {
            Order::Midx { midx, .. } => Some(midx),
            Order::Pack { .. } => None,
        }
    }
}

/// Slot of `kind` among the type bitmaps.
pub fn type_slot(kind: ObjectKind) -> usize {
    match kind {
        ObjectKind::Commit => 0,
        ObjectKind::Tree => 1,
        ObjectKind::Blob => 2,
        ObjectKind::Tag => 3,
    }
}

/// Read a MIDX `.rev` file (the same layout as a pack `.rev`, with the
/// MIDX checksum in the trailer). `None` if missing or not matching.
pub fn read_midx_rev(path: &Path, midx: &MultiPackIndex) -> Option<Vec<u32>> {
    let data = std::fs::read(path).ok()?;
    let h = midx.hash_kind().byte_len();
    let n = midx.len() as usize;
    if data.len() != 12 + 4 * n + 2 * h || &data[..4] != b"RIDX" || be32(&data, 4) != 1 {
        return None;
    }
    if data[12 + 4 * n..12 + 4 * n + h] != *midx.checksum().as_bytes() {
        return None;
    }
    Some((0..n).map(|i| be32(&data, 12 + 4 * i)).collect())
}

/// The name hash git stores in the hash cache (`bitmap-format.adoc`,
/// appendix B): over the whole path, whitespace skipped.
pub fn name_hash_v1(path: &[u8]) -> u32 {
    let mut hash: u32 = 0;
    for &c in path {
        if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            continue;
        }
        hash = (hash >> 2).wrapping_add(u32::from(c) << 24);
    }
    hash
}
