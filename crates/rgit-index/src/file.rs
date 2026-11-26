// SPDX-License-Identifier: MIT

//! Reading and writing the index file on disk.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rgit_fs::{LockError, LockFile, LockOptions};
use rgit_hash::HashKind;

use crate::{Error, Index, ReadOptions, WriteOptions};

/// Environment variable naming an alternative index file.
pub const INDEX_FILE_ENV: &str = "GIT_INDEX_FILE";

/// The index file of a repository: `$GIT_INDEX_FILE` if set (taken as is,
/// relative to the current directory), else `<git_dir>/index`.
pub fn index_path(git_dir: &Path) -> PathBuf {
    match std::env::var_os(INDEX_FILE_ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => git_dir.join("index"),
    }
}

/// Called with the path of every index file written by
/// [`Index::write_file`] (or reported with [`notify_written`]); a program
/// sets it once, e.g. to run the `post-index-change` hook.
static WRITE_OBSERVER: std::sync::OnceLock<fn(&Path)> = std::sync::OnceLock::new();

/// Sets the function told about every index file written; only the first
/// call has an effect.
pub fn set_write_observer(observer: fn(&Path)) {
    let _ = WRITE_OBSERVER.set(observer);
}

/// Tells the observer (see [`set_write_observer`]) that the index file at
/// `path` was written, for callers that commit a lock file themselves
/// after [`Index::write_locked`].
pub fn notify_written(path: &Path) {
    if let Some(f) = WRITE_OBSERVER.get() {
        f(path);
    }
}

/// Failure to write the index file.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// `index.lock` could not be taken.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Writing or renaming the lock file failed.
    #[error("unable to write new index file: {0}")]
    Io(#[from] io::Error),
}

impl Index {
    /// Reads the index file at `path`. A missing file is an empty index
    /// (version 2), as in git. The shared index of a split index is looked
    /// for next to it.
    pub fn read_file(
        path: impl AsRef<Path>,
        kind: HashKind,
        opts: &ReadOptions,
    ) -> Result<Index, Error> {
        Index::read_file_in(path, kind, opts, None)
    }

    /// [`Index::read_file`], also looking for the shared index of a split
    /// index in `git_dir`.
    pub fn read_file_in(
        path: impl AsRef<Path>,
        kind: HashKind,
        opts: &ReadOptions,
        git_dir: Option<&Path>,
    ) -> Result<Index, Error> {
        let path = path.as_ref();
        match FileData::open(path) {
            Ok(data) => crate::read::parse_split(data.as_slice(), kind, opts, &mut |id| {
                load_shared(path, git_dir, id)
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Index::new(kind)),
            Err(source) => Err(Error::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the index to `path` through `<path>.lock`: the lock is taken,
    /// the new content written and renamed over `path`. On error the lock
    /// is rolled back. A split index writes its shared index next to it.
    pub fn write_file(
        &self,
        path: impl AsRef<Path>,
        lock: &LockOptions,
        opts: &WriteOptions,
    ) -> Result<(), WriteError> {
        let path = path.as_ref();
        let mut file = LockFile::acquire(path, lock)?;
        file.write_all(&self.file_bytes(path, lock, opts))?;
        file.commit()?;
        notify_written(path);
        Ok(())
    }

    /// Writes the index into an already acquired lock file without
    /// committing it, for callers that hold `index.lock` across a longer
    /// operation.
    pub fn write_locked(&self, file: &mut LockFile, opts: &WriteOptions) -> io::Result<()> {
        let target = file.target().to_path_buf();
        let bytes = self.file_bytes(&target, &LockOptions::default(), opts);
        file.write_all(&bytes)
    }

    /// The content of the index file at `path`; for a split index the
    /// shared index is written (or freshened) first. When it cannot be
    /// written the index is written whole, as git does.
    fn file_bytes(&self, path: &Path, lock: &LockOptions, opts: &WriteOptions) -> Vec<u8> {
        use crate::split::Plan;
        use crate::write::{Content, write};
        let dir = path.parent().unwrap_or(Path::new("."));
        match crate::split::plan(self, opts) {
            Plan::Whole => self.to_bytes(opts),
            Plan::Reuse => {
                let split = self.split.as_ref().expect("planned from a split index");
                freshen(&dir.join(shared_name(&split.base_id)));
                let (own, link) = crate::split::own_entries(self, split);
                write(self, opts, Content::Split(own, link))
            }
            Plan::NewShared => {
                let shared = write(self, opts, Content::Shared);
                let n = self.kind.byte_len();
                let Ok(id) =
                    rgit_hash::ObjectId::from_bytes(self.kind, &shared[shared.len() - n..])
                else {
                    return self.to_bytes(opts);
                };
                let name = dir.join(shared_name(&id));
                let written = LockFile::acquire(&name, lock)
                    .map_err(|_| ())
                    .and_then(|mut f| {
                        f.write_all(&shared).map_err(|_| ())?;
                        f.commit().map_err(|_| ())
                    });
                if written.is_err() {
                    return self.to_bytes(opts);
                }
                expire_shared(dir, &name, opts.split.expire_before);
                let mut link = id.as_bytes().to_vec();
                crate::ewah::encode(&[], &mut link);
                crate::ewah::encode(&[], &mut link);
                write(self, opts, Content::Split(Vec::new(), link))
            }
        }
    }
}

/// The file name of the shared index with checksum `id`.
pub fn shared_index_name(id: &rgit_hash::ObjectId) -> String {
    format!("sharedindex.{}", id.to_hex())
}

fn shared_name(id: &rgit_hash::ObjectId) -> String {
    shared_index_name(id)
}

/// Marks a shared index as used now (its mtime decides when it expires).
fn freshen(path: &Path) {
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Removes the shared indexes in `dir` other than `keep` that were last
/// modified before `before` (seconds since the epoch; 0 keeps all).
fn expire_shared(dir: &Path, keep: &Path, before: u64) {
    if before == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path == keep
            || !e
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"sharedindex.")
        {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|d| d.as_secs() < before || before == u64::MAX);
        if old {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The shared index `id` of the split index at `index`: next to it, else
/// in `git_dir`.
fn load_shared(
    index: &Path,
    git_dir: Option<&Path>,
    id: &rgit_hash::ObjectId,
) -> Result<Vec<u8>, Error> {
    let name = shared_name(id);
    let mut tried = vec![index.parent().unwrap_or(Path::new(".")).join(&name)];
    if let Some(g) = git_dir {
        tried.push(g.join(&name));
    }
    let mut last = None;
    for p in &tried {
        match std::fs::read(p) {
            Ok(data) => {
                freshen(p);
                return Ok(data);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(Error::SharedIndexOpen {
        path: tried[0].display().to_string(),
        source: last.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)),
    })
}

/// Index files at least this large are memory mapped instead of read.
const MAP_MIN: u64 = 64 << 10;

/// The bytes of an index file: memory mapped when it is large, else read.
enum FileData {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl FileData {
    fn open(path: &Path) -> io::Result<FileData> {
        let mut file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        if len >= MAP_MIN {
            // SAFETY: the mapping is read-only and private to this process
            // and only lives while the index is parsed. Index files are
            // never changed in place: git and rgit write `index.lock` and
            // rename it over the index, which leaves this mapping intact.
            // The parser bounds-checks every offset against the length.
            if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
                return Ok(FileData::Mapped(map));
            }
        }
        let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
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
