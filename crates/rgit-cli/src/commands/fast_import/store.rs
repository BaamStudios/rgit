// SPDX-License-Identifier: MIT
//! Where `fast-import` puts objects: a pack file that grows while the
//! stream is read and is closed at `checkpoint`, at the end, when it
//! reaches `--max-pack-size`, or on a crash. A closed pack with fewer
//! objects than `fastimport.unpackLimit` is written as loose objects
//! instead.
//!
//! Objects of the open pack are read back from it (`cat-blob`, `ls`, trees
//! of new commits); all others come from the object database.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_hash::{HashKind, Hasher, ObjectId};
use rgit_object::ObjectKind;
use crate::repo_objects::Repo;
use rgit_pack::index_pack::{IndexVersion, IndexedObject, IndexedPack};

use crate::Die;
use crate::messages::fast_import as msg;

/// Per-type counters for `--stats`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Counts {
    pub stored: u64,
    pub duplicates: u64,
}

/// An object written by this process.
#[derive(Debug, Clone, Copy)]
enum Known {
    /// In the open pack: offset and length of the entry.
    Open { offset: u64, len: u64, kind: ObjectKind },
    /// In a closed pack or loose.
    Closed(ObjectKind),
}

struct OpenPack {
    file: File,
    path: PathBuf,
    entries: Vec<IndexedObject>,
    offset: u64,
}

/// A pack that was closed and kept.
pub struct ClosedPack {
    pub path: PathBuf,
    /// The number of the pack (counts up from 0 per process).
    pub id: u32,
}

pub struct Store {
    hash: HashKind,
    pack_dir: PathBuf,
    open: Option<OpenPack>,
    known: HashMap<ObjectId, Known>,
    level: Compression,
    pub unpack_limit: u64,
    pub max_pack_size: u64,
    /// The number of the open (or next) pack.
    pub pack_id: u32,
    pub counts: [Counts; 4],
    pub packs_written: u64,
}

fn kind_index(kind: ObjectKind) -> usize {
    match kind {
        ObjectKind::Blob => 0,
        ObjectKind::Tree => 1,
        ObjectKind::Commit => 2,
        ObjectKind::Tag => 3,
    }
}

fn io_fatal(path: &Path, e: &std::io::Error) -> Die {
    Die::Fatal(msg::unable_to_write(
        &path.display().to_string(),
        &crate::global::strerror(e),
    ))
}

/// The pack entry header: type and size.
fn entry_header(kind: ObjectKind, size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    let mut c = ((kind.pack_type() as u64) << 4) | (size & 0x0f);
    let mut rest = size >> 4;
    while rest > 0 {
        out.push((c | 0x80) as u8);
        c = rest & 0x7f;
        rest >>= 7;
    }
    out.push(c as u8);
    out
}

impl Store {
    pub fn new(hash: HashKind, objects_dir: &Path, level: Option<u32>) -> Store {
        Store {
            hash,
            pack_dir: objects_dir.join("pack"),
            open: None,
            known: HashMap::new(),
            level: level.map_or(Compression::default(), |l| Compression::new(l.min(9))),
            unpack_limit: 100,
            max_pack_size: 0,
            pack_id: 0,
            counts: [Counts::default(); 4],
            packs_written: 0,
        }
    }

