// SPDX-License-Identifier: MIT

//! Writing index entries to the work tree and removing them.
//!
//! Nothing is ever written through a symbolic link: leading directories
//! are checked with `lstat` and a non-directory in the way is removed
//! (or reported), and the file itself is created with `O_EXCL` after
//! whatever was at its place has been removed.

use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::Entry;

use crate::messages as msg;

/// Content conversion applied to regular files on their way into the work
/// tree (line endings, `ident`, `working-tree-encoding`; filter drivers
/// are #112). [`Identity`] leaves the blob unchanged.
pub trait Filter {
    /// The work tree form of `data`, the blob `id` of the index entry at
    /// `path`.
    fn to_worktree(&self, path: &[u8], id: &ObjectId, data: Vec<u8>) -> Vec<u8>;
}

/// The filter that changes nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Identity;

impl Filter for Identity {
    fn to_worktree(&self, _path: &[u8], _id: &ObjectId, data: Vec<u8>) -> Vec<u8> {
        data
    }
}

/// Failure to write one entry.
#[derive(Debug, thiserror::Error)]
pub enum CheckoutError {
    /// A leading directory is blocked by a file (only without `force`).
    #[error("{}", msg::cannot_create_directory(.path, .error))]
    LeadingDirectory {
        /// The leading directory, as shown.
        path: String,
        /// `strerror` text.
        error: String,
    },
    /// Creating or writing the file failed.
    #[error("{}", msg::unable_to_create_file(.path, .error))]
    CreateFile {
        /// The path, as shown.
        path: String,
        /// `strerror` text.
        error: String,
    },
    /// Creating the symbolic link failed.
    #[error("{}", msg::unable_to_create_symlink(.path, .error))]
    CreateSymlink {
        /// The path, as shown.
        path: String,
        /// `strerror` text.
        error: String,
    },
}

pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

pub(crate) fn join(root: &Path, rel: &[u8]) -> PathBuf {
    root.join(path_from_bytes(rel))
}

pub(crate) fn lossy(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

/// The text of an I/O error as `strerror` prints it.
pub(crate) fn strerror(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Makes every leading directory of `rel` (relative to `root`) a real
/// directory. A file or symbolic link in the way is removed when `force`
/// is set, and reported otherwise.
///
/// # Errors
/// [`CheckoutError::LeadingDirectory`].
pub fn create_leading_dirs(root: &Path, rel: &[u8], force: bool) -> Result<(), CheckoutError> {
    for (i, &b) in rel.iter().enumerate() {
        if b != b'/' {
            continue;
        }
        let dir = &rel[..i];
        let full = join(root, dir);
        let fail = |e: &io::Error| CheckoutError::LeadingDirectory {
            path: lossy(dir),
            error: strerror(e),
        };
        match fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() => continue,
            Ok(_) => {
                if !force {
                    return Err(CheckoutError::LeadingDirectory {
                        path: lossy(dir),
                        error: "File exists".into(),
                    });
                }
                fs::remove_file(&full).map_err(|e| fail(&e))?;
            }
            Err(_) => {}
        }
        fs::create_dir(&full).map_err(|e| fail(&e))?;
    }
    Ok(())
}

/// Removes whatever is at `full` (a file, symbolic link or directory tree).
fn remove_any(full: &Path) -> io::Result<()> {
    match fs::symlink_metadata(full) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(full),
        Ok(_) => fs::remove_file(full),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Writes `entry` to `rel` below `root` (`rel` is the entry's path, or the
/// path with a `--prefix`): leading directories are created (replacing
/// files in the way), whatever is at `rel` is removed, and the file,
/// symbolic link (a regular file holding the target when `symlinks` is
/// false) or, for a gitlink, an empty directory is created. `data` is the
/// content (already filtered) or the link target.
///
/// Returns the `lstat` data of the new file; `None` for a gitlink.
///
/// # Errors
/// When a directory, file or link cannot be created.
pub fn write_entry(
    root: &Path,
    rel: &[u8],
    entry: &Entry,
    data: &[u8],
    symlinks: bool,
) -> Result<Option<Metadata>, CheckoutError> {
    create_leading_dirs(root, rel, true)?;
    let full = join(root, rel);
    let create_err = |e: &io::Error| CheckoutError::CreateFile {
        path: lossy(rel),
        error: strerror(e),
    };
    if entry.mode.is_gitlink() {
        match fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                fs::remove_file(&full).map_err(|e| create_err(&e))?;
                fs::create_dir(&full).map_err(|e| create_err(&e))?;
            }
            Err(_) => fs::create_dir(&full).map_err(|e| create_err(&e))?,
        }
        return Ok(None);
    }
    remove_any(&full).map_err(|e| create_err(&e))?;
    if entry.mode.is_symlink() && symlinks {
        let target = path_from_bytes(data);
        let res = make_symlink(&target, &full);
        res.map_err(|e| CheckoutError::CreateSymlink {
            path: lossy(rel),
            error: strerror(&e),
        })?;
    } else {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let exec = entry.mode.0 & 0o111 != 0 && entry.mode.is_file();
            opts.mode(if exec { 0o777 } else { 0o666 });
        }
        let mut f = opts.open(&full).map_err(|e| create_err(&e))?;
        f.write_all(data).map_err(|e| create_err(&e))?;
    }
    fs::symlink_metadata(&full)
        .map(Some)
        .map_err(|e| create_err(&e))
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn make_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Removes the work tree file of a deleted index entry at `rel` and then
/// every leading directory that became empty. A gitlink's directory is only
/// removed when empty. A path beyond a symbolic link is left alone.
///
/// # Errors
/// When an existing file cannot be removed.
pub fn remove_entry(root: &Path, rel: &[u8], gitlink: bool) -> io::Result<()> {
    if beyond_non_dir(root, rel) {
        return Ok(());
    }
    let full = join(root, rel);
    match fs::symlink_metadata(&full) {
        Ok(m) if m.is_dir() => {
            if gitlink {
                let _ = fs::remove_dir(&full);
            }
        }
        Ok(_) => fs::remove_file(&full)?,
        Err(_) => {}
    }
    remove_empty_parents(root, rel);
    Ok(())
}

/// `rmdir` the leading directories of `rel`, deepest first, until one is
/// not empty.
pub fn remove_empty_parents(root: &Path, rel: &[u8]) {
    let mut end = rel.len();
    while let Some(slash) = rel[..end].iter().rposition(|&b| b == b'/') {
        if fs::remove_dir(join(root, &rel[..slash])).is_err() {
            break;
        }
        end = slash;
    }
}

/// Whether a leading directory of `rel` is missing or not a real directory.
pub(crate) fn beyond_non_dir(root: &Path, rel: &[u8]) -> bool {
    rel.iter().enumerate().any(|(i, &b)| {
        b == b'/' && !fs::symlink_metadata(join(root, &rel[..i])).is_ok_and(|m| m.is_dir())
    })
}
