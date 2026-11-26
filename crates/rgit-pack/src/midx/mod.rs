// SPDX-License-Identifier: MIT

//! Multi-pack indexes (`objects/pack/multi-pack-index` and the incremental
//! chain in `objects/pack/multi-pack-index.d/`).
//!
//! Format: `Documentation/gitformat-pack.adoc` ("multi-pack-index (MIDX)
//! files"), `Documentation/gitformat-chunk.adoc` and
//! `Documentation/technical/multi-pack-index.adoc`.
//!
//! - [`MultiPackIndex`]: one MIDX file (a stand-alone MIDX or one layer of
//!   a chain), validated when opened like git does.
//! - [`MidxChain`]: what a pack directory holds: the stand-alone file or
//!   the layers listed in `multi-pack-index-chain`, base first.
//! - [`write`]: writing MIDX files byte-identically to git.
//! - [`verify`]: `git multi-pack-index verify`.
//!
//! Problems found when opening are reported the way git reports them: some
//! make the MIDX unusable but not fatal (the caller prints them as
//! `error: ...` and goes on without it), others are fatal ([`OpenError`]).

pub mod messages;
pub mod verify;
pub mod write;

use std::ops::Range;
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId, Prefix};

use crate::mmap::FileData;
use messages as m;

/// `MIDX`.
pub const SIGNATURE: u32 = 0x4d49_4458;
/// Header length.
pub const HEADER_LEN: usize = 12;
/// Name of the stand-alone file in `objects/pack`.
pub const FILE_NAME: &str = "multi-pack-index";
/// Directory of the incremental chain in `objects/pack`.
pub const CHAIN_DIR: &str = "multi-pack-index.d";
/// The chain file inside [`CHAIN_DIR`].
pub const CHAIN_FILE: &str = "multi-pack-index-chain";

pub(crate) const ID_PNAM: u32 = u32::from_be_bytes(*b"PNAM");
pub(crate) const ID_OIDF: u32 = u32::from_be_bytes(*b"OIDF");
pub(crate) const ID_OIDL: u32 = u32::from_be_bytes(*b"OIDL");
pub(crate) const ID_OOFF: u32 = u32::from_be_bytes(*b"OOFF");
pub(crate) const ID_LOFF: u32 = u32::from_be_bytes(*b"LOFF");
pub(crate) const ID_RIDX: u32 = u32::from_be_bytes(*b"RIDX");
pub(crate) const ID_BTMP: u32 = u32::from_be_bytes(*b"BTMP");

/// The flag in an `OOFF` offset that makes it an index into `LOFF`.
pub(crate) const LARGE_FLAG: u32 = 0x8000_0000;

/// A problem that makes git die when it loads the MIDX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenError(pub String);

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OpenError {}

/// The hash version byte of the MIDX header for `kind`.
pub fn hash_version(kind: HashKind) -> u8 {
    match kind {
        HashKind::Sha1 => 1,
        HashKind::Sha256 => 2,
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(a)
}

/// One chunk of the table of contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Chunk id (`PNAM`, ...), big-endian.
    pub id: u32,
    /// Start in the file.
    pub start: usize,
    /// Length.
    pub len: usize,
}

/// Read the chunk table of a chunk-based file (gitformat-chunk(5)) that
/// starts at `toc` and has `count` entries. Problems are git's messages;
/// they make the file unusable.
pub fn read_chunk_table(
    b: &[u8],
    toc: usize,
    count: usize,
    hash_len: usize,
) -> Result<Vec<Chunk>, String> {
    let limit = b.len().saturating_sub(hash_len) as u64;
    let mut out: Vec<Chunk> = Vec::with_capacity(count);
    if toc + (count + 1) * 12 > b.len() {
        return Err(m::TERMINATING_EARLY.into());
    }
    for i in 0..count {
        let at = toc + i * 12;
        let id = be32(b, at);
        let start = be64(b, at + 4);
        let next = be64(b, at + 16);
        if id == 0 {
            return Err(m::TERMINATING_EARLY.into());
        }
        if start % 4 != 0 {
            return Err(m::chunk_unaligned(id));
        }
        if next < start || next > limit {
            return Err(m::improper_offsets(start, next));
        }
        if out.iter().any(|c| c.id == id) {
            return Err(m::duplicate_chunk(id));
        }
        out.push(Chunk {
            id,
            start: start as usize,
            len: (next - start) as usize,
        });
    }
    let last = be32(b, toc + count * 12);
    if last != 0 {
        return Err(m::final_chunk_nonzero(last));
    }
    Ok(out)
}

