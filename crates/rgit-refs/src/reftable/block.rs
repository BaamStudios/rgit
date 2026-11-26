// SPDX-License-Identifier: MIT
//! Blocks of a reftable: a 4-byte header (type, length), prefix-compressed
//! records, the table of restart points and their count.
//!
//! Offsets inside a block count from the start of the block; in the first
//! block of a file they count from the start of the file, so the file
//! header is part of that block. The records of a log block are deflated;
//! [`Block`] holds them inflated, with the block header in front, so the
//! restart offsets apply as stored.

use std::cmp::Ordering;
use std::io::Write as _;

use flate2::write::ZlibEncoder;
use flate2::{Compression, Decompress, FlushDecompress, Status};

use super::FormatError;
use super::record::{Cursor, put_varint};

/// Block type of reference blocks.
pub const BLOCK_REF: u8 = b'r';
/// Block type of log blocks.
pub const BLOCK_LOG: u8 = b'g';
/// Block type of object blocks.
pub const BLOCK_OBJ: u8 = b'o';
/// Block type of index blocks.
pub const BLOCK_INDEX: u8 = b'i';

/// The largest number of restart points a block can list.
const MAX_RESTARTS: usize = 0xffff;

fn put_u24(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&(v as u32).to_be_bytes()[1..]);
}

fn get_u24(data: &[u8], at: usize) -> Option<usize> {
    let b = data.get(at..at + 3)?;
    Some(u32::from_be_bytes([0, b[0], b[1], b[2]]) as usize)
}

/// Collects records into one block.
pub(crate) struct BlockWriter {
    typ: u8,
    /// The block so far; starts with `header_off` bytes that belong to the
    /// file header (first block only) and the 4-byte block header.
    buf: Vec<u8>,
    header_off: usize,
    block_size: usize,
    restart_interval: usize,
    restarts: Vec<usize>,
    last_key: Vec<u8>,
    entries: usize,
}

impl BlockWriter {
    pub fn new(typ: u8, block_size: usize, header_off: usize, restart_interval: usize) -> Self {
        let mut buf = vec![0u8; header_off + 4];
        buf[header_off] = typ;
        BlockWriter {
            typ,
            buf,
            header_off,
            block_size,
            restart_interval: restart_interval.max(1),
            restarts: Vec::new(),
            last_key: Vec::new(),
            entries: 0,
        }
    }

    pub fn typ(&self) -> u8 {
        self.typ
    }

    pub fn entries(&self) -> usize {
        self.entries
    }

    pub fn last_key(&self) -> &[u8] {
        &self.last_key
    }

    /// Adds a record: `key`, the three `extra` bits and the encoded value.
    /// Returns `false`, leaving the block as it was, when the record does
    /// not fit any more.
    pub fn add(&mut self, key: &[u8], extra: u8, value: &[u8]) -> bool {
        let at_interval = self.entries.is_multiple_of(self.restart_interval);
        let prefix = if at_interval {
            0
        } else {
            common_prefix(&self.last_key, key)
        };
        // A record that shares nothing with its predecessor carries its
        // whole key, so it serves as a restart point as well.
        let restart = prefix == 0 && self.restarts.len() < MAX_RESTARTS;
        let start = self.buf.len();
        put_varint(&mut self.buf, prefix as u64);
        put_varint(
            &mut self.buf,
            (((key.len() - prefix) as u64) << 3) | u64::from(extra),
        );
        self.buf.extend_from_slice(&key[prefix..]);
        self.buf.extend_from_slice(value);
        let restarts = self.restarts.len() + usize::from(restart);
        if self.buf.len() + 3 * restarts + 2 > self.block_size {
            self.buf.truncate(start);
            return false;
        }
        if restart {
            self.restarts.push(start);
        }
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.entries += 1;
        true
    }

