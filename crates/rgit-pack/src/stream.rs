// SPDX-License-Identifier: MIT

//! Streaming the content of a whole (non-delta) pack entry.
//!
//! Large blobs are stored whole (`core.bigFileThreshold` turns off delta
//! compression for them), so they can be inflated piece by piece straight
//! from the file. The entry is read with ordinary file reads rather than
//! through the mapping of the pack: touched pages of a mapping count
//! towards the resident memory of the process, which is what streaming is
//! meant to keep small.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use flate2::{Decompress, FlushDecompress, Status};

use crate::data::EntryKind;
use crate::{Error, ObjectKind, Pack};

const INPUT_BUF: usize = 64 * 1024;

/// Reads the inflated content of one whole pack entry. The stream must
/// produce exactly the size from the entry header; anything else is an
/// [`io::Error`] of kind [`io::ErrorKind::InvalidData`] wrapping an
/// [`Error`].
pub struct EntryReader {
    input: BufReader<File>,
    z: Decompress,
    size: u64,
    done: bool,
    offset: u64,
    path: PathBuf,
}

impl std::fmt::Debug for EntryReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryReader")
            .field("path", &self.path)
            .field("offset", &self.offset)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl EntryReader {
    /// Read the zlib stream at `data_offset` of the pack file `path`,
    /// which inflates to `size` bytes.
    pub fn open(path: &std::path::Path, data_offset: u64, size: u64) -> io::Result<EntryReader> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(data_offset))?;
        Ok(EntryReader {
            input: BufReader::with_capacity(INPUT_BUF, file),
            z: Decompress::new(true),
            size,
            done: false,
            offset: data_offset,
            path: path.to_path_buf(),
        })
    }

    /// Size of the content.
    pub fn size(&self) -> u64 {
        self.size
    }

    fn fail(&self, e: Error) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

impl Read for EntryReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.done {
            return Ok(0);
        }
        loop {
            let produced = self.z.total_out();
            // Room for the rest, or one byte to notice an overlong stream.
            let room = (self.size - produced).max(1).min(out.len() as u64) as usize;
            let input = self.input.fill_buf_checked()?;
            let eof = input.is_empty();
            let before_in = self.z.total_in();
            let status = self
                .z
                .decompress(input, &mut out[..room], FlushDecompress::None)
                .map_err(|e| {
                    self.fail(Error::Inflate {
                        detail: e.message().unwrap_or("invalid data").to_string(),
                    })
                })?;
            let used = (self.z.total_in() - before_in) as usize;
            self.input.consume_bytes(used);
            let made = (self.z.total_out() - produced) as usize;
            let total = self.z.total_out();
            if total > self.size || (status == Status::StreamEnd && total != self.size) {
                return Err(self.fail(Error::InflateSize {
                    offset: self.offset,
                    expected: self.size,
                    actual: total,
                }));
            }
            if status == Status::StreamEnd {
                self.done = true;
                return Ok(made);
            }
            if made > 0 {
                return Ok(made);
            }
            if used == 0 && eof {
                return Err(self.fail(Error::InflateTruncated));
            }
        }
    }
}

/// `fill_buf`/`consume` under names that do not need `BufRead` in scope
/// at the call site.
trait Buffered {
    fn fill_buf_checked(&mut self) -> io::Result<&[u8]>;
    fn consume_bytes(&mut self, n: usize);
}

impl Buffered for BufReader<File> {
    fn fill_buf_checked(&mut self) -> io::Result<&[u8]> {
        io::BufRead::fill_buf(self)
    }

    fn consume_bytes(&mut self, n: usize) {
        io::BufRead::consume(self, n);
    }
}

impl Pack {
    /// Open the entry at `offset` for streaming its content, if it is a
    /// whole object (`None` for a delta, which has to be reconstructed in
    /// memory). Returns the type, the size and the reader.
    pub fn stream_at(&self, offset: u64) -> Result<Option<(ObjectKind, u64, EntryReader)>, Error> {
        let data = self.data()?;
        let h = data.entry_header(offset)?;
        let EntryKind::Base(kind) = h.kind else {
            return Ok(None);
        };
        let path = self.path().to_path_buf();
        let io_err = |e: io::Error| Error::io(&path, e);
        let mut file = File::open(&path).map_err(io_err)?;
        file.seek(SeekFrom::Start(h.data_offset)).map_err(io_err)?;
        let reader = EntryReader {
            input: BufReader::with_capacity(INPUT_BUF, file),
            z: Decompress::new(true),
            size: h.size,
            done: false,
            offset,
            path,
        };
        Ok(Some((kind, h.size, reader)))
    }

    /// [`Pack::stream_at`] for the object `id`; `None` if the pack does not
    /// list it or stores it as a delta.
    pub fn stream(
        &self,
        id: &rgit_hash::ObjectId,
    ) -> Result<Option<(ObjectKind, u64, EntryReader)>, Error> {
        let Some(pos) = self.index().lookup(id) else {
            return Ok(None);
        };
        let offset = self.index().offset_at(pos)?;
        self.stream_at(offset)
    }
}

#[cfg(test)]
mod tests {
    use rgit_hash::HashKind;

    use super::*;
    use crate::write::{ListedObject, WriteOptions, write_pack};

    #[test]
    fn streams_whole_entries() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let id = rgit_hash::hash_object(HashKind::Sha1, "blob", &data).unwrap();

        struct Mem(Vec<u8>, ObjectId);
        use rgit_hash::ObjectId;
        impl crate::write::ObjectSource for Mem {
            fn read(
                &self,
                id: &ObjectId,
            ) -> Result<Option<crate::Object>, crate::write::SourceError> {
                Ok((*id == self.1).then(|| crate::Object {
                    kind: ObjectKind::Blob,
                    data: self.0.clone(),
                }))
            }
        }
        let src = Mem(data.clone(), id);
        let objects = [ListedObject {
            id,
            kind: ObjectKind::Blob,
            tag_target: None,
            name_hash: 0,
        }];
        let (summary, bytes) = write_pack(
            Vec::new(),
            HashKind::Sha1,
            &objects,
            &src,
            &[],
            &WriteOptions::default(),
        )
        .unwrap();
        let idx = summary
            .indexed(HashKind::Sha1)
            .index_bytes(Default::default())
            .unwrap();
        let pack_path = dir.path().join("p.pack");
        std::fs::write(&pack_path, &bytes).unwrap();
        let pack = Pack::from_bytes(idx, bytes, &pack_path, HashKind::Sha1).unwrap();
        let (kind, size, mut r) = pack.stream(&id).unwrap().unwrap();
        assert_eq!((kind, size), (ObjectKind::Blob, data.len() as u64));
        let mut out = Vec::new();
        let mut chunk = [0u8; 1000];
        loop {
            let n = r.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(out, data);
    }
}
