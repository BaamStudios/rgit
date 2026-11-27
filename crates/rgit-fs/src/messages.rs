// SPDX-License-Identifier: MIT
//! User-visible messages of this crate, collected in one place for review.
//!
//! These strings reproduce what the reference `git` binary (2.56) prints, as
//! observed by provoking lock conflicts (see CLEANROOM.md, "User-visible
//! messages"). Callers add their own prefix (`fatal: `, `cannot lock ref
//! '...': `, `warning: `).

use std::io;
use std::path::Path;

/// Explanation printed after "File exists." when no PID information is
/// available.
pub const LOCK_EXISTS_HINT: &str =
    "Another git process seems to be running in this repository, or the lock file may be stale";

/// `Unable to create '<lock>': File exists.` followed by an empty line and
/// the explanation `hint`.
pub fn lock_exists(lock_path: &Path, hint: &str) -> String {
    format!(
        "Unable to create '{}': File exists.\n\n{hint}",
        lock_path.display()
    )
}

/// `Unable to create '<lock>': <strerror>` for every error except
/// "File exists".
pub fn unable_to_create(lock_path: &Path, err: &io::Error) -> String {
    format!(
        "Unable to create '{}': {}",
        lock_path.display(),
        strerror(err)
    )
}

/// Explanation when the PID file names a process that no longer exists.
pub fn holder_stale(pid: u64) -> String {
    format!(
        "Lock was held by process {pid}, which is no longer running; the lock file appears to be stale"
    )
}

/// Explanation when the PID file names a process that is still running.
pub fn holder_alive(pid: u64) -> String {
    format!(
        "Lock may be held by process {pid}; if no git process is running, the lock file may be stale (PIDs can be reused)"
    )
}

/// Body of the warning printed (as `warning: ...`) before the lock error when
/// the PID file cannot be parsed.
pub fn malformed_pid_file(pid_path: &Path) -> String {
    format!("malformed lock pid file '{}'", pid_path.display())
}

/// Body of the fatal error for a `core.sharedRepository` octal mode that
/// does not give the owner read and write permission.
pub fn shared_repository_owner_perms(value: &str) -> String {
    format!(
        "problem with core.sharedRepository filemode value ({value}).\nThe owner of files must always have read and write permissions."
    )
}

/// Body of the fatal error for a `core.sharedRepository` value that is not
/// understood; `name` is the variable (`arg` for `git init --shared=`).
pub fn bad_boolean(value: &str, name: &str) -> String {
    format!("bad boolean config value '{value}' for '{name}'")
}

/// The C library's text for an I/O error (`strerror`), i.e. Rust's
/// `Display` without the trailing ` (os error N)`.
///
/// On Windows the system describes its error codes in other words ("The
/// system cannot find the file specified."). Git for Windows reports the
/// C library's text for the corresponding `errno` value instead, so the
/// common codes are worded that way here.
pub fn strerror(err: &io::Error) -> String {
    if cfg!(windows)
        && let Some(code) = err.raw_os_error()
        && let Some(text) = windows_text(code, err.kind())
    {
        return text.to_owned();
    }
    let text = err.to_string();
    if let Some(code) = err.raw_os_error() {
        let suffix = format!(" (os error {code})");
        if let Some(stripped) = text.strip_suffix(&suffix) {
            return stripped.to_owned();
        }
    }
    text
}

/// The C library's text for the `errno` value a Windows error code stands
/// for: `code` is the system's code, `kind` what `std` makes of it. `None`
/// for codes without a counterpart; they keep the system's text.
fn windows_text(code: i32, kind: io::ErrorKind) -> Option<&'static str> {
    use io::ErrorKind as K;
    // Codes `std` has no kind for (winerror.h): ERROR_TOO_MANY_OPEN_FILES,
    // ERROR_INVALID_HANDLE, ERROR_INVALID_NAME, ERROR_BAD_PATHNAME,
    // ERROR_FILENAME_EXCED_RANGE.
    match code {
        4 => return Some("Too many open files"),
        6 => return Some("Bad file descriptor"),
        123 => return Some("Invalid argument"),
        161 => return Some("No such file or directory"),
        206 => return Some("Filename too long"),
        _ => {}
    }
    Some(match kind {
        K::NotFound => "No such file or directory",
        K::PermissionDenied => "Permission denied",
        K::AlreadyExists => "File exists",
        K::NotADirectory => "Not a directory",
        K::IsADirectory => "Is a directory",
        K::DirectoryNotEmpty => "Directory not empty",
        K::InvalidInput => "Invalid argument",
        K::StorageFull => "No space left on device",
        K::BrokenPipe => "Broken pipe",
        K::ReadOnlyFilesystem => "Read-only file system",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strerror_strips_os_error_suffix() {
        // 2: ENOENT, and ERROR_FILE_NOT_FOUND on Windows.
        let e = io::Error::from_raw_os_error(2);
        assert_eq!(strerror(&e), "No such file or directory");
        assert_eq!(strerror(&io::Error::other("custom")), "custom");
    }

    #[test]
    fn windows_codes() {
        use io::ErrorKind as K;
        assert_eq!(
            windows_text(5, K::PermissionDenied),
            Some("Permission denied")
        );
        assert_eq!(windows_text(183, K::AlreadyExists), Some("File exists"));
        assert_eq!(windows_text(6, K::Other), Some("Bad file descriptor"));
        assert_eq!(
            windows_text(206, K::InvalidFilename),
            Some("Filename too long")
        );
        assert_eq!(windows_text(1224, K::Other), None);
    }

    #[cfg(unix)]
    #[test]
    fn strerror_of_errno_values() {
        let e = io::Error::from_raw_os_error(libc::EEXIST);
        assert_eq!(strerror(&e), "File exists");
        let e = io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(strerror(&e), "Permission denied");
    }
}
