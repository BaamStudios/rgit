// SPDX-License-Identifier: MIT
//! The listening side of `rgit daemon`: bind the sockets, accept
//! connections and start one `rgit daemon --serve` child per connection
//! (ADR 0004), with the connection as its standard input and output and
//! the client's address in `REMOTE_ADDR` and `REMOTE_PORT`.
//!
//! A thread per socket accepts; a thread per child waits for it, so its
//! end is logged at once and `--max-connections` counts right. When the
//! limit is reached, a client that has two connections open loses the
//! newer one; if that does not help within a second, the new connection
//! is dropped (as git 2.56 does).

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
pub fn bind(opts: &Options, log: &Logger, port: u16) -> Result<Vec<TcpListener>, Die> {
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
            log.error(&msg::no_sockets_for_host(host, port));
        }
    }
    if listeners.is_empty() {
        return Err(log.fatal(msg::no_listen_sockets(port)));
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

    /// End the newer connection of a client that has more than one.
    fn kill_some_child(&self) {
        let Ok(children) = self.children.lock() else {
            return;
        };
        let duplicate = children
            .iter()
            .enumerate()
            .find(|(i, child)| children[..*i].iter().any(|c| c.addr == child.addr));
        if let Some((_, child)) = duplicate {
            sys::terminate(child.pid);
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
        if let Ok(mut children) = self.children.lock() {
            children.push(Child {
                pid,
                addr: peer.ip(),
            });
        }
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
