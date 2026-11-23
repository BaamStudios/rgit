// SPDX-License-Identifier: MIT

//! `pkt-line` (t0070, t5411, t5701, t5702, t5703): encode and decode
//! pkt-lines and side-band data.
//!
//! Observed behaviour of the reference binary:
//! - `pack`: every line of stdin (with its newline) becomes a data packet;
//!   a line `0000` is a flush packet, `0001` a delimiter. A payload over
//!   65516 bytes fails with `fatal: protocol error: impossibly long line`.
//! - `pack-raw-stdin`: all of stdin is one data packet (even when empty);
//!   too long fails with `fatal: packet write failed - data exceeds max
//!   packet size`.
//! - `unpack`: prints each data packet without one trailing newline,
//!   followed by a newline; flush, delimiter and response-end packets as
//!   `0000`, `0001`, `0002`. A truncated packet ends the input silently; a
//!   bad length is fatal.
//! - `unpack-sideband [--[no-]reader-use-sideband] [--[no-]chomp-newline]`:
//!   band 1 to stdout, band 2 to stderr, up to a flush packet; one trailing
//!   newline of the payload is dropped unless `--no-chomp-newline`. With
//!   `--reader-use-sideband` band 2 is shown as `remote: ...` progress and
//!   band 3 is a fatal `remote error`.
//! - `send-split-sideband`: a fixed stream with a progress message split
//!   across packets around a band 1 packet, then response-end and flush.
//! - `receive-sideband`: band 1 to stdout, band 2 and 3 as `remote: ...`
//!   on stderr, up to a flush packet; a short read or a packet without
//!   band is reported with a `sideband: ` prefix but exits 0.

use std::io::{self, Read, Write};

use rgit_protocol::sideband::{Progress, ProgressStyle};

use crate::messages;

/// Largest payload of a data packet.
const MAX_PAYLOAD: usize = 65516;

pub fn pkt_line(args: &[String]) -> i32 {
    let Some(action) = args.first() else {
        return fatal(messages::TOO_FEW_ARGUMENTS);
    };
    let rest = &args[1..];
    match action.as_str() {
        "pack" => pack(),
        "pack-raw-stdin" => pack_raw_stdin(),
        "unpack" => unpack(),
        "unpack-sideband" => unpack_sideband(rest),
        "send-split-sideband" => send_split_sideband(),
        "receive-sideband" => receive_sideband(),
        other => fatal(&messages::invalid_argument(other)),
    }
}

fn fatal(msg: &str) -> i32 {
    eprintln!("fatal: {msg}");
    128
}

fn read_stdin() -> Vec<u8> {
    let mut data = Vec::new();
    let _ = io::stdin().lock().read_to_end(&mut data);
    data
}

fn write_out(data: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(data);
    let _ = out.flush();
}

fn data_packet(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

fn pack() -> i32 {
    let input = read_stdin();
    let mut out = Vec::new();
    for line in input.split_inclusive(|&b| b == b'\n') {
        let bare = line.strip_suffix(b"\n").unwrap_or(line);
        match bare {
            b"0000" | b"0001" => out.extend_from_slice(bare),
            _ if line.len() > MAX_PAYLOAD => {
                write_out(&out);
                return fatal(messages::IMPOSSIBLY_LONG_LINE);
            }
            _ => data_packet(&mut out, line),
        }
    }
    write_out(&out);
    0
}

fn pack_raw_stdin() -> i32 {
    let input = read_stdin();
    if input.len() > MAX_PAYLOAD {
        return fatal(messages::PACKET_TOO_LONG);
    }
    let mut out = Vec::new();
    data_packet(&mut out, &input);
    write_out(&out);
    0
}

/// One packet of the input.
enum Pkt<'a> {
    Flush,
    Delim,
    ResponseEnd,
    Data(&'a [u8]),
}

/// Reads packets from a buffer the way git's reader does.
struct Packets<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Packets<'a> {
    fn new(input: &'a [u8]) -> Self {
        Packets { input, pos: 0 }
    }

    /// The next packet; `Ok(None)` at the end of the input or in the middle
    /// of a packet; `Err` with the message of a bad length.
    fn next(&mut self) -> Result<Option<Pkt<'a>>, String> {
        let rest = &self.input[self.pos..];
        if rest.len() < 4 {
            return Ok(None);
        }
        let head = &rest[..4];
        let text = String::from_utf8_lossy(head);
        let len = match std::str::from_utf8(head)
            .ok()
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|h| usize::from_str_radix(h, 16).ok())
        {
            Some(n) => n,
            None => return Err(messages::bad_line_length_character(&text)),
        };
        let pkt = match len {
            0 => Pkt::Flush,
            1 => Pkt::Delim,
            2 => Pkt::ResponseEnd,
            3 => return Err(messages::bad_line_length(len)),
            n => {
                if rest.len() < n {
                    return Ok(None);
                }
                self.pos += n;
                return Ok(Some(Pkt::Data(&rest[4..n])));
            }
        };
        self.pos += 4;
        Ok(Some(pkt))
    }
}

