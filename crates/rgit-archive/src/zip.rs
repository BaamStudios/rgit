// SPDX-License-Identifier: MIT
//! The zip writer of `git archive --format=zip`.
//!
//! Format: PKWARE's APPNOTE.TXT (local headers, central directory, end of
//! central directory record, data descriptors, Zip64 extensions) and
//! Info-ZIP's "extended timestamp" extra field (`UT`, 0x5455). The choices
//! git makes were matched against the reference `git`:
//!
//! - every entry carries the `UT` field with the modification time, the
//!   DOS date and time fields hold the local time;
//! - "version needed" is 1.0 (4.5 with Zip64 fields); "version made by"
//!   is 0 (MS-DOS) except for executables and symbolic links, which are
//!   Unix entries (0x0317) with the mode in the high half of the external
//!   attributes; directories have the MS-DOS directory attribute 0x10;
//! - bit 11 of the flags (UTF-8) is set for paths that are valid UTF-8
//!   and not plain ASCII;
//! - content is deflated (raw deflate) unless that does not make it
//!   smaller, the level is 0 or the entry is a symbolic link, in which
//!   case it is stored;
//! - content written as a stream (large blobs) has bit 3 set and a data
//!   descriptor after it; the local header then has CRC 0 and, when
//!   deflated, compressed size 0;
//! - the internal attributes say "text" (1) for content the caller
//!   classifies as text;
//! - the commit id becomes the archive comment.

use std::io::{self, Read, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_UTF8: u16 = 1 << 11;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const MADE_BY_UNIX: u16 = 0x0317;
const ATTR_DIRECTORY: u32 = 0x10;
const MAX32: u64 = 0xffff_ffff;

/// What kind of entry is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A directory (also used for submodules); the path ends with `/`.
    Directory,
    /// A regular file.
    File {
        /// The executable bit is set.
        executable: bool,
    },
    /// A symbolic link; the content is the target.
    Symlink,
}

struct Central {
    made_by: u16,
    needed: u16,
    flags: u16,
    method: u16,
    time: u16,
    date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    path: Vec<u8>,
    internal: u16,
    external: u32,
    offset: u64,
}

/// A zip archive being written.
pub struct ZipWriter<W: Write> {
    out: W,
    level: u32,
    time: u16,
    date: u16,
    mtime: u32,
    offset: u64,
    entries: Vec<Central>,
}

/// The DOS date and time of the (local) time `t` in seconds since the
/// epoch: `(time, date)`. Times before 1980 are clamped to 1980-01-01.
pub fn dos_datetime(t: i64) -> (u16, u16) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    if year < 1980 {
        return (0, (1 << 5) | 1);
    }
    let year = (year - 1980).min(127);
    let time = ((secs / 3600) << 11) | (((secs / 60) % 60) << 5) | ((secs % 60) / 2);
    let date = (year << 9) | (month << 5) | day;
    (time as u16, date as u16)
}

/// Whether `path` needs the UTF-8 flag; `None` when it is not valid UTF-8
/// (git warns about such paths).
pub fn needs_utf8_flag(path: &[u8]) -> Option<bool> {
    if path.is_ascii() {
        return Some(false);
    }
    std::str::from_utf8(path).ok().map(|_| true)
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn le64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn clamp32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Raw deflate of `data` at `level`.
pub fn deflate(data: &[u8], level: u32) -> io::Result<Vec<u8>> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::new(level));
    e.write_all(data)?;
    e.finish()
}

