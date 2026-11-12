// SPDX-License-Identifier: MIT

//! Loose objects: one zlib-compressed file per object at
//! `objects/<first two hex digits>/<remaining hex digits>`, holding
//! `<type> <size>\0<content>` (see `gitformat-loose`).
//!
//! Compressed bytes written by rgit may differ from git's (a different
//! deflate implementation and settings); the inflated bytes, the path and
//! the object id are identical, and both read each other's objects.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_fs::{OBJECT_PREFIX, SharedRepository, TempFile, harden};
use rgit_hash::{HashKind, Hasher, ObjectId, Prefix, object_hasher};
use rgit_object::{Error as ObjError, ObjectHeader, ObjectKind};

use crate::store::{ObjectStore, RawObject};
use crate::zlib::{InflateError, Inflater};
use crate::{Error, LooseOptions};

/// git looks for the NUL of a loose object header within this many bytes.
const MAX_LOOSE_HEADER: usize = 32;

/// Never preallocate more than this for an object, whatever its header claims.
const MAX_PREALLOC: u64 = 64 << 20;

/// The loose objects of one `objects` directory.
#[derive(Debug)]
pub struct LooseStore {
    dir: PathBuf,
    hash: HashKind,
    options: LooseOptions,
}

impl LooseStore {
    /// The loose store in `objects_dir` (normally `$GIT_DIR/objects`).
    pub fn new(
        objects_dir: impl Into<PathBuf>,
        hash: HashKind,
        options: LooseOptions,
    ) -> LooseStore {
        LooseStore {
            dir: objects_dir.into(),
            hash,
            options,
        }
    }

    /// The `objects` directory.
    pub fn objects_dir(&self) -> &Path {
        &self.dir
    }

    /// The write settings.
    pub fn options(&self) -> &LooseOptions {
        &self.options
    }

    /// The path of `oid`'s file (whether it exists or not).
    pub fn path_of(&self, oid: &ObjectId) -> PathBuf {
        let hex = oid.to_hex();
        self.dir.join(&hex[..2]).join(&hex[2..])
    }

