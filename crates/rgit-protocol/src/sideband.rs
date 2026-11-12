// SPDX-License-Identifier: MIT
//! Sideband multiplexing (`side-band`, `side-band-64k`,
//! `Documentation/gitprotocol-pack.adoc`, "Packfile Data").
//!
//! Each packet starts with a channel byte: 1 = data, 2 = progress (printed
//! on stderr), 3 = fatal error.
//!
//! Progress output, as observed from git 2.56.0:
//! - channel 2 data is cut into lines at every `\n` and `\r`; a piece
//!   without terminator is kept until the next channel 2 packet, so lines
//!   split over packets are reassembled;
//! - every line is written as `remote: <line><terminator>`;
//! - when stderr is a terminal and `TERM` is set and not `dumb`, each line
//!   is preceded by `ESC [ K` (erase to end of line);
//! - otherwise a non-empty, terminated line gets eight spaces before its
//!   terminator, to cover the rest of a longer line it overwrites;
//! - an unterminated piece left at the end of the stream, or before a
//!   channel 3 message, is written with `\n` (and without the spaces);
//! - channel 3 is written as `remote: <msg>\n` (`msg` verbatim, so a
//!   message ending in LF gives an empty line) and ends the stream with an
//!   error;
//! - an empty packet is `protocol error: missing sideband designator`, an
//!   unknown channel `protocol error: bad band #<n>`, end of input
//!   `unexpected disconnect while reading sideband packet` (git prefixes
//!   these with the program name, e.g. `fetch-pack: `).

use std::ffi::OsStr;
use std::io::{self, IsTerminal, Read, Write};

use crate::error::{Error, Result};
use crate::messages::{ANSI_CLEAR_LINE, REMOTE_PREFIX, SPACES_SUFFIX};
use crate::pktline::{Packet, Reader, Writer};

/// Channel number of pack data.
pub const BAND_DATA: u8 = 1;
/// Channel number of progress messages.
pub const BAND_PROGRESS: u8 = 2;
/// Channel number of fatal errors.
pub const BAND_ERROR: u8 = 3;

/// How progress lines are decorated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStyle {
    /// `ESC [ K` before each line (terminal).
    Ansi,
    /// Eight spaces after each non-empty terminated line.
    Spaces,
}

impl ProgressStyle {
    /// Style for the given terminal state and `TERM` value.
    pub fn for_terminal(is_terminal: bool, term: Option<&OsStr>) -> ProgressStyle {
        match term {
            Some(t) if is_terminal && t != "dumb" => ProgressStyle::Ansi,
            _ => ProgressStyle::Spaces,
        }
    }

    /// Style for this process's stderr and `TERM`.
    pub fn detect() -> ProgressStyle {
        ProgressStyle::for_terminal(
            io::stderr().is_terminal(),
            std::env::var_os("TERM").as_deref(),
        )
    }
}

/// Writes remote progress (channel 2) and errors (channel 3) the way git
/// shows them.
#[derive(Debug)]
pub struct Progress<W> {
    out: W,
    style: ProgressStyle,
    pending: Vec<u8>,
}

impl Progress<io::Stderr> {
    /// Progress on stderr with the detected style.
    pub fn stderr() -> Progress<io::Stderr> {
        Progress::new(io::stderr(), ProgressStyle::detect())
    }
}

impl<W: Write> Progress<W> {
    /// Progress written to `out`.
    pub fn new(out: W, style: ProgressStyle) -> Progress<W> {
        Progress {
            out,
            style,
            pending: Vec::new(),
        }
    }

    /// The output.
    pub fn into_inner(self) -> W {
        self.out
    }

