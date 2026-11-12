// SPDX-License-Identifier: MIT

//! The `.pack` file: header, entry headers and zlib streams.

use std::path::{Path, PathBuf};

use flate2::{Decompress, FlushDecompress, Status};
use rgit_hash::{HashKind, Hasher, ObjectId};

use crate::delta::read_size;
use crate::mmap::FileData;
use crate::{Error, ObjectKind};

/// Size of the pack header (`PACK`, version, object count).
pub const PACK_HEADER_LEN: u64 = 12;

/// What an entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A whole object.
    Base(ObjectKind),
    /// A delta against the entry at `base_offset` in the same pack.
    OfsDelta {
        /// Offset of the base entry.
        base_offset: u64,
    },
    /// A delta against the object `base`.
    RefDelta {
        /// Id of the base object.
        base: ObjectId,
    },
}

/// A decoded entry header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryHeader {
    /// Offset of the entry (its first header byte).
    pub offset: u64,
    /// Object or delta.
    pub kind: EntryKind,
    /// Inflated size: of the object, or of the delta data.
    pub size: u64,
    /// Offset of the zlib stream.
    pub data_offset: u64,
}

/// A validated `.pack` file.
pub struct PackData {
    data: FileData,
    path: PathBuf,
    kind: HashKind,
    version: u32,
    num_objects: u32,
}

impl PackData {
    /// Open the pack at `path` and check its header.
    pub fn open(path: impl AsRef<Path>, kind: HashKind) -> Result<PackData, Error> {
        let path = path.as_ref();
        let data = FileData::open(path).map_err(|e| Error::io(path, e))?;
        PackData::parse(data, path.to_path_buf(), kind)
    }

    /// Parse a pack held in memory; `path` is only used in messages.
    pub fn from_bytes(
        bytes: Vec<u8>,
        path: impl Into<PathBuf>,
        kind: HashKind,
    ) -> Result<PackData, Error> {
        PackData::parse(FileData::Owned(bytes), path.into(), kind)
    }

    fn parse(data: FileData, path: PathBuf, kind: HashKind) -> Result<PackData, Error> {
        let b = data.as_slice();
        if b.len() < PACK_HEADER_LEN as usize || &b[..4] != b"PACK" {
            return Err(Error::NotAPack { path });
        }
        let version = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
        if version != 2 && version != 3 {
            return Err(Error::PackVersion { path, version });
        }
        if b.len() < PACK_HEADER_LEN as usize + kind.byte_len() {
            return Err(Error::NotAPack { path });
        }
        let num_objects = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
        Ok(PackData {
            data,
            path,
            kind,
            version,
            num_objects,
        })
    }

    /// Path of the `.pack` file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hash algorithm of the pack.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// Pack version (2 or 3).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Object count from the header.
    pub fn num_objects(&self) -> u32 {
        self.num_objects
    }

    /// The whole file.
    pub fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Offset where the trailing checksum starts (end of the entries).
    pub fn entries_end(&self) -> u64 {
        (self.bytes().len() - self.kind.byte_len()) as u64
    }

    /// The trailing checksum.
    pub fn checksum(&self) -> &[u8] {
        &self.bytes()[self.entries_end() as usize..]
    }

    /// Hash everything before the trailer and compare with it.
    pub fn verify_checksum(&self) -> Result<(), Error> {
        let end = self.entries_end() as usize;
        let mut hasher = Hasher::new(self.kind);
        hasher.update(&self.bytes()[..end]);
        match hasher.finalize() {
            Ok(id) if id.as_bytes() == self.checksum() => Ok(()),
            _ => Err(Error::Checksum {
                path: self.path.clone(),
            }),
        }
    }

    /// The entries region (header to trailer), bounds-checked from `offset`.
    fn entry_bytes(&self, offset: u64) -> Result<&[u8], Error> {
        if offset < PACK_HEADER_LEN || offset >= self.entries_end() {
            return Err(Error::OffsetBeyondPack);
        }
        Ok(&self.bytes()[offset as usize..self.entries_end() as usize])
    }

