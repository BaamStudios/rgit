// SPDX-License-Identifier: MIT
//! The listening side of `rgit daemon`: bind the sockets, accept
//! connections and start one `rgit daemon --serve` child per connection
//! (ADR 0004), with the connection as its standard input and output and
//! the client's address in `REMOTE_ADDR` and `REMOTE_PORT`.
//!
//! A thread per socket accepts; a thread per child waits for it, so its
//! end is logged at once and `--max-connections` counts right. When the
//! limit is reached, a client that has two connections open loses its
//! newest one (the client that came first, if several have); if that
//! does not help within a second, the new connection is dropped (as git
//! 2.56 does).

use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rgit_server::daemon::messages as msg;
use socket2::{Domain, Protocol, Socket, Type};

use super::imp::Options;
use super::log::Logger;
use super::serve::remote_addr;
use super::sys;
use crate::prelude::*;

fn listener(addr: SocketAddr, reuseaddr: bool) -> std::io::Result<TcpListener> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if reuseaddr {
        socket.set_reuse_address(true)?;
    }
    if addr.is_ipv6() {
        // The IPv4 socket is a separate one.
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    Ok(socket.into())
}

/// Bind the sockets of `--listen` (or all addresses) on `port`.
///
/// `port` is `--port` as git keeps it: a number that is negative as a C
/// `int` is no port the address lookup takes (it fails for every host);
/// otherwise its low 16 bits are the port, 0 meaning one the system
/// picks (observed with git 2.56).
pub fn bind(opts: &Options, log: &Logger, port: u32) -> Result<Vec<TcpListener>, Die> {
    if (port as i32) < 0 {
        if opts.listen.is_empty() {
            log.error(&msg::getaddrinfo_failed(
                msg::ANY_HOST,
                msg::SERVNAME_NOT_SUPPORTED,
            ));
        }
        for host in &opts.listen {
            log.error(&msg::getaddrinfo_failed(host, msg::SERVNAME_NOT_SUPPORTED));
            log.error(&msg::no_sockets_for_host(host, port));
        }
        return Err(log.fatal(msg::no_listen_sockets(port)));
    }
    let shown = port;
    let port = (port & 0xffff) as u16;
    let mut listeners = Vec::new();
    if opts.listen.is_empty() {
        let any: [IpAddr; 2] = [Ipv6Addr::UNSPECIFIED.into(), Ipv4Addr::UNSPECIFIED.into()];
        for ip in any {
            match listener(SocketAddr::new(ip, port), opts.reuseaddr) {
                Ok(l) => listeners.push(l),
                // A system without IPv6 is no error.
                Err(e) if e.raw_os_error() == Some(libc::EAFNOSUPPORT) => {}
                Err(e) => log.error(&msg::could_not_bind(
                    &ip.to_string(),
                    &rgit_messages::strerror(&e),
                )),
            }
        }
    }
    for host in &opts.listen {
        let before = listeners.len();
        match (host.as_str(), port).to_socket_addrs() {
            Ok(addrs) => {
                for addr in addrs {
                    match listener(addr, opts.reuseaddr) {
                        Ok(l) => listeners.push(l),
                        Err(e) => log.error(&msg::could_not_bind(
                            &addr.ip().to_string(),
                            &rgit_messages::strerror(&e),
                        )),
                    }
                }
            }
            Err(e) => {
                // "failed to lookup address information: <reason>"
                let text = rgit_messages::strerror(&e);
                let reason = text.rsplit_once(": ").map_or(text.as_str(), |(_, r)| r);
                log.error(&msg::getaddrinfo_failed(host, reason));
            }
        }
        if listeners.len() == before {
            log.error(&msg::no_sockets_for_host(host, shown));
        }
    }
    if listeners.is_empty() {
        return Err(log.fatal(msg::no_listen_sockets(shown)));
    }
    Ok(listeners)
}

/// A connection that is being served.
struct Child {
    pid: u32,
    addr: IpAddr,
}

