// SPDX-License-Identifier: MIT
//! The tar writer of `git archive --format=tar`.
//!
//! The layout follows POSIX ustar with pax extensions (IEEE Std 1003.1,
//! "pax Interchange Format") and was matched byte for byte against the
//! reference `git`:
//!
//! - every header has uid/gid 0, owner and group `root`, magic
//!   `ustar\0`, version `00`, device numbers `0000000`, numeric fields in
//!   zero-padded octal ending in NUL and the checksum as seven digits and
//!   a NUL;
//! - a global pax header (`pax_global_header`, type `g`) comes first when
//!   there is a commit id (record `comment`) or the time does not fit the
//!   eleven octal digits of the header (record `mtime`; the field then
//!   holds `77777777777`);
//! - a path longer than 100 bytes is split at the last `/` that leaves at
//!   most 154 bytes for the `prefix` field and at most 100 for `name`;
//!   when that is impossible, or a link target is longer than 100 bytes,
//!   or a size does not fit, an extended header (type `x`) named
//!   `<object id>.paxheader` carries `path`, `linkpath` and `size`, and
//!   the entry itself is named `<object id>.data` (a link target is
//!   replaced by `see <object id>.paxheader`);
//! - the archive ends with two zero blocks and is padded with zeros to a
//!   multiple of 10240 bytes.

use std::io::{self, Read, Write};

/// Size of a tar block.
pub const BLOCK: u64 = 512;
/// The output is padded to a multiple of this (blocking factor 20).
pub const RECORD: u64 = 10240;

/// Largest value of an eleven digit octal field.
const MAX_OCTAL_11: u64 = 0o77777777777;

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
    /// A symbolic link.
    Symlink,
}

/// A tar stream being written.
pub struct TarWriter<W: Write> {
    out: W,
    umask: u32,
    mtime: u64,
    written: u64,
}

/// One pax record: `<len> <key>=<value>\n`, the length counting itself.
pub fn pax_record(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    let rest = 1 + key.len() + 1 + value.len() + 1;
    let mut len = rest + 1;
    loop {
        let total = rest + len.to_string().len();
        if total == len {
            break;
        }
        len = total;
    }
    out.extend_from_slice(len.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(key.as_bytes());
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
}

/// Split `path` into the ustar `prefix` and `name` fields, if it fits.
pub fn split_path(path: &[u8]) -> Option<(&[u8], &[u8])> {
    if path.len() <= 100 {
        return Some((&[], path));
    }
    // The last slash that leaves a non-empty name and at most 154 bytes of
    // prefix; earlier slashes only make the name longer.
    let limit = path.len() - 1;
    let i = path[..limit.min(155)].iter().rposition(|&b| b == b'/')?;
    let name = &path[i + 1..];
    (name.len() <= 100).then(|| (&path[..i], name))
}

fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(&s.as_bytes()[s.len() - digits..]);
    field[digits] = 0;
}

fn put(field: &mut [u8], value: &[u8]) {
    let n = value.len().min(field.len());
    field[..n].copy_from_slice(&value[..n]);
}