    /// Completes the block. Returns the bytes to write (without the file
    /// header part of the first block) and the block length to account
    /// for alignment: the stored length for plain blocks, the number of
    /// bytes written for log blocks.
    pub fn finish(mut self) -> std::io::Result<(Vec<u8>, usize)> {
        for i in 0..self.restarts.len() {
            let r = self.restarts[i];
            put_u24(&mut self.buf, r);
        }
        let count = self.restarts.len() as u16;
        self.buf.extend_from_slice(&count.to_be_bytes());
        let len = self.buf.len();
        let h = self.header_off;
        self.buf[h + 1] = (len >> 16) as u8;
        self.buf[h + 2] = (len >> 8) as u8;
        self.buf[h + 3] = len as u8;
        if self.typ != BLOCK_LOG {
            self.buf.drain(..h);
            return Ok((self.buf, len));
        }
        let mut out = self.buf[h..h + 4].to_vec();
        let mut enc = ZlibEncoder::new(&mut out, Compression::best());
        enc.write_all(&self.buf[h + 4..])?;
        enc.finish()?;
        let written = out.len();
        Ok((out, written))
    }
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// A block read from a table.
#[derive(Clone, Debug)]
pub struct Block {
    /// The block type.
    pub typ: u8,
    /// The block from the offset origin (file start for the first block)
    /// up to its stored length; log records inflated.
    data: Vec<u8>,
    /// Where the records start.
    first: usize,
    /// Where the restart table starts.
    restarts_at: usize,
    restart_count: usize,
    /// The length stored in the block header.
    pub len: usize,
    /// How many bytes the block takes in the file, without padding and
    /// without the file header part of the first block.
    pub stored: usize,
    /// Length of the object ids in the records.
    hash_len: usize,
}

impl Block {
    /// Parses the block that starts at `off` in `file`. `header_off` is
    /// the size of the file header when this is the first block of the
    /// file, else zero. `end` is where the block's section ends.
    pub(crate) fn parse(
        file: &[u8],
        off: usize,
        header_off: usize,
        end: usize,
        hash_len: usize,
    ) -> Result<Block, FormatError> {
        let start = off + header_off;
        let typ = *file.get(start).ok_or(FormatError::Truncated)?;
        let len = get_u24(file, start + 1).ok_or(FormatError::Truncated)?;
        if len < header_off + 4 + 2 {
            return Err(FormatError::Corrupt("block length too small"));
        }
        let (data, stored) = if typ == BLOCK_LOG {
            let body = len - header_off - 4;
            let mut data = vec![0u8; header_off];
            data.extend_from_slice(&file[start..start + 4]);
            data.resize(header_off + 4 + body, 0);
            let src = file.get(start + 4..end).ok_or(FormatError::Truncated)?;
            let mut z = Decompress::new(true);
            let status = z
                .decompress(src, &mut data[header_off + 4..], FlushDecompress::Finish)
                .map_err(|_| FormatError::Corrupt("log block does not inflate"))?;
            if status != Status::StreamEnd || z.total_out() as usize != body {
                return Err(FormatError::Corrupt("log block has the wrong length"));
            }
            (data, 4 + z.total_in() as usize)
        } else {
            let data = file
                .get(off..off + len)
                .filter(|_| off + len <= end)
                .ok_or(FormatError::Truncated)?
                .to_vec();
            (data, len - header_off)
        };
        let restart_count = usize::from(u16::from_be_bytes([data[len - 2], data[len - 1]]));
        let restarts_at = (len - 2)
            .checked_sub(3 * restart_count)
            .filter(|&r| r >= header_off + 4)
            .ok_or(FormatError::Corrupt("restart table larger than block"))?;
        Ok(Block {
            typ,
            data,
            first: header_off + 4,
            restarts_at,
            restart_count,
            len,
            stored,
            hash_len,
        })
    }

    /// The number of restart points.
    pub fn restart_count(&self) -> usize {
        self.restart_count
    }

    fn restart(&self, i: usize) -> usize {
        get_u24(&self.data, self.restarts_at + 3 * i).unwrap_or(self.first)
    }

