// SPDX-License-Identifier: MIT
//! pkt-line framing (`Documentation/gitprotocol-common.adoc`, packet types
//! from `gitprotocol-v2.adoc`).
//!
//! A pkt-line is four lowercase hex digits giving the total length
//! (including the four digits) followed by the payload. Lengths 0, 1 and 2
//! are the special flush (`0000`), delimiter (`0001`) and response-end
//! (`0002`) packets; 3 is invalid.
//!
//! Limits, as observed from git 2.56.0:
//! - writers never send more than [`MAX_DATA_LEN`] (65516) payload bytes;
//! - readers accept up to [`MAX_READ_DATA_LEN`] (65519) payload bytes and
//!   reject longer packets with `protocol error: bad line length <n>`, `n`
//!   being the payload length;
//! - upper-case hex digits are accepted when reading.

use std::borrow::Cow;
use std::io::{self, Read, Write};

use crate::error::{Error, Result};
use crate::trace::{Direction, Traced, Tracer};

/// Largest payload a writer may send (65520 bytes per pkt-line).
pub const MAX_DATA_LEN: usize = 65516;
/// Largest total pkt-line length a writer may send.
pub const MAX_PKT_LEN: usize = MAX_DATA_LEN + 4;
/// Largest payload a reader accepts.
pub const MAX_READ_DATA_LEN: usize = 65519;

/// The flush packet.
pub const FLUSH_PKT: &[u8; 4] = b"0000";
/// The delimiter packet (protocol v2).
pub const DELIM_PKT: &[u8; 4] = b"0001";
/// The response-end packet (protocol v2, stateless connections).
pub const RESPONSE_END_PKT: &[u8; 4] = b"0002";

/// One packet as returned by [`Reader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    /// `0000`.
    Flush,
    /// `0001`.
    Delim,
    /// `0002`.
    ResponseEnd,
    /// A data packet (with the trailing LF removed if the reader chomps).
    Data(&'a [u8]),
}

impl<'a> Packet<'a> {
    /// The payload of a data packet.
    pub fn data(self) -> Option<&'a [u8]> {
        match self {
            Packet::Data(d) => Some(d),
            _ => None,
        }
    }

    /// The payload as text, if it is a data packet with valid UTF-8.
    pub fn text(self) -> Option<&'a str> {
        self.data().and_then(|d| std::str::from_utf8(d).ok())
    }

    /// How the packet appears in a trace and in "unexpected" errors (without
    /// escaping).
    pub fn describe(self) -> String {
        match self {
            Packet::Flush => "0000".into(),
            Packet::Delim => "0001".into(),
            Packet::ResponseEnd => "0002".into(),
            Packet::Data(d) => String::from_utf8_lossy(d).into_owned(),
        }
    }

    /// An [`Error::Unexpected`] for this packet.
    pub fn unexpected(self) -> Error {
        Error::Unexpected(self.describe())
    }
}

/// Append one data pkt-line to `out`.
pub fn encode_data(out: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    if data.len() > MAX_DATA_LEN {
        return Err(Error::DataTooLong(data.len()));
    }
    out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
    out.extend_from_slice(data);
    Ok(())
}

/// Encode one data pkt-line into a new buffer.
pub fn encode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() + 4);
    encode_data(&mut out, data)?;
    Ok(out)
}

fn hex_value(b: u8) -> Option<usize> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as usize),
        b'a'..=b'f' => Some((b - b'a' + 10) as usize),
        b'A'..=b'F' => Some((b - b'A' + 10) as usize),
        _ => None,
    }
}