    /// The output.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    fn line(&self, out: &mut Vec<u8>, text: &[u8], term: Option<u8>) {
        if self.style == ProgressStyle::Ansi {
            out.extend_from_slice(ANSI_CLEAR_LINE);
        }
        out.extend_from_slice(REMOTE_PREFIX);
        out.extend_from_slice(text);
        if self.style == ProgressStyle::Spaces && term.is_some() && !text.is_empty() {
            out.extend_from_slice(SPACES_SUFFIX);
        }
        out.push(term.unwrap_or(b'\n'));
    }

    /// Feed the payload of one channel 2 packet (without the channel byte).
    pub fn feed(&mut self, data: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(data);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(i) = self.pending[start..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
        {
            let end = start + i;
            self.line(&mut out, &self.pending[start..end], Some(self.pending[end]));
            start = end + 1;
        }
        self.pending.drain(..start);
        self.write(&out)
    }

    /// Write an unterminated piece left from earlier packets, if any.
    pub fn finish(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut out = Vec::new();
        self.line(&mut out, &self.pending, None);
        self.pending.clear();
        self.write(&out)
    }

    /// Write a channel 3 message.
    pub fn remote_error(&mut self, msg: &[u8]) -> io::Result<()> {
        self.finish()?;
        let mut out = REMOTE_PREFIX.to_vec();
        out.extend_from_slice(msg);
        out.push(b'\n');
        self.write(&out)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.out.write_all(buf)?;
        self.out.flush()
    }
}

