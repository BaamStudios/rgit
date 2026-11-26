// SPDX-License-Identifier: MIT
//! `git remote-fd <remote> <url>` (Documentation/git-remote-fd.adoc): the
//! remote helper of `fd::<infd>[,<outfd>][/<anything>]` URLs. It only
//! knows `connect`: the service is reached through file descriptors the
//! caller opened, `<infd>` alone being a bidirectional socket.
//!
//! The helper's argument is the address without `fd::`. Behaviour
//! observed with git 2.56: a malformed address fails with
//! `fatal: Bad URL syntax` before any command is read; otherwise like
//! `remote-ext` (see there).

use std::fs::File;
use std::io::{self, Write};

use crate::commands::remote_ext::{helper_args, serve};
use crate::messages::remote_helper as msg;
use crate::prelude::*;

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let url = match helper_args(argv, msg::FD_USAGE) {
        Ok(url) => url,
        Err(code) => return Ok(code),
    };
    let (input, output) =
        parse_address(&url).ok_or_else(|| Die::Fatal(msg::BAD_URL_SYNTAX.into()))?;
    serve(|_service| connect_fds(input, output))
}

/// `<in>[,<out>]` before an optional `/<anything>`.
fn parse_address(url: &str) -> Option<(i32, Option<i32>)> {
    let fds = url.split('/').next().unwrap_or(url);
    let (a, b) = match fds.split_once(',') {
        Some((a, b)) => (a, Some(b)),
        None => (fds, None),
    };
    let num = |s: &str| -> Option<i32> {
        let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let out = match b {
        Some(b) => Some(num(b)?),
        None => None,
    };
    Some((num(a)?, out))
}

#[cfg(unix)]
fn fd_file(fd: i32) -> Result<File, Die> {
    use std::os::fd::{BorrowedFd, OwnedFd};
    if fd < 0 {
        return Err(Die::Fatal(msg::COPY_FAILED.into()));
    }
    // SAFETY: the caller of the helper hands us `fd` for exactly this use
    // (git-remote-fd(1)); it is only borrowed long enough to duplicate it,
    // so it is never closed here. A descriptor that is not open makes the
    // duplication fail, which is reported like a failed copy.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let owned: OwnedFd = borrowed
        .try_clone_to_owned()
        .map_err(|_| Die::Fatal(msg::COPY_FAILED.into()))?;
    Ok(File::from(owned))
}

#[cfg(not(unix))]
fn fd_file(_fd: i32) -> Result<File, Die> {
    Err(Die::Fatal(msg::COPY_FAILED.into()))
}

/// Copy the service's output (`input`) to standard output and standard
/// input to the service (`output`, or `input` for a socket) until the
/// service's side ends.
fn connect_fds(input: i32, output: Option<i32>) -> CmdResult {
    let mut from_remote = fd_file(input)?;
    let mut to_remote = fd_file(output.unwrap_or(input))?;
    let socket = output.is_none();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let _ = io::copy(&mut stdin, &mut to_remote);
        let _ = to_remote.flush();
        if socket {
            shutdown_write(&to_remote);
        }
        drop(to_remote);
    });
    let mut stdout = io::stdout().lock();
    io::copy(&mut from_remote, &mut stdout).map_err(|_| Die::Fatal(msg::COPY_FAILED.into()))?;
    let _ = stdout.flush();
    Ok(0)
}

/// End the writing direction of a socket (other descriptors are left
/// alone).
#[cfg(unix)]
fn shutdown_write(f: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: shutdown(2) on a descriptor we own; on anything but a socket
    // it fails with ENOTSOCK, which is ignored.
    unsafe {
        libc::shutdown(f.as_raw_fd(), libc::SHUT_WR);
    }
}

#[cfg(not(unix))]
fn shutdown_write(_f: &File) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert_eq!(parse_address("7"), Some((7, None)));
        assert_eq!(parse_address("7/abc"), Some((7, None)));
        assert_eq!(parse_address("7,8/abc"), Some((7, Some(8))));
        assert_eq!(parse_address("-1"), Some((-1, None)));
        assert_eq!(parse_address("7,8x"), None);
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address("fd::7"), None);
    }
}
