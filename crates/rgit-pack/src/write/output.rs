// SPDX-License-Identifier: MIT

//! The byte stream of a pack: header, entries and trailer, hashed while
//! written so that the output can be a pipe or a socket.

use std::io::Write;

use rgit_hash::{HashKind, Hasher, ObjectId};

use super::WriteError;

/// Writes a version 2 pack to `W` and keeps the running checksum, the
/// current offset and the CRC32 of the entry being written (the values a
/// `.idx` needs).
pub struct PackStream<W: Write> {
    out: W,
    hasher: Hasher,
    offset: u64,
    crc: crc32fast::Hasher,
    announced: u32,
    written: u32,
}

impl<W: Write> PackStream<W> {
    /// Write the 12-byte header for a pack of `count` objects.
    pub fn new(out: W, kind: HashKind, count: u32) -> Result<PackStream<W>, WriteError> {
        let mut s = PackStream {
            out,
            hasher: Hasher::new(kind),
            offset: 0,
            crc: crc32fast::Hasher::new(),
            announced: count,
            written: 0,
        };
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(b"PACK");
        header.extend_from_slice(&2u32.to_be_bytes());
        header.extend_from_slice(&count.to_be_bytes());
        s.put(&header)?;
        Ok(s)
    }

    /// Offset of the next byte.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Start a new entry; returns its offset.
    pub fn begin_entry(&mut self) -> u64 {
        self.crc = crc32fast::Hasher::new();
        self.offset
    }

    /// Append bytes of the current entry.
    pub fn put(&mut self, bytes: &[u8]) -> Result<(), WriteError> {
        self.out.write_all(bytes)?;
        self.hasher.update(bytes);
        self.crc.update(bytes);
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Finish the current entry; returns the CRC32 of its bytes.
    pub fn end_entry(&mut self) -> u32 {
        self.written += 1;
        std::mem::replace(&mut self.crc, crc32fast::Hasher::new()).finalize()
    }

    /// Write the trailing checksum and flush. Returns the checksum and the
    /// writer.
    ///
    /// # Panics
    ///
    /// If the number of entries differs from the count in the header (a
    /// bug in the caller).
    pub fn finish(mut self) -> Result<(ObjectId, W), WriteError> {
        assert_eq!(
            self.written, self.announced,
            "pack header announced a different object count"
        );
        let id = self.hasher.finalize().map_err(|_| WriteError::Collision)?;
        self.out.write_all(id.as_bytes())?;
        self.out.flush()?;
        Ok((id, self.out))
    }
}
