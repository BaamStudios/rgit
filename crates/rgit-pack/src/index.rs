// SPDX-License-Identifier: MIT

//! Pack index (`.idx`) files, versions 1 and 2.
//!
//! Layout (Documentation/gitformat-pack.adoc):
//!
//! - v1: 256 fan-out entries, then `n` entries of (4-byte offset, id), then
//!   the pack checksum and the index checksum.
//! - v2: magic `\377tOc`, version 2, 256 fan-out entries, `n` sorted ids,
//!   `n` CRC32 values, `n` 4-byte offsets (msb set: index into the 64-bit
//!   table), the 64-bit offset table, the pack checksum and the index
//!   checksum.
//!
//! All integers are big-endian.

use std::ops::Range;
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, Hasher, ObjectId, Prefix};

use crate::Error;
use crate::mmap::FileData;

const V2_MAGIC: [u8; 4] = [0xff, b't', b'O', b'c'];
const FANOUT_LEN: usize = 256 * 4;

/// A parsed, validated pack index.
///
/// Opening checks the header, the fan-out table and the file size against
/// the object count; lookups after that cannot read out of bounds. The
/// trailing checksums are only checked by [`PackIndex::verify_checksum`].
pub struct PackIndex {
    data: FileData,
    path: PathBuf,
    kind: HashKind,
    version: u32,
    num: u32,
    /// Start of the fan-out table.
    fanout: usize,
    /// Start of the id table (v2) or of the entry table (v1).
    ids: usize,
    /// Distance between consecutive ids.
    id_stride: usize,
    /// Start of the CRC32 table (v2 only).
    crcs: usize,
    /// Start of the 4-byte offset table (v2 only).
    offsets: usize,
    /// Start of the 64-bit offset table (v2 only).
    large: usize,
    /// Number of entries in the 64-bit offset table.
    large_count: usize,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(a)
}

impl PackIndex {
    /// Open and validate the index at `path` for a repository using `kind`.
    pub fn open(path: impl AsRef<Path>, kind: HashKind) -> Result<PackIndex, Error> {
        let path = path.as_ref();
        let data = FileData::open(path).map_err(|e| Error::io(path, e))?;
        PackIndex::parse(data, path.to_path_buf(), kind)
    }

    /// Parse an index held in memory; `path` is only used in messages.
    pub fn from_bytes(
        bytes: Vec<u8>,
        path: impl Into<PathBuf>,
        kind: HashKind,
    ) -> Result<PackIndex, Error> {
        PackIndex::parse(FileData::Owned(bytes), path.into(), kind)
    }

    fn parse(data: FileData, path: PathBuf, kind: HashKind) -> Result<PackIndex, Error> {
        let b = data.as_slice();
        let h = kind.byte_len();
        let len = b.len();
        if len < FANOUT_LEN + 2 * h {
            return Err(Error::IndexTooSmall { path });
        }
        let (version, fanout) = if b[..4] == V2_MAGIC {
            let version = be32(b, 4);
            if version != 2 {
                return Err(Error::IndexVersion { path, version });
            }
            if len < 8 + FANOUT_LEN + 2 * h {
                return Err(Error::IndexTooSmall { path });
            }
            (2, 8)
        } else {
            (1, 0)
        };
        let mut prev = 0;
        for i in 0..256 {
            let n = be32(b, fanout + 4 * i);
            if n < prev {
                return Err(Error::IndexNonMonotonic { path });
            }
            prev = n;
        }
        let num = prev;
        let n = num as usize;
        let mut idx = PackIndex {
            data: FileData::Owned(Vec::new()),
            path,
            kind,
            version,
            num,
            fanout,
            ids: 0,
            id_stride: 0,
            crcs: 0,
            offsets: 0,
            large: 0,
            large_count: 0,
        };
        let wrong_size = |idx: PackIndex| Error::IndexWrongSize {
            path: idx.path,
            version,
        };
        if version == 1 {
            // Overflow-safe: n < 2^32 and the entry is at most 36 bytes.
            let expect = (FANOUT_LEN as u64) + (n as u64) * (4 + h as u64) + 2 * h as u64;
            if len as u64 != expect {
                return Err(wrong_size(idx));
            }
            idx.ids = FANOUT_LEN + 4;
            idx.id_stride = 4 + h;
        } else {
            let min = 8 + FANOUT_LEN as u64 + (n as u64) * (h as u64 + 8) + 2 * h as u64;
            // At most n - 1 large offsets can be needed (the first object
            // starts below 2^31), but accept n like other readers do.
            let max = min + 8 * (n as u64);
            let len64 = len as u64;
            if len64 < min || len64 > max || (len64 - min) % 8 != 0 {
                return Err(wrong_size(idx));
            }
            idx.ids = 8 + FANOUT_LEN;
            idx.id_stride = h;
            idx.crcs = idx.ids + n * h;
            idx.offsets = idx.crcs + n * 4;
            idx.large = idx.offsets + n * 4;
            idx.large_count = ((len64 - min) / 8) as usize;
        }
        idx.data = data;
        Ok(idx)
    }

