// SPDX-License-Identifier: MIT

//! Writing `.idx` (versions 1 and 2) and `.rev` files
//! (Documentation/gitformat-pack.adoc).

use rgit_hash::{HashKind, Hasher};

use super::{Error, IndexedPack};
use crate::PackIndex;

/// Offsets above this go to the 64-bit table of an index version 2.
pub const DEFAULT_LARGE_OFFSET_LIMIT: u64 = 0x7fff_ffff;

/// The `.idx` format to write (`--index-version=<version>[,<offset>]`,
/// `pack.indexVersion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexVersion {
    /// 1 or 2. Version 1 is upgraded to 2 when an offset exceeds
    /// `large_offset_limit`.
    pub version: u32,
    /// Offsets greater than this are stored in the 64-bit table.
    pub large_offset_limit: u64,
}

impl Default for IndexVersion {
    fn default() -> IndexVersion {
        IndexVersion {
            version: 2,
            large_offset_limit: DEFAULT_LARGE_OFFSET_LIMIT,
        }
    }
}

impl IndexVersion {
    /// The settings that reproduce an existing index (for `--verify`): its
    /// version and, for version 2, a threshold just below its smallest
    /// 64-bit offset. `None` if `idx` is not a valid index.
    pub fn of(idx: &[u8], hash: HashKind) -> Option<IndexVersion> {
        let index = PackIndex::from_bytes(idx.to_vec(), "", hash).ok()?;
        let mut v = IndexVersion {
            version: index.version(),
            large_offset_limit: DEFAULT_LARGE_OFFSET_LIMIT,
        };
        if v.version == 2 {
            let n = index.len() as usize;
            let table = 8 + 1024 + n * (hash.byte_len() + 4);
            let large = table + 4 * n;
            let be32 = |at: usize| u32::from_be_bytes(idx[at..at + 4].try_into().expect("4"));
            let min = (0..n)
                .map(|i| be32(table + 4 * i))
                .filter(|o| o & 0x8000_0000 != 0)
                .filter_map(|o| {
                    let at = large + 8 * (o & 0x7fff_ffff) as usize;
                    idx.get(at..at + 8)
                        .map(|b| u64::from_be_bytes(b.try_into().expect("8")))
                })
                .min();
            if let Some(min) = min {
                v.large_offset_limit = v.large_offset_limit.min(min.saturating_sub(1));
            }
        }
        Some(v)
    }
}

fn hash_id(kind: HashKind) -> u32 {
    match kind {
        HashKind::Sha1 => 1,
        HashKind::Sha256 => 2,
    }
}

impl IndexedPack {
    /// Object positions in index order: by id, then by offset.
    fn sorted(&self) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.objects.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| {
            let (a, b) = (&self.objects[a as usize], &self.objects[b as usize]);
            a.id.cmp(&b.id).then(a.offset.cmp(&b.offset))
        });
        order
    }

    /// The `.idx` file content.
    pub fn index_bytes(&self, v: IndexVersion) -> Result<Vec<u8>, Error> {
        let order = self.sorted();
        let objs = &self.objects;
        let limit = v.large_offset_limit;
        let version = if v.version == 1 && objs.iter().any(|o| o.offset > limit) {
            2
        } else {
            v.version
        };
        let h = self.hash.byte_len();
        let mut out = Vec::with_capacity(8 + 1024 + objs.len() * (h + 12) + 2 * h);
        if version == 2 {
            out.extend_from_slice(&[0xff, b't', b'O', b'c', 0, 0, 0, 2]);
        }
        let mut fanout = [0u32; 256];
        for &i in &order {
            fanout[objs[i as usize].id.as_bytes()[0] as usize] += 1;
        }
        let mut total = 0u32;
        for n in fanout {
            total += n;
            out.extend_from_slice(&total.to_be_bytes());
        }
        if version == 1 {
            for &i in &order {
                let o = &objs[i as usize];
                let off = u32::try_from(o.offset).map_err(|_| Error::OffsetTooLarge)?;
                out.extend_from_slice(&off.to_be_bytes());
                out.extend_from_slice(o.id.as_bytes());
            }
        } else {
            for &i in &order {
                out.extend_from_slice(objs[i as usize].id.as_bytes());
            }
            for &i in &order {
                out.extend_from_slice(&objs[i as usize].crc32.to_be_bytes());
            }
            let mut large = Vec::new();
            for &i in &order {
                let off = objs[i as usize].offset;
                if off > limit {
                    let n = (large.len() / 8) as u32;
                    out.extend_from_slice(&(0x8000_0000 | n).to_be_bytes());
                    large.extend_from_slice(&off.to_be_bytes());
                } else {
                    out.extend_from_slice(&(off as u32).to_be_bytes());
                }
            }
            out.extend_from_slice(&large);
        }
        out.extend_from_slice(self.checksum.as_bytes());
        let mut hasher = Hasher::new(self.hash);
        hasher.update(&out);
        let sum = hasher.finalize().map_err(|_| Error::ChecksumMismatch)?;
        out.extend_from_slice(sum.as_bytes());
        Ok(out)
    }

    /// The `.rev` file content: index positions in pack order.
    pub fn rev_bytes(&self) -> Result<Vec<u8>, Error> {
        let order = self.sorted();
        let mut pos_of = vec![0u32; order.len()];
        for (pos, &i) in order.iter().enumerate() {
            pos_of[i as usize] = pos as u32;
        }
        let mut by_offset: Vec<u32> = (0..self.objects.len() as u32).collect();
        by_offset.sort_unstable_by_key(|&i| self.objects[i as usize].offset);
        let h = self.hash.byte_len();
        let mut out = Vec::with_capacity(12 + 4 * order.len() + 2 * h);
        out.extend_from_slice(b"RIDX");
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&hash_id(self.hash).to_be_bytes());
        for i in by_offset {
            out.extend_from_slice(&pos_of[i as usize].to_be_bytes());
        }
        out.extend_from_slice(self.checksum.as_bytes());
        let mut hasher = Hasher::new(self.hash);
        hasher.update(&out);
        let sum = hasher.finalize().map_err(|_| Error::ChecksumMismatch)?;
        out.extend_from_slice(sum.as_bytes());
        Ok(out)
    }
}
