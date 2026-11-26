// SPDX-License-Identifier: MIT

//! Large blobs go straight into a pack (`core.bigFileThreshold`).
//!
//! `git add` and `git hash-object -w` store files above the threshold
//! deflated in a pack instead of as loose objects, without delta
//! compression and without ever holding the content in memory. All large
//! objects of one command go into one pack (`pack.packSizeLimit` starts
//! a new one when the next object would not fit); an object that exists
//! already is not stored again. The pack and its `.idx` appear under their
//! final names when the command is done with them
//! ([`crate::ObjectDatabase::flush_bulk`]).
//!
//! Each object is written as it is read: entry header, then the deflated
//! content while its id is computed. When the id turns out to exist, the
//! entry is cut off again. The object count in the pack header and the
//! trailing checksum are fixed up at the end by reading the file once more.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_fs::{PACK_PREFIX, TempFile};
use rgit_hash::{HashKind, Hasher, ObjectId, object_hasher};
use rgit_object::ObjectKind;
use rgit_pack::index_pack::{IndexVersion, IndexedObject, IndexedPack};

use crate::Error;

/// Settings for packs of large objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BulkOptions {
    /// zlib level (`pack.compression`, else `core.compression`).
    pub compression: u32,
    /// `pack.packSizeLimit` (0: none).
    pub size_limit: u64,
}

/// The pack being written.
struct OpenPack {
    tmp: TempFile,
    out: BufWriter<File>,
    offset: u64,
    entries: Vec<IndexedObject>,
}

/// Packs of large objects written by one command.
pub(crate) struct Bulk {
    pack_dir: PathBuf,
    hash: HashKind,
    opts: BulkOptions,
    current: Option<OpenPack>,
    /// Objects already stored by this command (in finished packs or the
    /// current one).
    seen: HashSet<ObjectId>,
}

/// Counts, checksums and writes the bytes of one entry.
struct EntryWriter<'a> {
    out: &'a mut BufWriter<File>,
    crc: &'a mut crc32fast::Hasher,
    written: &'a mut u64,
}

