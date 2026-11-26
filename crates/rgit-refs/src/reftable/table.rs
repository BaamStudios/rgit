// SPDX-License-Identifier: MIT
//! Reading one reftable file: header, footer, sections and the records in
//! them, with lookups through the index blocks where a table has them.

use std::sync::Arc;

use rgit_hash::HashKind;

use super::FormatError;
use super::block::{BLOCK_INDEX, BLOCK_LOG, BLOCK_OBJ, BLOCK_REF, Block, Pos};
use super::record::{IndexRecord, LogRecord, ObjRecord, RefRecord};

/// The magic bytes at the start of the header and of the footer.
pub(crate) const MAGIC: &[u8; 4] = b"REFT";

/// Size of the file header of format `version`.
pub(crate) fn header_size(version: u8) -> usize {
    if version == 2 { 28 } else { 24 }
}

/// Size of the footer of format `version`.
pub(crate) fn footer_size(version: u8) -> usize {
    if version == 2 { 72 } else { 68 }
}

/// One block as listed by [`Table::blocks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    /// Block type (`r`, `g`, `o` or `i`).
    pub typ: u8,
    /// Position in the file (0 for the first block).
    pub offset: u64,
    /// The length stored in the block header.
    pub len: usize,
    /// Number of restart points.
    pub restarts: usize,
}

/// A reftable file held in memory.
#[derive(Debug)]
pub struct Table {
    name: String,
    data: Vec<u8>,
    version: u8,
    block_size: usize,
    min_update_index: u64,
    max_update_index: u64,
    kind: HashKind,
    /// File length without the footer.
    size: usize,
    ref_index_pos: usize,
    obj_pos: usize,
    obj_id_len: usize,
    obj_index_pos: usize,
    log_pos: usize,
    log_index_pos: usize,
    /// Type of the first block, 0 for an empty table.
    first_type: u8,
}

struct Section {
    start: usize,
    end: usize,
    index: usize,
}

fn be64(data: &[u8], at: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&data[at..at + 8]);
    u64::from_be_bytes(raw)
}

impl Table {
    /// Parses the file `data`; `name` is its file name in the stack.
    pub fn parse(name: impl Into<String>, data: Vec<u8>) -> Result<Table, FormatError> {
        if data.len() < 8 || &data[..4] != MAGIC {
            return Err(FormatError::Corrupt("not a reftable file"));
        }
        let version = data[4];
        if version != 1 && version != 2 {
            return Err(FormatError::Corrupt("unknown reftable version"));
        }
        let (hs, fs) = (header_size(version), footer_size(version));
        if data.len() < hs + fs {
            return Err(FormatError::Truncated);
        }
        let size = data.len() - fs;
        let footer = &data[size..];
        if footer[..hs] != data[..hs] {
            return Err(FormatError::Corrupt("footer does not repeat the header"));
        }
        let crc = u32::from_be_bytes([
            footer[fs - 4],
            footer[fs - 3],
            footer[fs - 2],
            footer[fs - 1],
        ]);
        if crc32fast::hash(&footer[..fs - 4]) != crc {
            return Err(FormatError::Corrupt("footer checksum mismatch"));
        }
        let kind = if version == 2 {
            let id = u32::from_be_bytes([data[24], data[25], data[26], data[27]]);
            HashKind::from_format_id(id).ok_or(FormatError::Corrupt("unknown hash function"))?
        } else {
            HashKind::Sha1
        };
        let block_size = u32::from_be_bytes([0, data[5], data[6], data[7]]) as usize;
        let pos = |at: usize| -> Result<usize, FormatError> {
            usize::try_from(be64(footer, hs + at))
                .ok()
                .filter(|&p| p <= size)
                .ok_or(FormatError::Corrupt("section position outside of file"))
        };
        let obj = be64(footer, hs + 8);
        let obj_pos = usize::try_from(obj >> 5)
            .ok()
            .filter(|&p| p <= size)
            .ok_or(FormatError::Corrupt("section position outside of file"))?;
        let first_type = if size > hs { data[hs] } else { 0 };
        Ok(Table {
            name: name.into(),
            version,
            block_size,
            min_update_index: be64(&data, 8),
            max_update_index: be64(&data, 16),
            kind,
            size,
            ref_index_pos: pos(0)?,
            obj_pos,
            obj_id_len: (obj & 0x1f) as usize,
            obj_index_pos: pos(16)?,
            log_pos: pos(24)?,
            log_index_pos: pos(32)?,
            first_type,
            data,
        })
    }

    /// The file name in the stack.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Format version (1 or 2).
    pub fn version(&self) -> u8 {
        self.version
    }

    /// The block size of the header (0: blocks are not aligned).
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// The smallest update index of the table.
    pub fn min_update_index(&self) -> u64 {
        self.min_update_index
    }

    /// The largest update index of the table.
    pub fn max_update_index(&self) -> u64 {
        self.max_update_index
    }

    /// The hash function of the object ids.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// The length of the file.
    pub fn file_len(&self) -> u64 {
        self.data.len() as u64
    }

