// SPDX-License-Identifier: MIT
//! The `git://` transport: a TCP connection to `git daemon`.
//!
//! The client opens a connection to the host (port 9418 unless the URL
//! names one) and sends one pkt-line request
//! (`Documentation/gitprotocol-pack.adoc`, "Git Transport"):
//!
//! ```text
//! <service> SP <path> NUL host=<host>[:<port>] NUL [NUL <param> NUL ...]
//! ```
//!
//! Extra parameters carry the requested protocol version
//! ([`ConnectOptions::git_protocol`], split at `:`). After the request the
//! connection is a plain stateful stream: the server's response follows
//! directly.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};

use rgit_config::{ConfigSet, Environment};

use crate::command::Command;
use crate::{ChildConnection, ConnectOptions, Connection, Error, Result, Transport, messages};

/// Default port of `git daemon`.
pub const DEFAULT_PORT: u16 = 9418;

/// A `git://host[:port]/path` location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonTarget {
    /// Host name or address (without brackets).
    pub host: String,
    /// Port, if the URL names one.
    pub port: Option<u16>,
    /// Repository path as sent to the daemon (`/repo.git`, `~user/x`).
    pub path: String,
}

impl DaemonTarget {
    /// Parse the part of a `git://` URL after the scheme.
    pub fn parse(rest: &str) -> Result<DaemonTarget> {
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if path.is_empty() {
            return Err(Error::Refused(messages::NO_PATH.into()));
        }
        // A user name is not used by the git protocol.
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
            match stripped.split_once(']') {
                Some((h, p)) => (h.to_string(), p.strip_prefix(':')),
                None => (authority.to_string(), None),
            }
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => (h.to_string(), Some(p)),
                _ => (authority.to_string(), None),
            }
        };
        // A port that is not a number stays part of the host name (the
        // name lookup then fails), as with git.
        let (host, port) = match port {
            None | Some("") => (host, None),
            Some(p) => match p.parse::<u16>() {
                Ok(n) => (host, Some(n)),
                Err(_) => (format!("{host}:{p}"), None),
            },
        };
        let path = match path.strip_prefix("/~") {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        };
        Ok(DaemonTarget { host, port, path })
    }

    /// The `host=` parameter.
    fn host_param(&self) -> String {
        // The test suite's knob for the virtual host sent to the daemon.
        let virtual_host = std::env::var("GIT_OVERRIDE_VIRTUAL_HOST")
            .ok()
            .filter(|h| !h.is_empty());
        let host = virtual_host.unwrap_or_else(|| self.host.clone());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        match self.port {
            Some(p) => format!("{host}:{p}"),
            None => host,
        }
    }

    /// The request packet (without the length header).
    pub fn request(&self, options: &ConnectOptions) -> Vec<u8> {
        let mut req = Vec::new();
        req.extend_from_slice(options.service.name().as_bytes());
        req.push(b' ');
        req.extend_from_slice(self.path.as_bytes());
        req.push(0);
        req.extend_from_slice(b"host=");
        req.extend_from_slice(self.host_param().as_bytes());
        req.push(0);
        if let Some(gp) = options.git_protocol() {
            req.push(0);
            for p in gp.split(':') {
                req.extend_from_slice(p.as_bytes());
                req.push(0);
            }
        }
        req
    }
}

/// A transport to `git daemon`.
#[derive(Debug, Clone)]
pub struct DaemonTransport {
    /// Where to connect.
    pub target: DaemonTarget,
    /// The proxy command (`core.gitProxy`, `GIT_PROXY_COMMAND`) that is run
    /// as `<command> <host> <port>` and carries the connection on its
    /// standard input and output.
    pub proxy: Option<String>,
}

/// The proxy command for `host`: `GIT_PROXY_COMMAND` if set, else the
/// first `core.gitProxy` value that applies. A value `<command> for
/// <domain>` applies to the host `<domain>` and to hosts ending in
/// `.<domain>`; a value without `for` to every host. The command `none`
/// means no proxy (as observed from git 2.56).
pub fn proxy_for(host: &str, env: &dyn Environment, config: &ConfigSet) -> Option<String> {
    if let Some(cmd) = env.var("GIT_PROXY_COMMAND") {
        if !cmd.is_empty() {
            return Some(cmd.to_string_lossy().into_owned());
        }
    }
    for entry in config.get_all("core.gitproxy") {
        let Some(value) = entry.value.as_deref() else {
            continue;
        };
        let value = String::from_utf8_lossy(value);
        let (cmd, applies) = match value.split_once(" for ") {
            Some((cmd, domain)) => {
                let matches = host == domain
                    || host
                        .strip_suffix(domain)
                        .is_some_and(|rest| rest.ends_with('.'));
                (cmd, matches)
            }
            None => (value.as_ref(), true),
        };
        if applies {
            return (cmd != "none").then(|| cmd.to_owned());
        }
    }
    None
}

impl DaemonTransport {
    /// Run the proxy command for the target.
    fn open_proxy(&self, proxy: &str) -> Result<Box<dyn Connection>> {
        let host = self.target.host.as_str();
        if host.starts_with('-') {
            return Err(Error::Other(messages::strange_hostname(host)));
        }
        let port = self.target.port.unwrap_or(DEFAULT_PORT).to_string();
        let mut cmd = Command::new(proxy);
        cmd.arg(host).arg(port);
        match ChildConnection::spawn(&cmd) {
            Ok(c) => Ok(Box::new(c)),
            Err(_) => Err(Error::Other(messages::cannot_start_proxy(proxy))),
        }
    }

