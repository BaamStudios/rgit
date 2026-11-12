// SPDX-License-Identifier: MIT

//! First pass: read the pack stream once, front to back.

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::sync::OnceLock;

use flate2::{Decompress, FlushDecompress, Status};
use rgit_hash::{Hasher, ObjectId, object_hasher};

use super::{BadEntry, Base, Checker, Entry, Error, Options, Phase, Progress, Slots};
use crate::ObjectKind;

const BUF_LEN: usize = 128 * 1024;
/// Output chunk while inflating.
const CHUNK: usize = 64 * 1024;

/// What the first pass found.
pub(crate) struct Pass1 {
    pub entries: Vec<Entry>,
    /// Ids of base objects; deltas are filled by the second pass.
    pub slots: Slots,
    /// The trailing checksum.
    pub checksum: ObjectId,
    /// Offset of the trailing checksum.
    pub entries_end: u64,
    /// Pack version from the header.
    pub version: u32,
    /// Bytes read after the trailer (with `Options::allow_trailing_data`).
    pub trailing: Vec<u8>,
}

/// Buffered input that hashes, counts and copies what is consumed.
struct Input<'a> {
    reader: &'a mut dyn Read,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    /// Bytes consumed so far (= offset in the pack).
    offset: u64,
    hasher: Hasher,
    crc: crc32fast::Hasher,
    copy: Option<BufWriter<&'a mut File>>,
    max: Option<u64>,
}

impl<'a> Input<'a> {
    /// Refill the buffer; `Ok(false)` at end of input.
    fn fill(&mut self) -> Result<bool, Error> {
        if self.pos < self.len {
            return Ok(true);
        }
        self.pos = 0;
        self.len = 0;
        loop {
            match self.reader.read(&mut self.buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.len = n;
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::Read(e)),
            }
        }
    }

    fn available(&mut self) -> Result<&[u8], Error> {
        if !self.fill()? {
            return Err(Error::EarlyEof);
        }
        Ok(&self.buf[self.pos..self.len])
    }

    /// Mark `n` buffered bytes as consumed; `hash` adds them to the pack
    /// checksum and the entry CRC.
    fn consume(&mut self, n: usize, hash: bool) -> Result<(), Error> {
        let bytes = &self.buf[self.pos..self.pos + n];
        if hash {
            self.hasher.update(bytes);
            self.crc.update(bytes);
        }
        if let Some(w) = &mut self.copy {
            w.write_all(bytes).map_err(Error::Write)?;
        }
        self.pos += n;
        self.offset += n as u64;
        if let Some(max) = self.max {
            if self.offset > max {
                return Err(Error::TooLarge(max));
            }
        }
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let b = self.available()?[0];
        self.consume(1, true)?;
        Ok(b)
    }

    fn exact(&mut self, out: &mut [u8], hash: bool) -> Result<(), Error> {
        let mut done = 0;
        while done < out.len() {
            let avail = self.available()?;
            let n = avail.len().min(out.len() - done);
            out[done..done + n].copy_from_slice(&avail[..n]);
            self.consume(n, hash)?;
            done += n;
        }
        Ok(())
    }

    /// Bytes that were written to the copy but are not part of the hash
    /// (the reconstructed `--pack_header`).
    fn inject(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.hasher.update(bytes);
        if let Some(w) = &mut self.copy {
            w.write_all(bytes).map_err(Error::Write)?;
        }
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Inflate one zlib stream that must produce exactly `size` bytes.
    /// Returns the data if `keep`, otherwise an empty vector.
    fn inflate(&mut self, size: u64, keep: bool, offset: u64) -> Result<Vec<u8>, Error> {
        let bad = |code: i32, detail: Option<String>| Error::BadObject {
            offset,
            what: BadEntry::Inflate { code, detail },
        };
        let mut z = Decompress::new(true);
        let mut out: Vec<u8> = Vec::new();
        if keep {
            out.reserve(usize::try_from(size.min(1 << 20)).unwrap_or(0));
        }
        let mut chunk = vec![0u8; CHUNK.min(usize::try_from(size).unwrap_or(CHUNK)).max(1)];
        let mut produced: u64 = 0;
        loop {
            // Give zlib room for the rest of the object, or one byte to
            // detect a stream that is longer than the header says.
            let room = (size - produced).min(chunk.len() as u64).max(1) as usize;
            let before_in = z.total_in();
            let before_out = z.total_out();
            let status = {
                if !self.fill()? {
                    return Err(Error::EarlyEof);
                }
                let input = &self.buf[self.pos..self.len];
                z.decompress(input, &mut chunk[..room], FlushDecompress::None)
            };
            let used = (z.total_in() - before_in) as usize;
            let made = (z.total_out() - before_out) as usize;
            self.consume(used, true)?;
            produced += made as u64;
            if produced > size {
                return Err(bad(-5, None));
            }
            if keep {
                out.extend_from_slice(&chunk[..made]);
            }
            match status {
                Ok(Status::StreamEnd) => {
                    if produced != size {
                        return Err(bad(1, None));
                    }
                    return Ok(out);
                }
                Ok(_) => {
                    // zlib takes all input it is given while it has room
                    // for output; no progress at all means it is stuck.
                    if used == 0 && made == 0 {
                        return Err(bad(-5, None));
                    }
                }
                Err(e) => {
                    if e.needs_dictionary().is_some() {
                        return Err(bad(2, None));
                    }
                    return Err(bad(-3, Some(e.message().unwrap_or("").to_string())));
                }
            }
        }
    }
}