/// One MIDX file, validated.
pub struct MultiPackIndex {
    data: FileData,
    path: PathBuf,
    kind: HashKind,
    version: u8,
    chunks: Vec<Chunk>,
    pack_names: Vec<String>,
    num_objects: u32,
    fanout: usize,
    oids: usize,
    offsets: usize,
    large: Option<Chunk>,
    ridx: Option<Chunk>,
    btmp: Option<Chunk>,
}

impl std::fmt::Debug for MultiPackIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiPackIndex")
            .field("path", &self.path)
            .field("packs", &self.pack_names.len())
            .field("objects", &self.num_objects)
            .finish()
    }
}

impl MultiPackIndex {
    /// Open the MIDX at `path`.
    ///
    /// - `Ok(None)`: there is no such file, or it is unusable; the reasons
    ///   were appended to `errors` (git prints them as `error: ...` and goes
    ///   on without the MIDX).
    /// - `Err`: a problem git dies on.
    pub fn open(
        path: impl AsRef<Path>,
        kind: HashKind,
        errors: &mut Vec<String>,
    ) -> Result<Option<MultiPackIndex>, OpenError> {
        let path = path.as_ref();
        let data = match FileData::open(path) {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        MultiPackIndex::parse(data, path.to_path_buf(), kind, errors)
    }

    /// Parse a MIDX held in memory (`path` is used in messages only).
    pub fn from_bytes(
        bytes: Vec<u8>,
        path: impl Into<PathBuf>,
        kind: HashKind,
        errors: &mut Vec<String>,
    ) -> Result<Option<MultiPackIndex>, OpenError> {
        MultiPackIndex::parse(FileData::Owned(bytes), path.into(), kind, errors)
    }

    fn parse(
        data: FileData,
        path: PathBuf,
        kind: HashKind,
        errors: &mut Vec<String>,
    ) -> Result<Option<MultiPackIndex>, OpenError> {
        let b = data.as_slice();
        let hash_len = kind.byte_len();
        if b.len() < HEADER_LEN + 12 + hash_len {
            errors.push(m::too_small(&path.display().to_string()));
            return Ok(None);
        }
        let sig = be32(b, 0);
        if sig != SIGNATURE {
            return Err(OpenError(m::signature_mismatch(sig, SIGNATURE)));
        }
        let version = b[4];
        if version != 1 && version != 2 {
            return Err(OpenError(m::version_unknown(version)));
        }
        let hv = b[5];
        if hv != hash_version(kind) {
            errors.push(m::hash_version_mismatch(hv, hash_version(kind)));
            return Ok(None);
        }
        let count = usize::from(b[6]);
        let num_packs = be32(b, 8);
        let chunks = match read_chunk_table(b, HEADER_LEN, count, hash_len) {
            Ok(c) => c,
            Err(e) => {
                errors.push(e);
                return Ok(None);
            }
        };
        let find = |id: u32| chunks.iter().find(|c| c.id == id).copied();
        let Some(pnam) = find(ID_PNAM) else {
            return Err(OpenError(m::PNAM_MISSING.into()));
        };
        let Some(oidf) = find(ID_OIDF) else {
            return Err(OpenError(m::OIDF_MISSING.into()));
        };
        if oidf.len != 256 * 4 {
            errors.push(m::OIDF_SIZE.into());
            return Err(OpenError(m::OIDF_MISSING.into()));
        }
        for i in 0..255 {
            let a = be32(b, oidf.start + 4 * i);
            let c = be32(b, oidf.start + 4 * (i + 1));
            if a > c {
                errors.push(m::fanout_order(i, a, c));
                return Err(OpenError(m::OIDF_MISSING.into()));
            }
        }
        let num_objects = be32(b, oidf.start + 4 * 255);
        let Some(oidl) = find(ID_OIDL) else {
            return Err(OpenError(m::OIDL_MISSING.into()));
        };
        if oidl.len as u64 != u64::from(num_objects) * hash_len as u64 {
            errors.push(m::OIDL_SIZE.into());
            return Err(OpenError(m::OIDL_MISSING.into()));
        }
        let Some(ooff) = find(ID_OOFF) else {
            return Err(OpenError(m::OOFF_MISSING.into()));
        };
        if ooff.len as u64 != u64::from(num_objects) * 8 {
            errors.push(m::OOFF_SIZE.into());
            return Err(OpenError(m::OOFF_MISSING.into()));
        }
        let large = find(ID_LOFF);
        let ridx = find(ID_RIDX);
        let btmp = find(ID_BTMP);

        // Pack names: NUL-terminated, version 1 in strictly increasing order.
        let mut pack_names: Vec<String> = Vec::with_capacity(num_packs as usize);
        let names = &b[pnam.start..pnam.start + pnam.len];
        let mut pos = 0;
        for _ in 0..num_packs {
            let Some(end) = names[pos.min(names.len())..].iter().position(|&c| c == 0) else {
                return Err(OpenError(m::PNAM_SHORT.into()));
            };
            let name = String::from_utf8_lossy(&names[pos..pos + end]).into_owned();
            if version == 1
                && let Some(prev) = pack_names.last()
                && *prev >= name
            {
                return Err(OpenError(m::pack_names_order(prev, &name)));
            }
            pack_names.push(name);
            pos += end + 1;
        }
        Ok(Some(MultiPackIndex {
            path,
            kind,
            version,
            pack_names,
            num_objects,
            fanout: oidf.start,
            oids: oidl.start,
            offsets: ooff.start,
            large,
            ridx,
            btmp,
            chunks,
            data,
        }))
    }

    fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hash algorithm.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// Format version (1 or 2).
    pub fn version(&self) -> u8 {
        self.version
    }

    /// The chunks in file order.
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// Whether the MIDX has a chunk with `id` (e.g. `b"RIDX"`).
    pub fn has_chunk(&self, id: &[u8; 4]) -> bool {
        let id = u32::from_be_bytes(*id);
        self.chunks.iter().any(|c| c.id == id)
    }

    /// Whether the reverse index chunk is present (and of the right size).
    pub fn has_reverse_index(&self) -> bool {
        self.ridx
            .is_some_and(|c| c.len as u64 == u64::from(self.num_objects) * 4)
    }

    /// Whether the bitmapped-packs chunk is present.
    pub fn has_bitmapped_packs(&self) -> bool {
        self.btmp.is_some()
    }

    /// Names of the packs (their `.idx` file names), by pack-int-id.
    pub fn pack_names(&self) -> &[String] {
        &self.pack_names
    }

    /// Number of packs.
    pub fn num_packs(&self) -> u32 {
        self.pack_names.len() as u32
    }

    /// Number of objects.
    pub fn len(&self) -> u32 {
        self.num_objects
    }

    /// Whether the MIDX lists no objects.
    pub fn is_empty(&self) -> bool {
        self.num_objects == 0
    }

    /// The trailing checksum (it names the file's `.bitmap`/`.rev` and the
    /// layers of a chain).
    pub fn checksum(&self) -> ObjectId {
        let b = self.bytes();
        let h = self.kind.byte_len();
        ObjectId::from_bytes(self.kind, &b[b.len() - h..]).expect("hash length")
    }

    /// Whether the trailing checksum matches the content.
    pub fn checksum_ok(&self) -> bool {
        let b = self.bytes();
        let h = self.kind.byte_len();
        rgit_hash::hash(self.kind, &b[..b.len() - h])
            .map(|id| id.as_bytes() == &b[b.len() - h..])
            .unwrap_or(false)
    }

    /// Fan-out entry `i` (number of ids whose first byte is at most `i`).
    pub fn fanout(&self, i: u8) -> u32 {
        be32(self.bytes(), self.fanout + 4 * usize::from(i))
    }

    fn fanout_range(&self, first: u8) -> Range<u32> {
        let end = self.fanout(first);
        let start = if first == 0 {
            0
        } else {
            self.fanout(first - 1)
        };
        start..end.max(start)
    }

    fn id_bytes(&self, pos: u32) -> &[u8] {
        let h = self.kind.byte_len();
        let at = self.oids + pos as usize * h;
        &self.bytes()[at..at + h]
    }

    /// Id at sorted position `pos` (`pos < len()`).
    pub fn id_at(&self, pos: u32) -> ObjectId {
        ObjectId::from_bytes(self.kind, self.id_bytes(pos)).expect("hash length")
    }

    /// Position of `id`, if listed.
    pub fn lookup(&self, id: &ObjectId) -> Option<u32> {
        if id.kind() != self.kind {
            return None;
        }
        let want = id.as_bytes();
        let Range { mut start, mut end } = self.fanout_range(want[0]);
        end = end.min(self.num_objects);
        while start < end {
            let mid = start + (end - start) / 2;
            match self.id_bytes(mid).cmp(want) {
                std::cmp::Ordering::Less => start = mid + 1,
                std::cmp::Ordering::Greater => end = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    /// Positions of all ids starting with `prefix`.
    pub fn lookup_prefix(&self, prefix: &Prefix) -> Range<u32> {
        if prefix.kind() != self.kind {
            return 0..0;
        }
        let first = prefix.as_id().as_bytes()[0];
        let range = if prefix.hex_len() >= 2 {
            self.fanout_range(first)
        } else {
            let lo = self.fanout_range(first).start;
            let hi = self.fanout_range(first | 0x0f).end;
            lo..hi
        };
        let range = range.start.min(self.num_objects)..range.end.min(self.num_objects);
        let lower = partition(range.clone(), |p| {
            prefix.cmp_oid(&self.id_at(p)) == std::cmp::Ordering::Greater
        });
        let upper = partition(lower..range.end, |p| {
            prefix.cmp_oid(&self.id_at(p)) != std::cmp::Ordering::Less
        });
        lower..upper
    }

    /// All ids in sorted order.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = ObjectId> + '_ {
        (0..self.num_objects).map(|p| self.id_at(p))
    }

    /// The raw pack-int-id of position `pos` (not checked against the
    /// number of packs).
    pub fn raw_pack_id(&self, pos: u32) -> u32 {
        be32(self.bytes(), self.offsets + pos as usize * 8)
    }

    /// Pack-int-id of the object at `pos`.
    pub fn pack_id(&self, pos: u32) -> Result<u32, String> {
        let id = self.raw_pack_id(pos);
        if id >= self.num_packs() {
            return Err(m::bad_pack_int_id(id, self.num_packs()));
        }
        Ok(id)
    }

    /// Offset in its pack of the object at `pos`.
    pub fn offset(&self, pos: u32) -> Result<u64, String> {
        let b = self.bytes();
        let off = be32(b, self.offsets + pos as usize * 8 + 4);
        match self.large {
            Some(l) if off & LARGE_FLAG != 0 => {
                let i = (off & !LARGE_FLAG) as usize;
                if (i + 1) * 8 > l.len {
                    return Err(m::LARGE_OFFSET_OOB.into());
                }
                Ok(be64(b, l.start + i * 8))
            }
            _ => Ok(u64::from(off)),
        }
    }

    /// Pack-int-id and offset of `id`, if listed.
    pub fn find(&self, id: &ObjectId) -> Option<Result<(u32, u64), String>> {
        let pos = self.lookup(id)?;
        Some(self.pack_id(pos).and_then(|p| Ok((p, self.offset(pos)?))))
    }

    /// The `.idx` name of pack `id`.
    pub fn pack_name(&self, id: u32) -> Option<&str> {
        self.pack_names.get(id as usize).map(String::as_str)
    }

    /// Whether the MIDX lists the pack whose `.idx` or `.pack` file name is
    /// `name`.
    pub fn contains_pack(&self, name: &str) -> bool {
        self.pack_id_by_name(name).is_some()
    }

    /// The pack-int-id of the pack whose `.idx` or `.pack` name is `name`.
    pub fn pack_id_by_name(&self, name: &str) -> Option<u32> {
        let stem = name
            .strip_suffix(".idx")
            .or_else(|| name.strip_suffix(".pack"))?;
        self.pack_names
            .iter()
            .position(|n| n.strip_suffix(".idx").unwrap_or(n) == stem)
            .map(|p| p as u32)
    }

    /// Reverse index entries (pseudo-pack order), if present.
    pub fn reverse_index(&self) -> Option<Vec<u32>> {
        let c = self.ridx.filter(|_| self.has_reverse_index())?;
        let b = self.bytes();
        Some(
            (0..self.num_objects as usize)
                .map(|i| be32(b, c.start + 4 * i))
                .collect(),
        )
    }

    /// The raw bytes of the file (for verification).
    pub fn raw(&self) -> &[u8] {
        self.bytes()
    }
}

/// First position in `range` for which `pred` is false (`pred` must be true
/// for a prefix of the range and false after it).
fn partition(range: Range<u32>, pred: impl Fn(u32) -> bool) -> u32 {
    let Range { mut start, mut end } = range;
    while start < end {
        let mid = start + (end - start) / 2;
        if pred(mid) {
            start = mid + 1;
        } else {
            end = mid;
        }
    }
    start
}

/// The MIDX of a pack directory: a stand-alone file, or the layers of an
/// incremental chain (base first). When both exist, git reads the
/// stand-alone file; so does this.
#[derive(Debug)]
pub struct MidxChain {
    layers: Vec<MultiPackIndex>,
    incremental: bool,
}

impl MidxChain {
    /// Load the MIDX of `pack_dir`. `Ok(None)` when there is none or it is
    /// unusable (reasons in `errors`).
    pub fn open(
        pack_dir: &Path,
        kind: HashKind,
        errors: &mut Vec<String>,
    ) -> Result<Option<MidxChain>, OpenError> {
        let single = pack_dir.join(FILE_NAME);
        if single.exists() {
            return Ok(
                MultiPackIndex::open(&single, kind, errors)?.map(|m| MidxChain {
                    layers: vec![m],
                    incremental: false,
                }),
            );
        }
        let Some(hashes) = read_chain_file(pack_dir, kind) else {
            return Ok(None);
        };
        let mut layers = Vec::with_capacity(hashes.len());
        for h in hashes {
            let p = layer_path(pack_dir, &h);
            match MultiPackIndex::open(&p, kind, errors)? {
                Some(m) if m.checksum() == h => layers.push(m),
                _ => break,
            }
        }
        if layers.is_empty() {
            return Ok(None);
        }
        Ok(Some(MidxChain {
            layers,
            incremental: true,
        }))
    }

    /// A chain over already opened layers (base first).
    pub fn from_layers(layers: Vec<MultiPackIndex>, incremental: bool) -> MidxChain {
        MidxChain {
            layers,
            incremental,
        }
    }

    /// The layers, base first.
    pub fn layers(&self) -> &[MultiPackIndex] {
        &self.layers
    }

    /// Take the layers.
    pub fn into_layers(self) -> Vec<MultiPackIndex> {
        self.layers
    }

    /// Whether this is an incremental chain (`multi-pack-index.d`).
    pub fn is_incremental(&self) -> bool {
        self.incremental
    }

    /// The newest layer.
    pub fn tip(&self) -> &MultiPackIndex {
        self.layers.last().expect("non-empty chain")
    }

    /// Total number of objects in all layers.
    pub fn num_objects(&self) -> u64 {
        self.layers.iter().map(|l| u64::from(l.len())).sum()
    }

    /// Whether any layer lists the pack named `name` (`.idx` or `.pack`).
    pub fn contains_pack(&self, name: &str) -> bool {
        self.layers.iter().any(|l| l.contains_pack(name))
    }

    /// The layer and position of `id`, newest layer last searched (layers
    /// do not overlap).
    pub fn lookup(&self, id: &ObjectId) -> Option<(&MultiPackIndex, u32)> {
        self.layers
            .iter()
            .find_map(|l| l.lookup(id).map(|p| (l, p)))
    }
}

/// Path of the chain layer with checksum `hash`.
pub fn layer_path(pack_dir: &Path, hash: &ObjectId) -> PathBuf {
    pack_dir
        .join(CHAIN_DIR)
        .join(format!("multi-pack-index-{}.midx", hash.to_hex()))
}

/// The checksums listed in `multi-pack-index-chain`, base first; `None`
/// when there is no chain file.
pub fn read_chain_file(pack_dir: &Path, kind: HashKind) -> Option<Vec<ObjectId>> {
    let text = std::fs::read_to_string(pack_dir.join(CHAIN_DIR).join(CHAIN_FILE)).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        match ObjectId::from_hex(kind, line.trim().as_bytes()) {
            Ok(id) => out.push(id),
            Err(_) => break,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::write::{Entry, PackInput, build, select_entries};
    use super::*;

    fn id(b: u8, n: u8) -> ObjectId {
        let mut v = [0u8; 20];
        v[0] = b;
        v[19] = n;
        ObjectId::from_bytes(HashKind::Sha1, &v).unwrap()
    }

    fn entry(id: ObjectId, pack: u32, offset: u64) -> Entry {
        Entry { id, pack, offset }
    }

    fn open(b: Vec<u8>) -> (Result<Option<MultiPackIndex>, OpenError>, Vec<String>) {
        let mut errors = Vec::new();
        let r = MultiPackIndex::from_bytes(b, "m", HashKind::Sha1, &mut errors);
        (r, errors)
    }

    #[test]
    fn roundtrip() {
        let entries = vec![
            entry(id(0x01, 1), 1, 12),
            entry(id(0x01, 2), 0, 0x8000_0000),
            entry(id(0xfe, 0), 1, 99),
        ];
        let bytes = build(HashKind::Sha1, 1, &["a.idx", "b.idx"], &entries);
        let (m, errors) = open(bytes);
        let m = m.unwrap().unwrap();
        assert!(errors.is_empty());
        assert!(m.checksum_ok());
        assert!(!m.has_chunk(b"LOFF"));
        assert_eq!(m.len(), 3);
        assert_eq!(m.pack_names(), ["a.idx", "b.idx"]);
        assert_eq!(m.find(&id(0x01, 2)), Some(Ok((0, 0x8000_0000))));
        assert_eq!(m.find(&id(0xfe, 0)), Some(Ok((1, 99))));
        assert_eq!(m.find(&id(0xfe, 1)), None);
        assert_eq!(m.pack_id_by_name("b.pack"), Some(1));
    }

    #[test]
    fn large_offsets() {
        let entries = vec![
            entry(id(0x01, 1), 0, 0x1_0000_0000),
            entry(id(0x02, 1), 0, 0x8000_0001),
            entry(id(0x03, 1), 0, 7),
        ];
        let bytes = build(HashKind::Sha1, 1, &["a.idx"], &entries);
        let m = open(bytes).0.unwrap().unwrap();
        assert!(m.has_chunk(b"LOFF"));
        for e in &entries {
            assert_eq!(m.find(&e.id), Some(Ok((0, e.offset))));
        }
    }

    #[test]
    fn broken_headers() {
        let good = build(HashKind::Sha1, 1, &["a.idx"], &[entry(id(1, 1), 0, 12)]);
        let with = |at: usize, byte: u8| {
            let mut b = good.clone();
            b[at] = byte;
            open(b)
        };
        assert!(with(0, 0).0.unwrap_err().0.contains("signature 0x00494458"));
        assert_eq!(
            with(4, 3).0.unwrap_err().0,
            "multi-pack-index version 3 not recognized"
        );
        let (r, e) = with(5, 2);
        assert!(r.unwrap().is_none());
        assert_eq!(
            e,
            ["multi-pack-index hash version 2 does not match version 1"]
        );
        assert_eq!(with(6, 1).1, ["final chunk has non-zero id 4f494446"]);
        assert_eq!(
            with(6, 7).1,
            ["terminating chunk id appears earlier than expected"]
        );
        assert_eq!(
            with(12, 1).0.unwrap_err().0,
            "multi-pack-index required pack-name chunk missing or corrupted"
        );
    }

    /// A minimal v2 `.idx` listing `ids` at offsets 12, 13, ...
    fn idx(dir: &Path, name: &str, ids: &[ObjectId]) -> crate::PackIndex {
        let mut b = vec![0xff, b't', b'O', b'c', 0, 0, 0, 2];
        let mut sorted = ids.to_vec();
        sorted.sort();
        for i in 0..=255u8 {
            let n = sorted.iter().filter(|x| x.as_bytes()[0] <= i).count() as u32;
            b.extend_from_slice(&n.to_be_bytes());
        }
        for x in &sorted {
            b.extend_from_slice(x.as_bytes());
        }
        for _ in &sorted {
            b.extend_from_slice(&0u32.to_be_bytes());
        }
        for i in 0..sorted.len() {
            b.extend_from_slice(&(12 + i as u32).to_be_bytes());
        }
        b.extend_from_slice(&[0u8; 40]);
        let p = dir.join(name);
        std::fs::write(&p, &b).unwrap();
        crate::PackIndex::open(&p, HashKind::Sha1).unwrap()
    }

    #[test]
    fn selection_prefers_preferred_then_newest_then_lowest_id() {
        let dir = tempfile::tempdir().unwrap();
        let x = id(5, 5);
        let a = idx(dir.path(), "a.idx", &[x]);
        let b = idx(dir.path(), "b.idx", &[x]);
        let pick = |ma: i128, mb: i128, preferred: Option<usize>| {
            let packs = [
                PackInput {
                    name: "a.idx".into(),
                    index: &a,
                    mtime: ma,
                },
                PackInput {
                    name: "b.idx".into(),
                    index: &b,
                    mtime: mb,
                },
            ];
            select_entries(&packs, preferred, &|_| false).unwrap()[0].pack
        };
        assert_eq!(pick(1, 1, None), 0);
        assert_eq!(pick(1, 2, None), 1);
        assert_eq!(pick(2, 1, None), 0);
        assert_eq!(pick(2, 1, Some(1)), 1);
    }
}
