// SPDX-License-Identifier: MIT
//! A minimal zip archive writer (PKWARE APPNOTE.TXT): regular files,
//! compressed with raw deflate, no zip64. Used by `git diagnose`.

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

struct Entry {
    name: Vec<u8>,
    crc: u32,
    compressed: u32,
    size: u32,
    offset: u32,
}

/// Writes a zip archive into `out`; [`ZipWriter::finish`] writes the
/// central directory.
pub struct ZipWriter<W: Write> {
    out: W,
    written: u64,
    entries: Vec<Entry>,
    /// MS-DOS time and date of every entry.
    dos_time: u16,
    dos_date: u16,
}

fn too_large() -> io::Error {
    io::Error::other("zip archive too large")
}

/// MS-DOS date and time fields for a broken-down local time.
pub fn dos_datetime(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> (u16, u16) {
    let year = (year - 1980).clamp(0, 127) as u32;
    let date = (year << 9) | (month << 5) | day;
    let time = (hour << 11) | (min << 5) | (sec / 2);
    (time as u16, date as u16)
}

impl<W: Write> ZipWriter<W> {
    /// A writer stamping entries with the given MS-DOS time and date.
    pub fn new(out: W, (dos_time, dos_date): (u16, u16)) -> Self {
        ZipWriter {
            out,
            written: 0,
            entries: Vec::new(),
            dos_time,
            dos_date,
        }
    }

    fn put(&mut self, data: &[u8]) -> io::Result<()> {
        self.out.write_all(data)?;
        self.written += data.len() as u64;
        Ok(())
    }

    /// Add a file named `name` (with `/` separators) holding `data`.
    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data)?;
        let packed = enc.finish()?;
        let offset = u32::try_from(self.written).map_err(|_| too_large())?;
        let size = u32::try_from(data.len()).map_err(|_| too_large())?;
        let compressed = u32::try_from(packed.len()).map_err(|_| too_large())?;
        let name_len = u16::try_from(name.len()).map_err(|_| too_large())?;
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0u16.to_le_bytes()); // flags
        h.extend_from_slice(&8u16.to_le_bytes()); // deflate
        h.extend_from_slice(&self.dos_time.to_le_bytes());
        h.extend_from_slice(&self.dos_date.to_le_bytes());
        h.extend_from_slice(&crc.sum().to_le_bytes());
        h.extend_from_slice(&compressed.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&name_len.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // extra length
        h.extend_from_slice(name.as_bytes());
        self.put(&h)?;
        self.put(&packed)?;
        self.entries.push(Entry {
            name: name.as_bytes().to_vec(),
            crc: crc.sum(),
            compressed,
            size,
            offset,
        });
        Ok(())
    }

    /// Write the central directory and return the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        let start = u32::try_from(self.written).map_err(|_| too_large())?;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let mut h = Vec::with_capacity(46 + e.name.len());
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            h.extend_from_slice(&(3u16 << 8 | 20).to_le_bytes()); // made by: unix
            h.extend_from_slice(&20u16.to_le_bytes()); // version needed
            h.extend_from_slice(&0u16.to_le_bytes()); // flags
            h.extend_from_slice(&8u16.to_le_bytes()); // deflate
            h.extend_from_slice(&self.dos_time.to_le_bytes());
            h.extend_from_slice(&self.dos_date.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            h.extend_from_slice(&e.compressed.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes());
            h.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // extra
            h.extend_from_slice(&0u16.to_le_bytes()); // comment
            h.extend_from_slice(&0u16.to_le_bytes()); // disk
            h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            h.extend_from_slice(&(0o100644u32 << 16).to_le_bytes()); // external
            h.extend_from_slice(&e.offset.to_le_bytes());
            h.extend_from_slice(&e.name);
            self.put(&h)?;
        }
        let end = u32::try_from(self.written).map_err(|_| too_large())?;
        let count = u16::try_from(entries.len()).map_err(|_| too_large())?;
        let mut h = Vec::with_capacity(22);
        h.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&count.to_le_bytes());
        h.extend_from_slice(&count.to_le_bytes());
        h.extend_from_slice(&(end - start).to_le_bytes());
        h.extend_from_slice(&start.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.put(&h)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_layout() {
        let mut z = ZipWriter::new(Vec::new(), dos_datetime(2026, 11, 26, 12, 30, 10));
        z.add("a.txt", b"hello hello hello").unwrap();
        z.add("dir/b", b"").unwrap();
        let out = z.finish().unwrap();
        assert_eq!(&out[..4], b"PK\x03\x04");
        // End of central directory: two entries.
        let eocd = &out[out.len() - 22..];
        assert_eq!(&eocd[..4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 2);
        let cd_size = u32::from_le_bytes(eocd[12..16].try_into().unwrap()) as usize;
        let cd_start = u32::from_le_bytes(eocd[16..20].try_into().unwrap()) as usize;
        assert_eq!(cd_start + cd_size, out.len() - 22);
        assert_eq!(&out[cd_start..cd_start + 4], b"PK\x01\x02");
        // The first entry inflates back.
        let csize = u32::from_le_bytes(out[18..22].try_into().unwrap()) as usize;
        let data = &out[30 + 5..30 + 5 + csize];
        let mut d = flate2::read::DeflateDecoder::new(data);
        let mut s = String::new();
        std::io::Read::read_to_string(&mut d, &mut s).unwrap();
        assert_eq!(s, "hello hello hello");
    }

    #[test]
    fn dos_fields() {
        let (t, d) = dos_datetime(1980, 1, 1, 0, 0, 0);
        assert_eq!((t, d), (0, (1 << 5) | 1));
    }
}