    /// Whether the file for `oid` exists.
    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.path_of(oid).is_file()
    }

    fn open_file(&self, oid: &ObjectId) -> Result<Option<(File, PathBuf)>, Error> {
        let path = self.path_of(oid);
        match File::open(&path) {
            Ok(f) => Ok(Some((f, path))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(path.display().to_string(), e)),
        }
    }

    /// Type and size of `oid`, inflating only the start of the file.
    pub fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        let Some((file, path)) = self.open_file(oid)? else {
            return Ok(None);
        };
        if file.metadata().map(|m| m.len() == 0).unwrap_or(false) {
            return Err(Error::EmptyFile { oid: *oid, path });
        }
        let mut inf = Inflater::new(BufReader::with_capacity(4096, file));
        let (header, _) = read_header(&mut inf, oid, &path)?;
        Ok(Some(header))
    }

    /// The whole object. The content must have exactly the size given in the
    /// header; with [`LooseOptions::check_hash`] it must also hash to `oid`.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        let path = self.path_of(oid);
        let compressed = match fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(path.display().to_string(), e)),
        };
        let (obj, _) = self.inflate_all(oid, &path, &compressed)?;
        if self.options.check_hash {
            self.check_hash(oid, &path, &obj)?;
        }
        Ok(Some(obj))
    }

    fn check_hash(&self, oid: &ObjectId, path: &Path, obj: &RawObject) -> Result<(), Error> {
        let mut h = object_hasher(self.hash, obj.kind.name(), obj.data.len() as u64);
        h.update(&obj.data);
        let actual = h.finalize()?;
        if actual != *oid {
            return Err(Error::HashMismatch {
                oid: *oid,
                actual,
                path: path.to_owned(),
            });
        }
        Ok(())
    }

    /// Inflate a whole loose object; also says whether bytes follow the
    /// zlib stream.
    fn inflate_all(
        &self,
        oid: &ObjectId,
        path: &Path,
        compressed: &[u8],
    ) -> Result<(RawObject, bool), Error> {
        if compressed.is_empty() {
            return Err(Error::EmptyFile {
                oid: *oid,
                path: path.to_owned(),
            });
        }
        let mut inf = Inflater::new(compressed);
        let (header, mut data) = read_header(&mut inf, oid, path)?;
        data.reserve(header.size.min(MAX_PREALLOC) as usize);
        read_content(&mut inf, oid, path, header.size, &mut data)?;
        let trailing = inf
            .has_trailing_input()
            .map_err(|e| content_error(e, oid, path))?;
        Ok((
            RawObject {
                kind: header.kind,
                data,
            },
            trailing,
        ))
    }

    /// Open `oid` for streaming its content (for large blobs). The reader
    /// fails with an [`io::Error`] wrapping an [`Error`] when the object is
    /// damaged.
    pub fn open(&self, oid: &ObjectId) -> Result<Option<LooseReader>, Error> {
        let Some((file, path)) = self.open_file(oid)? else {
            return Ok(None);
        };
        if file.metadata().map(|m| m.len() == 0).unwrap_or(false) {
            return Err(Error::EmptyFile { oid: *oid, path });
        }
        let mut inf = Inflater::new(BufReader::new(file));
        let (header, pending) = read_header(&mut inf, oid, &path)?;
        Ok(Some(LooseReader {
            inf,
            header,
            pending,
            pending_pos: 0,
            produced: 0,
            oid: *oid,
            path,
        }))
    }

    /// Check one loose object the way `git fsck` does: it must inflate
    /// cleanly, have no bytes after the zlib stream, have the size from its
    /// header, and hash to the name it is stored under.
    ///
    /// A [`Error::HashMismatch`] is printed by fsck as
    /// [`crate::messages::hash_path_mismatch`].
    pub fn verify(&self, oid: &ObjectId) -> Result<RawObject, Error> {
        let path = self.path_of(oid);
        let compressed = fs::read(&path).map_err(|e| Error::io(path.display().to_string(), e))?;
        let (obj, trailing) = self.inflate_all(oid, &path, &compressed)?;
        if trailing {
            return Err(Error::TrailingGarbage { oid: *oid, path });
        }
        self.check_hash(oid, &path, &obj)?;
        Ok(obj)
    }

    /// Call `f` for every loose object whose id starts with `prefix`.
    pub fn for_each_prefix_match(
        &self,
        prefix: &Prefix,
        f: &mut dyn FnMut(ObjectId),
    ) -> Result<(), Error> {
        if prefix.kind() != self.hash {
            return Ok(());
        }
        let hex = prefix.to_string();
        if hex.len() >= 2 {
            self.scan_dir(&hex[..2], &mut |oid| {
                if oid.starts_with(prefix) {
                    f(oid);
                }
            })
        } else {
            for d in 0..16u8 {
                let dir = format!("{hex}{:x}", d);
                self.scan_dir(&dir, &mut |oid| {
                    if oid.starts_with(prefix) {
                        f(oid);
                    }
                })?;
            }
            Ok(())
        }
    }

    /// Call `f` for every loose object id.
    pub fn for_each_object(&self, f: &mut dyn FnMut(ObjectId)) -> Result<(), Error> {
        for d in 0..=255u8 {
            self.scan_dir(&format!("{d:02x}"), f)?;
        }
        Ok(())
    }

    /// List `objects/<fan>/`; entries that are not object names are skipped.
    fn scan_dir(&self, fan: &str, f: &mut dyn FnMut(ObjectId)) -> Result<(), Error> {
        let dir = self.dir.join(fan);
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(Error::io(dir.display().to_string(), e)),
        };
        let rest_len = self.hash.hex_len() - 2;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(dir.display().to_string(), e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.len() != rest_len
                || !name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                continue;
            }
            let full = format!("{fan}{name}");
            if let Ok(oid) = ObjectId::from_hex(self.hash, full.as_bytes()) {
                f(oid);
            }
        }
        Ok(())
    }

    /// Update the modification time of `oid`'s file, if it exists.
    pub fn freshen(&self, oid: &ObjectId) -> bool {
        let path = self.path_of(oid);
        match File::open(&path) {
            Ok(f) => {
                // Like git, a failure to update the time is not fatal; the
                // object is there.
                let _ = f.set_modified(std::time::SystemTime::now());
                true
            }
            Err(_) => false,
        }
    }

    /// Write an object unless its file exists already (then only its
    /// modification time is refreshed). Returns the object id.
    pub fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, Error> {
        let mut h = object_hasher(self.hash, kind.name(), data.len() as u64);
        h.update(data);
        let oid = h.finalize()?;
        if self.freshen(&oid) {
            return Ok(oid);
        }
        self.write_new(oid, kind, data.len() as u64, &mut &data[..], None)?;
        Ok(oid)
    }

    /// Write an object of known `size` read from `reader` without holding
    /// it in memory; the id is computed while writing.
    pub fn write_stream(
        &self,
        kind: ObjectKind,
        size: u64,
        reader: &mut dyn Read,
    ) -> Result<ObjectId, Error> {
        let hasher = object_hasher(self.hash, kind.name(), size);
        self.write_new(self.hash.null(), kind, size, reader, Some(hasher))
    }

    /// Compress into a temp file in `objects/`, then rename into place.
    /// With `hasher`, `oid` is unknown and computed from the data.
    fn write_new(
        &self,
        oid: ObjectId,
        kind: ObjectKind,
        size: u64,
        reader: &mut dyn Read,
        mut hasher: Option<Hasher>,
    ) -> Result<ObjectId, Error> {
        let dir_err = |e| Error::io(self.dir.display().to_string(), e);
        let mut tmp = TempFile::with_mode(&self.dir, OBJECT_PREFIX, 0o444).map_err(dir_err)?;
        let tmp_err = |e: io::Error| Error::io(tmp_display(&self.dir), e);
        {
            let mut z =
                ZlibEncoder::new(tmp.file_mut(), Compression::new(self.options.compression));
            let header = ObjectHeader { kind, size }.to_bytes();
            z.write_all(&header).map_err(tmp_err)?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut left = size;
            while left > 0 {
                let want = left.min(buf.len() as u64) as usize;
                let n = reader.read(&mut buf[..want]).map_err(tmp_err)?;
                if n == 0 {
                    return Err(tmp_err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "short read while writing object",
                    )));
                }
                if let Some(h) = hasher.as_mut() {
                    h.update(&buf[..n]);
                }
                z.write_all(&buf[..n]).map_err(tmp_err)?;
                left -= n as u64;
            }
            z.finish().map_err(tmp_err)?;
        }
        let oid = match hasher {
            Some(h) => {
                let oid = h.finalize()?;
                if self.freshen(&oid) {
                    return Ok(oid);
                }
                oid
            }
            None => oid,
        };
        if let Some(method) = self.options.fsync {
            harden(tmp.file_mut(), method).map_err(tmp_err)?;
        }
        if self.options.shared != SharedRepository::Umask {
            set_shared_file_mode(tmp.path(), self.options.shared).map_err(tmp_err)?;
        }
        let dest = self.path_of(&oid);
        let fan = dest.parent().expect("object path has a fan-out directory");
        create_fanout_dir(fan, self.options.shared)
            .map_err(|e| Error::io(fan.display().to_string(), e))?;
        tmp.persist(&dest)
            .map_err(|e| Error::io(dest.display().to_string(), e))?;
        Ok(oid)
    }
}