impl Write for EntryWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.crc.update(&buf[..n]);
        *self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Bulk {
    pub(crate) fn new(objects_dir: &Path, hash: HashKind, opts: BulkOptions) -> Bulk {
        Bulk {
            pack_dir: objects_dir.join("pack"),
            hash,
            opts,
            current: None,
            seen: HashSet::new(),
        }
    }

    /// Whether `oid` was stored by this command and is not readable yet
    /// (it sits in the unfinished pack).
    pub(crate) fn pending(&self, oid: &ObjectId) -> bool {
        self.current
            .as_ref()
            .is_some_and(|p| p.entries.iter().any(|e| e.id == *oid))
    }

    fn io_err(&self, e: io::Error) -> Error {
        Error::io(self.pack_dir.display().to_string(), e)
    }

    fn open_pack(&mut self) -> Result<&mut OpenPack, Error> {
        if self.current.is_none() {
            match std::fs::create_dir(&self.pack_dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(self.io_err(e)),
            }
            let mut tmp = TempFile::with_mode(&self.pack_dir, PACK_PREFIX, 0o444)
                .map_err(|e| self.io_err(e))?;
            let file = tmp.file_mut().try_clone().map_err(|e| self.io_err(e))?;
            let mut out = BufWriter::with_capacity(64 * 1024, file);
            let mut header = [0u8; 12];
            header[..4].copy_from_slice(b"PACK");
            header[4..8].copy_from_slice(&2u32.to_be_bytes());
            out.write_all(&header).map_err(|e| self.io_err(e))?;
            self.current = Some(OpenPack {
                tmp,
                out,
                offset: 12,
                entries: Vec::new(),
            });
        }
        Ok(self.current.as_mut().expect("opened"))
    }

    /// Store an object of `size` bytes read from `reader`, unless `exists`
    /// says it is stored already. Returns its id.
    pub(crate) fn write(
        &mut self,
        kind: ObjectKind,
        size: u64,
        reader: &mut dyn Read,
        exists: &dyn Fn(&ObjectId) -> bool,
    ) -> Result<ObjectId, Error> {
        // An object that would not fit any more starts a new pack.
        if self.opts.size_limit > 0 {
            if let Some(p) = &self.current {
                if !p.entries.is_empty() && p.offset + size > self.opts.size_limit {
                    self.finish()?;
                }
            }
        }
        let hash = self.hash;
        let level = Compression::new(self.opts.compression);
        let dir = self.pack_dir.display().to_string();
        let pack = self.open_pack()?;
        let io_err = |e: io::Error| Error::io(dir.clone(), e);
        let start = pack.offset;
        let mut crc = crc32fast::Hasher::new();
        let mut written = 0u64;
        let mut hasher: Hasher = object_hasher(hash, kind.name(), size);
        {
            let mut w = EntryWriter {
                out: &mut pack.out,
                crc: &mut crc,
                written: &mut written,
            };
            w.write_all(&entry_header(kind, size)).map_err(io_err)?;
            let mut z = ZlibEncoder::new(&mut w, level);
            let mut buf = vec![0u8; 64 * 1024];
            let mut left = size;
            while left > 0 {
                let want = left.min(buf.len() as u64) as usize;
                let n = match reader.read(&mut buf[..want]) {
                    Ok(0) => {
                        return Err(io_err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "short read while writing object",
                        )));
                    }
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(io_err(e)),
                };
                hasher.update(&buf[..n]);
                z.write_all(&buf[..n]).map_err(io_err)?;
                left -= n as u64;
            }
            z.finish().map_err(io_err)?;
        }
        let oid = hasher.finalize()?;
        if self.seen.contains(&oid) || exists(&oid) {
            // Cut the entry off again.
            let pack = self.current.as_mut().expect("open");
            pack.out.flush().map_err(io_err)?;
            let file = pack.out.get_mut();
            file.set_len(start).map_err(io_err)?;
            file.seek(SeekFrom::Start(start)).map_err(io_err)?;
            return Ok(oid);
        }
        let pack = self.current.as_mut().expect("open");
        pack.offset = start + written;
        pack.entries.push(IndexedObject {
            id: oid,
            kind,
            offset: start,
            crc32: crc.finalize(),
            delta: false,
        });
        self.seen.insert(oid);
        Ok(oid)
    }

    /// Finish the current pack: fix its header and trailer, write its
    /// index and move both into place. A pack without objects is removed.
    pub(crate) fn finish(&mut self) -> Result<(), Error> {
        let Some(pack) = self.current.take() else {
            return Ok(());
        };
        let OpenPack {
            tmp, out, entries, ..
        } = pack;
        let dir = self.pack_dir.display().to_string();
        let io_err = |e: io::Error| Error::io(dir.clone(), e);
        let mut file = out.into_inner().map_err(|e| io_err(e.into_error()))?;
        if entries.is_empty() {
            drop(file);
            drop(tmp);
            return Ok(());
        }
        let count = u32::try_from(entries.len()).map_err(|_| {
            io_err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many objects",
            ))
        })?;
        let checksum = fix_header(&mut file, self.hash, count).map_err(io_err)?;
        let pack_len = file.seek(SeekFrom::End(0)).map_err(io_err)?;
        file.sync_all().map_err(io_err)?;
        drop(file);
        let indexed = IndexedPack::from_parts(self.hash, entries, checksum, pack_len);
        let idx = indexed
            .index_bytes(IndexVersion::default())
            .map_err(|e| io_err(io::Error::other(e.to_string())))?;
        let mut idx_tmp = TempFile::with_mode(&self.pack_dir, "tmp_idx_", 0o444).map_err(io_err)?;
        idx_tmp.file_mut().write_all(&idx).map_err(io_err)?;
        idx_tmp.file_mut().sync_all().map_err(io_err)?;
        let base = self.pack_dir.join(format!("pack-{checksum}"));
        tmp.persist(base.with_extension("pack")).map_err(io_err)?;
        idx_tmp
            .persist(base.with_extension("idx"))
            .map_err(io_err)?;
        Ok(())
    }
}

/// The header of a whole-object pack entry: type and size.
fn entry_header(kind: ObjectKind, size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    let mut c = (kind.pack_type() << 4) | (size & 0x0f) as u8;
    let mut rest = size >> 4;
    while rest != 0 {
        out.push(c | 0x80);
        c = (rest & 0x7f) as u8;
        rest >>= 7;
    }
    out.push(c);
    out
}

/// Write `count` into the pack header and append the checksum of
/// everything before it. Returns the checksum.
fn fix_header(file: &mut File, hash: HashKind, count: u32) -> io::Result<ObjectId> {
    let len = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(8))?;
    file.write_all(&count.to_be_bytes())?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Hasher::new(hash);
    let mut left = len;
    let mut chunk = vec![0u8; 64 * 1024];
    while left > 0 {
        let n = (left as usize).min(chunk.len());
        file.read_exact(&mut chunk[..n])?;
        hasher.update(&chunk[..n]);
        left -= n as u64;
    }
    let id = hasher
        .finalize()
        .map_err(|e| io::Error::other(e.to_string()))?;
    file.write_all(id.as_bytes())?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_headers() {
        assert_eq!(entry_header(ObjectKind::Blob, 5), [0x35]);
        assert_eq!(entry_header(ObjectKind::Blob, 0x10), [0xb0, 0x01]);
        assert_eq!(
            entry_header(ObjectKind::Commit, 2_000_000),
            [0x90, 0xc8, 0xd0, 0x07]
        );
    }
}
