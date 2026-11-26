// SPDX-License-Identifier: MIT
//! One commit-graph file (a layer): header, chunk table and the chunk
//! accessors. Every offset taken from the file is bounds-checked; a corrupt
//! file gives the diagnostics git prints, never a panic.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};

use crate::bloom;
use crate::messages as msg;
use crate::{Diag, NO_PARENT, SIGNATURE};

/// Read-only file contents, memory mapped where possible. The crate's
/// only `unsafe` code (ADR 0003).
enum FileData {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl FileData {
    fn open(path: &Path) -> io::Result<FileData> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if len > 0 {
            // SAFETY: read-only private mapping. Commit-graph files are
            // written under a temporary name and renamed into place, so
            // their content does not change while mapped (the assumption
            // every git implementation makes for these files). All
            // offsets are bounds-checked against the length.
            if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
                return Ok(FileData::Mapped(map));
            }
        }
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(FileData::Owned(buf))
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            FileData::Mapped(m) => m,
            FileData::Owned(v) => v,
        }
    }
}

/// Chunk ids.
pub(crate) mod id {
    pub const OIDF: u32 = u32::from_be_bytes(*b"OIDF");
    pub const OIDL: u32 = u32::from_be_bytes(*b"OIDL");
    pub const CDAT: u32 = u32::from_be_bytes(*b"CDAT");
    pub const GDA2: u32 = u32::from_be_bytes(*b"GDA2");
    pub const GDO2: u32 = u32::from_be_bytes(*b"GDO2");
    pub const EDGE: u32 = u32::from_be_bytes(*b"EDGE");
    pub const BIDX: u32 = u32::from_be_bytes(*b"BIDX");
    pub const BDAT: u32 = u32::from_be_bytes(*b"BDAT");
    pub const BASE: u32 = u32::from_be_bytes(*b"BASE");
}

/// Bytes before the chunk table.
const HEADER_LEN: usize = 8;
/// One row of the chunk table.
const TOC_ROW: usize = 12;
/// Header of the `BDAT` chunk.
const BDAT_HEADER: usize = 12;

/// A byte range of the file.
#[derive(Debug, Clone, Copy, Default)]
struct Range {
    start: usize,
    len: usize,
}

/// The Bloom filter chunks of a layer.
#[derive(Debug, Clone, Copy)]
struct BloomChunks {
    index: Range,
    data: Range,
    settings: bloom::Settings,
}

/// What reading a layer's own changed-path filter gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterLookup<'a> {
    /// No usable filter for this commit.
    NotPresent,
    /// The filter (may be empty: not computed).
    Filter(&'a [u8]),
}

/// One commit-graph file.
pub struct Layer {
    data: FileData,
    path: PathBuf,
    /// The `objects` directory the file belongs to.
    pub(crate) object_dir: PathBuf,
    hash: HashKind,
    num_commits: u32,
    num_bases: u8,
    chunk_ids: Vec<u32>,
    fanout: usize,
    oid_lookup: usize,
    commit_data: usize,
    generation_data: Option<usize>,
    generation_overflow: Option<Range>,
    extra_edges: Option<Range>,
    base: Option<Range>,
    bloom: Option<BloomChunks>,
    /// Bloom filters of this layer are not used (incompatible settings).
    pub(crate) bloom_disabled: bool,
}

/// How to read a layer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadOptions {
    pub hash: HashKind,
    pub read_bloom: bool,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn be64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(a)
}

/// The format's hash version byte.
pub(crate) fn hash_version(hash: HashKind) -> u8 {
    match hash {
        HashKind::Sha1 => 1,
        HashKind::Sha256 => 2,
    }
}

/// Error of a commit lookup in a layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerError {
    /// `EDGE` points outside the chunk (git: an error, the commit is read
    /// from the object database instead).
    ExtraEdgesOutOfBounds,
    /// `GDO2` points outside the chunk (git: fatal).
    OverflowTooSmall,
}

