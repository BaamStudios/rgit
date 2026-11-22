// SPDX-License-Identifier: MIT
//! `git credential-cache--daemon [--debug] <socket-path>`
//! (Documentation/git-credential-cache--daemon.adoc): keeps credentials
//! in memory and serves `git credential-cache` over a unix socket. The
//! store itself is `rgit_transport::credential::cache::Cache`.
//!
//! Behaviour observed with git 2.56:
//!
//! - The socket path must be absolute. Its directory is created with mode
//!   0700 if missing (parents with the default mode); an existing
//!   directory that others can access is refused.
//! - Once listening, the daemon prints `ok` on standard output, which the
//!   client waits for, and closes standard output; without `--debug` it
//!   closes standard error too. It serves one client at a time and reads
//!   each request up to end of file.
//! - `exit` stops it; the socket is removed, the directory kept.
//!
//! As documented, the daemon also exits once its last credential has
//! expired. It also exits when its socket was removed or replaced, so a
//! daemon nobody can reach does not linger.

use crate::prelude::*;

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let debug = Cell::new(false);
    let mut parser = Parser::new(
        &[rgit_transport::messages::CREDENTIAL_CACHE_DAEMON_USAGE],
        vec![Opt::flag(
            None,
            "debug",
            &debug,
            rgit_transport::messages::CREDENTIAL_CACHE_DAEMON_HELP_DEBUG,
        )],
    );
    let args = parser.parse(argv)?.args;
    let Some(socket) = args.first().cloned() else {
        return Err(parser.usage_error().into());
    };
    drop(parser);
    imp::serve(std::path::Path::new(&socket), debug.get())
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use rgit_transport::credential::cache::{After, Cache, parse_request};
    use rgit_transport::messages as msg;

    use crate::commands::credential_cache::imp::with_short_path;
    use crate::global::strerror;
    use crate::prelude::*;

    /// How long one client may take to send its request.
    const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);
    /// How often an idle daemon checks that its socket is still there.
    const SOCKET_CHECK: Duration = Duration::from_secs(60);

    /// The socket path for the signal handler.
    static SOCKET: OnceLock<CString> = OnceLock::new();

    extern "C" fn on_signal(sig: libc::c_int) {
        if let Some(p) = SOCKET.get() {
            // SAFETY: unlink, signal and raise are async-signal-safe; the
            // path was fully initialised before the handler was installed.
            unsafe {
                libc::unlink(p.as_ptr());
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
        }
    }

    fn install_signal_handlers(socket: &Path) {
        let Ok(c) = CString::new(socket.as_os_str().as_bytes()) else {
            return;
        };
        if SOCKET.set(c).is_err() {
            return;
        }
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: installing handlers that only call async-signal-safe
        // functions; ignoring SIGPIPE so a client that went away cannot
        // kill the daemon.
        unsafe {
            for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
                libc::signal(sig, handler);
            }
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
    }

    fn redirect_to_null(fd: libc::c_int) {
        if let Ok(null) = OpenOptions::new().write(true).open("/dev/null") {
            // SAFETY: dup2 onto a standard descriptor we own.
            unsafe {
                libc::dup2(null.as_raw_fd(), fd);
            }
        }
    }

    fn init_socket_directory(dir: &Path) -> Result<(), Die> {
        match std::fs::metadata(dir) {
            Ok(meta) => {
                if meta.permissions().mode() & 0o077 != 0 {
                    return Err(Die::Fatal(msg::cache_socket_dir_too_loose(
                        &dir.display().to_string(),
                    )));
                }
                Ok(())
            }
            Err(_) => {
                let mkdir_err = |e: io::Error| {
                    Die::Fatal(msg::cache_unable_to_mkdir(
                        &dir.display().to_string(),
                        &strerror(&e),
                    ))
                };
                if let Some(parent) = dir.parent() {
                    std::fs::create_dir_all(parent).map_err(mkdir_err)?;
                }
                DirBuilder::new().mode(0o700).create(dir).map_err(mkdir_err)
            }
        }
    }

    /// Our socket is still the one at `socket`.
    fn socket_is_ours(socket: &Path, ino: u64) -> bool {
        std::fs::symlink_metadata(socket).is_ok_and(|m| m.ino() == ino)
    }

    pub fn serve(socket: &Path, debug: bool) -> CmdResult {
        if !socket.is_absolute() {
            return Err(Die::Fatal(msg::CACHE_SOCKET_NOT_ABSOLUTE.into()));
        }
        if let Some(dir) = socket.parent() {
            init_socket_directory(dir)?;
        }
        let _ = std::fs::remove_file(socket);
        let listener = with_short_path(socket, |p| UnixListener::bind(p)).map_err(|e| {
            Die::Fatal(msg::cache_unable_to_bind(
                &socket.display().to_string(),
                &strerror(&e),
            ))
        })?;
        let ino = std::fs::symlink_metadata(socket)
            .map(|m| m.ino())
            .unwrap_or(0);
        install_signal_handlers(socket);

        {
            let mut out = io::stdout().lock();
            let _ = out.write_all(b"ok\n");
            let _ = out.flush();
        }
        redirect_to_null(libc::STDOUT_FILENO);
        if !debug {
            redirect_to_null(libc::STDERR_FILENO);
        }

        let mut cache = Cache::new();
        loop {
            let now = Instant::now();
            let wait = cache
                .next_expiry()
                .map(|t| t.saturating_duration_since(now))
                .unwrap_or(SOCKET_CHECK)
                .min(SOCKET_CHECK);
            if !wait_readable(&listener, wait) {
                if cache.prune(Instant::now()) > 0 && cache.is_empty() {
                    break;
                }
                if !socket_is_ours(socket, ino) {
                    return Ok(0);
                }
                continue;
            }
            let Ok((mut stream, _)) = listener.accept() else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
            let mut data = Vec::new();
            if stream.read_to_end(&mut data).is_err() {
                continue;
            }
            let request = match parse_request(&data) {
                Ok(r) => r,
                Err(e) => {
                    let _ = writeln!(io::stderr(), "error: {e}");
                    continue;
                }
            };
            let mut answer = Vec::new();
            let after = match cache.handle(&request, Instant::now(), &mut answer) {
                Ok(a) => a,
                Err(e) => {
                    let _ = writeln!(io::stderr(), "error: {e}");
                    After::Continue
                }
            };
            let _ = stream.write_all(&answer);
            drop(stream);
            if after == After::Exit {
                break;
            }
        }
        if socket_is_ours(socket, ino) {
            let _ = std::fs::remove_file(socket);
        }
        Ok(0)
    }

    /// Wait until a client connects or `wait` passed.
    fn wait_readable(listener: &UnixListener, wait: Duration) -> bool {
        let mut fds = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(wait.as_millis().saturating_add(1)).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd for the duration of the call.
        let n = unsafe { libc::poll(&mut fds, 1, ms) };
        n > 0 && fds.revents & libc::POLLIN != 0
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    use crate::prelude::*;

    pub fn serve(_socket: &Path, _debug: bool) -> CmdResult {
        Err(Die::Fatal(
            rgit_transport::messages::CACHE_NO_UNIX_SOCKETS.into(),
        ))
    }
}
