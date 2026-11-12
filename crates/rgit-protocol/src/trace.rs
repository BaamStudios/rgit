// SPDX-License-Identifier: MIT
//! `GIT_TRACE_PACKET` output.
//!
//! Observed from git 2.56.0, each packet read or written produces one line
//!
//! ```text
//! packet:    ls-remote< 8a9d...2d62 HEAD\0multi_ack thin-pack ...
//! packet:  upload-pack> 0000
//! ```
//!
//! - `packet: `, the program name right-aligned in 12 columns, `<` for a
//!   read or `>` for a write, a space, the payload;
//! - flush, delimiter and response-end packets appear as `0000`, `0001`,
//!   `0002`;
//! - LF bytes are left out (all of them, not only a trailing one); other
//!   bytes outside `0x20..=0x7e` are written as a backslash and the octal
//!   value of the byte taken as a *signed* char, so `\0`, `\11` (tab),
//!   `\177`, and `\37777777777` for `0xff`; a backslash itself is not
//!   escaped;
//! - a packet starting with `PACK` (or `\1PACK` on a sideband stream) is
//!   written as `PACK ...`, and from then on sideband channel 1 packets are
//!   no longer traced; other channels and flush packets still are;
//! - long payloads are not shortened.

use std::borrow::Cow;

use rgit_trace::TRACE_PACKET;

/// Direction of a traced packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Read from the peer (`<`).
    Read,
    /// Written to the peer (`>`).
    Write,
}

impl Direction {
    fn symbol(self) -> u8 {
        match self {
            Direction::Read => b'<',
            Direction::Write => b'>',
        }
    }
}

/// What a packet looks like for tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Traced<'a> {
    /// `0000`.
    Flush,
    /// `0001`.
    Delim,
    /// `0002`.
    ResponseEnd,
    /// A data packet with its raw payload (before any LF is removed).
    Data(&'a [u8]),
}

/// Append `payload` with git's escaping (see the module documentation).
pub fn escape_into(out: &mut Vec<u8>, payload: &[u8]) {
    for &b in payload {
        if b == b'\n' {
            continue;
        }
        if (0x20..=0x7e).contains(&b) {
            out.push(b);
        } else {
            // C's "%o" of a sign-extended char.
            let v = b as i8 as i32 as u32;
            out.push(b'\\');
            out.extend_from_slice(format!("{v:o}").as_bytes());
        }
    }
}

/// The trace message (without the time/location header and without a
/// newline) for one packet.
pub fn format_packet(name: &str, dir: Direction, packet: Traced<'_>) -> Vec<u8> {
    let mut out = format!("packet: {name:>12}").into_bytes();
    out.push(dir.symbol());
    out.push(b' ');
    match packet {
        Traced::Flush => out.extend_from_slice(b"0000"),
        Traced::Delim => out.extend_from_slice(b"0001"),
        Traced::ResponseEnd => out.extend_from_slice(b"0002"),
        Traced::Data(p) => {
            if is_pack_start(p) {
                out.extend_from_slice(b"PACK ...");
            } else {
                escape_into(&mut out, p);
            }
        }
    }
    out
}

fn is_pack_start(p: &[u8]) -> bool {
    p.starts_with(b"PACK") || p.starts_with(b"\x01PACK")
}

/// Per-endpoint tracing state: the program name and whether pack data has
/// started.
#[derive(Debug, Clone)]
pub struct Tracer {
    name: Cow<'static, str>,
    pack_seen: bool,
}

impl Tracer {
    /// A tracer printing `name` (e.g. `ls-remote`, `fetch`, `upload-pack`).
    pub fn new(name: impl Into<Cow<'static, str>>) -> Tracer {
        Tracer {
            name: name.into(),
            pack_seen: false,
        }
    }

    /// The program name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Change the program name (the sideband demultiplexer traces as
    /// `sideband`).
    pub fn set_name(&mut self, name: impl Into<Cow<'static, str>>) {
        self.name = name.into();
    }