    /// Decode the entry header at `offset`.
    pub fn entry_header(&self, offset: u64) -> Result<EntryHeader, Error> {
        let b = self.entry_bytes(offset)?;
        let first = b[0];
        let ty = (first >> 4) & 7;
        let mut size = (first & 0x0f) as u64;
        let mut pos = 1usize;
        if first & 0x80 != 0 {
            let mut rest_pos = pos;
            let rest = read_size(b, &mut rest_pos).ok_or(Error::BadObjectHeader)?;
            if rest > (u64::MAX >> 4) {
                return Err(Error::BadObjectHeader);
            }
            size |= rest << 4;
            pos = rest_pos;
        }
        let kind = match ty {
            1 => EntryKind::Base(ObjectKind::Commit),
            2 => EntryKind::Base(ObjectKind::Tree),
            3 => EntryKind::Base(ObjectKind::Blob),
            4 => EntryKind::Base(ObjectKind::Tag),
            6 => {
                let mut c = *b.get(pos).ok_or(Error::BadObjectHeader)?;
                pos += 1;
                let mut rel = (c & 0x7f) as u64;
                while c & 0x80 != 0 {
                    c = *b.get(pos).ok_or(Error::BadObjectHeader)?;
                    pos += 1;
                    rel = rel
                        .checked_add(1)
                        .and_then(|r| r.checked_mul(128))
                        .and_then(|r| r.checked_add((c & 0x7f) as u64))
                        .ok_or_else(|| self.bad_base_offset(offset))?;
                }
                if rel == 0 || rel > offset {
                    return Err(self.bad_base_offset(offset));
                }
                EntryKind::OfsDelta {
                    base_offset: offset - rel,
                }
            }
            7 => {
                let h = self.kind.byte_len();
                let raw = b.get(pos..pos + h).ok_or(Error::BadObjectHeader)?;
                pos += h;
                EntryKind::RefDelta {
                    base: ObjectId::from_bytes(self.kind, raw).expect("length checked"),
                }
            }
            _ => {
                return Err(Error::UnknownType {
                    ty,
                    offset,
                    path: self.path.clone(),
                });
            }
        };
        Ok(EntryHeader {
            offset,
            kind,
            size,
            data_offset: offset + pos as u64,
        })
    }

    fn bad_base_offset(&self, offset: u64) -> Error {
        Error::DeltaBaseOffset {
            offset,
            path: self.path.clone(),
        }
    }

    /// Inflate the data of entry `h`, which must produce exactly `h.size`
    /// bytes. Returns the data and the compressed length consumed.
    pub fn inflate(&self, h: &EntryHeader) -> Result<(Vec<u8>, u64), Error> {
        let input = self.entry_bytes(h.data_offset)?;
        let size = usize::try_from(h.size).map_err(|_| Error::BadObjectHeader)?;
        inflate_exact(input, size, h.offset)
    }

    /// Inflate at most `limit` bytes of the data of entry `h` (for reading
    /// the sizes at the start of a delta without inflating all of it).
    pub fn inflate_prefix(&self, h: &EntryHeader, limit: usize) -> Result<Vec<u8>, Error> {
        let input = self.entry_bytes(h.data_offset)?;
        let mut d = Decompress::new(true);
        let mut out = Vec::with_capacity(limit);
        loop {
            let consumed = d.total_in() as usize;
            let before = out.len();
            let status = d
                .decompress_vec(&input[consumed..], &mut out, FlushDecompress::None)
                .map_err(zlib_error)?;
            if status == Status::StreamEnd || out.len() == limit {
                return Ok(out);
            }
            if out.len() == before && d.total_in() as usize == consumed {
                return Err(Error::InflateTruncated);
            }
        }
    }
}

fn zlib_error(e: flate2::DecompressError) -> Error {
    Error::Inflate {
        detail: e.message().unwrap_or("invalid data").to_string(),
    }
}

/// Inflate one zlib stream from the start of `input` into exactly `size`
/// bytes. Returns the data and the number of compressed bytes consumed.
pub(crate) fn inflate_exact(
    input: &[u8],
    size: usize,
    offset: u64,
) -> Result<(Vec<u8>, u64), Error> {
    let mut d = Decompress::new(true);
    // zlib cannot expand by more than about 1032:1; do not trust a huge
    // size from a corrupt header for the initial allocation.
    let first = size
        .saturating_add(1)
        .min(input.len().saturating_mul(1032).saturating_add(64))
        .max(1);
    let mut out: Vec<u8> = Vec::with_capacity(first);
    loop {
        if out.len() == out.capacity() {
            if out.len() > size {
                break;
            }
            // Room for the expected size plus one byte to detect overlong
            // streams.
            let want = (size + 1 - out.len()).min(out.len().max(64 * 1024));
            out.reserve_exact(want);
        }
        let consumed = d.total_in() as usize;
        let before = out.len();
        let status = d
            .decompress_vec(&input[consumed..], &mut out, FlushDecompress::None)
            .map_err(zlib_error)?;
        if status == Status::StreamEnd {
            break;
        }
        // No progress with room left in the output: the input ran out.
        // (With the output full, loop to grow it or to report the overrun.)
        if out.len() == before && d.total_in() as usize == consumed && out.len() < out.capacity() {
            return Err(Error::InflateTruncated);
        }
    }
    if out.len() != size {
        return Err(Error::InflateSize {
            offset,
            expected: size as u64,
            actual: out.len() as u64,
        });
    }
    Ok((out, d.total_in()))
}

impl std::fmt::Debug for PackData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackData")
            .field("path", &self.path)
            .field("version", &self.version)
            .field("objects", &self.num_objects)
            .finish()
    }
}
