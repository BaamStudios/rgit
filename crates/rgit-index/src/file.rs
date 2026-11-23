// SPDX-License-Identifier: MIT

//! Reading and writing the index file on disk.

use std::io::{self, Write};
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
    /// (version 2), as in git.
    pub fn read_file(
        path: impl AsRef<Path>,
        kind: HashKind,
        opts: &ReadOptions,
    ) -> Result<Index, Error> {
        let path = path.as_ref();
        match std::fs::read(path) {
            Ok(data) => Index::from_bytes(&data, kind, opts),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Index::new(kind)),
            Err(source) => Err(Error::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Writes the index to `path` through `<path>.lock`: the lock is taken,
    /// the new content written and renamed over `path`. On error the lock
    /// is rolled back.
    pub fn write_file(
        &self,
        path: impl AsRef<Path>,
        lock: &LockOptions,
        opts: &WriteOptions,
    ) -> Result<(), WriteError> {
        let path = path.as_ref();
        let mut file = LockFile::acquire(path, lock)?;
        file.write_all(&self.to_bytes(opts))?;
        file.commit()?;
        notify_written(path);
        Ok(())
    }

    /// Writes the index into an already acquired lock file without
    /// committing it, for callers that hold `index.lock` across a longer
    /// operation.
    pub fn write_locked(&self, file: &mut LockFile, opts: &WriteOptions) -> io::Result<()> {
        file.write_all(&self.to_bytes(opts))
    }
}