    /// The abbreviation length of the object blocks (0 without them).
    pub fn obj_id_len(&self) -> usize {
        self.obj_id_len
    }

    /// The raw bytes of the file.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    fn first_nonzero(&self, candidates: &[usize]) -> usize {
        candidates
            .iter()
            .copied()
            .find(|&p| p != 0)
            .unwrap_or(self.size)
    }

    fn section(&self, typ: u8) -> Option<Section> {
        match typ {
            BLOCK_REF if self.first_type == BLOCK_REF => Some(Section {
                start: 0,
                end: self.first_nonzero(&[self.ref_index_pos, self.obj_pos, self.log_pos]),
                index: self.ref_index_pos,
            }),
            BLOCK_OBJ if self.obj_pos != 0 => Some(Section {
                start: self.obj_pos,
                end: self.first_nonzero(&[self.obj_index_pos, self.log_pos]),
                index: self.obj_index_pos,
            }),
            BLOCK_LOG if self.log_pos != 0 || self.first_type == BLOCK_LOG => Some(Section {
                start: if self.log_pos != 0 { self.log_pos } else { 0 },
                end: self.first_nonzero(&[self.log_index_pos]),
                index: self.log_index_pos,
            }),
            _ => None,
        }
    }

    fn header_off(&self, off: usize) -> usize {
        if off == 0 {
            header_size(self.version)
        } else {
            0
        }
    }

    /// The type of the block at `off`, 0 when there is none.
    fn type_at(&self, off: usize) -> u8 {
        let at = off + self.header_off(off);
        if at < self.size { self.data[at] } else { 0 }
    }

    fn block_at(&self, off: usize) -> Result<Block, FormatError> {
        Block::parse(
            &self.data,
            off,
            self.header_off(off),
            self.size,
            self.kind.byte_len(),
        )
    }

    /// Where the block after `block` (which starts at `off`) begins.
    fn next_block_off(&self, off: usize, block: &Block) -> usize {
        let unpadded = off + self.header_off(off) + block.stored;
        if block.typ == BLOCK_LOG || self.block_size == 0 || block.len >= self.block_size {
            return unpadded;
        }
        // Aligned blocks are padded with NUL bytes; the last block of a
        // section may be followed directly by an unaligned one.
        if unpadded < self.size && self.data[unpadded] != 0 {
            unpadded
        } else {
            off + self.block_size
        }
    }

    /// Lists every block of the file in file order.
    pub fn blocks(&self) -> Result<Vec<BlockInfo>, FormatError> {
        let mut out = Vec::new();
        let mut off = 0;
        while off < self.size && self.type_at(off) != 0 {
            let block = self.block_at(off)?;
            out.push(BlockInfo {
                typ: block.typ,
                offset: off as u64,
                len: block.len,
                restarts: block.restart_count(),
            });
            off = self.next_block_off(off, &block);
        }
        Ok(out)
    }

    fn iter_section(self: &Arc<Self>, typ: u8) -> SectionIter {
        match self.section(typ) {
            Some(s) => SectionIter::at(self, typ, s.start, s.end),
            None => SectionIter::empty(self, typ),
        }
    }

    /// Finds the block that may hold `want` by walking the index levels
    /// from the block at `index`.
    fn lookup_index(&self, index: usize, want: &[u8]) -> Result<Option<usize>, FormatError> {
        let mut level = index;
        loop {
            let mut found = None;
            let mut off = level;
            while off < self.size && self.type_at(off) == BLOCK_INDEX {
                let block = self.block_at(off)?;
                let mut pos = block.seek(want)?;
                if block.next_key(&mut pos)?.is_some() {
                    let mut cur = block.cursor(&pos);
                    let rec = IndexRecord::decode_value(&mut cur, &pos.key)?;
                    found = usize::try_from(rec.offset).ok();
                    break;
                }
                off = self.next_block_off(off, &block);
            }
            match found {
                None => return Ok(None),
                Some(target) if self.type_at(target) == BLOCK_INDEX => {
                    if target >= level {
                        return Err(FormatError::Corrupt("index block refers forward"));
                    }
                    level = target;
                }
                Some(target) => return Ok(Some(target)),
            }
        }
    }

    fn seek_section(self: &Arc<Self>, typ: u8, want: &[u8]) -> Result<SectionIter, FormatError> {
        let Some(section) = self.section(typ) else {
            return Ok(SectionIter::empty(self, typ));
        };
        let mut off = section.start;
        if section.index != 0 {
            match self.lookup_index(section.index, want)? {
                Some(target) if self.type_at(target) == typ => off = target,
                Some(_) => return Err(FormatError::Corrupt("index refers to a foreign block")),
                None => return Ok(SectionIter::empty(self, typ)),
            }
        } else {
            // Few blocks: step forward while the next one starts at or
            // before the wanted key.
            let mut block = self.block_at(off)?;
            loop {
                let next = self.next_block_off(off, &block);
                if next >= section.end || self.type_at(next) != typ {
                    break;
                }
                let candidate = self.block_at(next)?;
                if candidate.first_key()?.as_slice() > want {
                    break;
                }
                off = next;
                block = candidate;
            }
        }
        let mut it = SectionIter::at(self, typ, off, section.end);
        it.load()?;
        if let Some(block) = &it.block {
            it.pos = block.seek(want)?;
        }
        Ok(it)
    }

    /// All reference records, sorted by name.
    pub fn refs(self: &Arc<Self>) -> RefIter {
        RefIter(self.iter_section(BLOCK_REF))
    }

    /// Reference records from the first name not less than `name`.
    pub fn seek_ref(self: &Arc<Self>, name: &[u8]) -> Result<RefIter, FormatError> {
        Ok(RefIter(self.seek_section(BLOCK_REF, name)?))
    }

    /// All log records, sorted by key.
    pub fn logs(self: &Arc<Self>) -> LogIter {
        LogIter(self.iter_section(BLOCK_LOG))
    }

    /// Log records from the first key not less than `key`.
    pub fn seek_log(self: &Arc<Self>, key: &[u8]) -> Result<LogIter, FormatError> {
        Ok(LogIter(self.seek_section(BLOCK_LOG, key)?))
    }

    /// All object records.
    pub fn objs(self: &Arc<Self>) -> Result<Vec<ObjRecord>, FormatError> {
        let mut it = self.iter_section(BLOCK_OBJ);
        let mut out = Vec::new();
        while let Some(extra) = it.next_key()? {
            let block = it.block.as_ref().ok_or(FormatError::Truncated)?;
            let mut cur = block.cursor(&it.pos);
            out.push(ObjRecord::decode_value(&mut cur, &it.pos.key, extra)?);
            it.pos.at = cur.at;
        }
        Ok(out)
    }

    /// Whether the table has reference records.
    pub fn has_refs(&self) -> bool {
        self.first_type == BLOCK_REF
    }

    /// Whether the table has log records.
    pub fn has_logs(&self) -> bool {
        self.log_pos != 0 || self.first_type == BLOCK_LOG
    }
}