    /// The id of `data` as an object of type `kind`.
    pub fn object_id(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, Die> {
        rgit_hash::hash_object(self.hash, kind.name(), data)
            .map_err(|_| Die::Fatal(format!("SHA-1 collision in {}", kind.name())))
    }

    /// The type of `oid`, if the object exists here or in the database.
    pub fn kind_of(&self, repo: &Repo, oid: &ObjectId) -> Option<ObjectKind> {
        match self.known.get(oid) {
            Some(Known::Open { kind, .. }) | Some(Known::Closed(kind)) => Some(*kind),
            None => repo.odb.header(oid).ok().flatten().map(|h| h.kind),
        }
    }

    /// Read `oid` from the open pack or the database.
    pub fn read(
        &mut self,
        repo: &Repo,
        oid: &ObjectId,
    ) -> Result<Option<(ObjectKind, Vec<u8>)>, Die> {
        if let Some(Known::Open { offset, len, kind }) = self.known.get(oid).copied() {
            let open = self.open.as_ref().expect("open pack");
            let mut entry = vec![0u8; len as usize];
            open.file
                .read_exact_at(&mut entry, offset)
                .map_err(|e| io_fatal(&open.path, &e))?;
            let mut i = 0;
            while entry[i] & 0x80 != 0 {
                i += 1;
            }
            let mut data = Vec::new();
            flate2::read::ZlibDecoder::new(&entry[i + 1..])
                .read_to_end(&mut data)
                .map_err(|e| io_fatal(&open.path, &e))?;
            return Ok(Some((kind, data)));
        }
        Ok(repo.odb.read(oid).ok().flatten().map(|o| (o.kind, o.data)))
    }

    /// Store an object unless it exists already; returns its id.
    pub fn store(
        &mut self,
        repo: &Repo,
        kind: ObjectKind,
        data: &[u8],
        edges: &mut dyn FnMut(&ClosedPack),
    ) -> Result<ObjectId, Die> {
        let oid = self.object_id(kind, data)?;
        let counts = &mut self.counts[kind_index(kind)];
        // Only objects of this import and of packs count as present (as
        // with git, a loose copy is written again).
        if self.known.contains_key(&oid) || repo.is_packed(&oid) {
            counts.duplicates += 1;
            return Ok(oid);
        }
        counts.stored += 1;
        let mut entry = entry_header(kind, data.len() as u64);
        let mut z = ZlibEncoder::new(entry, self.level);
        z.write_all(data).map_err(|e| io_fatal(&self.pack_dir, &e))?;
        entry = z.finish().map_err(|e| io_fatal(&self.pack_dir, &e))?;
        let full = self
            .open
            .as_ref()
            .is_some_and(|p| self.max_pack_size > 0 && !p.entries.is_empty() && p.offset + entry.len() as u64 + self.hash.byte_len() as u64 > self.max_pack_size);
        if full {
            if let Some(closed) = self.close(repo)? {
                edges(&closed);
            }
        }
        if self.open.is_none() {
            self.start()?;
        }
        let open = self.open.as_mut().expect("open pack");
        let offset = open.offset;
        open.file
            .write_all_at(&entry, offset)
            .map_err(|e| io_fatal(&open.path, &e))?;
        open.offset += entry.len() as u64;
        open.entries.push(IndexedObject {
            id: oid,
            kind,
            offset,
            crc32: crc32fast::hash(&entry),
            delta: false,
        });
        self.known.insert(
            oid,
            Known::Open {
                offset,
                len: entry.len() as u64,
                kind,
            },
        );
        Ok(oid)
    }

    fn start(&mut self) -> Result<(), Die> {
        let _ = std::fs::create_dir_all(&self.pack_dir);
        let path = self
            .pack_dir
            .join(format!("tmp_pack_{}_{}", std::process::id(), self.pack_id));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| {
                Die::Fatal(msg::unable_to_create(
                    &path.display().to_string(),
                    &crate::global::strerror(&e),
                ))
            })?;
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(b"PACK");
        header.extend_from_slice(&2u32.to_be_bytes());
        header.extend_from_slice(&0u32.to_be_bytes());
        file.write_all_at(&header, 0)
            .map_err(|e| io_fatal(&path, &e))?;
        self.open = Some(OpenPack {
            file,
            path,
            entries: Vec::new(),
            offset: 12,
        });
        Ok(())
    }

