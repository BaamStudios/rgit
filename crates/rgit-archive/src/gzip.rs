// SPDX-License-Identifier: MIT
//! The internal gzip of `git archive` (`tar.<format>.command` set to
//! `git archive gzip`, the default of `tgz` and `tar.gz`).
//!
//! Format: RFC 1952. The header has no file name and time 0, the
//! "extra flags" byte says 2 for level 9 and 4 for levels below 2 (as the
//! zlib library writes it), the operating system is Unix (3).

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

/// The highest level the internal gzip accepts.
pub const MAX_LEVEL: u32 = 9;

/// A gzip stream around `W`.
pub struct GzipWriter<W: Write> {
    encoder: DeflateEncoder<W>,
    crc: crc32fast::Hasher,
    size: u32,
}

impl<W: Write> GzipWriter<W> {
    /// Start a gzip stream at `level` (0 to 9).
    pub fn new(mut out: W, level: u32) -> io::Result<GzipWriter<W>> {
        let xfl = if level == 9 {
            2
        } else if level < 2 {
            4
        } else {
            0
        };
        out.write_all(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, xfl, 3])?;
        Ok(GzipWriter {
            encoder: DeflateEncoder::new(out, Compression::new(level)),
            crc: crc32fast::Hasher::new(),
            size: 0,
        })
    }

    /// Write the trailer and return the inner writer.
    pub fn finish(self) -> io::Result<W> {
        let crc = self.crc.finalize();
        let mut out = self.encoder.finish()?;
        out.write_all(&crc.to_le_bytes())?;
        out.write_all(&self.size.to_le_bytes())?;
        out.flush()?;
        Ok(out)
    }
}

impl<W: Write> Write for GzipWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.encoder.write(buf)?;
        self.crc.update(&buf[..n]);
        self.size = self.size.wrapping_add(n as u32);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn round_trip() {
        let mut g = GzipWriter::new(Vec::new(), 6).unwrap();
        g.write_all(b"hello hello hello").unwrap();
        let out = g.finish().unwrap();
        assert_eq!(&out[..10], &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3]);
        let mut d = flate2::read::GzDecoder::new(&out[..]);
        let mut s = String::new();
        d.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello hello hello");
    }
}