struct Daemon {
    log: Logger,
    exe: PathBuf,
    /// The daemon's own arguments, passed on behind `--serve`.
    args: Vec<String>,
    env: Vec<(OsString, OsString)>,
    max_connections: usize,
    children: Mutex<Vec<Child>>,
    /// One connection is taken in at a time.
    turn: Mutex<()>,
}

impl Daemon {
    fn live(&self) -> usize {
        self.children.lock().map_or(0, |c| c.len())
    }

    /// End one connection of a client that has more than one: the
    /// first child in the list whose successor has the same address (see
    /// [`Daemon::add_child`]), which is the newest connection of the
    /// client that came first.
    fn kill_some_child(&self) {
        let Ok(children) = self.children.lock() else {
            return;
        };
        if let Some(pair) = children.windows(2).find(|w| w[0].addr == w[1].addr) {
            sys::terminate(pair[0].pid);
        }
    }

    /// Keep the connections of one client together, newest first: a new
    /// child goes before the first one with its address, else at the end
    /// (observed with git 2.56: with clients A, B, A, A, B connected in
    /// that order, the limit ends the last connection of A).
    fn add_child(&self, child: Child) {
        if let Ok(mut children) = self.children.lock() {
            let at = children
                .iter()
                .position(|c| c.addr == child.addr)
                .unwrap_or(children.len());
            children.insert(at, child);
        }
    }

    fn handle(self: &Arc<Self>, stream: TcpStream, peer: SocketAddr) {
        let Ok(_turn) = self.turn.lock() else { return };
        if self.max_connections > 0 && self.live() >= self.max_connections {
            self.kill_some_child();
            std::thread::sleep(Duration::from_secs(1));
            if self.live() >= self.max_connections {
                drop(stream);
                self.log.error(msg::TOO_MANY_CHILDREN);
                return;
            }
        }
        let Ok(output) = stream.try_clone() else {
            return;
        };
        let mut command = Command::new(&self.exe);
        command
            .arg("daemon")
            .arg("--serve")
            .args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env("REMOTE_ADDR", remote_addr(&peer.ip()))
            .env("REMOTE_PORT", peer.port().to_string())
            .stdin(Stdio::from(OwnedFd::from(stream)))
            .stdout(Stdio::from(OwnedFd::from(output)));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.log
                    .error(&format!("unable to fork: {}", rgit_messages::strerror(&e)));
                return;
            }
        };
        // Our copies of the connection go with the command.
        drop(command);
        let pid = child.id();
        self.add_child(Child {
            pid,
            addr: peer.ip(),
        });
        let daemon = Arc::clone(self);
        std::thread::spawn(move || {
            let failed = !child.wait().is_ok_and(|status| status.success());
            if let Ok(mut children) = daemon.children.lock() {
                children.retain(|c| c.pid != pid);
            }
            daemon.log.info(&msg::disconnected(pid, failed));
        });
    }
}

/// Serve connections until the process is ended.
pub fn accept(
    listeners: Vec<TcpListener>,
    opts: &Options,
    log: Logger,
    exe: PathBuf,
    args: Vec<String>,
    env: Vec<(OsString, OsString)>,
) -> CmdResult {
    let daemon = Arc::new(Daemon {
        log,
        exe,
        args,
        env,
        max_connections: opts.max_connections as usize,
        children: Mutex::new(Vec::new()),
        turn: Mutex::new(()),
    });
    let threads: Vec<_> = listeners
        .into_iter()
        .map(|listener| {
            let daemon = Arc::clone(&daemon);
            std::thread::spawn(move || {
                loop {
                    match listener.accept() {
                        Ok((stream, peer)) => daemon.handle(stream, peer),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            daemon
                                .log
                                .error(&format!("accept returned {}", rgit_messages::strerror(&e)));
                            // Out of descriptors, most likely: give the
                            // children time to finish.
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        let _ = thread.join();
    }
    Ok(0)
}
