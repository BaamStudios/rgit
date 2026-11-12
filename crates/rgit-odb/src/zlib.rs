// SPDX-License-Identifier: MIT

//! Incremental zlib inflation over any reader, reporting zlib's own error
//! texts.

use std::io::{self, Read};

use flate2::{Decompress, FlushDecompress, Status};

/// Why inflation failed.
#[derive(Debug)]
pub(crate) enum InflateError {
    /// zlib reported a problem; the text follows `inflate: ` in git's
    /// message (e.g. `data stream error (incorrect header check)`).
    Data(String),
    /// The compressed input ended before the end of the stream.
    Truncated,
    /// Reading the compressed input failed.
    Io(io::Error),
}

const INPUT_BUF: usize = 64 * 1024;

/// Inflates a zlib stream read from `R`.
pub(crate) struct Inflater<R> {
    inner: R,
    z: Decompress,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    eof: bool,
    done: bool,
}

impl<R: Read> Inflater<R> {
    pub(crate) fn new(inner: R) -> Inflater<R> {
        Inflater {
            inner,
            z: Decompress::new(true),
            buf: vec![0; INPUT_BUF].into_boxed_slice(),
            pos: 0,
            len: 0,
            eof: false,
            done: false,
        }
    }

    fn refill(&mut self) -> Result<(), InflateError> {
        if self.pos == self.len && !self.eof {
            self.pos = 0;
            self.len = loop {
                match self.inner.read(&mut self.buf) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(InflateError::Io(e)),
                }
            };
            self.eof = self.len == 0;
        }
        Ok(())
    }

    /// Inflate into `out` (must not be empty). Returns 0 only at the end of
    /// the stream.
    pub(crate) fn read(&mut self, out: &mut [u8]) -> Result<usize, InflateError> {
        debug_assert!(!out.is_empty());
        loop {
            if self.done {
                return Ok(0);
            }
            self.refill()?;
            let in_before = self.z.total_in();
            let out_before = self.z.total_out();
            let status = self
                .z
                .decompress(&self.buf[self.pos..self.len], out, FlushDecompress::None)
                .map_err(|e| InflateError::Data(crate::messages::zlib_problem(&e)))?;
            let consumed = (self.z.total_in() - in_before) as usize;
            let produced = (self.z.total_out() - out_before) as usize;
            self.pos += consumed;
            if status == Status::StreamEnd {
                self.done = true;
                return Ok(produced);
            }
            if produced > 0 {
                return Ok(produced);
            }
            if consumed == 0 && self.pos == self.len && self.eof {
                return Err(InflateError::Truncated);
            }
        }
    }

    /// After the end of the stream: whether any input bytes follow it.
    pub(crate) fn has_trailing_input(&mut self) -> Result<bool, InflateError> {
        debug_assert!(self.done);
        if self.pos < self.len {
            return Ok(true);
        }
        let mut b = [0u8; 1];
        loop {
            match self.inner.read(&mut b) {
                Ok(n) => return Ok(n > 0),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(InflateError::Io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::*;

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::new(1));
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn inflates_and_detects_errors() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let z = deflate(&data);
        let mut inf = Inflater::new(&z[..]);
        let mut out = Vec::new();
        let mut chunk = [0u8; 777];
        loop {
            let n = inf.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(out, data);
        assert!(!inf.has_trailing_input().unwrap());

        let mut inf = Inflater::new(&b"blob 5\0hello"[..]);
        match inf.read(&mut chunk) {
            Err(InflateError::Data(m)) => {
                assert_eq!(m, "data stream error (incorrect header check)")
            }
            other => panic!("{other:?}"),
        }

        let mut inf = Inflater::new(&z[..z.len() / 2]);
        let r = loop {
            match inf.read(&mut chunk) {
                Ok(0) => break Ok(0),
                Ok(_) => {}
                Err(e) => break Err(e),
            }
        };
        assert!(matches!(r, Err(InflateError::Truncated)));
    }
}