impl<W: Write> ZipWriter<W> {
    /// A writer with compression `level` (0 to 9) giving every entry the
    /// time `mtime`; `local_mtime` is the same time shifted into the
    /// local time zone (for the DOS fields).
    pub fn new(out: W, level: u32, mtime: u64, local_mtime: i64) -> ZipWriter<W> {
        let (time, date) = dos_datetime(local_mtime);
        ZipWriter {
            out,
            level,
            time,
            date,
            mtime: clamp32(mtime),
            offset: 0,
            entries: Vec::new(),
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.out.write_all(data)?;
        self.offset += data.len() as u64;
        Ok(())
    }

    fn extended_time(&self) -> Vec<u8> {
        let mut x = Vec::with_capacity(9);
        le16(&mut x, 0x5455);
        le16(&mut x, 5);
        x.push(1);
        le32(&mut x, self.mtime);
        x
    }

    fn attributes(kind: Kind) -> (u16, u32) {
        match kind {
            Kind::Directory => (0, ATTR_DIRECTORY),
            Kind::File { executable: false } => (0, 0),
            Kind::File { executable: true } => (MADE_BY_UNIX, 0o100_755 << 16),
            Kind::Symlink => (MADE_BY_UNIX, 0o120_777 << 16),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn local_header(
        &mut self,
        path: &[u8],
        needed: u16,
        flags: u16,
        method: u16,
        crc: u32,
        csize: u64,
        usize: u64,
        zip64: bool,
    ) -> io::Result<()> {
        let mut extra = self.extended_time();
        if zip64 {
            le16(&mut extra, 0x0001);
            le16(&mut extra, 16);
            le64(&mut extra, usize);
            le64(&mut extra, csize);
        }
        let mut h = Vec::with_capacity(30 + path.len() + extra.len());
        h.extend_from_slice(b"PK\x03\x04");
        le16(&mut h, needed);
        le16(&mut h, flags);
        le16(&mut h, method);
        le16(&mut h, self.time);
        le16(&mut h, self.date);
        le32(&mut h, crc);
        if zip64 {
            le32(&mut h, u32::MAX);
            le32(&mut h, u32::MAX);
        } else {
            le32(&mut h, clamp32(csize));
            le32(&mut h, clamp32(usize));
        }
        le16(&mut h, path.len() as u16);
        le16(&mut h, extra.len() as u16);
        h.extend_from_slice(path);
        h.extend_from_slice(&extra);
        self.write(&h)
    }

    /// Add an entry with in-memory content (empty for directories).
    /// `utf8` sets the UTF-8 flag, `text` the text attribute.
    pub fn add(
        &mut self,
        path: &[u8],
        kind: Kind,
        content: &[u8],
        utf8: bool,
        text: bool,
    ) -> io::Result<()> {
        let offset = self.offset;
        let (made_by, external) = Self::attributes(kind);
        let mut flags = if utf8 { FLAG_UTF8 } else { 0 };
        let (method, data, crc) = if kind == Kind::Directory {
            (METHOD_STORE, None, 0)
        } else {
            let crc = crc32fast::hash(content);
            // Link targets are always stored.
            let compressed = if self.level > 0 && !content.is_empty() && kind != Kind::Symlink {
                Some(deflate(content, self.level)?)
            } else {
                None
            };
            match compressed {
                Some(c) if c.len() < content.len() => (METHOD_DEFLATE, Some(c), crc),
                _ => (METHOD_STORE, None, crc),
            }
        };
        let body: &[u8] = data.as_deref().unwrap_or(if kind == Kind::Directory {
            &[]
        } else {
            content
        });
        let usize = if kind == Kind::Directory {
            0
        } else {
            content.len() as u64
        };
        let csize = body.len() as u64;
        let zip64 = usize >= MAX32 || csize >= MAX32;
        let needed = if zip64 || offset >= MAX32 { 45 } else { 10 };
        flags &= !FLAG_DESCRIPTOR;
        self.local_header(path, needed, flags, method, crc, csize, usize, zip64)?;
        self.write(body)?;
        self.entries.push(Central {
            made_by,
            needed,
            flags,
            method,
            time: self.time,
            date: self.date,
            crc,
            csize,
            usize,
            path: path.to_vec(),
            internal: u16::from(text && kind != Kind::Directory),
            external,
            offset,
        });
        Ok(())
    }

    /// Add a file of `size` bytes read from `r` without holding it in
    /// memory: with a data descriptor. `text` is the text attribute.
    pub fn add_stream(
        &mut self,
        path: &[u8],
        kind: Kind,
        size: u64,
        r: &mut dyn Read,
        utf8: bool,
        text: bool,
    ) -> io::Result<()> {
        let offset = self.offset;
        let (made_by, external) = Self::attributes(kind);
        let flags = FLAG_DESCRIPTOR | if utf8 { FLAG_UTF8 } else { 0 };
        let method = if self.level == 0 {
            METHOD_STORE
        } else {
            METHOD_DEFLATE
        };
        let zip64 = size >= MAX32;
        let needed = if zip64 || offset >= MAX32 { 45 } else { 10 };
        let local_csize = if method == METHOD_STORE { size } else { 0 };
        self.local_header(path, needed, flags, method, 0, local_csize, size, zip64)?;
        let mut hasher = crc32fast::Hasher::new();
        let mut buf = vec![0u8; 65536];
        let mut left = size;
        let mut csize = 0u64;
        let mut encoder = (method == METHOD_DEFLATE)
            .then(|| DeflateEncoder::new(Vec::new(), Compression::new(self.level)));
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = r.read(&mut buf[..want])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short object content",
                ));
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
            match encoder.as_mut() {
                Some(e) => {
                    e.write_all(&buf[..n])?;
                    let produced = std::mem::take(e.get_mut());
                    csize += produced.len() as u64;
                    self.write(&produced)?;
                }
                None => {
                    csize += n as u64;
                    self.write(&buf[..n])?;
                }
            }
        }
        if let Some(e) = encoder {
            let rest = e.finish()?;
            csize += rest.len() as u64;
            self.write(&rest)?;
        }
        let crc = hasher.finalize();
        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(b"PK\x07\x08");
        le32(&mut d, crc);
        if zip64 {
            le64(&mut d, csize);
            le64(&mut d, size);
        } else {
            le32(&mut d, clamp32(csize));
            le32(&mut d, clamp32(size));
        }
        self.write(&d)?;
        self.entries.push(Central {
            made_by,
            needed,
            flags,
            method,
            time: self.time,
            date: self.date,
            crc,
            csize,
            usize: size,
            path: path.to_vec(),
            internal: u16::from(text),
            external,
            offset,
        });
        Ok(())
    }