    /// Connect to the first address of the host that accepts.
    fn open(&self, verbose: bool) -> Result<TcpStream> {
        let host = self.target.host.as_str();
        let port = self.target.port.unwrap_or(DEFAULT_PORT);
        let mut err = io::stderr();
        if verbose {
            let _ = write!(err, "{}", messages::looking_up(host));
        }
        let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
            Ok(a) => a.collect(),
            Err(_) => return Err(Error::Other(messages::unable_to_look_up(host, port))),
        };
        if verbose {
            let _ = writeln!(err, "{}", messages::DONE);
            let _ = write!(err, "{}", messages::connecting_to(host, port));
        }
        let mut failures = String::new();
        for (i, addr) in addrs.iter().enumerate() {
            match TcpStream::connect(addr) {
                Ok(s) => {
                    if verbose {
                        let _ = writeln!(err, "{} {}", addr.ip(), messages::DONE);
                    }
                    return Ok(s);
                }
                Err(e) => failures.push_str(&messages::connect_attempt_failed(
                    host,
                    i,
                    &addr.ip().to_string(),
                    &rgit_trace::messages::strerror(&e),
                )),
            }
        }
        Err(Error::Other(messages::unable_to_connect(host, &failures)))
    }
}

impl Transport for DaemonTransport {
    fn connect(&mut self, options: &ConnectOptions) -> Result<Box<dyn Connection>> {
        if self.target.host.contains('\n') || self.target.path.contains('\n') {
            return Err(Error::Other(messages::NEWLINE_FORBIDDEN.into()));
        }
        let mut conn: Box<dyn Connection> = match &self.proxy {
            Some(proxy) => self.open_proxy(proxy)?,
            None => {
                let stream = self.open(options.verbose)?;
                let read = stream.try_clone()?;
                Box::new(TcpConnection {
                    read,
                    write: stream,
                })
            }
        };
        let req = self.target.request(options);
        if let Some(name) = &options.trace_name {
            rgit_protocol::trace::Tracer::new(name.clone()).packet(
                rgit_protocol::trace::Direction::Write,
                rgit_protocol::trace::Traced::Data(&req),
            );
        }
        let len = req.len() + 4;
        conn.write_all(format!("{len:04x}").as_bytes())?;
        conn.write_all(&req)?;
        conn.flush()?;
        Ok(conn)
    }
}

/// A stateful TCP connection.
#[derive(Debug)]
struct TcpConnection {
    read: TcpStream,
    write: TcpStream,
}

impl Read for TcpConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read.read(buf)
    }
}

impl Write for TcpConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write.flush()
    }
}

impl Connection for TcpConnection {
    fn is_stateless(&self) -> bool {
        false
    }

    fn end_request(&mut self) -> Result<()> {
        self.write.flush()?;
        Ok(())
    }

    fn split(&mut self) -> (&mut (dyn Read + Send), &mut dyn crate::WriteHalf) {
        (&mut self.read, &mut self.write)
    }

    fn close(self: Box<Self>) -> Result<()> {
        let _ = self.write.shutdown(Shutdown::Write);
        let mut read = self.read;
        let _ = io::copy(&mut read, &mut io::sink());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Service;

    #[test]
    fn parse_targets() {
        let t = DaemonTarget::parse("example.com/r.git").unwrap();
        assert_eq!(
            t,
            DaemonTarget {
                host: "example.com".into(),
                port: None,
                path: "/r.git".into()
            }
        );
        let t = DaemonTarget::parse("127.0.0.1:1234/~u/x").unwrap();
        assert_eq!(t.port, Some(1234));
        assert_eq!(t.path, "~u/x");
        let t = DaemonTarget::parse("[::1]:99/x").unwrap();
        assert_eq!(t.host, "::1");
        assert_eq!(t.host_param(), "[::1]:99");
        assert!(DaemonTarget::parse("h").is_err());
        assert_eq!(DaemonTarget::parse("h:x/r").unwrap().host, "h:x");
    }

    #[test]
    fn proxy_selection() {
        use rgit_config::{MapEnv, Origin, Scope, parse_bytes};
        let cfg = |t: &str| parse_bytes(t.as_bytes(), Origin::Stdin, Scope::Local).unwrap();
        let none = MapEnv::new(Vec::<(String, String)>::new());
        let c = cfg("[core]\n gitProxy = p for invalid\n gitProxy = none\n gitProxy = q\n");
        assert_eq!(proxy_for("h.invalid", &none, &c).as_deref(), Some("p"));
        assert_eq!(proxy_for("invalid", &none, &c).as_deref(), Some("p"));
        assert_eq!(proxy_for("hinvalid", &none, &c), None);
        let c = cfg("[core]\n gitProxy = p for .invalid\n gitProxy = q\n");
        assert_eq!(proxy_for("h.invalid", &none, &c).as_deref(), Some("q"));
        let env = MapEnv::new([("GIT_PROXY_COMMAND".to_string(), "e".to_string())]);
        assert_eq!(proxy_for("h", &env, &c).as_deref(), Some("e"));
    }

    #[test]
    fn request_bytes() {
        let t = DaemonTarget::parse("h:9/r").unwrap();
        let mut o = ConnectOptions::new(Service::UploadPack);
        assert_eq!(t.request(&o), b"git-upload-pack /r\0host=h:9\0");
        o.protocol_version = rgit_protocol::ProtocolVersion::V2;
        assert_eq!(
            t.request(&o),
            b"git-upload-pack /r\0host=h:9\0\0version=2\0"
        );
    }
}