fn unpack() -> i32 {
    let input = read_stdin();
    let mut reader = Packets::new(&input);
    let mut out = Vec::new();
    let code = loop {
        match reader.next() {
            Ok(None) => break 0,
            Ok(Some(Pkt::Flush)) => out.extend_from_slice(b"0000\n"),
            Ok(Some(Pkt::Delim)) => out.extend_from_slice(b"0001\n"),
            Ok(Some(Pkt::ResponseEnd)) => out.extend_from_slice(b"0002\n"),
            Ok(Some(Pkt::Data(d))) => {
                out.extend_from_slice(d.strip_suffix(b"\n").unwrap_or(d));
                out.push(b'\n');
            }
            Err(e) => break fatal(&e),
        }
    };
    write_out(&out);
    code
}

fn chomp(data: &[u8], on: bool) -> &[u8] {
    if on {
        data.strip_suffix(b"\n").unwrap_or(data)
    } else {
        data
    }
}

fn unpack_sideband(args: &[String]) -> i32 {
    let mut reader_sideband = false;
    let mut chomp_newline = true;
    for a in args {
        match a.as_str() {
            "--reader-use-sideband" => reader_sideband = true,
            "--no-reader-use-sideband" => reader_sideband = false,
            "--chomp-newline" => chomp_newline = true,
            "--no-chomp-newline" => chomp_newline = false,
            other => {
                let name = other.trim_start_matches('-');
                eprintln!("{}", messages::unknown_option(name));
                eprint!("{}", messages::UNPACK_SIDEBAND_USAGE);
                return 129;
            }
        }
    }
    let input = read_stdin();
    let mut reader = Packets::new(&input);
    let mut stdout = io::stdout().lock();
    let mut progress = Progress::new(io::stderr(), ProgressStyle::detect());
    loop {
        let pkt = match reader.next() {
            Ok(None) | Ok(Some(Pkt::Flush)) => break,
            Ok(Some(Pkt::Delim | Pkt::ResponseEnd)) => continue,
            Ok(Some(Pkt::Data(d))) => d,
            Err(e) => {
                let _ = stdout.flush();
                return fatal(&e);
            }
        };
        let Some((&band, payload)) = pkt.split_first() else {
            if reader_sideband {
                let _ = stdout.flush();
                return fatal(&messages::reader_prefixed(messages::MISSING_SIDEBAND));
            }
            continue;
        };
        if !reader_sideband {
            match band {
                1 => {
                    let _ = stdout.write_all(chomp(payload, chomp_newline));
                }
                2 => {
                    let mut err = io::stderr().lock();
                    let _ = err.write_all(chomp(payload, chomp_newline));
                }
                _ => {}
            }
            continue;
        }
        match band {
            1 => {
                let _ = stdout.write_all(chomp(payload, chomp_newline));
            }
            2 => {
                let _ = progress.feed(payload);
            }
            3 => {
                let _ = stdout.flush();
                let text = String::from_utf8_lossy(chomp(payload, true)).into_owned();
                return fatal(&messages::remote_error(&text));
            }
            _ => {}
        }
    }
    let _ = progress.finish();
    let _ = stdout.flush();
    0
}

fn send_split_sideband() -> i32 {
    let mut out = Vec::new();
    data_packet(&mut out, b"\x02Foo.\n");
    data_packet(&mut out, b"\x02Bar.\n");
    data_packet(&mut out, b"\x02Hello,");
    data_packet(&mut out, b"\x01primary: regular output\n");
    data_packet(&mut out, b"\x02 world!\n");
    out.extend_from_slice(b"0002");
    out.extend_from_slice(b"0000");
    write_out(&out);
    0
}

fn receive_sideband() -> i32 {
    let input = read_stdin();
    let mut reader = Packets::new(&input);
    let mut stdout = io::stdout().lock();
    let mut progress = Progress::new(io::stderr(), ProgressStyle::detect());
    let mut problem = None;
    loop {
        let pkt = match reader.next() {
            Ok(None) => {
                problem = Some(messages::SIDEBAND_DISCONNECT.to_owned());
                break;
            }
            Ok(Some(Pkt::Flush | Pkt::Delim | Pkt::ResponseEnd)) => break,
            Ok(Some(Pkt::Data(d))) => d,
            Err(e) => {
                let _ = stdout.flush();
                let _ = progress.finish();
                return fatal(&e);
            }
        };
        let Some((&band, payload)) = pkt.split_first() else {
            problem = Some(messages::MISSING_SIDEBAND.to_owned());
            break;
        };
        match band {
            1 => {
                let _ = stdout.write_all(payload);
            }
            2 => {
                let _ = progress.feed(payload);
            }
            3 => {
                let _ = progress.remote_error(chomp(payload, true));
                break;
            }
            b => {
                problem = Some(messages::bad_band(b));
                break;
            }
        }
    }
    let _ = stdout.flush();
    let _ = progress.finish();
    if let Some(p) = problem {
        eprintln!("{}", messages::sideband_prefixed(&p));
    }
    0
}