/// What one sideband packet carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band<'a> {
    /// Channel 1.
    Data(&'a [u8]),
    /// Channel 2.
    Progress(&'a [u8]),
    /// Channel 3.
    Error(&'a [u8]),
}

/// Split a sideband packet payload into channel and data.
pub fn demux(payload: &[u8]) -> Result<Band<'_>> {
    match payload.split_first() {
        None => Err(Error::MissingBand),
        Some((&BAND_DATA, rest)) => Ok(Band::Data(rest)),
        Some((&BAND_PROGRESS, rest)) => Ok(Band::Progress(rest)),
        Some((&BAND_ERROR, rest)) => Ok(Band::Error(rest)),
        Some((&b, _)) => Err(Error::BadBand(b)),
    }
}

/// Reads channel 1 data from a sideband stream, writing progress to a
/// [`Progress`]. The stream ends at a flush packet (or any other special
/// packet). Implements [`Read`]; protocol errors surface as
/// [`io::Error`]s wrapping [`Error`] (see [`SidebandReader::take_error`]).
#[derive(Debug)]
pub struct SidebandReader<R, W> {
    reader: Reader<R>,
    progress: Progress<W>,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
    error: Option<Error>,
}

impl<R: Read, W: Write> SidebandReader<R, W> {
    /// Demultiplex the packets of `reader`. The reader's trace name becomes
    /// `sideband` (as in git), LF chomping and `ERR` detection are turned
    /// off.
    pub fn new(mut reader: Reader<R>, progress: Progress<W>) -> SidebandReader<R, W> {
        reader.chomp_newline(false).err_packets(false);
        reader.tracer_mut().set_name("sideband");
        SidebandReader {
            reader,
            progress,
            buf: Vec::new(),
            pos: 0,
            done: false,
            error: None,
        }
    }

    /// The next piece of channel 1 data; `None` at the end of the stream.
    pub fn next_data(&mut self) -> Result<Option<&[u8]>> {
        loop {
            if self.done {
                return Ok(None);
            }
            let packet = match self.reader.read_or_eof() {
                Ok(Some(p)) => p,
                Ok(None) | Err(Error::HungUp) => {
                    self.done = true;
                    self.progress.finish()?;
                    return Err(Error::SidebandDisconnect);
                }
                Err(e) => {
                    self.done = true;
                    return Err(e);
                }
            };
            let Packet::Data(payload) = packet else {
                self.done = true;
                self.progress.finish()?;
                return Ok(None);
            };
            match demux(payload) {
                Ok(Band::Data(d)) => {
                    if d.is_empty() {
                        continue;
                    }
                    self.buf.clear();
                    self.buf.extend_from_slice(d);
                    return Ok(Some(&self.buf));
                }
                Ok(Band::Progress(p)) => self.progress.feed(p)?,
                Ok(Band::Error(m)) => {
                    self.done = true;
                    let msg = m.to_vec();
                    self.progress.remote_error(&msg)?;
                    let text = String::from_utf8_lossy(&msg);
                    let text = text.strip_suffix('\n').unwrap_or(&text);
                    return Err(Error::SidebandFatal(text.to_owned()));
                }
                Err(e) => {
                    self.done = true;
                    self.progress.finish()?;
                    return Err(e);
                }
            }
        }
    }

    /// The protocol error behind the last `io::Error` returned by
    /// [`Read::read`], if any.
    pub fn take_error(&mut self) -> Option<Error> {
        self.error.take()
    }

    /// The packet reader and the progress writer.
    pub fn into_parts(self) -> (Reader<R>, Progress<W>) {
        (self.reader, self.progress)
    }
}

impl<R: Read, W: Write> Read for SidebandReader<R, W> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() {
            self.pos = 0;
            self.buf.clear();
            match self.next_data() {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(0),
                Err(Error::Io(e)) => return Err(e),
                Err(e) => {
                    let io = io::Error::other(e.to_string());
                    self.error = Some(e);
                    return Err(io);
                }
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Sideband variant negotiated with the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebandMode {
    /// `side-band`: pkt-lines of at most 1000 bytes.
    Small,
    /// `side-band-64k`: pkt-lines of at most 65520 bytes.
    Large,
}

impl SidebandMode {
    /// Data bytes per packet, excluding length and channel byte.
    pub fn max_data(self) -> usize {
        match self {
            SidebandMode::Small => 1000 - 4 - 1,
            SidebandMode::Large => crate::pktline::MAX_DATA_LEN - 1,
        }
    }
}

/// Send `data` on `band`, split into as many packets as needed. Empty data
/// sends nothing.
pub fn send<W: Write>(
    writer: &mut Writer<W>,
    mode: SidebandMode,
    band: u8,
    data: &[u8],
) -> Result<()> {
    let mut pkt = Vec::with_capacity(mode.max_data().min(data.len()) + 1);
    for chunk in data.chunks(mode.max_data()) {
        pkt.clear();
        pkt.push(band);
        pkt.extend_from_slice(chunk);
        writer.write_data(&pkt)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pktline::encode;

    fn progress(style: ProgressStyle, packets: &[&[u8]]) -> Vec<u8> {
        let mut p = Progress::new(Vec::new(), style);
        for d in packets {
            p.feed(d).unwrap();
        }
        p.finish().unwrap();
        p.into_inner()
    }

    #[test]
    fn style_detection() {
        let t = Some(OsStr::new("xterm"));
        assert_eq!(ProgressStyle::for_terminal(true, t), ProgressStyle::Ansi);
        assert_eq!(ProgressStyle::for_terminal(false, t), ProgressStyle::Spaces);
        assert_eq!(
            ProgressStyle::for_terminal(true, Some(OsStr::new("dumb"))),
            ProgressStyle::Spaces
        );
        assert_eq!(
            ProgressStyle::for_terminal(true, None),
            ProgressStyle::Spaces
        );
    }

    // Expected bytes observed from git 2.56.0 (non-terminal stderr).
    #[test]
    fn spaces_style() {
        let out = progress(
            ProgressStyle::Spaces,
            &[
                b"Foo.\n", b"Hel", b"lo\n", b"a\rb\n", b"\n", b"x\r\n", b"\r", b"tail",
            ],
        );
        assert_eq!(
            out,
            b"remote: Foo.        \nremote: Hello        \n\
              remote: a        \rremote: b        \nremote: \n\
              remote: x        \rremote: \nremote: \rremote: tail\n"
        );
    }

    // Expected bytes observed from git 2.56.0 on a pty with TERM=xterm.
    #[test]
    fn ansi_style() {
        let out = progress(ProgressStyle::Ansi, &[b"a\rb\n\n", b"tail"]);
        assert_eq!(
            out,
            b"\x1b[Kremote: a\r\x1b[Kremote: b\n\x1b[Kremote: \n\x1b[Kremote: tail\n"
        );
    }

    #[test]
    fn multiple_lines_in_one_packet() {
        let out = progress(ProgressStyle::Spaces, &[b"one\ntwo\nthr", b"ee\n"]);
        assert_eq!(
            out,
            b"remote: one        \nremote: two        \nremote: three        \n"
        );
    }

    fn stream(packets: &[&[u8]]) -> Vec<u8> {
        let mut s = Vec::new();
        for p in packets {
            if p.is_empty() {
                s.extend_from_slice(b"0000");
            } else {
                s.extend(encode(p).unwrap());
            }
        }
        s
    }

    fn run(input: &[u8]) -> (Vec<u8>, Vec<u8>, Option<Error>) {
        let reader = Reader::new(input, "test");
        let mut sb = SidebandReader::new(reader, Progress::new(Vec::new(), ProgressStyle::Spaces));
        let mut data = Vec::new();
        let err = loop {
            match sb.next_data() {
                Ok(Some(d)) => data.extend_from_slice(d),
                Ok(None) => break None,
                Err(e) => break Some(e),
            }
        };
        let (_, p) = sb.into_parts();
        (data, p.into_inner(), err)
    }

    #[test]
    fn demultiplex() {
        let input = stream(&[
            b"\x02Foo.\n",
            b"\x01PACK",
            b"\x02Hel",
            b"\x01data",
            b"\x02lo",
            b"",
        ]);
        let (data, err, e) = run(&input);
        assert_eq!(data, b"PACKdata");
        assert_eq!(err, b"remote: Foo.        \nremote: Hello\n");
        assert!(e.is_none());
    }

    #[test]
    fn channel_three() {
        // Observed: "remote: part\nremote: boom\n" and "remote: boom\n\n".
        let (_, err, e) = run(&stream(&[b"\x02part", b"\x03boom"]));
        assert_eq!(err, b"remote: part\nremote: boom\n");
        assert_eq!(e.unwrap().to_string(), "remote error: boom");
        let (_, err, _) = run(&stream(&[b"\x02progress\n", b"\x03boom\n"]));
        assert_eq!(err, b"remote: progress        \nremote: boom\n\n");
    }

    #[test]
    fn protocol_errors() {
        let (_, _, e) = run(b"0004");
        assert_eq!(
            e.unwrap().to_string(),
            "protocol error: missing sideband designator"
        );
        let (_, _, e) = run(&stream(&[b"\x05huh\n"]));
        assert_eq!(e.unwrap().to_string(), "protocol error: bad band #5");
        for input in [&b"1234"[..], b"0010\x02ab", b"00", b""] {
            let (_, _, e) = run(input);
            assert_eq!(
                e.unwrap().to_string(),
                "unexpected disconnect while reading sideband packet",
                "{input:?}"
            );
        }
    }

    #[test]
    fn read_impl() {
        let input = stream(&[b"\x01abc", b"\x02x\n", b"\x01def", b""]);
        let reader = Reader::new(&input[..], "test");
        let mut sb = SidebandReader::new(reader, Progress::new(Vec::new(), ProgressStyle::Spaces));
        let mut data = Vec::new();
        sb.read_to_end(&mut data).unwrap();
        assert_eq!(data, b"abcdef");
        let input = stream(&[b"\x03no"]);
        let mut sb = SidebandReader::new(
            Reader::new(&input[..], "test"),
            Progress::new(Vec::new(), ProgressStyle::Spaces),
        );
        assert!(sb.read_to_end(&mut Vec::new()).is_err());
        assert!(matches!(sb.take_error(), Some(Error::SidebandFatal(_))));
    }

    #[test]
    fn send_splits() {
        let mut w = Writer::new(Vec::new(), "test");
        let data = vec![7u8; 2000];
        send(&mut w, SidebandMode::Small, BAND_DATA, &data).unwrap();
        send(&mut w, SidebandMode::Large, BAND_PROGRESS, b"hi\n").unwrap();
        let bytes = w.into_inner();
        let mut r = Reader::new(&bytes[..], "test");
        r.chomp_newline(false);
        let mut lens = Vec::new();
        while let Some(p) = r.read_or_eof().unwrap() {
            lens.push(p.data().unwrap().len());
        }
        assert_eq!(lens, vec![996, 996, 11, 4]);
    }
}