impl<W: Write> TarWriter<W> {
    /// A writer giving every entry the time `mtime` and masking the
    /// permission bits of files and directories with `umask`
    /// (`tar.umask`).
    pub fn new(out: W, umask: u32, mtime: u64) -> TarWriter<W> {
        TarWriter {
            out,
            umask,
            mtime,
            written: 0,
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.out.write_all(data)?;
        self.written += data.len() as u64;
        Ok(())
    }

    fn pad(&mut self) -> io::Result<()> {
        let rem = self.written % BLOCK;
        if rem != 0 {
            let zeros = [0u8; BLOCK as usize];
            self.write(&zeros[..(BLOCK - rem) as usize])?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        prefix: &[u8],
        name: &[u8],
        mode: u32,
        size: u64,
        typeflag: u8,
        link: &[u8],
    ) -> io::Result<()> {
        let mut h = [0u8; BLOCK as usize];
        put(&mut h[0..100], name);
        octal(&mut h[100..108], u64::from(mode));
        octal(&mut h[108..116], 0);
        octal(&mut h[116..124], 0);
        octal(&mut h[124..136], size.min(MAX_OCTAL_11));
        octal(&mut h[136..148], self.mtime.min(MAX_OCTAL_11));
        h[148..156].fill(b' ');
        h[156] = typeflag;
        put(&mut h[157..257], link);
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        put(&mut h[265..297], b"root");
        put(&mut h[297..329], b"root");
        octal(&mut h[329..337], 0);
        octal(&mut h[337..345], 0);
        put(&mut h[345..500], prefix);
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        let chk = format!("{sum:07o}");
        h[148..155].copy_from_slice(&chk.as_bytes()[chk.len() - 7..]);
        h[155] = 0;
        self.write(&h)
    }

    fn extended(&mut self, name: &[u8], typeflag: u8, records: &[u8]) -> io::Result<()> {
        self.header(&[], name, 0o666, records.len() as u64, typeflag, &[])?;
        self.write(records)?;
        self.pad()
    }

    /// The global header: the commit id (`comment`) and, when it does not
    /// fit the header field, the time. Writes nothing when there is
    /// neither.
    pub fn begin(&mut self, commit: Option<&str>) -> io::Result<()> {
        let mut records = Vec::new();
        if let Some(c) = commit {
            pax_record(&mut records, "comment", c.as_bytes());
        }
        if self.mtime > MAX_OCTAL_11 {
            pax_record(&mut records, "mtime", self.mtime.to_string().as_bytes());
        }
        if records.is_empty() {
            return Ok(());
        }
        self.extended(b"pax_global_header", b'g', &records)
    }

    /// Write the header of one entry. `oid` names the extended header if
    /// one is needed; `link` is the target of a symbolic link. The
    /// content of a file must follow through [`TarWriter::data`] and
    /// [`TarWriter::end_entry`].
    pub fn entry(
        &mut self,
        path: &[u8],
        kind: Kind,
        oid: &str,
        size: u64,
        link: &[u8],
    ) -> io::Result<()> {
        let (mode, typeflag) = match kind {
            Kind::Directory => (0o777 & !self.umask, b'5'),
            Kind::File { executable: true } => (0o777 & !self.umask, b'0'),
            Kind::File { executable: false } => (0o666 & !self.umask, b'0'),
            Kind::Symlink => (0o777, b'2'),
        };
        let mut records = Vec::new();
        let split = split_path(path);
        if split.is_none() {
            pax_record(&mut records, "path", path);
        }
        let long_link = link.len() > 100;
        if long_link {
            pax_record(&mut records, "linkpath", link);
        }
        if size > MAX_OCTAL_11 {
            pax_record(&mut records, "size", size.to_string().as_bytes());
        }
        let size_field = if size > MAX_OCTAL_11 { 0 } else { size };
        let paxname = format!("{oid}.paxheader");
        if !records.is_empty() {
            self.extended(paxname.as_bytes(), b'x', &records)?;
        }
        let dataname;
        let (prefix, name) = match split {
            Some(s) => s,
            None => {
                dataname = format!("{oid}.data");
                (&[][..], dataname.as_bytes())
            }
        };
        let seelink;
        let link = if long_link {
            seelink = format!("see {paxname}");
            seelink.as_bytes()
        } else {
            link
        };
        self.header(prefix, name, mode, size_field, typeflag, link)
    }

    /// Content of the current entry.
    pub fn data(&mut self, data: &[u8]) -> io::Result<()> {
        self.write(data)
    }

    /// Copy exactly `size` bytes from `r` as content of the current entry.
    pub fn copy(&mut self, r: &mut dyn Read, size: u64) -> io::Result<()> {
        let mut buf = vec![0u8; 65536];
        let mut left = size;
        while left > 0 {
            let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let n = r.read(&mut buf[..want])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short object content",
                ));
            }
            self.write(&buf[..n])?;
            left -= n as u64;
        }
        Ok(())
    }

    /// Pad the content of the current entry to a full block.
    pub fn end_entry(&mut self) -> io::Result<()> {
        self.pad()
    }

    /// Write a whole entry with in-memory content.
    pub fn add(&mut self, path: &[u8], kind: Kind, oid: &str, content: &[u8]) -> io::Result<()> {
        match kind {
            Kind::Symlink => self.entry(path, kind, oid, 0, content),
            Kind::Directory => self.entry(path, kind, oid, 0, &[]),
            Kind::File { .. } => {
                self.entry(path, kind, oid, content.len() as u64, &[])?;
                self.data(content)?;
                self.end_entry()
            }
        }
    }

    /// End the archive: two zero blocks, then zeros up to a full record.
    pub fn finish(mut self) -> io::Result<W> {
        let zeros = [0u8; BLOCK as usize];
        self.write(&zeros)?;
        self.write(&zeros)?;
        while self.written % RECORD != 0 {
            self.write(&zeros)?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

/// The commit id of a tar archive made by `git archive` from the first
/// 1024 bytes of it (`git get-tar-commit-id`): the `comment` record of a
/// leading global pax header.
pub fn commit_id(head: &[u8]) -> Option<&[u8]> {
    if head.len() < 1024 {
        return None;
    }
    let h = &head[..512];
    if h[156] != b'g' {
        return None;
    }
    let content = &head[512..1024];
    // The first record only, as long as it is a `comment`.
    let space = content.iter().position(|&b| b == b' ')?;
    let len: usize = std::str::from_utf8(&content[..space]).ok()?.parse().ok()?;
    if len > content.len() || len < space + 2 || content[len - 1] != b'\n' {
        return None;
    }
    let rec = &content[space + 1..len - 1];
    rec.strip_prefix(b"comment=")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_count_their_own_length() {
        let mut r = Vec::new();
        pax_record(&mut r, "comment", &[b'a'; 40]);
        assert_eq!(&r[..3], b"52 ");
        assert_eq!(r.len(), 52);
        let mut r = Vec::new();
        pax_record(&mut r, "path", &[b'x'; 91]);
        assert_eq!(r.len(), 101);
        assert!(r.starts_with(b"101 "));
    }

    #[test]
    fn split_long_paths() {
        assert_eq!(split_path(b"a/b"), Some((&b""[..], &b"a/b"[..])));
        let mut p = b"d/".repeat(60);
        p.extend_from_slice(b"f");
        let (prefix, name) = split_path(&p).unwrap();
        assert!(prefix.len() <= 155 && name.len() <= 100);
        assert_eq!([prefix, b"/", name].concat(), p);
        assert_eq!(split_path(&[b'x'; 101]), None);
    }

    #[test]
    fn empty_archive_is_one_record() {
        let w = TarWriter::new(Vec::new(), 0o002, 0);
        let out = w.finish().unwrap();
        assert_eq!(out, vec![0u8; RECORD as usize]);
    }

    #[test]
    fn commit_id_round_trip() {
        let mut w = TarWriter::new(Vec::new(), 0o002, 1234);
        w.begin(Some("0123456789012345678901234567890123456789"))
            .unwrap();
        let out = w.finish().unwrap();
        assert_eq!(
            commit_id(&out[..1024]),
            Some(&b"0123456789012345678901234567890123456789"[..])
        );
        let w = TarWriter::new(Vec::new(), 0o002, 1234);
        assert_eq!(commit_id(&w.finish().unwrap()[..1024]), None);
    }

    #[test]
    fn header_layout() {
        let mut w = TarWriter::new(Vec::new(), 0o002, 0o10245714140);
        w.add(b"a", Kind::File { executable: false }, "0", b"hi\n")
            .unwrap();
        let out = w.finish().unwrap();
        assert_eq!(&out[100..108], b"0000664\0");
        assert_eq!(&out[124..136], b"00000000003\0");
        assert_eq!(&out[136..148], b"10245714140\0");
        assert_eq!(&out[257..265], b"ustar\x0000");
        assert_eq!(&out[512..515], b"hi\n");
    }
}
