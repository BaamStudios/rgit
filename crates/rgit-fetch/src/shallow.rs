// SPDX-License-Identifier: MIT
//! The `shallow` file (`Documentation/technical/shallow.adoc`): one
//! commit id per line; these commits are treated as having no parents.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use rgit_hash::{HashKind, ObjectId};
use rgit_protocol::v2::ShallowInfo;

use crate::Result;

/// The `shallow` file of the repository with common directory
/// `common_dir`: `$GIT_SHALLOW_FILE` if set, else `<common_dir>/shallow`.
pub fn file_for(common_dir: &Path) -> std::path::PathBuf {
    match std::env::var_os("GIT_SHALLOW_FILE").filter(|v| !v.is_empty()) {
        Some(f) => std::path::PathBuf::from(f),
        None => common_dir.join("shallow"),
    }
}

/// The commits listed in `path` (none if it does not exist).
pub fn read(path: &Path, hash: HashKind) -> Vec<ObjectId> {
    let Ok(text) = std::fs::read(path) else {
        return Vec::new();
    };
    text.split(|&b| b == b'\n')
        .filter_map(|l| ObjectId::from_hex(hash, l.trim_ascii()).ok())
        .collect()
}

/// Write `commits` to `path` (sorted, through `<path>.lock`); an empty set
/// removes the file.
pub fn write(path: &Path, commits: &BTreeSet<ObjectId>) -> Result<()> {
    if commits.is_empty() {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    let mut text = String::new();
    for c in commits {
        text.push_str(&c.to_hex());
        text.push('\n');
    }
    let mut lock = rgit_fs::LockFile::acquire(path, &rgit_fs::LockOptions::default())
        .map_err(|e| crate::Error::fatal(e.to_string()))?;
    lock.write_all(text.as_bytes())?;
    lock.commit()
        .map_err(|e| crate::Error::fatal(e.to_string()))?;
    Ok(())
}

/// `current` with the `shallow` lines of `info` added and its `unshallow`
/// lines removed.
pub fn apply(current: &[ObjectId], info: &ShallowInfo) -> BTreeSet<ObjectId> {
    let mut set: BTreeSet<ObjectId> = current.iter().copied().collect();
    set.extend(info.shallow.iter().copied());
    for id in &info.unshallow {
        set.remove(id);
    }
    set
}