/// Parse a four-byte length header.
pub fn parse_length(raw: [u8; 4]) -> Result<usize> {
    raw.iter().try_fold(0usize, |acc, &b| {
        hex_value(b)
            .map(|v| acc * 16 + v)
            .ok_or(Error::BadLengthCharacter(raw))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Flush,
    Delim,
    ResponseEnd,
    Data,
    Eof,
}

/// Reads pkt-lines from a byte stream.
///
/// By default the reader removes one trailing LF from data packets
/// ([`Reader::chomp_newline`]) and turns `ERR <msg>` packets into
/// [`Error::Remote`] ([`Reader::err_packets`]).
#[derive(Debug)]
pub struct Reader<R> {
    inner: R,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    kind: Kind,
    peeked: bool,
    chomp: bool,
    err_packets: bool,
    tracer: Tracer,
}

impl<R: Read> Reader<R> {
    /// A reader that traces as `name` (git's program name, e.g.
    /// `ls-remote`, `fetch`, `upload-pack`).
    pub fn new(inner: R, name: impl Into<Cow<'static, str>>) -> Reader<R> {
        Reader {
            inner,
            buf: Vec::new(),
            start: 0,
            end: 0,
            kind: Kind::Eof,
            peeked: false,
            chomp: true,
            err_packets: true,
            tracer: Tracer::new(name),
        }
    }

    /// Whether one trailing LF is removed from data packets (default on).
    pub fn chomp_newline(&mut self, on: bool) -> &mut Self {
        self.chomp = on;
        self
    }

    /// Whether `ERR <msg>` packets become [`Error::Remote`] (default on).
    pub fn err_packets(&mut self, on: bool) -> &mut Self {
        self.err_packets = on;
        self
    }

    /// The tracing state (program name).
    pub fn tracer_mut(&mut self) -> &mut Tracer {
        &mut self.tracer
    }

    /// The underlying stream.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// The underlying stream; a peeked packet is lost.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Read the next packet. End of input, at a packet boundary or inside a
    /// packet, is [`Error::HungUp`].
    pub fn read(&mut self) -> Result<Packet<'_>> {
        self.read_or_eof()?.ok_or(Error::HungUp)
    }

    /// Read the next packet; `None` if the input ends cleanly at a packet
    /// boundary. End of input inside a packet is [`Error::HungUp`].
    pub fn read_or_eof(&mut self) -> Result<Option<Packet<'_>>> {
        if self.peeked {
            self.peeked = false;
        } else {
            self.advance()?;
        }
        Ok(self.current())
    }

    /// Look at the next packet without consuming it.
    pub fn peek(&mut self) -> Result<Option<Packet<'_>>> {
        if !self.peeked {
            self.advance()?;
            self.peeked = true;
        }
        Ok(self.current())
    }

    fn current(&self) -> Option<Packet<'_>> {
        match self.kind {
            Kind::Flush => Some(Packet::Flush),
            Kind::Delim => Some(Packet::Delim),
            Kind::ResponseEnd => Some(Packet::ResponseEnd),
            Kind::Data => Some(Packet::Data(&self.buf[self.start..self.end])),
            Kind::Eof => None,
        }
    }

    fn advance(&mut self) -> Result<()> {
        // A failed read leaves the reader at end of input.
        self.kind = Kind::Eof;
        let mut hdr = [0u8; 4];
        let got = read_full(&mut self.inner, &mut hdr)?;
        if got == 0 {
            return Ok(());
        }
        if got < 4 {
            return Err(Error::HungUp);
        }
        let len = parse_length(hdr)?;
        let kind = match len {
            0 => Kind::Flush,
            1 => Kind::Delim,
            2 => Kind::ResponseEnd,
            3 => return Err(Error::BadLength(3)),
            _ => Kind::Data,
        };
        match kind {
            Kind::Flush => self.tracer.packet(Direction::Read, Traced::Flush),
            Kind::Delim => self.tracer.packet(Direction::Read, Traced::Delim),
            Kind::ResponseEnd => self.tracer.packet(Direction::Read, Traced::ResponseEnd),
            _ => {
                let n = len - 4;
                if n > MAX_READ_DATA_LEN {
                    return Err(Error::BadLength(n));
                }
                self.buf.resize(n, 0);
                if read_full(&mut self.inner, &mut self.buf)? < n {
                    return Err(Error::HungUp);
                }
                self.tracer
                    .packet(Direction::Read, Traced::Data(&self.buf[..n]));
                self.start = 0;
                self.end = n;
                if self.err_packets {
                    if let Some(msg) = self.buf.strip_prefix(b"ERR ") {
                        let msg = msg.strip_suffix(b"\n").unwrap_or(msg);
                        return Err(Error::Remote(String::from_utf8_lossy(msg).into_owned()));
                    }
                }
                if self.chomp && self.buf.last() == Some(&b'\n') {
                    self.end -= 1;
                }
            }
        }
        self.kind = kind;
        Ok(())
    }
}

/// Read until `buf` is full or end of input; returns the bytes read.
fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// Writes pkt-lines. Every packet goes to the underlying stream with a
/// single `write_all`; call [`Writer::flush_io`] (or flush the stream) when
/// the peer has to see the data.
#[derive(Debug)]
pub struct Writer<W> {
    inner: W,
    tracer: Tracer,
    scratch: Vec<u8>,
}

