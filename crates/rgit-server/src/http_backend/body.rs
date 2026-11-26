// SPDX-License-Identifier: MIT
//! The request body of a service call: `CONTENT_LENGTH` bytes of the
//! server's standard input (all of it when the length is unknown, as with
//! chunked transfer encoding), inflated when `Content-Encoding` is
//! `gzip` or `x-gzip`, and written to the service.

use std::io::{self, Read, Write};

use flate2::{Decompress, FlushDecompress, Status};

use super::messages as msg;

/// How to read the body.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Body {
    /// `CONTENT_LENGTH`, if known.
    pub length: Option<u64>,
    /// `Content-Encoding: gzip`.
    pub gzip: bool,
}

/// Why the body could not be passed on: the messages for standard error.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// An `error:` line before the fatal one.
    pub error: Option<String>,
    /// The `fatal:` message.
    pub fatal: String,
}

impl Failure {
    fn fatal(msg: impl Into<String>) -> Failure {
        Failure {
            error: None,
            fatal: msg.into(),
        }
    }
}

/// A sink that stops taking data once the reader of the other end is gone
/// (the service ended early; its own messages say why).
struct Sink<W: Write> {
    inner: W,
    closed: bool,
}

impl<W: Write> Sink<W> {
    fn put(&mut self, data: &[u8]) {
        if !self.closed && self.inner.write_all(data).is_err() {
            self.closed = true;
        }
    }
}

/// Copy the body from `input` to `output`.
pub(crate) fn copy<R: Read, W: Write>(body: Body, input: R, output: W) -> Result<(), Failure> {
    let mut sink = Sink {
        inner: output,
        closed: false,
    };
    let mut input: Box<dyn Read> = match body.length {
        Some(n) => Box::new(input.take(n)),
        None => Box::new(input),
    };
    let mut buf = vec![0u8; 64 * 1024];
    if !body.gzip {
        loop {
            match read(&mut input, &mut buf)? {
                0 => break,
                n => sink.put(&buf[..n]),
            }
        }
        let _ = sink.inner.flush();
        return Ok(());
    }

    // gzip: zlib with the gzip wrapper, like git's inflateInit2(16 + 15).
    let mut z = Decompress::new_gzip(15);
    let mut out = vec![0u8; 64 * 1024];
    'outer: loop {
        let n = read(&mut input, &mut buf)?;
        if n == 0 {
            return Err(Failure::fatal(msg::GZIP_TRUNCATED));
        }
        let mut pos = 0;
        while pos < n {
            let before_in = z.total_in();
            let before_out = z.total_out();
            let status = z
                .decompress(&buf[pos..n], &mut out, FlushDecompress::None)
                .map_err(|e| {
                    let reason = e.message().unwrap_or("data stream error").to_owned();
                    Failure {
                        error: Some(msg::inflate_error(&reason_text(&reason))),
                        // Z_DATA_ERROR
                        fatal: msg::inflate_failed(-3),
                    }
                })?;
            pos += (z.total_in() - before_in) as usize;
            let produced = (z.total_out() - before_out) as usize;
            sink.put(&out[..produced]);
            match status {
                Status::StreamEnd => break 'outer,
                Status::Ok | Status::BufError => {
                    if produced == 0 && z.total_in() == before_in {
                        // Needs more input.
                        break;
                    }
                }
            }
        }
    }
    let _ = sink.inner.flush();
    Ok(())
}

/// zlib's message as git prints it: `data stream error (<reason>)`.
fn reason_text(reason: &str) -> String {
    if reason.starts_with("data stream error") {
        reason.to_owned()
    } else {
        format!("data stream error ({reason})")
    }
}

fn read(input: &mut dyn Read, buf: &mut [u8]) -> Result<usize, Failure> {
    loop {
        match input.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Failure::fatal(msg::read_failed(&e.to_string()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn plain_with_and_without_length() {
        let mut out = Vec::new();
        let body = Body {
            length: Some(3),
            gzip: false,
        };
        copy(body, &b"abcdef"[..], &mut out).unwrap();
        assert_eq!(out, b"abc");
        out.clear();
        let body = Body {
            length: None,
            gzip: false,
        };
        copy(body, &b"abcdef"[..], &mut out).unwrap();
        assert_eq!(out, b"abcdef");
    }

    #[test]
    fn gzip_whole_truncated_and_bad() {
        let data = b"0032want 0123456789012345678901234567890123456789\n0000".repeat(50);
        let packed = gz(&data);
        let body = Body {
            length: None,
            gzip: true,
        };
        let mut out = Vec::new();
        copy(body, &packed[..], &mut out).unwrap();
        assert_eq!(out, data);

        let mut out = Vec::new();
        let err = copy(body, &packed[..10], &mut out).unwrap_err();
        assert_eq!(err.fatal, msg::GZIP_TRUNCATED);
        let err = copy(body, &b""[..], &mut out).unwrap_err();
        assert_eq!(err.fatal, msg::GZIP_TRUNCATED);

        let err = copy(body, &b"0000 not gzip"[..], &mut out).unwrap_err();
        assert_eq!(
            err.error.as_deref(),
            Some("inflate: data stream error (incorrect header check)")
        );
        assert_eq!(err.fatal, "zlib error inflating request, result -3");
    }
}