    /// Write the central directory and the end records with `comment`
    /// (the commit id).
    pub fn finish(mut self, comment: &[u8]) -> io::Result<W> {
        let cd_start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let mut extra = self.extended_time();
            let mut z = Vec::new();
            if e.usize >= MAX32 {
                le64(&mut z, e.usize);
            }
            if e.csize >= MAX32 {
                le64(&mut z, e.csize);
            }
            if e.offset >= MAX32 {
                le64(&mut z, e.offset);
            }
            if !z.is_empty() {
                le16(&mut extra, 0x0001);
                le16(&mut extra, z.len() as u16);
                extra.extend_from_slice(&z);
            }
            let mut h = Vec::with_capacity(46 + e.path.len() + extra.len());
            h.extend_from_slice(b"PK\x01\x02");
            le16(&mut h, e.made_by);
            le16(&mut h, e.needed);
            le16(&mut h, e.flags);
            le16(&mut h, e.method);
            le16(&mut h, e.time);
            le16(&mut h, e.date);
            le32(&mut h, e.crc);
            le32(&mut h, clamp32(e.csize));
            le32(&mut h, clamp32(e.usize));
            le16(&mut h, e.path.len() as u16);
            le16(&mut h, extra.len() as u16);
            le16(&mut h, 0);
            le16(&mut h, 0);
            le16(&mut h, e.internal);
            le32(&mut h, e.external);
            le32(&mut h, clamp32(e.offset));
            h.extend_from_slice(&e.path);
            h.extend_from_slice(&extra);
            self.write(&h)?;
        }
        let cd_size = self.offset - cd_start;
        let count = entries.len() as u64;
        let mut t = Vec::new();
        if count >= 0xffff || cd_size >= MAX32 || cd_start >= MAX32 {
            let eocd64 = self.offset;
            t.extend_from_slice(b"PK\x06\x06");
            le64(&mut t, 44);
            // Made by MS-DOS, needs 4.5.
            le16(&mut t, 0);
            le16(&mut t, 45);
            le32(&mut t, 0);
            le32(&mut t, 0);
            le64(&mut t, count);
            le64(&mut t, count);
            le64(&mut t, cd_size);
            le64(&mut t, cd_start);
            t.extend_from_slice(b"PK\x06\x07");
            le32(&mut t, 0);
            le64(&mut t, eocd64);
            le32(&mut t, 1);
        }
        t.extend_from_slice(b"PK\x05\x06");
        le16(&mut t, 0);
        le16(&mut t, 0);
        let c16 = u16::try_from(count).unwrap_or(u16::MAX);
        le16(&mut t, c16);
        le16(&mut t, c16);
        le32(&mut t, clamp32(cd_size));
        le32(&mut t, clamp32(cd_start));
        le16(&mut t, comment.len() as u16);
        t.extend_from_slice(comment);
        self.write(&t)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_dates() {
        // 2005-05-28 00:00:00
        assert_eq!(dos_datetime(1_117_238_400), (0, 0x32bc));
        assert_eq!(dos_datetime(0), (0, 0x21));
    }

    #[test]
    fn small_content_is_stored() {
        let mut w = ZipWriter::new(Vec::new(), 6, 0, 0);
        w.add(b"a", Kind::File { executable: false }, b"x\n", false, true)
            .unwrap();
        w.add(
            b"b",
            Kind::File { executable: true },
            &[b'b'; 200],
            false,
            true,
        )
        .unwrap();
        let out = w.finish(b"").unwrap();
        assert_eq!(&out[8..10], &[0, 0]);
        let second = 30 + 1 + 9 + 2;
        assert_eq!(&out[second + 8..second + 10], &[8, 0]);
        let eocd = &out[out.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert_eq!(&eocd[8..12], &[2, 0, 2, 0]);
    }

    #[test]
    fn stream_has_descriptor() {
        let mut w = ZipWriter::new(Vec::new(), 0, 0, 0);
        let data = b"hello";
        w.add_stream(
            b"f",
            Kind::File { executable: false },
            5,
            &mut &data[..],
            false,
            true,
        )
        .unwrap();
        let out = w.finish(b"c").unwrap();
        assert_eq!(&out[6..8], &[8, 0]);
        let d = 30 + 1 + 9 + 5;
        assert_eq!(&out[d..d + 4], b"PK\x07\x08");
        assert_eq!(*out.last().unwrap(), b'c');
    }
}