impl<W: Write> Writer<W> {
    /// A writer that traces as `name`.
    pub fn new(inner: W, name: impl Into<Cow<'static, str>>) -> Writer<W> {
        Writer {
            inner,
            tracer: Tracer::new(name),
            scratch: Vec::new(),
        }
    }

    /// The tracing state (program name).
    pub fn tracer_mut(&mut self) -> &mut Tracer {
        &mut self.tracer
    }

    /// The underlying stream.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// The underlying stream.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Write a data packet with `data` as payload, unchanged.
    pub fn write_data(&mut self, data: &[u8]) -> Result<()> {
        self.scratch.clear();
        encode_data(&mut self.scratch, data)?;
        self.tracer.packet(Direction::Write, Traced::Data(data));
        self.inner.write_all(&self.scratch)?;
        Ok(())
    }

    /// Write a text line: `line` followed by LF.
    pub fn write_line(&mut self, line: &str) -> Result<()> {
        let mut data = Vec::with_capacity(line.len() + 1);
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        self.write_data(&data)
    }

    /// Write a flush packet.
    pub fn flush_pkt(&mut self) -> Result<()> {
        self.special(FLUSH_PKT, Traced::Flush)
    }

    /// Write a delimiter packet.
    pub fn delim_pkt(&mut self) -> Result<()> {
        self.special(DELIM_PKT, Traced::Delim)
    }

    /// Write a response-end packet.
    pub fn response_end_pkt(&mut self) -> Result<()> {
        self.special(RESPONSE_END_PKT, Traced::ResponseEnd)
    }

    /// Write an `ERR <msg>` packet.
    pub fn write_err(&mut self, msg: &str) -> Result<()> {
        self.write_line(&format!("ERR {msg}"))
    }

    /// Flush the underlying stream.
    pub fn flush_io(&mut self) -> Result<()> {
        self.inner.flush()?;
        Ok(())
    }

