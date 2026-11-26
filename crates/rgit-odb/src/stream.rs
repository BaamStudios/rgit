// SPDX-License-Identifier: MIT

//! [`ObjectReader`]: an object's content as a stream, for objects too large
//! to hold in memory (`core.bigFileThreshold`).

use std::io::{self, Read};

use rgit_object::{ObjectHeader, ObjectKind, RawObject};

/// The default of `core.bigFileThreshold`: 512 MiB.
pub const DEFAULT_BIG_FILE_THRESHOLD: u64 = 512 << 20;

/// An object's type, size and content as a reader.
///
/// Loose objects and whole pack entries are inflated piece by piece as
/// they are read; other objects (deltas) are reconstructed in memory and
/// read from there. A damaged object fails while reading with an
/// [`io::Error`] of kind [`io::ErrorKind::InvalidData`].
pub struct ObjectReader {
    header: ObjectHeader,
    inner: Box<dyn Read + Send>,
}

impl ObjectReader {
    /// A reader over `inner`, which yields the content of an object with
    /// `header`.
    pub fn new(header: ObjectHeader, inner: Box<dyn Read + Send>) -> ObjectReader {
        ObjectReader { header, inner }
    }

    /// A reader over an object already in memory.
    pub fn from_raw(raw: RawObject) -> ObjectReader {
        let header = raw.header();
        ObjectReader {
            header,
            inner: Box::new(io::Cursor::new(raw.data)),
        }
    }

    /// Type and size.
    pub fn header(&self) -> ObjectHeader {
        self.header
    }

    /// The type.
    pub fn kind(&self) -> ObjectKind {
        self.header.kind
    }

    /// The content size.
    pub fn size(&self) -> u64 {
        self.header.size
    }

    /// Copy the whole content to `out`; returns the number of bytes.
    pub fn copy_to(&mut self, out: &mut dyn io::Write) -> io::Result<u64> {
        io::copy(self, out)
    }

    /// Read the whole content into memory.
    pub fn into_raw(mut self) -> io::Result<RawObject> {
        let mut data = Vec::new();
        self.inner.read_to_end(&mut data)?;
        Ok(RawObject {
            kind: self.header.kind,
            data,
        })
    }
}

impl std::fmt::Debug for ObjectReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectReader")
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

impl Read for ObjectReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}
