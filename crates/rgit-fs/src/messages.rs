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
pub fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    if let Some(code) = err.raw_os_error() {
        let suffix = format!(" (os error {code})");
        if let Some(stripped) = text.strip_suffix(&suffix) {
            return stripped.to_owned();
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn strerror_strips_os_error_suffix() {
        let e = io::Error::from_raw_os_error(libc::EEXIST);
        assert_eq!(strerror(&e), "File exists");
        let e = io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(strerror(&e), "Permission denied");
    }
}