    /// The full key of the record at restart point `i`.
    fn restart_key(&self, i: usize) -> Result<Vec<u8>, FormatError> {
        let at = self.restart(i);
        if at < self.first || at >= self.restarts_at {
            return Err(FormatError::Corrupt("restart offset outside of block"));
        }
        let mut key = Vec::new();
        Cursor::new(&self.data[..self.restarts_at], at).key(&mut key)?;
        Ok(key)
    }

    /// The position of the first record.
    pub(crate) fn start(&self) -> Pos {
        Pos {
            at: self.first,
            key: Vec::new(),
        }
    }

    /// Reads the key of the record at `pos` and returns its `extra` bits;
    /// `None` at the end of the block. The value follows at
    /// [`Block::cursor`]; the caller consumes it and stores the cursor's
    /// position back into `pos`.
    pub(crate) fn next_key(&self, pos: &mut Pos) -> Result<Option<u8>, FormatError> {
        if pos.at >= self.restarts_at {
            return Ok(None);
        }
        let mut cur = Cursor::new(&self.data[..self.restarts_at], pos.at);
        let extra = cur.key(&mut pos.key)?;
        pos.at = cur.at;
        Ok(Some(extra))
    }

    /// A cursor at `pos`.
    pub(crate) fn cursor(&self, pos: &Pos) -> Cursor<'_> {
        Cursor::new(&self.data[..self.restarts_at], pos.at)
    }

    /// Reads the next key and skips its value.
    fn skip_record(&self, pos: &mut Pos) -> Result<bool, FormatError> {
        let Some(extra) = self.next_key(pos)? else {
            return Ok(false);
        };
        let mut cur = self.cursor(pos);
        skip_value(&mut cur, self.typ, extra, self.hash_len)?;
        pos.at = cur.at;
        Ok(true)
    }

    /// The key of the first record.
    pub(crate) fn first_key(&self) -> Result<Vec<u8>, FormatError> {
        let mut pos = self.start();
        self.next_key(&mut pos)?;
        Ok(pos.key)
    }

    /// The position of the first record whose key is not less than `want`.
    pub(crate) fn seek(&self, want: &[u8]) -> Result<Pos, FormatError> {
        // The last restart point whose key is not above `want`.
        let (mut lo, mut hi) = (0, self.restart_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.restart_key(mid)?.as_slice() <= want {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut pos = self.start();
        if lo > 0 {
            pos.at = self.restart(lo - 1);
        }
        let mut before = pos.clone();
        loop {
            before.at = pos.at;
            before.key.clear();
            before.key.extend_from_slice(&pos.key);
            if !self.skip_record(&mut pos)? || pos.key.as_slice().cmp(want) != Ordering::Less {
                return Ok(before);
            }
        }
    }
}

/// A position inside a block: the offset of the next record and the key of
/// the record before it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pos {
    pub at: usize,
    pub key: Vec<u8>,
}

/// Skips the value of a record of block type `typ`.
fn skip_value(
    cur: &mut Cursor<'_>,
    typ: u8,
    extra: u8,
    hash_len: usize,
) -> Result<(), FormatError> {
    match typ {
        BLOCK_REF => {
            cur.varint()?;
            match extra {
                0 => {}
                1 => {
                    cur.bytes(hash_len)?;
                }
                2 => {
                    cur.bytes(2 * hash_len)?;
                }
                3 => {
                    cur.string()?;
                }
                _ => return Err(FormatError::Corrupt("unknown reference value type")),
            }
        }
        BLOCK_LOG => match extra {
            0 => {}
            1 => {
                cur.bytes(2 * hash_len)?;
                cur.string()?;
                cur.string()?;
                cur.varint()?;
                cur.bytes(2)?;
                cur.string()?;
            }
            _ => return Err(FormatError::Corrupt("unknown log record type")),
        },
        BLOCK_OBJ => {
            let count = if extra == 0 {
                cur.varint()?
            } else {
                u64::from(extra)
            };
            for _ in 0..count {
                cur.varint()?;
            }
        }
        BLOCK_INDEX => {
            cur.varint()?;
        }
        _ => return Err(FormatError::Corrupt("unknown block type")),
    }
    Ok(())
}