    /// Close the open pack: write its trailer and index, or its objects as
    /// loose objects when there are fewer than the unpack limit. Returns
    /// the pack when one was kept.
    pub fn close(&mut self, repo: &Repo) -> Result<Option<ClosedPack>, Die> {
        let Some(mut open) = self.open.take() else {
            return Ok(None);
        };
        let id = self.pack_id;
        self.pack_id += 1;
        if open.entries.is_empty() {
            drop(open.file);
            let _ = std::fs::remove_file(&open.path);
            return Ok(None);
        }
        let count = open.entries.len() as u64;
        if count <= self.unpack_limit {
            // Read everything back before the pack goes away.
            let entries = std::mem::take(&mut open.entries);
            self.open = Some(open);
            let mut objects = Vec::with_capacity(entries.len());
            for e in &entries {
                let (kind, data) = self.read(repo, &e.id)?.expect("own object");
                objects.push((kind, data, e.id));
            }
            let open = self.open.take().expect("open pack");
            drop(open.file);
            let _ = std::fs::remove_file(&open.path);
            for (kind, data, oid) in objects {
                repo.odb.loose()
                    .write(kind, &data)
                    .map_err(|e| Die::Fatal(e.to_string()))?;
                self.known.insert(oid, Known::Closed(kind));
            }
            return Ok(None);
        }
        open.file
            .write_all_at(&(count as u32).to_be_bytes(), 8)
            .map_err(|e| io_fatal(&open.path, &e))?;
        let mut hasher = Hasher::new(self.hash);
        let mut buf = vec![0u8; 1 << 16];
        let mut pos = 0u64;
        while pos < open.offset {
            let n = ((open.offset - pos) as usize).min(buf.len());
            open.file
                .read_exact_at(&mut buf[..n], pos)
                .map_err(|e| io_fatal(&open.path, &e))?;
            hasher.update(&buf[..n]);
            pos += n as u64;
        }
        let checksum = hasher
            .finalize()
            .map_err(|_| Die::Fatal("SHA-1 collision in pack".into()))?;
        open.file
            .write_all_at(checksum.as_bytes(), open.offset)
            .map_err(|e| io_fatal(&open.path, &e))?;
        let _ = open.file.sync_all();
        let pack_len = open.offset + self.hash.byte_len() as u64;
        for e in &open.entries {
            self.known.insert(e.id, Known::Closed(e.kind));
        }
        let indexed = IndexedPack::from_parts(self.hash, open.entries, checksum, pack_len);
        let idx = indexed
            .index_bytes(IndexVersion::default())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let hex = checksum.to_hex();
        let pack_path = self.pack_dir.join(format!("pack-{hex}.pack"));
        let idx_path = self.pack_dir.join(format!("pack-{hex}.idx"));
        drop(open.file);
        set_read_only(&open.path);
        std::fs::rename(&open.path, &pack_path).map_err(|e| io_fatal(&pack_path, &e))?;
        let tmp_idx = self
            .pack_dir
            .join(format!("tmp_idx_{}_{id}", std::process::id()));
        std::fs::write(&tmp_idx, idx).map_err(|e| io_fatal(&tmp_idx, &e))?;
        set_read_only(&tmp_idx);
        std::fs::rename(&tmp_idx, &idx_path).map_err(|e| io_fatal(&idx_path, &e))?;
        self.packs_written += 1;
        Ok(Some(ClosedPack {
            path: pack_path,
            id,
        }))
    }

    /// Remove the open pack without keeping its objects.
    pub fn abandon(&mut self) {
        if let Some(open) = self.open.take() {
            drop(open.file);
            let _ = std::fs::remove_file(&open.path);
        }
    }
}

#[cfg(unix)]
fn set_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
}

#[cfg(not(unix))]
fn set_read_only(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Positioned reads and writes where std names them differently.
#[cfg(windows)]
trait FileExt {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
    fn write_all_at(&self, buf: &[u8], offset: u64) -> std::io::Result<()>;
}

#[cfg(windows)]
impl FileExt for File {
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt as _;
        while !buf.is_empty() {
            match self.seek_read(buf, offset) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt as _;
        while !buf.is_empty() {
            match self.seek_write(buf, offset) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