    /// The message for `packet`, or `None` if it is not traced. Updates the
    /// pack state. Exposed for tests; [`Tracer::packet`] writes it.
    pub fn message(&mut self, dir: Direction, packet: Traced<'_>) -> Option<Vec<u8>> {
        if let Traced::Data(p) = packet {
            if self.pack_seen && p.first() == Some(&1) {
                return None;
            }
            if is_pack_start(p) {
                self.pack_seen = true;
            }
        }
        Some(format_packet(&self.name, dir, packet))
    }

    /// Trace `packet` to `GIT_TRACE_PACKET` if enabled.
    pub fn packet(&mut self, dir: Direction, packet: Traced<'_>) {
        if !TRACE_PACKET.enabled() {
            return;
        }
        if let Some(msg) = self.message(dir, packet) {
            TRACE_PACKET.write_line(rgit_trace::location!(), &msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(name: &str, dir: Direction, p: Traced<'_>) -> String {
        String::from_utf8(format_packet(name, dir, p)).unwrap()
    }

    #[test]
    fn layout() {
        assert_eq!(
            fmt("ls-remote", Direction::Read, Traced::Data(b"version 2\n")),
            "packet:    ls-remote< version 2"
        );
        assert_eq!(
            fmt("upload-pack", Direction::Write, Traced::Flush),
            "packet:  upload-pack> 0000"
        );
        assert_eq!(
            fmt("fetch", Direction::Write, Traced::Delim),
            "packet:        fetch> 0001"
        );
        assert_eq!(
            fmt("a-very-long-program", Direction::Read, Traced::ResponseEnd),
            "packet: a-very-long-program< 0002"
        );
        assert_eq!(
            fmt("ls-remote", Direction::Read, Traced::Data(b"")),
            "packet:    ls-remote< "
        );
    }

    #[test]
    fn escaping_matches_git() {
        // Observed from git 2.56.0 with a crafted ref advertisement.
        let p = b"x HEAD\0agent=foo/1\n";
        assert_eq!(
            fmt("ls-remote", Direction::Read, Traced::Data(p)),
            "packet:    ls-remote< x HEAD\\0agent=foo/1"
        );
        let p = b"refs/heads/a\x01b\x7fc\xffd\te\rf\\g\n";
        assert_eq!(
            fmt("ls-remote", Direction::Read, Traced::Data(p)),
            "packet:    ls-remote< refs/heads/a\\1b\\177c\\37777777777d\\11e\\15f\\g"
        );
        let mut out = Vec::new();
        escape_into(&mut out, b"\x80\n\n\x0b");
        assert_eq!(out, b"\\37777777600\\13");
    }

    #[test]
    fn all_lf_are_dropped() {
        // Observed from git 2.56.0 on sideband progress packets.
        let t = |p: &[u8]| fmt("sideband", Direction::Read, Traced::Data(p));
        assert_eq!(t(b"\x02\n"), "packet:     sideband< \\2");
        assert_eq!(t(b"\x02a\nb\n"), "packet:     sideband< \\2ab");
        assert_eq!(t(b"\x02\n\n\n"), "packet:     sideband< \\2");
        assert_eq!(t(b"\x02a\n\nb\n\n"), "packet:     sideband< \\2ab");
        assert_eq!(
            t(b"\x02x\x01\n\xffy\n"),
            "packet:     sideband< \\2x\\1\\37777777777y"
        );
    }

    #[test]
    fn pack_stops_channel_one() {
        let mut t = Tracer::new("sideband");
        let r = Direction::Read;
        let msg = |m: Option<Vec<u8>>| m.map(|v| String::from_utf8(v).unwrap());
        assert_eq!(
            msg(t.message(r, Traced::Data(b"\x02Foo.\n"))).unwrap(),
            "packet:     sideband< \\2Foo."
        );
        assert_eq!(
            msg(t.message(r, Traced::Data(b"\x01PACK\0\0\0\x02"))).unwrap(),
            "packet:     sideband< PACK ..."
        );
        assert_eq!(msg(t.message(r, Traced::Data(b"\x01more pack"))), None);
        assert_eq!(
            msg(t.message(r, Traced::Data(b"\x02between\n"))).unwrap(),
            "packet:     sideband< \\2between"
        );
        assert_eq!(
            msg(t.message(r, Traced::Flush)).unwrap(),
            "packet:     sideband< 0000"
        );
    }
}
