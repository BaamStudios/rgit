// SPDX-License-Identifier: MIT
//! The byte stream a mail protocol runs over, and line-oriented reading.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use rustls::{ClientConnection, StreamOwned};

use crate::tls::{self, TlsOptions};

/// A callback receiving protocol log lines (debug / verbose output).
pub type Log = Box<dyn FnMut(&str)>;

/// A connection: plain TCP, TLS over TCP, or a tunnel command's pipes.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
    Tunnel(Box<Tunnel>),
}

/// The pipes of a tunnel command; dropping closes its input and waits.
pub struct Tunnel {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // Close the pipe first so the command sees the end of input.
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

impl Stream {
    /// Connect to `host:port` over TCP.
    pub fn connect(host: &str, port: u16, timeout: Option<Duration>) -> io::Result<Stream> {
        let mut last = None;
        for addr in (host, port).to_socket_addrs()? {
            let res = match timeout {
                Some(t) => TcpStream::connect_timeout(&addr, t),
                None => TcpStream::connect(addr),
            };
            match res {
                Ok(s) => {
                    s.set_read_timeout(timeout)?;
                    return Ok(Stream::Plain(s));
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other(format!("could not resolve {host}"))))
    }

    /// Connect to `host:port` and start TLS right away.
    pub fn connect_tls(
        host: &str,
        port: u16,
        timeout: Option<Duration>,
        opts: &TlsOptions,
    ) -> io::Result<Stream> {
        Stream::connect(host, port, timeout)?.start_tls(host, opts)
    }

    /// Run `command` through the shell and talk to its standard input and
    /// output.
    pub fn tunnel(command: &str) -> io::Result<Stream> {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take();
        let output = child.stdout.take().expect("piped stdout");
        Ok(Stream::Tunnel(Box::new(Tunnel {
            child,
            input,
            output,
        })))
    }

    /// Upgrade a plain TCP connection to TLS (implicit TLS or STARTTLS).
    pub fn start_tls(self, host: &str, opts: &TlsOptions) -> io::Result<Stream> {
        let Stream::Plain(tcp) = self else {
            return Err(io::Error::other("TLS is only possible on a TCP connection"));
        };
        let config = tls::client_config(opts)?;
        let conn = ClientConnection::new(config, tls::server_name(host)?)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut s = StreamOwned::new(conn, tcp);
        // Complete the handshake now so certificate errors show up here.
        while s.conn.is_handshaking() {
            s.conn.complete_io(&mut s.sock)?;
        }
        Ok(Stream::Tls(Box::new(s)))
    }

    pub fn is_tls(&self) -> bool {
        matches!(self, Stream::Tls(_))
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
            Stream::Tunnel(t) => t.output.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
            Stream::Tunnel(t) => match t.input.as_mut() {
                Some(i) => i.write(buf),
                None => Err(io::ErrorKind::BrokenPipe.into()),
            },
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
            Stream::Tunnel(t) => t.input.as_mut().map_or(Ok(()), |i| i.flush()),
        }
    }
}

/// A stream with buffered line reading.
pub struct LineStream {
    inner: Option<BufReader<Stream>>,
}

impl LineStream {
    pub fn new(stream: Stream) -> LineStream {
        LineStream {
            inner: Some(BufReader::new(stream)),
        }
    }

    fn reader(&mut self) -> &mut BufReader<Stream> {
        self.inner.as_mut().expect("stream present")
    }

    /// Read one line without its line ending; an error at end of input.
    pub fn read_line(&mut self) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        let n = self.reader().read_until(b'\n', &mut line)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed by the server",
            ));
        }
        if line.ends_with(b"\n") {
            line.pop();
        }
        if line.ends_with(b"\r") {
            line.pop();
        }
        Ok(line)
    }

    /// Read exactly `n` bytes (an IMAP literal).
    pub fn read_exact_bytes(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0; n];
        self.reader().read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Write and flush.
    pub fn send(&mut self, data: &[u8]) -> io::Result<()> {
        let s = self.reader().get_mut();
        s.write_all(data)?;
        s.flush()
    }

    pub fn is_tls(&self) -> bool {
        self.inner.as_ref().is_some_and(|r| r.get_ref().is_tls())
    }

    /// Replace the stream by its TLS upgrade (after STARTTLS).
    pub fn start_tls(&mut self, host: &str, opts: &TlsOptions) -> io::Result<()> {
        let reader = self.inner.take().expect("stream present");
        let stream = reader.into_inner().start_tls(host, opts)?;
        self.inner = Some(BufReader::new(stream));
        Ok(())
    }
}
