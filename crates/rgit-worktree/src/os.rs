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

/// The bytes of `path`.
#[cfg(unix)]
pub(crate) fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
pub(crate) fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().replace('\\', "/").into_bytes()
}

/// `root` joined with the work tree relative `rel` (empty `rel` is `root`).
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

/// The top of a work tree for `lstat` calls on many paths below it: on
/// unix the directory is opened once and paths are looked up relative to
/// it, which saves resolving its own components again and again.
pub(crate) struct Root<'a> {
    path: &'a Path,
    #[cfg(unix)]
    dir: Option<std::fs::File>,
}

impl Root<'_> {
    pub(crate) fn open(path: &Path) -> Root<'_> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let dir = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(path)
                .ok();
            Root { path, dir }
        }
        #[cfg(not(unix))]
        Root { path }
    }

    /// `lstat` of `rel` (relative to the top, not empty); `None` when it
    /// does not exist or a leading component is not a directory.
    pub(crate) fn lstat(&self, rel: &[u8]) -> io::Result<Option<crate::stat::FileStat>> {
        #[cfg(unix)]
        if let Some(dir) = &self.dir {
            use std::os::fd::AsRawFd;
            let Ok(name) = std::ffi::CString::new(rel) else {
                return Ok(None);
            };
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: `name` is a NUL-terminated string and `st` points to
            // writable memory for one `stat`, which fstatat fills on success.
            let rc = unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                let e = io::Error::last_os_error();
                return match e.kind() {
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Ok(None),
                    _ => Err(e),
                };
            }
            // SAFETY: fstatat succeeded, so `st` is initialised.
            let st = unsafe { st.assume_init() };
            return Ok(Some(crate::stat::FileStat::from_stat(&st)));
        }
        match std::fs::symlink_metadata(join(self.path, rel)) {
            Ok(m) => Ok(Some(crate::stat::FileStat::from_metadata(&m))),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}