/// Walks the records of one section across its blocks.
pub(crate) struct SectionIter {
    table: Arc<Table>,
    typ: u8,
    /// The block to load next.
    next_off: usize,
    end: usize,
    block: Option<Block>,
    pos: Pos,
}

impl SectionIter {
    fn at(table: &Arc<Table>, typ: u8, off: usize, end: usize) -> Self {
        SectionIter {
            table: Arc::clone(table),
            typ,
            next_off: off,
            end,
            block: None,
            pos: Pos::default(),
        }
    }

    fn empty(table: &Arc<Table>, typ: u8) -> Self {
        Self::at(table, typ, 0, 0)
    }

    /// Loads the block at `next_off`; `false` at the end of the section.
    fn load(&mut self) -> Result<bool, FormatError> {
        self.block = None;
        let off = self.next_off;
        if off >= self.end || self.table.type_at(off) != self.typ {
            self.next_off = self.end;
            return Ok(false);
        }
        let block = self.table.block_at(off)?;
        self.next_off = self.table.next_block_off(off, &block);
        self.pos = block.start();
        self.block = Some(block);
        Ok(true)
    }

    /// Advances to the next record: its key is in `pos.key`, its value at
    /// `pos.at`; the caller moves `pos.at` behind the value.
    fn next_key(&mut self) -> Result<Option<u8>, FormatError> {
        loop {
            if let Some(block) = &self.block {
                if let Some(extra) = block.next_key(&mut self.pos)? {
                    return Ok(Some(extra));
                }
            }
            if !self.load()? {
                return Ok(None);
            }
        }
    }
}

/// Iterator over reference records of one table.
pub struct RefIter(SectionIter);

impl RefIter {
    /// The next record.
    pub fn next_record(&mut self) -> Result<Option<RefRecord>, FormatError> {
        let it = &mut self.0;
        let Some(extra) = it.next_key()? else {
            return Ok(None);
        };
        let block = it.block.as_ref().ok_or(FormatError::Truncated)?;
        let mut cur = block.cursor(&it.pos);
        let rec = RefRecord::decode_value(
            &mut cur,
            &it.pos.key,
            extra,
            it.table.kind,
            it.table.min_update_index,
        )?;
        it.pos.at = cur.at;
        Ok(Some(rec))
    }
}

/// Iterator over log records of one table.
pub struct LogIter(SectionIter);

impl LogIter {
    /// The next record.
    pub fn next_record(&mut self) -> Result<Option<LogRecord>, FormatError> {
        let it = &mut self.0;
        let Some(extra) = it.next_key()? else {
            return Ok(None);
        };
        let block = it.block.as_ref().ok_or(FormatError::Truncated)?;
        let mut cur = block.cursor(&it.pos);
        let rec = LogRecord::decode_value(&mut cur, &it.pos.key, extra, it.table.kind)?;
        it.pos.at = cur.at;
        Ok(Some(rec))
    }
}
