// SPDX-License-Identifier: MIT
//! Large files (`core.bigFileThreshold`, Documentation/config/core.adoc).
//!
//! Files above the threshold that need no content conversion are hashed
//! and stored as a stream: `git add`, `git update-index` and `git
//! hash-object -w` put them deflated into a pack of their own (see
//! [`rgit_odb::ObjectDatabase::write_bulk`]) instead of reading them into
//! memory. Blobs above the threshold are streamed to the work tree and to
//! standard output.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use rgit_hash::{HashKind, ObjectId, object_hasher};
use rgit_object::ObjectKind;
use rgit_odb::ObjectDatabase;

/// Why a large file could not be hashed or stored.
#[derive(Debug)]
pub enum FileError {
    /// Opening or reading the file failed.
    Read(io::Error),
    /// Storing the object failed.
    Odb(rgit_odb::Error),
}

/// The blob id of the regular file `path` of `size` bytes, read as a
/// stream; with `write` the blob is stored (in the pack of large objects).
pub fn blob_from_file(
    odb: &ObjectDatabase,
    hash: HashKind,
    path: &Path,
    size: u64,
    write: bool,
) -> Result<ObjectId, FileError> {
    let mut file = File::open(path).map_err(FileError::Read)?;
    if write {
        return odb
            .write_bulk(ObjectKind::Blob, size, &mut file)
            .map_err(|e| match e {
                rgit_odb::Error::Io { source, .. }
                    if source.kind() == io::ErrorKind::UnexpectedEof =>
                {
                    FileError::Read(source)
                }
                e => FileError::Odb(e),
            });
    }
    hash_stream(hash, ObjectKind::Blob, size, &mut file).map_err(FileError::Read)
}

/// The blob id of the regular file `path` of `size` bytes, read as a
/// stream, without a repository to store it in.
pub fn blob_from_file_unstored(
    hash: HashKind,
    path: &Path,
    size: u64,
) -> Result<ObjectId, FileError> {
    let mut file = File::open(path).map_err(FileError::Read)?;
    hash_stream(hash, ObjectKind::Blob, size, &mut file).map_err(FileError::Read)
}

/// The id of an object of `size` bytes read from `reader`.
pub fn hash_stream(
    hash: HashKind,
    kind: ObjectKind,
    size: u64,
    reader: &mut dyn Read,
) -> io::Result<ObjectId> {
    let mut h = object_hasher(hash, kind.name(), size);
    let mut buf = vec![0u8; 64 * 1024];
    let mut left = size;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = match reader.read(&mut buf[..want]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "file changed while it was read",
                ));
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        h.update(&buf[..n]);
        left -= n as u64;
    }
    h.finalize().map_err(|e| io::Error::other(e.to_string()))
}

/// Whether the work tree file `meta` of a path whose content is not
/// converted is streamed instead of read into memory.
pub fn streams_file(odb: &ObjectDatabase, meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_file() && odb.is_big(meta.len())
}