/// Read the whole pack from `reader`. `copy` receives every byte. Data
/// after the trailer is an error, unless `opts.allow_trailing_data` and
/// not `to_eof`: then the bytes already buffered are returned and the input
/// is not read further.
pub(crate) fn read_pack(
    reader: &mut dyn Read,
    copy: Option<&mut File>,
    opts: &Options,
    checker: &Checker<'_>,
    mut progress: Option<&mut (dyn Progress + 'static)>,
    to_eof: bool,
) -> Result<Pass1, Error> {
    let mut input = Input {
        reader,
        buf: vec![0; BUF_LEN],
        pos: 0,
        len: 0,
        offset: 0,
        hasher: Hasher::new(opts.hash),
        crc: crc32fast::Hasher::new(),
        copy: copy.map(|f| BufWriter::with_capacity(BUF_LEN, f)),
        max: opts.max_input_size,
    };
    let (version, count) = match opts.pack_header {
        Some((version, count)) => {
            let mut hdr = [0u8; 12];
            hdr[..4].copy_from_slice(b"PACK");
            hdr[4..8].copy_from_slice(&version.to_be_bytes());
            hdr[8..].copy_from_slice(&count.to_be_bytes());
            input.inject(&hdr)?;
            (version, count)
        }
        None => {
            let mut hdr = [0u8; 12];
            input.exact(&mut hdr, true)?;
            if &hdr[..4] != b"PACK" {
                return Err(Error::SignatureMismatch);
            }
            let version = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
            let count = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
            (version, count)
        }
    };
    if version != 2 && version != 3 {
        return Err(Error::Version(version));
    }
    if let Some(p) = progress.as_deref_mut() {
        p.start(Phase::Reading, count as u64);
    }
    // Do not trust the count for the allocation.
    let mut entries: Vec<Entry> = Vec::with_capacity((count as usize).min(1 << 16));
    let mut slots: Slots = Vec::with_capacity((count as usize).min(1 << 16));
    let hlen = opts.hash.byte_len();
    for i in 0..count {
        let offset = input.offset;
        input.crc = crc32fast::Hasher::new();
        let first = input.byte()?;
        let ty = (first >> 4) & 7;
        let mut size = (first & 0x0f) as u64;
        let mut shift = 4u32;
        let mut c = first;
        while c & 0x80 != 0 {
            c = input.byte()?;
            if shift > 63 || ((c & 0x7f) as u64) > (u64::MAX >> shift) {
                return Err(Error::BadObject {
                    offset,
                    what: BadEntry::BadHeader,
                });
            }
            size |= ((c & 0x7f) as u64) << shift;
            shift += 7;
        }
        let base = match ty {
            1 => Base::None(ObjectKind::Commit),
            2 => Base::None(ObjectKind::Tree),
            3 => Base::None(ObjectKind::Blob),
            4 => Base::None(ObjectKind::Tag),
            6 => {
                let out_of_bound = Error::BadObject {
                    offset,
                    what: BadEntry::DeltaBaseOutOfBound,
                };
                let mut c = input.byte()?;
                let mut rel = (c & 0x7f) as u64;
                while c & 0x80 != 0 {
                    c = input.byte()?;
                    rel = match rel
                        .checked_add(1)
                        .and_then(|r| r.checked_mul(128))
                        .map(|r| r | (c & 0x7f) as u64)
                    {
                        Some(r) => r,
                        None => return Err(out_of_bound),
                    };
                }
                if rel == 0 || rel > offset {
                    return Err(out_of_bound);
                }
                let base_offset = offset - rel;
                match entries.binary_search_by_key(&base_offset, |e| e.offset) {
                    Ok(idx) => Base::Ofs(idx as u32),
                    Err(_) => return Err(out_of_bound),
                }
            }
            7 => {
                let mut raw = [0u8; 32];
                input.exact(&mut raw[..hlen], true)?;
                Base::Ref(ObjectId::from_bytes(opts.hash, &raw[..hlen]).expect("length matches"))
            }
            _ => {
                return Err(Error::BadObject {
                    offset,
                    what: BadEntry::UnknownType(ty),
                });
            }
        };
        let data_offset = input.offset;
        let slot = OnceLock::new();
        match base {
            Base::None(kind) => {
                let data = input.inflate(size, true, offset)?;
                let mut h = object_hasher(opts.hash, kind.as_str(), size);
                h.update(&data);
                let id = h.finalize().map_err(|_| Error::CollisionAttack(offset))?;
                checker.check(&id, kind, &data)?;
                let _ = slot.set((id, kind));
            }
            _ => {
                input.inflate(size, false, offset)?;
            }
        }
        let crc32 = std::mem::replace(&mut input.crc, crc32fast::Hasher::new()).finalize();
        entries.push(Entry {
            offset,
            data_offset,
            size,
            crc32,
            base,
        });
        slots.push(slot);
        if let Some(p) = progress.as_deref_mut() {
            p.update(i as u64 + 1, input.offset);
        }
    }
    let entries_end = input.offset;
    let computed = std::mem::replace(&mut input.hasher, Hasher::new(opts.hash))
        .finalize()
        .map_err(|_| Error::ChecksumMismatch)?;
    let mut trailer = [0u8; 32];
    input.exact(&mut trailer[..hlen], false)?;
    if computed.as_bytes() != &trailer[..hlen] {
        return Err(Error::ChecksumMismatch);
    }
    // Data after the trailer.
    let mut trailing = Vec::new();
    if opts.allow_trailing_data && !to_eof {
        trailing.extend_from_slice(&input.buf[input.pos..input.len]);
    } else if input.pos < input.len || input.fill()? {
        return Err(Error::JunkAtEnd);
    }
    if let Some(p) = progress {
        p.finish(None);
    }
    if let Some(w) = input.copy.take() {
        let f = w.into_inner().map_err(|e| Error::Write(e.into_error()))?;
        f.flush().map_err(Error::Write)?;
    }
    Ok(Pass1 {
        entries,
        slots,
        checksum: computed,
        entries_end,
        version,
        trailing,
    })
}