impl Layer {
    /// Open and check `path`. `Err` holds the diagnostics; the layer is
    /// unusable then.
    pub(crate) fn open(
        path: &Path,
        object_dir: &Path,
        opts: ReadOptions,
        diags: &mut Vec<Diag>,
    ) -> Result<Layer, io::Error> {
        let data = FileData::open(path)?;
        match Self::parse(data, path, object_dir, opts, diags) {
            Some(l) => Ok(l),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt commit-graph",
            )),
        }
    }

    fn parse(
        data: FileData,
        path: &Path,
        object_dir: &Path,
        opts: ReadOptions,
        diags: &mut Vec<Diag>,
    ) -> Option<Layer> {
        let b = data.as_slice();
        let h = opts.hash.byte_len();
        let min = HEADER_LEN + 4 * TOC_ROW + 256 * 4 + h;
        let err = |diags: &mut Vec<Diag>, m: String| diags.push(Diag::Error(m));
        if b.len() < min {
            err(diags, msg::TOO_SMALL.to_string());
            return None;
        }
        let sig = be32(b, 0);
        if sig != SIGNATURE {
            err(diags, msg::signature_mismatch(sig));
            return None;
        }
        if b[4] != 1 {
            err(diags, msg::version_mismatch(b[4]));
            return None;
        }
        let want = hash_version(opts.hash);
        if b[5] != want {
            err(diags, msg::hash_version_mismatch(b[5], want));
            return None;
        }
        let num_chunks = b[6];
        let num_bases = b[7];
        let toc_end = HEADER_LEN + (usize::from(num_chunks) + 1) * TOC_ROW;
        if toc_end > b.len() - h {
            err(diags, msg::too_small_for_chunks(num_chunks));
            return None;
        }
        let mut chunks: Vec<(u32, Range)> = Vec::new();
        for i in 0..usize::from(num_chunks) {
            let row = HEADER_LEN + i * TOC_ROW;
            let cid = be32(b, row);
            let off = be64(b, row + 4);
            let next = be64(b, row + TOC_ROW + 4);
            if cid == 0 {
                // A zero id ends the table early.
                break;
            }
            if off < toc_end as u64 || next < off || next > (b.len() - h) as u64 {
                err(diags, msg::improper_chunk_offset(off, next));
                return None;
            }
            if chunks.iter().any(|(c, _)| *c == cid) {
                err(diags, msg::duplicate_chunk(cid));
                return None;
            }
            chunks.push((
                cid,
                Range {
                    start: off as usize,
                    len: (next - off) as usize,
                },
            ));
        }
        let last = be32(b, HEADER_LEN + usize::from(num_chunks) * TOC_ROW);
        if last != 0 {
            err(diags, msg::final_chunk_nonzero(last));
            return None;
        }
        let chunk = |want: u32| chunks.iter().find(|(c, _)| *c == want).map(|(_, r)| *r);

        // OID fanout.
        let Some(fanout) = chunk(id::OIDF) else {
            err(diags, msg::FANOUT_MISSING.to_string());
            return None;
        };
        if fanout.len != 256 * 4 {
            err(diags, msg::FANOUT_WRONG_SIZE.to_string());
            err(diags, msg::FANOUT_MISSING.to_string());
            return None;
        }
        let mut prev = 0;
        for i in 0..256 {
            let v = be32(b, fanout.start + 4 * i);
            if v < prev {
                err(diags, msg::FANOUT_ORDER.to_string());
                err(diags, msg::FANOUT_MISSING.to_string());
                return None;
            }
            prev = v;
        }
        let num_commits = prev;
        let n = num_commits as usize;

        let Some(lookup) = chunk(id::OIDL) else {
            err(diags, msg::LOOKUP_MISSING.to_string());
            return None;
        };
        if lookup.len / h != n || lookup.len % h != 0 {
            err(diags, msg::LOOKUP_WRONG_SIZE.to_string());
            err(diags, msg::LOOKUP_MISSING.to_string());
            return None;
        }
        let Some(cdat) = chunk(id::CDAT) else {
            err(diags, msg::CDAT_MISSING.to_string());
            return None;
        };
        if cdat.len / (h + 16) != n || cdat.len % (h + 16) != 0 {
            err(diags, msg::CDAT_WRONG_SIZE.to_string());
            err(diags, msg::CDAT_MISSING.to_string());
            return None;
        }
        let generation_data = match chunk(id::GDA2) {
            Some(r) if r.len == n * 4 => Some(r.start),
            Some(_) => {
                err(diags, msg::GENERATIONS_WRONG_SIZE.to_string());
                None
            }
            None => None,
        };
        let generation_overflow = chunk(id::GDO2);
        let extra_edges = chunk(id::EDGE);
        let base = chunk(id::BASE);
        if num_bases > 0 {
            match base {
                Some(r) if r.len >= usize::from(num_bases) * h => {}
                _ => {
                    err(diags, msg::BASE_TOO_SMALL.to_string());
                    return None;
                }
            }
        }
        let mut bloom = None;
        if opts.read_bloom {
            if let (Some(index), Some(bdat)) = (chunk(id::BIDX), chunk(id::BDAT)) {
                if index.len != n * 4 {
                    diags.push(Diag::Warning(msg::BLOOM_INDEX_TOO_SMALL.to_string()));
                } else if bdat.len < BDAT_HEADER {
                    diags.push(Diag::Warning(msg::bloom_data_too_small(bdat.len as u64)));
                } else {
                    let settings = bloom::Settings {
                        hash_version: be32(b, bdat.start),
                        num_hashes: be32(b, bdat.start + 4),
                        bits_per_entry: be32(b, bdat.start + 8),
                        max_changed_paths: bloom::DEFAULT_MAX_CHANGED_PATHS,
                    };
                    bloom = Some(BloomChunks {
                        index,
                        data: bdat,
                        settings,
                    });
                }
            }
        }
        let chunk_ids = chunks.iter().map(|(c, _)| *c).collect();
        Some(Layer {
            path: path.to_path_buf(),
            object_dir: object_dir.to_path_buf(),
            hash: opts.hash,
            num_commits,
            num_bases,
            chunk_ids,
            fanout: fanout.start,
            oid_lookup: lookup.start,
            commit_data: cdat.start,
            generation_data,
            generation_overflow,
            extra_edges,
            base,
            bloom,
            bloom_disabled: false,
            data,
        })
    }

    fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of commits in this file.
    pub fn num_commits(&self) -> u32 {
        self.num_commits
    }

    /// Number of base layers named in the header.
    pub fn num_bases(&self) -> u8 {
        self.num_bases
    }

    /// The chunk ids in table order.
    pub fn chunk_ids(&self) -> &[u32] {
        &self.chunk_ids
    }

    /// Whether the file has the `GDA2` chunk.
    pub fn has_generation_data(&self) -> bool {
        self.generation_data.is_some()
    }

    /// Whether the file has `GDO2`.
    pub fn has_generation_overflow(&self) -> bool {
        self.generation_overflow.is_some()
    }

    /// Whether the file has `EDGE`.
    pub fn has_extra_edges(&self) -> bool {
        self.extra_edges.is_some()
    }

    /// The Bloom settings of the file, if it has usable filter chunks.
    pub fn bloom_settings(&self) -> Option<bloom::Settings> {
        self.bloom.map(|b| b.settings)
    }

    /// The trailing checksum.
    pub fn checksum(&self) -> ObjectId {
        let b = self.bytes();
        let h = self.hash.byte_len();
        ObjectId::from_bytes(self.hash, &b[b.len() - h..]).expect("length checked")
    }

    /// The whole file without the trailing checksum, and the checksum.
    pub(crate) fn content_and_checksum(&self) -> (&[u8], &[u8]) {
        let b = self.bytes();
        b.split_at(b.len() - self.hash.byte_len())
    }

    /// The hashes in the `BASE` chunk.
    pub fn base_hashes(&self) -> Vec<ObjectId> {
        let Some(r) = self.base else {
            return Vec::new();
        };
        let h = self.hash.byte_len();
        let b = self.bytes();
        (0..usize::from(self.num_bases))
            .filter_map(|i| {
                let at = r.start + i * h;
                ObjectId::from_bytes(self.hash, b.get(at..at + h)?).ok()
            })
            .collect()
    }

    /// The id at local position `i`.
    pub fn id_at(&self, i: u32) -> ObjectId {
        let h = self.hash.byte_len();
        let at = self.oid_lookup + i as usize * h;
        ObjectId::from_bytes(self.hash, &self.bytes()[at..at + h]).expect("length checked")
    }

    /// The stored fanout value `i`.
    pub(crate) fn fanout_at(&self, i: usize) -> u32 {
        be32(self.bytes(), self.fanout + 4 * i)
    }

    /// Binary search for `oid`; the local position.
    pub fn find(&self, oid: &ObjectId) -> Option<u32> {
        let key = oid.as_bytes();
        let first = usize::from(*key.first()?);
        let fanout = self.fanout;
        let b = self.bytes();
        let mut lo = if first == 0 {
            0
        } else {
            be32(b, fanout + 4 * (first - 1))
        };
        let mut hi = be32(b, fanout + 4 * first).min(self.num_commits);
        let h = self.hash.byte_len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = self.oid_lookup + mid as usize * h;
            match b[at..at + h].cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn cdat(&self, i: u32) -> &[u8] {
        let w = self.hash.byte_len() + 16;
        let at = self.commit_data + i as usize * w;
        &self.bytes()[at..at + w]
    }

    /// The root tree of local commit `i`.
    pub fn tree_at(&self, i: u32) -> ObjectId {
        let h = self.hash.byte_len();
        ObjectId::from_bytes(self.hash, &self.cdat(i)[..h]).expect("length checked")
    }

    /// The commit date (34 bits) of local commit `i`.
    pub fn date_at(&self, i: u32) -> u64 {
        let h = self.hash.byte_len();
        let c = self.cdat(i);
        (u64::from(be32(c, h + 8) & 3) << 32) | u64::from(be32(c, h + 12))
    }

    /// The topological level (generation number v1) of local commit `i`.
    pub fn level_at(&self, i: u32) -> u32 {
        let h = self.hash.byte_len();
        be32(self.cdat(i), h + 8) >> 2
    }

    /// The raw parent positions of local commit `i` (global positions).
    pub fn parents_at(&self, i: u32, out: &mut Vec<u32>) -> Result<(), LayerError> {
        out.clear();
        let h = self.hash.byte_len();
        let c = self.cdat(i);
        let p1 = be32(c, h);
        let p2 = be32(c, h + 4);
        if p1 == NO_PARENT {
            return Ok(());
        }
        out.push(p1);
        if p2 == NO_PARENT {
            return Ok(());
        }
        if p2 & 0x8000_0000 == 0 {
            out.push(p2);
            return Ok(());
        }
        let Some(edges) = self.extra_edges else {
            return Err(LayerError::ExtraEdgesOutOfBounds);
        };
        let b = self.bytes();
        let mut k = (p2 & 0x7fff_ffff) as usize;
        loop {
            if (k + 1) * 4 > edges.len {
                return Err(LayerError::ExtraEdgesOutOfBounds);
            }
            let v = be32(b, edges.start + 4 * k);
            out.push(v & 0x7fff_ffff);
            if v & 0x8000_0000 != 0 {
                return Ok(());
            }
            k += 1;
        }
    }

    /// The corrected commit date offset of local commit `i`, if the file
    /// has generation data.
    pub fn generation_offset_at(&self, i: u32) -> Result<Option<u64>, LayerError> {
        let Some(gda) = self.generation_data else {
            return Ok(None);
        };
        let b = self.bytes();
        let v = be32(b, gda + 4 * i as usize);
        if v & 0x8000_0000 == 0 {
            return Ok(Some(u64::from(v)));
        }
        let k = (v & 0x7fff_ffff) as usize;
        match self.generation_overflow {
            Some(r) if (k + 1) * 8 <= r.len => Ok(Some(be64(b, r.start + 8 * k))),
            _ => Err(LayerError::OverflowTooSmall),
        }
    }

    /// The changed-path filter of local commit `i`.
    pub fn bloom_filter(&self, i: u32, diags: &mut Vec<Diag>) -> FilterLookup<'_> {
        let Some(bc) = self.bloom else {
            return FilterLookup::NotPresent;
        };
        if self.bloom_disabled {
            return FilterLookup::NotPresent;
        }
        let b = self.bytes();
        let end = be32(b, bc.index.start + 4 * i as usize);
        let start = if i == 0 {
            0
        } else {
            be32(b, bc.index.start + 4 * (i as usize - 1))
        };
        let avail = (bc.data.len - BDAT_HEADER) as u64;
        let path = self.path.to_string_lossy();
        if u64::from(end) > avail {
            diags.push(Diag::Warning(msg::bloom_offset_out_of_range(end, i, &path)));
            return FilterLookup::NotPresent;
        }
        if start > end {
            diags.push(Diag::Warning(msg::bloom_offsets_decreasing(
                start,
                end,
                i - 1,
                i,
                &path,
            )));
            return FilterLookup::NotPresent;
        }
        let s = bc.data.start + BDAT_HEADER + start as usize;
        let e = bc.data.start + BDAT_HEADER + end as usize;
        FilterLookup::Filter(&b[s..e])
    }

    /// Whether the file names the hash kind it was opened with.
    pub fn hash_kind(&self) -> HashKind {
        self.hash
    }
}
