// SPDX-License-Identifier: MIT
//! Where `fast-import` puts objects: a pack file that grows while the
//! stream is read and is closed at `checkpoint`, at the end, when it
//! reaches `--max-pack-size`, or on a crash. A closed pack with fewer
//! objects than `fastimport.unpackLimit` is written as loose objects
//! instead.
//!
//! Objects of the open pack are read back from it (`cat-blob`, `ls`, trees
//! of new commits); all others come from the object database.
//!
//! As with git, blobs are stored as deltas against the previous blob
//! written and trees against the previous version of the same directory,
//! when that object is in the open pack, its delta chain is shorter than
//! `--depth` and the delta is at most the object's size minus the length
//! of an object id. Blobs above the big-file threshold are streamed into
//! the pack whole and are no delta base. As observed with git,
//! `--big-file-threshold` governs only until the first pack is started;
//! from then on `core.bigFileThreshold` does.

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
    pub deltas: u64,
    pub attempts: u64,
}

/// An object written by this process.
#[derive(Debug, Clone, Copy)]
enum Known {
    /// In the open pack: offset and length of the entry, where its
    /// compressed data starts, and for deltas the base and the depth.
    Open {
        offset: u64,
        len: u64,
        kind: ObjectKind,
        data_at: u8,
        base: Option<ObjectId>,
        depth: u32,
    },
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
    /// `--depth`.
    pub max_depth: u32,
    /// Blobs above this many bytes are streamed and not deltified.
    pub big_file_threshold: u64,
    /// `core.bigFileThreshold`, which replaces the threshold when the
    /// first pack is started.
    config_threshold: Option<u64>,
    /// The previous blob written: the base for the next one.
    last_blob: Option<(ObjectId, Vec<u8>)>,
}

/// Takes the pieces of a streamed blob.
pub type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> Result<(), Die>;

/// The default `--depth`.
pub const DEFAULT_DEPTH: u32 = 50;

/// The pack type of an offset delta.
const OFS_DELTA: u8 = 6;

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
fn entry_header(pack_type: u8, size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    let mut c = (u64::from(pack_type) << 4) | (size & 0x0f);
    let mut rest = size >> 4;
    while rest > 0 {
        out.push((c | 0x80) as u8);
        c = rest & 0x7f;
        rest >>= 7;
    }
    out.push(c as u8);
    out
}

/// The distance to the base of an offset delta, as stored after the
/// entry header.
fn ofs_bytes(mut n: u64) -> Vec<u8> {
    let mut out = vec![(n & 0x7f) as u8];
    n >>= 7;
    while n > 0 {
        n -= 1;
        out.push(0x80 | (n & 0x7f) as u8);
        n >>= 7;
    }
    out.reverse();
    out
}

impl Store {
    pub fn new(
        hash: HashKind,
        objects_dir: &Path,
        level: Option<u32>,
        big_file_threshold: u64,
    ) -> Store {
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
            max_depth: DEFAULT_DEPTH,
            big_file_threshold,
            config_threshold: Some(big_file_threshold),
            last_blob: None,
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
        if let Some(r) = self.read_open(oid)? {
            return Ok(Some(r));
        }
        Ok(repo.odb.read(oid).ok().flatten().map(|o| (o.kind, o.data)))
    }

