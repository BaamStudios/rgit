// SPDX-License-Identifier: MIT
//! The `git-upload-archive` protocol (`git archive --remote`).
//!
//! Observed from the reference `git` (`GIT_TRACE_PACKET`):
//!
//! 1. The client sends one `argument <arg>` packet per `git archive`
//!    argument it forwards, then a flush packet.
//! 2. The server answers `ACK` and a flush packet (before it has looked at
//!    the arguments).
//! 3. The archive follows multiplexed: channel 1 carries the archive,
//!    channel 2 the messages of the archiver. A flush packet ends a
//!    successful archive; when the archiver fails, the server sends its
//!    reason on channel 3 instead.

use std::io::{self, Read, Write};
use std::sync::mpsc;

use rgit_protocol::pktline::{Packet, Reader, Writer};
use rgit_protocol::sideband::{self, SidebandMode};

use crate::messages;

/// The most arguments the archiver accepts from a client.
pub const MAX_ARGS: usize = 64;

/// Send the request: the arguments and a flush packet.
///
/// # Errors
/// Writing failed.
pub fn write_request<W: Write>(w: &mut Writer<W>, args: &[String]) -> rgit_protocol::Result<()> {
    for a in args {
        w.write_line(&format!("argument {a}"))?;
    }
    w.flush_pkt()?;
    w.flush_io()
}

/// The server's answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ack {
    /// `ACK`: the archive follows.
    Ack,
    /// `NACK <reason>`.
    Nack(String),
    /// `ACK` without the flush packet after it.
    MissingFlush,
    /// A flush, delimiter or response-end packet instead of an answer.
    NoAnswer,
    /// Anything else.
    Other(String),
}

/// Read the server's `ACK` and the flush packet after it.
///
/// # Errors
/// The connection failed or ended.
pub fn read_ack<R: Read>(r: &mut Reader<R>) -> rgit_protocol::Result<Ack> {
    let ack = match r.read()? {
        Packet::Data(d) => {
            let line = String::from_utf8_lossy(d);
            let line = line.strip_suffix('\n').unwrap_or(&line).to_string();
            if line == "ACK" {
                Ack::Ack
            } else if let Some(reason) = line.strip_prefix("NACK ") {
                Ack::Nack(reason.to_string())
            } else {
                Ack::Other(line)
            }
        }
        _ => Ack::NoAnswer,
    };
    if ack == Ack::Ack {
        match r.read()? {
            Packet::Flush => {}
            _ => return Ok(Ack::MissingFlush),
        }
    }
    Ok(ack)
}

/// Read the arguments of a request (server side, the archiver).
///
/// # Errors
/// A message for `fatal:` (bad packet, too many arguments) or the
/// protocol error.
pub fn read_arguments<R: Read>(r: &mut Reader<R>) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    loop {
        let packet = r.read().map_err(|e| e.to_string())?;
        let data = match packet {
            Packet::Flush => return Ok(args),
            Packet::Data(d) => d,
            _ => return Err(messages::ARGUMENT_EXPECTED.into()),
        };
        let data = data.strip_suffix(b"\n").unwrap_or(data);
        let Some(arg) = data.strip_prefix(b"argument ") else {
            return Err(messages::ARGUMENT_EXPECTED.into());
        };
        if args.len() >= MAX_ARGS {
            return Err(messages::too_many_options(MAX_ARGS));
        }
        args.push(String::from_utf8_lossy(arg).into_owned());
    }
}

/// Send the `ACK` and the flush packet after it (server side).
///
/// # Errors
/// Writing failed.
pub fn write_ack<W: Write>(w: &mut Writer<W>) -> rgit_protocol::Result<()> {
    w.write_line("ACK")?;
    w.flush_pkt()?;
    w.flush_io()
}

enum Chunk {
    Out(Vec<u8>),
    Err(Vec<u8>),
}

fn pump(mut r: impl Read, tx: mpsc::Sender<Chunk>, out: bool) {
    let mut buf = vec![0u8; 65536];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                let data = buf[..n].to_vec();
                let chunk = if out {
                    Chunk::Out(data)
                } else {
                    Chunk::Err(data)
                };
                if tx.send(chunk).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Relay the archiver's standard output on channel 1 and its standard
/// error on channel 2 until both end (server side).
///
/// # Errors
/// Writing to the client failed.
pub fn multiplex<W: Write>(
    w: &mut Writer<W>,
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
) -> rgit_protocol::Result<()> {
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    let a = std::thread::spawn(move || pump(stdout, tx, true));
    let b = std::thread::spawn(move || pump(stderr, tx2, false));
    let mut result = Ok(());
    for chunk in rx {
        if result.is_err() {
            continue;
        }
        let (band, data) = match &chunk {
            Chunk::Out(d) => (1, d),
            Chunk::Err(d) => (2, d),
        };
        result = sideband::send(w, SidebandMode::Large, band, data).and_then(|()| w.flush_io());
    }
    let _ = a.join();
    let _ = b.join();
    result
}

/// End a successful archive (flush packet), or report a failed archiver
/// on channel 3.
///
/// # Errors
/// Writing failed.
pub fn finish<W: Write>(w: &mut Writer<W>, ok: bool) -> rgit_protocol::Result<()> {
    if ok {
        w.flush_pkt()?;
    } else {
        sideband::send(
            w,
            SidebandMode::Large,
            3,
            messages::ARCHIVER_DIED.as_bytes(),
        )?;
    }
    w.flush_io()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let mut w = Writer::new(Vec::new(), "test");
        write_request(&mut w, &["--format=zip".into(), "HEAD".into()]).unwrap();
        let bytes = w.into_inner();
        assert_eq!(
            bytes,
            b"001aargument --format=zip\n0012argument HEAD\n0000".to_vec()
        );
        let mut r = Reader::new(&bytes[..], "test");
        r.chomp_newline(true);
        assert_eq!(
            read_arguments(&mut r).unwrap(),
            vec!["--format=zip".to_string(), "HEAD".to_string()]
        );
    }

    #[test]
    fn ack() {
        let mut w = Writer::new(Vec::new(), "test");
        write_ack(&mut w).unwrap();
        let bytes = w.into_inner();
        assert_eq!(bytes, b"0008ACK\n0000".to_vec());
        let mut r = Reader::new(&bytes[..], "test");
        assert_eq!(read_ack(&mut r).unwrap(), Ack::Ack);
    }

    #[test]
    fn bad_argument() {
        let bytes = b"0007foo0000";
        let mut r = Reader::new(&bytes[..], "test");
        assert_eq!(
            read_arguments(&mut r).unwrap_err(),
            messages::ARGUMENT_EXPECTED
        );
    }
}
