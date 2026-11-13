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

use crate::{ConnectOptions, Connection, Error, Result, Transport, messages};

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
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
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
}

impl DaemonTransport {
    /// Connect to the first address of the host that accepts.
    fn open(&self) -> Result<TcpStream> {
        let host = self.target.host.as_str();
        let port = self.target.port.unwrap_or(DEFAULT_PORT);
        let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
            Ok(a) => a.collect(),
            Err(_) => return Err(Error::Other(messages::unable_to_look_up(host, port))),
        };
        let mut failures = String::new();
        for (i, addr) in addrs.iter().enumerate() {
            match TcpStream::connect(addr) {
                Ok(s) => return Ok(s),
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
        let mut stream = self.open()?;
        let req = self.target.request(options);
        let len = req.len() + 4;
        stream.write_all(format!("{len:04x}").as_bytes())?;
        stream.write_all(&req)?;
        stream.flush()?;
        let read = stream.try_clone()?;
        Ok(Box::new(TcpConnection {
            read,
            write: stream,
        }))
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

    fn split(&mut self) -> (&mut (dyn Read + Send), &mut (dyn Write + Send)) {
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