    /// Path of the `.idx` file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hash algorithm of the ids.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// Index format version, 1 or 2.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Number of objects.
    pub fn len(&self) -> u32 {
        self.num
    }

    /// Whether the index lists no objects.
    pub fn is_empty(&self) -> bool {
        self.num == 0
    }

    fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    fn id_bytes(&self, pos: u32) -> &[u8] {
        let at = self.ids + pos as usize * self.id_stride;
        &self.bytes()[at..at + self.kind.byte_len()]
    }

    /// Id of the object at sorted position `pos` (`pos < len()`).
    ///
    /// # Panics
    /// If `pos` is out of range.
    pub fn id_at(&self, pos: u32) -> ObjectId {
        assert!(pos < self.num, "index position out of range");
        ObjectId::from_bytes(self.kind, self.id_bytes(pos)).expect("length checked")
    }

    /// Pack offset of the object at position `pos` (`pos < len()`).
    ///
    /// Fails when a v2 large offset refers outside the 64-bit table.
    ///
    /// # Panics
    /// If `pos` is out of range.
    pub fn offset_at(&self, pos: u32) -> Result<u64, Error> {
        assert!(pos < self.num, "index position out of range");
        let b = self.bytes();
        if self.version == 1 {
            return Ok(be32(b, FANOUT_LEN + pos as usize * self.id_stride) as u64);
        }
        let v = be32(b, self.offsets + 4 * pos as usize);
        if v & 0x8000_0000 == 0 {
            return Ok(v as u64);
        }
        let i = (v & 0x7fff_ffff) as usize;
        if i >= self.large_count {
            return Err(Error::OffsetBeyondIndex {
                path: self.pack_path(),
            });
        }
        Ok(be64(b, self.large + 8 * i))
    }

    /// CRC32 of the packed entry at position `pos` (v2 only).
    ///
    /// # Panics
    /// If `pos` is out of range.
    pub fn crc32_at(&self, pos: u32) -> Option<u32> {
        assert!(pos < self.num, "index position out of range");
        (self.version == 2).then(|| be32(self.bytes(), self.crcs + 4 * pos as usize))
    }

    /// Range of positions whose ids start with byte `first`.
    fn fanout_range(&self, first: u8) -> Range<u32> {
        let b = self.bytes();
        let end = be32(b, self.fanout + 4 * first as usize);
        let start = if first == 0 {
            0
        } else {
            be32(b, self.fanout + 4 * (first as usize - 1))
        };
        start..end
    }

    /// Position of `id`, if the index lists it.
    pub fn lookup(&self, id: &ObjectId) -> Option<u32> {
        if id.kind() != self.kind {
            return None;
        }
        let want = id.as_bytes();
        let Range { mut start, mut end } = self.fanout_range(want[0]);
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

    /// Positions of all ids that start with `prefix` (empty if none).
    pub fn lookup_prefix(&self, prefix: &Prefix) -> Range<u32> {
        if prefix.kind() != self.kind {
            return 0..0;
        }
        let first = prefix.as_id().as_bytes()[0];
        let range = if prefix.hex_len() >= 2 {
            self.fanout_range(first)
        } else {
            // One hex digit: ids starting with 0xN0 ..= 0xNf.
            let lo = self.fanout_range(first).start;
            let hi = self.fanout_range(first | 0x0f).end;
            lo..hi
        };
        // First position not before the prefix, then first one after it.
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
        (0..self.num).map(|p| self.id_at(p))
    }

    /// The checksum of the pack this index belongs to (copied from the
    /// pack's trailer when the index was written).
    pub fn pack_checksum(&self) -> &[u8] {
        let b = self.bytes();
        let h = self.kind.byte_len();
        &b[b.len() - 2 * h..b.len() - h]
    }

    /// The trailing checksum over the index itself.
    pub fn index_checksum(&self) -> &[u8] {
        let b = self.bytes();
        &b[b.len() - self.kind.byte_len()..]
    }

    /// Hash the whole index and compare with its trailing checksum.
    pub fn verify_checksum(&self) -> Result<(), Error> {
        let b = self.bytes();
        let h = self.kind.byte_len();
        let mut hasher = Hasher::new(self.kind);
        hasher.update(&b[..b.len() - h]);
        match hasher.finalize() {
            Ok(id) if id.as_bytes() == self.index_checksum() => Ok(()),
            _ => Err(Error::Checksum {
                path: self.path.clone(),
            }),
        }
    }

    /// The `.pack` path next to this index (same name, `.pack` extension).
    pub fn pack_path(&self) -> PathBuf {
        self.path.with_extension("pack")
    }
}

/// First position in `range` for which `before` is false (`before` must be
/// true for a prefix of the range and false after it).
fn partition(range: Range<u32>, before: impl Fn(u32) -> bool) -> u32 {
    let Range { mut start, mut end } = range;
    while start < end {
        let mid = start + (end - start) / 2;
        if before(mid) {
            start = mid + 1;
        } else {
            end = mid;
        }
    }
    start
}

impl std::fmt::Debug for PackIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackIndex")
            .field("path", &self.path)
            .field("version", &self.version)
            .field("objects", &self.num)
            .finish()
    }
}