fn tmp_display(dir: &Path) -> String {
    format!("{}/{OBJECT_PREFIX}XXXXXX", dir.display())
}

/// Loose object files are read-only: `core.sharedRepository` widens read
/// access but never adds write bits.
fn set_shared_file_mode(path: &Path, shared: SharedRepository) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let current = fs::metadata(path)?.permissions().mode() & 0o777;
        let mode = shared.adjust_file_mode(current) & !0o222;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, shared);
    Ok(())
}

/// Create `objects/xx`. In a shared repository it gets the group (and other)
/// permissions of the owner and the set-group-id bit, as git does.
fn create_fanout_dir(dir: &Path, shared: SharedRepository) -> io::Result<()> {
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    if shared != SharedRepository::Umask {
        use std::os::unix::fs::PermissionsExt;
        let current = fs::metadata(dir)?.permissions().mode() & 0o777;
        let mut mode = shared.adjust_file_mode(current) & 0o777;
        if mode & 0o070 != 0 {
            mode |= 0o2000;
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = shared;
    Ok(())
}

/// Inflate the header. Returns it and the content bytes inflated with it.
fn read_header<R: Read>(
    inf: &mut Inflater<R>,
    oid: &ObjectId,
    path: &Path,
) -> Result<(ObjectHeader, Vec<u8>), Error> {
    let mut buf = [0u8; MAX_LOOSE_HEADER];
    let mut n = 0;
    let nul = loop {
        if let Some(p) = buf[..n].iter().position(|&b| b == 0) {
            break p;
        }
        if n == MAX_LOOSE_HEADER {
            return Err(Error::HeaderTooLong { oid: *oid });
        }
        match inf.read(&mut buf[n..]) {
            Ok(0) => return Err(Error::HeaderTooLong { oid: *oid }),
            Ok(k) => n += k,
            Err(InflateError::Data(zlib)) => return Err(Error::InflateHeader { oid: *oid, zlib }),
            Err(InflateError::Truncated) => return Err(Error::HeaderTooLong { oid: *oid }),
            Err(InflateError::Io(e)) => return Err(Error::io(path.display().to_string(), e)),
        }
    };
    let header = match ObjectHeader::parse(&buf[..=nul]) {
        Ok((h, _)) => h,
        Err(ObjError::UnknownKind(found)) => return Err(Error::InvalidType { oid: *oid, found }),
        Err(_) => return Err(Error::BadHeader { oid: *oid }),
    };
    Ok((header, buf[nul + 1..n].to_vec()))
}

fn content_error(e: InflateError, oid: &ObjectId, path: &Path) -> Error {
    match e {
        InflateError::Data(zlib) => Error::InflateContent {
            oid: *oid,
            path: path.to_owned(),
            zlib,
        },
        InflateError::Truncated => Error::Truncated {
            oid: *oid,
            path: path.to_owned(),
        },
        InflateError::Io(e) => Error::io(path.display().to_string(), e),
    }
}

/// Inflate the rest of the content into `data` (which may already hold a
/// first part) and check that the stream ends after exactly `size` bytes.
fn read_content<R: Read>(
    inf: &mut Inflater<R>,
    oid: &ObjectId,
    path: &Path,
    size: u64,
    data: &mut Vec<u8>,
) -> Result<(), Error> {
    let mismatch = |actual: u64| Error::SizeMismatch {
        oid: *oid,
        path: path.to_owned(),
        expected: size,
        actual,
    };
    if data.len() as u64 > size {
        return Err(mismatch(data.len() as u64));
    }
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = inf
            .read(&mut chunk)
            .map_err(|e| content_error(e, oid, path))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
        if data.len() as u64 > size {
            return Err(mismatch(data.len() as u64));
        }
    }
    if (data.len() as u64) < size {
        return Err(mismatch(data.len() as u64));
    }
    Ok(())
}

/// Streaming access to a loose object's content.
pub struct LooseReader {
    inf: Inflater<BufReader<File>>,
    header: ObjectHeader,
    pending: Vec<u8>,
    pending_pos: usize,
    produced: u64,
    oid: ObjectId,
    path: PathBuf,
}

impl LooseReader {
    /// Type and size from the header.
    pub fn header(&self) -> ObjectHeader {
        self.header
    }

    fn fail(&self, e: Error) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

impl std::fmt::Debug for LooseReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LooseReader")
            .field("oid", &self.oid)
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

impl Read for LooseReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let n = if self.pending_pos < self.pending.len() {
            let n = out.len().min(self.pending.len() - self.pending_pos);
            out[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
            self.pending_pos += n;
            n
        } else {
            self.inf
                .read(out)
                .map_err(|e| self.fail(content_error(e, &self.oid, &self.path)))?
        };
        self.produced += n as u64;
        let size = self.header.size;
        if self.produced > size || (n == 0 && self.produced < size) {
            return Err(self.fail(Error::SizeMismatch {
                oid: self.oid,
                path: self.path.clone(),
                expected: size,
                actual: self.produced,
            }));
        }
        Ok(n)
    }
}

impl ObjectStore for LooseStore {
    fn hash_kind(&self) -> HashKind {
        self.hash
    }

    fn contains(&self, oid: &ObjectId) -> bool {
        LooseStore::contains(self, oid)
    }

    fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        LooseStore::header(self, oid)
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        LooseStore::read(self, oid)
    }

    fn for_each_prefix_match(
        &self,
        prefix: &Prefix,
        f: &mut dyn FnMut(ObjectId),
    ) -> Result<(), Error> {
        LooseStore::for_each_prefix_match(self, prefix, f)
    }

    fn for_each_object(&self, f: &mut dyn FnMut(ObjectId)) -> Result<(), Error> {
        LooseStore::for_each_object(self, f)
    }

    fn freshen(&self, oid: &ObjectId) -> bool {
        LooseStore::freshen(self, oid)
    }
}