    fn special(&mut self, raw: &[u8; 4], traced: Traced<'_>) -> Result<()> {
        self.tracer.packet(Direction::Write, traced);
        self.inner.write_all(raw)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(input: &[u8]) -> Reader<&[u8]> {
        Reader::new(input, "test")
    }

    #[test]
    fn encode_examples_from_spec() {
        assert_eq!(encode(b"a\n").unwrap(), b"0006a\n");
        assert_eq!(encode(b"a").unwrap(), b"0005a");
        assert_eq!(encode(b"foobar\n").unwrap(), b"000bfoobar\n");
        assert_eq!(encode(b"").unwrap(), b"0004");
    }

    #[test]
    fn encode_limits() {
        let max = vec![b'x'; MAX_DATA_LEN];
        let enc = encode(&max).unwrap();
        assert_eq!(&enc[..4], b"fff0");
        assert_eq!(enc.len(), MAX_PKT_LEN);
        let too_long = vec![b'x'; MAX_DATA_LEN + 1];
        assert!(matches!(encode(&too_long), Err(Error::DataTooLong(65517))));
    }

    #[test]
    fn read_special_and_data() {
        let mut r = reader(b"0006a\n0005a00000001000200040000");
        assert_eq!(r.read().unwrap(), Packet::Data(b"a"));
        assert_eq!(r.read().unwrap(), Packet::Data(b"a"));
        assert_eq!(r.read().unwrap(), Packet::Flush);
        assert_eq!(r.read().unwrap(), Packet::Delim);
        assert_eq!(r.read().unwrap(), Packet::ResponseEnd);
        assert_eq!(r.read().unwrap(), Packet::Data(b""));
        assert_eq!(r.read().unwrap(), Packet::Flush);
        assert_eq!(r.read_or_eof().unwrap(), None);
        assert!(matches!(r.read(), Err(Error::HungUp)));
    }

    #[test]
    fn no_chomp_and_only_one_lf() {
        let mut r = reader(b"0007a\n\n0006a\n");
        assert_eq!(r.read().unwrap(), Packet::Data(b"a\n"));
        r.chomp_newline(false);
        assert_eq!(r.read().unwrap(), Packet::Data(b"a\n"));
    }

    #[test]
    fn binary_clean() {
        let data: Vec<u8> = (0..=255u8).collect();
        let enc = encode(&data).unwrap();
        let mut r = reader(&enc);
        r.chomp_newline(false);
        assert_eq!(r.read().unwrap(), Packet::Data(&data[..]));
    }

    #[test]
    fn uppercase_hex_accepted() {
        let mut r = reader(b"000AABCDEF");
        assert_eq!(r.read().unwrap(), Packet::Data(b"ABCDEF"));
    }

    #[test]
    fn bad_length_character() {
        let e = reader(b"zzzz").read().unwrap_err();
        assert_eq!(
            e.to_string(),
            "protocol error: bad line length character: zzzz"
        );
        let e = reader(b"00g1xxxxx").read().unwrap_err();
        assert_eq!(
            e.to_string(),
            "protocol error: bad line length character: 00g1"
        );
    }

    #[test]
    fn bad_length() {
        let e = reader(b"0003").read().unwrap_err();
        assert_eq!(e.to_string(), "protocol error: bad line length 3");
        // Boundaries observed from git 2.56.0: fff3 is read, fff4 is not.
        let mut big = b"fff3".to_vec();
        big.extend(std::iter::repeat_n(b'x', 65519));
        let mut r = reader(&big);
        assert_eq!(r.read().unwrap().data().unwrap().len(), 65519);
        let e = reader(b"fff4").read().unwrap_err();
        assert_eq!(e.to_string(), "protocol error: bad line length 65520");
        let e = reader(b"ffff").read().unwrap_err();
        assert_eq!(e.to_string(), "protocol error: bad line length 65531");
    }

    #[test]
    fn eof_inside_packet() {
        for input in [&b"00"[..], b"000", b"0010abc"] {
            let mut r = reader(input);
            let e = r.read_or_eof().unwrap_err();
            assert_eq!(e.to_string(), "the remote end hung up unexpectedly");
        }
    }

    #[test]
    fn err_packet() {
        let e = reader(&encode(b"ERR access denied: foo\n").unwrap())
            .read()
            .unwrap_err();
        assert_eq!(e.to_string(), "remote error: access denied: foo");
        let mut r = reader(b"0009ERR x");
        r.err_packets(false);
        assert_eq!(r.read().unwrap(), Packet::Data(b"ERR x"));
        // "ERR" needs the space.
        assert_eq!(reader(b"0007ERR").read().unwrap(), Packet::Data(b"ERR"));
    }

    #[test]
    fn peek_then_read() {
        let mut r = reader(b"000eversion 2\n0000");
        assert_eq!(r.peek().unwrap(), Some(Packet::Data(b"version 2")));
        assert_eq!(r.peek().unwrap(), Some(Packet::Data(b"version 2")));
        assert_eq!(r.read().unwrap(), Packet::Data(b"version 2"));
        assert_eq!(r.peek().unwrap(), Some(Packet::Flush));
        assert_eq!(r.read().unwrap(), Packet::Flush);
        assert_eq!(r.peek().unwrap(), None);
        assert_eq!(r.read_or_eof().unwrap(), None);
    }

    #[test]
    fn writer_output() {
        let mut w = Writer::new(Vec::new(), "test");
        w.write_line("command=ls-refs").unwrap();
        w.delim_pkt().unwrap();
        w.write_data(b"peel").unwrap();
        w.flush_pkt().unwrap();
        w.response_end_pkt().unwrap();
        w.write_err("nope").unwrap();
        assert_eq!(
            w.into_inner(),
            b"0014command=ls-refs\n00010008peel00000002000dERR nope\n"
        );
    }

    #[test]
    fn round_trip() {
        let mut w = Writer::new(Vec::new(), "test");
        let payloads: Vec<Vec<u8>> = (0..200usize)
            .map(|i| (0..(i * 331 % 2000)).map(|j| (j * 7 + i) as u8).collect())
            .collect();
        for p in &payloads {
            w.write_data(p).unwrap();
        }
        w.flush_pkt().unwrap();
        let bytes = w.into_inner();
        let mut r = reader(&bytes);
        r.chomp_newline(false).err_packets(false);
        for p in &payloads {
            assert_eq!(r.read().unwrap(), Packet::Data(&p[..]));
        }
        assert_eq!(r.read().unwrap(), Packet::Flush);
        assert_eq!(r.read_or_eof().unwrap(), None);
    }
}
