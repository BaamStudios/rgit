// SPDX-License-Identifier: MIT
//! Writing executable scripts (fake `ssh`, editors, hooks, helpers) without
//! "Text file busy" races.
//!
//! Test binaries run many tests on parallel threads, and those threads spawn
//! processes. If thread 1 writes script `A` through a descriptor of this
//! process, any child forked by thread 2 in that window inherits the write
//! descriptor and keeps it until its own `exec`. When `A` is executed
//! meanwhile (by this process or by a child such as `rgit`), the kernel sees
//! an open writer and `execve` fails with `ETXTBSY`. Renaming does not help
//! (the inode is the same); only never holding the write descriptor in the
//! multi-threaded test process does.
//!
//! [`write_executable`] therefore lets a short-lived `/bin/sh` create the
//! file: the write descriptor exists only in that single-threaded child and is
//! closed when it exits, before this function returns.

use std::path::Path;

/// Writes `contents` to `path` and makes it executable (mode 755 on Unix),
/// such that no descriptor writing to `path` is ever open in the calling
/// process (see the module documentation). Parent directories must exist.
///
/// Use this for every file a test executes, directly or through git/rgit.
///
/// # Panics
/// If the file cannot be written.
#[cfg(unix)]
pub fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let path = path.as_ref();
    let mut child = Command::new("/bin/sh")
        .args(["-c", r#"cat >"$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn /bin/sh to write {}: {e}", path.display()));
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin
        .write_all(contents.as_ref())
        .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    drop(stdin);
    let status = child.wait().expect("wait for /bin/sh");
    assert!(status.success(), "write {}: sh exited {status}", path.display());
}

/// Writes `contents` to `path` (no executable bit off Unix).
///
/// # Panics
/// If the file cannot be written.
#[cfg(not(unix))]
pub fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    std::fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
