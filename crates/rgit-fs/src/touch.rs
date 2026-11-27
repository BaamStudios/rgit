// SPDX-License-Identifier: MIT
//! Setting the modification time of a file that may be read-only.
//!
//! Object files and packs are written read-only, and their modification
//! time is what tells recent objects from old ones ("freshening"). On Unix
//! the owner may set the times through a descriptor opened for reading. On
//! Windows a handle opened for reading may not, and a read-only file cannot
//! be opened for writing; a handle that asks for nothing but the right to
//! write attributes can be opened on such a file and may set the times.
//! Where even that is refused, the read-only attribute is taken away for
//! the moment, which is what Git for Windows does.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::SystemTime;

/// Sets the modification time of `path` to `time`, also when the file is
/// read-only.
///
/// # Errors
/// The file cannot be opened, or the system refuses to change the time.
pub fn set_modified(path: impl AsRef<Path>, time: SystemTime) -> io::Result<()> {
    let path = path.as_ref();
    match open_for_times(path).and_then(|file| file.set_modified(time)) {
        Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied => {
            set_modified_writable(path, time).map_err(|_| e)
        }
        result => result,
    }
}

/// [`set_modified`] for a read-only file whose times cannot be set as it
/// is: writable for the moment, then read-only again.
fn set_modified_writable(path: &Path, time: SystemTime) -> io::Result<()> {
    let read_only = std::fs::metadata(path)?.permissions();
    if !read_only.readonly() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut writable = read_only.clone();
    #[allow(clippy::permissions_set_readonly_false)]
    writable.set_readonly(false);
    std::fs::set_permissions(path, writable)?;
    let result = File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(time));
    std::fs::set_permissions(path, read_only)?;
    result
}

#[cfg(not(windows))]
fn open_for_times(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_for_times(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    /// `FILE_WRITE_ATTRIBUTES` (winnt.h).
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    /// `FILE_FLAG_BACKUP_SEMANTICS` (winbase.h): lets a directory be opened.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn read_only_files_get_the_time() {
        let path = std::env::temp_dir().join(format!("rgit-fs-touch-{}", std::process::id()));
        let read_only = |on: bool| {
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut perm = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(on);
                std::fs::set_permissions(&path, perm).unwrap();
            }
        };
        // A file an interrupted run left behind is read-only.
        read_only(false);
        std::fs::write(&path, b"x").unwrap();
        read_only(true);

        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(978_307_200);
        set_modified(&path, then).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.modified().unwrap(), then);
        assert!(meta.permissions().readonly());
        assert_eq!(std::fs::read(&path).unwrap(), b"x");

        read_only(false);
        std::fs::remove_file(&path).unwrap();
        assert!(set_modified(&path, then).is_err());
    }
}