    /// Read `oid` from the open pack, resolving deltas.
    fn read_open(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, Die> {
        let Some(Known::Open {
            offset,
            len,
            kind,
            data_at,
            base,
            ..
        }) = self.known.get(oid).copied()
        else {
            return Ok(None);
        };
        let open = self.open.as_ref().expect("open pack");
        let mut entry = vec![0u8; len as usize];
        open.file
            .read_exact_at(&mut entry, offset)
            .map_err(|e| io_fatal(&open.path, &e))?;
        let mut data = Vec::new();
        flate2::read::ZlibDecoder::new(&entry[usize::from(data_at)..])
            .read_to_end(&mut data)
            .map_err(|e| io_fatal(&open.path, &e))?;
        if let Some(base) = base {
            let Some((_, base_data)) = self.read_open(&base)? else {
                return Err(Die::Fatal(msg::object_not_found(&base.to_hex())));
            };
            data = rgit_pack::apply_delta(&base_data, &data)
                .map_err(|e| Die::Fatal(format!("{}: {e}", open.path.display())))?;
        }
        Ok(Some((kind, data)))
    }

    /// Write `oid` of the open pack as a loose object.
    fn write_loose(&self, repo: &Repo, oid: &ObjectId) -> Result<(), Die> {
        let fatal = |e: rgit_odb::Error| Die::Fatal(e.to_string());
        let Some(Known::Open {
            offset,
            len,
            kind,
            data_at,
            base: None,
            ..
        }) = self.known.get(oid).copied()
        else {
            let (kind, data) = self.read_open(oid)?.expect("own object");
            repo.odb.loose().write(kind, &data).map_err(fatal)?;
            return Ok(());
        };
        let open = self.open.as_ref().expect("open pack");
        let mut header = [0u8; 10];
        let n = usize::from(data_at).min(header.len());
        open.file
            .read_exact_at(&mut header[..n], offset)
            .map_err(|e| io_fatal(&open.path, &e))?;
        let mut size = u64::from(header[0] & 0x0f);
        for (i, b) in header[1..n].iter().enumerate() {
            size |= u64::from(b & 0x7f) << (4 + 7 * i);
        }
        let mut reader = flate2::read::ZlibDecoder::new(PackReader {
            file: &open.file,
            at: offset + u64::from(data_at),
            end: offset + len,
        });
        repo.odb
            .loose()
            .write_stream(kind, size, &mut reader)
            .map_err(fatal)?;
        Ok(())
    }

    /// The delta depth of `oid` when it is in the open pack and a delta
    /// against it stays within `--depth`.
    fn base_depth(&self, oid: &ObjectId) -> Option<u32> {
        match self.known.get(oid) {
            Some(Known::Open { depth, .. }) if *depth < self.max_depth => Some(*depth),
            _ => None,
        }
    }

    fn compress(&self, payload: &[u8]) -> Result<Vec<u8>, Die> {
        let mut z = ZlibEncoder::new(Vec::new(), self.level);
        z.write_all(payload)
            .map_err(|e| io_fatal(&self.pack_dir, &e))?;
        z.finish().map_err(|e| io_fatal(&self.pack_dir, &e))
    }

    /// Store an object unless it exists already; returns its id. Blobs
    /// are deltified against the previous blob, trees against `base`
    /// (the previous version of the directory).
    pub fn store(
        &mut self,
        repo: &Repo,
        kind: ObjectKind,
        data: &[u8],
        base: Option<ObjectId>,
        edges: &mut dyn FnMut(&ClosedPack),
    ) -> Result<ObjectId, Die> {
        let oid = self.object_id(kind, data)?;
        let ki = kind_index(kind);
        // Only objects of this import and of packs count as present (as
        // with git, a loose copy is written again).
        if self.known.contains_key(&oid) || repo.is_packed(&oid) {
            self.counts[ki].duplicates += 1;
            return Ok(oid);
        }
        self.counts[ki].stored += 1;
        let base = match kind {
            ObjectKind::Blob => self.last_blob.as_ref().map(|(b, _)| *b),
            ObjectKind::Tree => base,
            _ => None,
        };
        // (base, depth of the new entry, delta)
        let mut delta: Option<(ObjectId, u32, Vec<u8>)> = None;
        let max = data.len().saturating_sub(self.hash.byte_len());
        if let Some((base, depth)) = base.and_then(|b| Some((b, self.base_depth(&b)?))) {
            if max > 0 {
                self.counts[ki].attempts += 1;
                let made = match &self.last_blob {
                    Some((b, d)) if kind == ObjectKind::Blob && *b == base => {
                        rgit_pack::write::delta(d, data, max)
                    }
                    _ => match self.read_open(&base)? {
                        Some((_, d)) => rgit_pack::write::delta(&d, data, max),
                        None => None,
                    },
                };
                delta = made.map(|d| (base, depth + 1, d));
            }
        }
        let mut body = self.compress(delta.as_ref().map_or(data, |d| &d.2))?;
        // Header and base offset take at most 20 bytes.
        let full = self.open.as_ref().is_some_and(|p| {
            self.max_pack_size > 0
                && !p.entries.is_empty()
                && p.offset + body.len() as u64 + 20 + self.hash.byte_len() as u64
                    > self.max_pack_size
        });
        if full {
            if let Some(closed) = self.close(repo)? {
                edges(&closed);
            }
            // The base went with the closed pack.
            if delta.take().is_some() {
                body = self.compress(data)?;
            }
        }
        if delta.is_some() {
            self.counts[ki].deltas += 1;
        }
        if self.open.is_none() {
            self.start()?;
        }
        let offset = self.open.as_ref().expect("open pack").offset;
        let (mut entry, base, depth) = match &delta {
            Some((base, depth, d)) => {
                let Some(Known::Open { offset: at, .. }) = self.known.get(base) else {
                    unreachable!("delta base in the open pack")
                };
                let mut h = entry_header(OFS_DELTA, d.len() as u64);
                h.extend_from_slice(&ofs_bytes(offset - at));
                (h, Some(*base), *depth)
            }
            None => (entry_header(kind.pack_type(), data.len() as u64), None, 0),
        };
        let data_at = entry.len() as u8;
        entry.extend_from_slice(&body);
        let open = self.open.as_mut().expect("open pack");
        open.file
            .write_all_at(&entry, offset)
            .map_err(|e| io_fatal(&open.path, &e))?;
        open.offset += entry.len() as u64;
        open.entries.push(IndexedObject {
            id: oid,
            kind,
            offset,
            crc32: crc32fast::hash(&entry),
            delta: base.is_some(),
        });
        self.known.insert(
            oid,
            Known::Open {
                offset,
                len: entry.len() as u64,
                kind,
                data_at,
                base,
                depth,
            },
        );
        if kind == ObjectKind::Blob {
            self.last_blob = Some((oid, data.to_vec()));
        }
        Ok(oid)
    }

    /// Store a blob of `len` bytes that `fill` hands over in pieces,
    /// without holding it in memory: it is written whole and is no delta
    /// base. Returns its id.
    pub fn store_stream(
        &mut self,
        repo: &Repo,
        len: u64,
        fill: &mut dyn FnMut(Sink<'_>) -> Result<(), Die>,
        edges: &mut dyn FnMut(&ClosedPack),
    ) -> Result<ObjectId, Die> {
        self.last_blob = None;
        let full = self.open.as_ref().is_some_and(|p| {
            self.max_pack_size > 0 && !p.entries.is_empty() && p.offset + len > self.max_pack_size
        });
        if full {
            if let Some(closed) = self.close(repo)? {
                edges(&closed);
            }
        }
        if self.open.is_none() {
            self.start()?;
        }
        let open = self.open.as_ref().expect("open pack");
        let offset = open.offset;
        let header = entry_header(ObjectKind::Blob.pack_type(), len);
        let mut out = PackWriter {
            file: &open.file,
            at: offset,
            crc: crc32fast::Hasher::new(),
        };
        out.write_all(&header)
            .map_err(|e| io_fatal(&open.path, &e))?;
        let mut hasher = Hasher::new(self.hash);
        hasher.update(format!("blob {len}\0").as_bytes());
        let mut z = ZlibEncoder::new(out, self.level);
        let path = &open.path;
        fill(&mut |chunk: &[u8]| {
            hasher.update(chunk);
            z.write_all(chunk).map_err(|e| io_fatal(path, &e))
        })?;
        let out = z.finish().map_err(|e| io_fatal(&open.path, &e))?;
        let (end, crc) = (out.at, out.crc.finalize());
        let oid = hasher
            .finalize()
            .map_err(|_| Die::Fatal("SHA-1 collision in blob".into()))?;
        let counts = &mut self.counts[kind_index(ObjectKind::Blob)];
        if self.known.contains_key(&oid) || repo.is_packed(&oid) {
            // The next entry overwrites what was written.
            counts.duplicates += 1;
            return Ok(oid);
        }
        counts.stored += 1;
        let open = self.open.as_mut().expect("open pack");
        open.offset = end;
        open.entries.push(IndexedObject {
            id: oid,
            kind: ObjectKind::Blob,
            offset,
            crc32: crc,
            delta: false,
        });
        self.known.insert(
            oid,
            Known::Open {
                offset,
                len: end - offset,
                kind: ObjectKind::Blob,
                data_at: header.len() as u8,
                base: None,
                depth: 0,
            },
        );
        Ok(oid)
    }

    fn start(&mut self) -> Result<(), Die> {
        if let Some(t) = self.config_threshold.take() {
            self.big_file_threshold = t;
        }
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
            // Write everything loose before the pack goes away; whole
            // entries are inflated while being written.
            let entries = std::mem::take(&mut open.entries);
            self.open = Some(open);
            for e in &entries {
                self.write_loose(repo, &e.id)?;
            }
            for e in &entries {
                self.known.insert(e.id, Known::Closed(e.kind));
            }
            let open = self.open.take().expect("open pack");
            drop(open.file);
            let _ = std::fs::remove_file(&open.path);
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
        let pack_len = open.offset + self.hash.byte_len() as u64;
        // A streamed duplicate may have left bytes behind the trailer.
        open.file
            .set_len(pack_len)
            .map_err(|e| io_fatal(&open.path, &e))?;
        let _ = open.file.sync_all();
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

/// Reads a range of the open pack.
struct PackReader<'a> {
    file: &'a File,
    at: u64,
    end: u64,
}

impl Read for PackReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = usize::try_from(self.end - self.at)
            .unwrap_or(usize::MAX)
            .min(buf.len())
            .min(1 << 20);
        if n == 0 {
            return Ok(0);
        }
        self.file.read_exact_at(&mut buf[..n], self.at)?;
        self.at += n as u64;
        Ok(n)
    }
}

/// Writes at an increasing offset of the open pack, keeping the CRC.
struct PackWriter<'a> {
    file: &'a File,
    at: u64,
    crc: crc32fast::Hasher,
}

impl Write for PackWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write_all_at(buf, self.at)?;
        self.crc.update(buf);
        self.at += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
