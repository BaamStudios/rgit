// SPDX-License-Identifier: MIT
//! `git credential-cache [--timeout=<n>] [--socket=<path>] <action>`
//! (Documentation/git-credential-cache.adoc): the client of
//! `git credential-cache--daemon`.
//!
//! Behaviour observed with git 2.56:
//!
//! - The request is `action=<action>`, `timeout=<n>` and the client's
//!   standard input unchanged (nothing for `exit`); the answer is copied
//!   to standard output.
//! - Only `store` starts a daemon when none answers; `get`, `erase` and
//!   `exit` without a daemon do nothing. Unknown actions are ignored.
//! - The default socket is `~/.git-credential-cache/socket` if that
//!   directory exists, else `$XDG_CACHE_HOME/git/credential/socket`
//!   (`~/.cache/...` when unset or empty).

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rgit_transport::credential::cache;
use rgit_transport::messages as msg;

use crate::prelude::*;

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let timeout = Cell::new(cache::DEFAULT_TIMEOUT as i32);
    let socket: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::CREDENTIAL_CACHE_USAGE],
        vec![
            Opt::integer(None, "timeout", &timeout, msg::CREDENTIAL_CACHE_HELP_TIMEOUT),
            Opt::string(None, "socket", &socket, "path", msg::CREDENTIAL_CACHE_HELP_SOCKET),
        ],
    );
    let args = parser.parse(argv)?.args;
    let Some(action) = args.first().cloned() else {
        return Err(parser.usage_error().into());
    };
    drop(parser);
    let socket = match socket.borrow().clone() {
        Some(s) => PathBuf::from(s),
        None => default_socket(),
    };
    let timeout = i64::from(timeout.get());
    match action.as_str() {
        "exit" => imp::send(&socket, &cache::encode_request("exit", timeout, b""), false),
        "get" | "erase" | "store" => {
            let mut input = Vec::new();
            io::stdin()
                .lock()
                .read_to_end(&mut input)
                .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
            let request = cache::encode_request(&action, timeout, &input);
            imp::send(&socket, &request, action == "store")
        }
        _ => Ok(0),
    }
}

/// Where the daemon listens unless `--socket` is given.
pub fn default_socket() -> PathBuf {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_default();
    let legacy = home.join(".git-credential-cache");
    if legacy.is_dir() {
        return legacy.join("socket");
    }
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => home.join(".cache"),
    };
    base.join(Path::new("git/credential/socket"))
}

#[cfg(unix)]
pub(crate) mod imp {
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use rgit_transport::messages as msg;

    use crate::global::strerror;
    use crate::prelude::*;

    /// Send `request` and copy the answer to standard output. Without a
    /// daemon, start one if `spawn` is set, else do nothing.
    pub fn send(socket: &Path, request: &[u8], spawn: bool) -> CmdResult {
        let connect = || with_short_path(socket, |p| UnixStream::connect(p));
        let mut stream = match connect() {
            Ok(s) => s,
            Err(_) if !spawn => return Ok(0),
            Err(_) => {
                start_daemon(socket)?;
                connect().map_err(|e| Die::Fatal(msg::cache_unable_to_connect(&strerror(&e))))?
            }
        };
        let _ = stream.write_all(request);
        let _ = stream.shutdown(Shutdown::Write);
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        let mut out = io::stdout().lock();
        let _ = out.write_all(&answer);
        let _ = out.flush();
        Ok(0)
    }

    /// Longest socket path `sockaddr_un` holds (with its NUL).
    const MAX_SOCKET_PATH: usize = 107;

    /// Run `f` with `socket`, or, if the path is too long for a socket
    /// address, from inside its directory with just the file name (the
    /// working directory is restored afterwards).
    pub(crate) fn with_short_path<T>(
        socket: &Path,
        f: impl FnOnce(&Path) -> io::Result<T>,
    ) -> io::Result<T> {
        use std::os::unix::ffi::OsStrExt;
        let (Some(dir), Some(name)) = (socket.parent(), socket.file_name()) else {
            return f(socket);
        };
        if socket.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH {
            return f(socket);
        }
        let old = std::env::current_dir()?;
        std::env::set_current_dir(dir)?;
        let result = f(Path::new(name));
        let _ = std::env::set_current_dir(old);
        result
    }

    /// Start `credential-cache--daemon` and wait until it says `ok`. The
    /// daemon keeps running after this process exits.
    fn start_daemon(socket: &Path) -> Result<(), Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(strerror(&e)))?;
        let mut child = Command::new(exe)
            .arg("credential-cache--daemon")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| Die::Fatal(msg::cache_daemon_did_not_start(&strerror(&e))))?;
        let mut said = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            let _ = out.read_to_end(&mut said);
        }
        if said != b"ok\n" {
            // The daemon failed (and has exited or is about to): reap it.
            let _ = child.wait();
            let said = String::from_utf8_lossy(&said);
            return Err(Die::Fatal(msg::cache_daemon_did_not_start(
                said.trim_end_matches('\n'),
            )));
        }
        Ok(())
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    use crate::prelude::*;

    pub fn send(_socket: &Path, _request: &[u8], _spawn: bool) -> CmdResult {
        Err(Die::Fatal(
            rgit_transport::messages::CACHE_NO_UNIX_SOCKETS.into(),
        ))
    }
}
