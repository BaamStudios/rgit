// SPDX-License-Identifier: MIT

//! Small platform helpers: byte paths and reading a file without following
//! a symbolic link in its last component.

use std::io;
use std::path::{Path, PathBuf};

/// `bytes` as a path (`/`-separated work tree paths are used as is).
#[cfg(unix)]
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
pub(crate) fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// `root` joined with the work tree relative `rel`.
pub(crate) fn join(root: &Path, rel: &[u8]) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(path_from_bytes(rel))
    }
}

/// Opens `path` for reading; with `nofollow` a symbolic link as the last
/// component is an error (`ELOOP`) instead of being followed.
pub(crate) fn open_read(path: &Path, nofollow: bool) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    if nofollow {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    if nofollow && std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::other("Too many levels of symbolic links"));
    }
    opts.open(path)
}

/// The text of an I/O error as `strerror` prints it, without Rust's
/// ` (os error N)` suffix.
pub(crate) fn strerror(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}
