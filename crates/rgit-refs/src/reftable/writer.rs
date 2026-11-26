// SPDX-License-Identifier: MIT
//! Writing one reftable file.
//!
//! The layout choices that the format leaves to the writer follow what the
//! reference git writes, so that the same records and options give the
//! same bytes (see `tests/reftable_format.rs`):
//!
//! * a block is closed when the next record would exceed the block size;
//!   it is padded to the block size only when another reference, object or
//!   index block follows;
//! * a section with more than three blocks gets an index, and an index
//!   level with more than three blocks gets another level above it;
//! * object blocks are written together with the reference index;
//! * log blocks are deflated with the best compression level and never
//!   padded.

use std::collections::BTreeMap;

use rgit_hash::HashKind;

use super::block::{BLOCK_INDEX, BLOCK_LOG, BLOCK_OBJ, BLOCK_REF, BlockWriter};
use super::record::{IndexRecord, LogRecord, ObjRecord, RefRecord, RefValue};
use super::table::{MAGIC, header_size};

/// The default block size.
pub const DEFAULT_BLOCK_SIZE: u32 = 4096;
/// The largest block size the format can express.
pub const MAX_BLOCK_SIZE: u32 = 0xff_ffff;
/// The default number of records between restart points.
pub const DEFAULT_RESTART_INTERVAL: u16 = 16;
/// A section with more blocks than this gets an index.
const INDEX_THRESHOLD: usize = 3;

/// How to write tables (`reftable.*` configuration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteOptions {
    /// `reftable.blockSize`.
    pub block_size: u32,
    /// `reftable.restartInterval`.
    pub restart_interval: u16,
    /// `reftable.indexObjects`.
    pub index_objects: bool,
    /// The object format of the repository.
    pub hash: HashKind,
}

impl WriteOptions {
    /// git's defaults for a repository with object format `hash`.
    pub fn new(hash: HashKind) -> Self {
        WriteOptions {
            block_size: DEFAULT_BLOCK_SIZE,
            restart_interval: DEFAULT_RESTART_INTERVAL,
            index_objects: true,
            hash,
        }
    }
}

/// Why a table could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// A record does not fit into a block of the configured size.
    #[error("entry too large")]
    EntryTooLarge,
    /// Records were not added in ascending key order, or a reference
    /// after a log record.
    #[error("records out of order")]
    OutOfOrder,
    /// The update index of a record is outside of the table's range.
    #[error("update index outside of the table's range")]
    UpdateIndex,
    /// Deflating a log block failed.
    #[error("unable to compress log block")]
    Compress,
}

/// Builds a table in memory.
pub struct Writer {
    opts: WriteOptions,
    min_update_index: u64,
    max_update_index: u64,
    out: Vec<u8>,
    block: Option<BlockWriter>,
    /// File position of the block being filled.
    block_start: u64,
    /// NUL bytes owed before the next aligned block.
    pending_padding: usize,
    /// One record per finished block of the current section or level.
    index: Vec<IndexRecord>,
    /// Object id -> positions of the reference blocks that mention it.
    objects: BTreeMap<Vec<u8>, Vec<u64>>,
    last_ref: Option<Vec<u8>>,
    last_log: Option<Vec<u8>>,
    in_logs: bool,
    ref_blocks: usize,
    ref_index_pos: u64,
    obj_pos: u64,
    obj_id_len: u64,
    obj_index_pos: u64,
    log_pos: u64,
    log_index_pos: u64,
    scratch: Vec<u8>,
}

impl Writer {
    /// Starts a table for the update indices `min..=max`.
    pub fn new(opts: WriteOptions, min_update_index: u64, max_update_index: u64) -> Self {
        let mut w = Writer {
            opts,
            min_update_index,
            max_update_index,
            out: Vec::new(),
            block: None,
            block_start: 0,
            pending_padding: 0,
            index: Vec::new(),
            objects: BTreeMap::new(),
            last_ref: None,
            last_log: None,
            in_logs: false,
            ref_blocks: 0,
            ref_index_pos: 0,
            obj_pos: 0,
            obj_id_len: 0,
            obj_index_pos: 0,
            log_pos: 0,
            log_index_pos: 0,
            scratch: Vec::new(),
        };
        w.out = w.header();
        w
    }

    fn version(&self) -> u8 {
        if self.opts.hash == HashKind::Sha1 {
            1
        } else {
            2
        }
    }

    fn header(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(28);
        h.extend_from_slice(MAGIC);
        h.push(self.version());
        let bs = self.opts.block_size;
        h.extend_from_slice(&[(bs >> 16) as u8, (bs >> 8) as u8, bs as u8]);
        h.extend_from_slice(&self.min_update_index.to_be_bytes());
        h.extend_from_slice(&self.max_update_index.to_be_bytes());
        if self.version() == 2 {
            h.extend_from_slice(&self.opts.hash.format_id().to_be_bytes());
        }
        h
    }

    /// The block being filled, started if necessary.
    fn block(&mut self, typ: u8) -> &mut BlockWriter {
        if self.block.is_none() {
            let first = self.out.len() == header_size(self.version()) && self.pending_padding == 0;
            let (start, header_off) = if first {
                (0, self.out.len())
            } else if typ == BLOCK_LOG {
                (self.out.len(), 0)
            } else {
                (self.out.len() + self.pending_padding, 0)
            };
            self.block_start = start as u64;
            self.block = Some(BlockWriter::new(
                typ,
                self.opts.block_size as usize,
                header_off,
                usize::from(self.opts.restart_interval),
            ));
        }
        self.block.as_mut().expect("block was just started")
    }

    /// Writes the block being filled, if it has records.
    fn flush_block(&mut self) -> Result<(), WriteError> {
        let Some(block) = self.block.take() else {
            return Ok(());
        };
        if block.entries() == 0 {
            return Ok(());
        }
        let typ = block.typ();
        let last_key = block.last_key().to_vec();
        let (bytes, len) = block.finish().map_err(|_| WriteError::Compress)?;
        if typ == BLOCK_LOG {
            self.pending_padding = 0;
        } else {
            let padded = self.out.len() + self.pending_padding;
            self.out.resize(padded, 0);
            self.pending_padding = (self.opts.block_size as usize).saturating_sub(len);
        }
        debug_assert!(self.block_start == 0 || self.block_start == self.out.len() as u64);
        self.out.extend_from_slice(&bytes);
        if typ == BLOCK_REF {
            self.ref_blocks += 1;
        }
        self.index.push(IndexRecord {
            last_key,
            offset: self.block_start,
        });
        Ok(())
    }

    /// Adds a record to the current block of type `typ`, starting a new
    /// block when the current one is full.
    fn add(&mut self, typ: u8, key: &[u8], extra: u8, value: &[u8]) -> Result<(), WriteError> {
        if self.block(typ).add(key, extra, value) {
            return Ok(());
        }
        self.flush_block()?;
        if self.block(typ).add(key, extra, value) {
            Ok(())
        } else {
            Err(WriteError::EntryTooLarge)
        }
    }

    /// Ends the current section: writes its last block and, above the
    /// threshold, its index. Returns the position of the index root.
    fn finish_section(&mut self) -> Result<u64, WriteError> {
        self.flush_block()?;
        let mut root = 0;
        while self.index.len() > INDEX_THRESHOLD {
            let level = std::mem::take(&mut self.index);
            let mut first = None;
            for rec in &level {
                let mut value = std::mem::take(&mut self.scratch);
                value.clear();
                rec.encode_value(&mut value);
                let result = self.add(BLOCK_INDEX, &rec.last_key, 0, &value);
                self.scratch = value;
                result?;
                if first.is_none() {
                    first = Some(self.block_start);
                }
            }
            self.flush_block()?;
            root = first.unwrap_or(0);
        }
        self.index.clear();
        Ok(root)
    }

    fn finish_refs(&mut self) -> Result<(), WriteError> {
        if self.in_logs {
            return Ok(());
        }
        self.in_logs = true;
        self.ref_index_pos = self.finish_section()?;
        if self.ref_index_pos != 0 && self.opts.index_objects && !self.objects.is_empty() {
            self.write_objects()?;
        }
        self.objects.clear();
        Ok(())
    }

    fn write_objects(&mut self) -> Result<(), WriteError> {
        let objects = std::mem::take(&mut self.objects);
        // The shortest length (at least two bytes) at which every object
        // id is distinct from its neighbours.
        let mut id_len = 2;
        let mut prev: Option<&Vec<u8>> = None;
        for id in objects.keys() {
            if let Some(p) = prev {
                let common = p.iter().zip(id).take_while(|(a, b)| a == b).count();
                id_len = id_len.max(common + 1);
            }
            prev = Some(id);
        }
        let mut first = None;
        for (id, offsets) in objects {
            let mut rec = ObjRecord {
                prefix: id[..id_len.min(id.len())].to_vec(),
                offsets,
            };
            let mut value = Vec::new();
            rec.encode_value(&mut value);
            if !self
                .block(BLOCK_OBJ)
                .add(&rec.prefix, rec.value_type(), &value)
            {
                self.flush_block()?;
                if !self
                    .block(BLOCK_OBJ)
                    .add(&rec.prefix, rec.value_type(), &value)
                {
                    // Too many positions for one block: store none, which
                    // tells readers to scan every reference block.
                    rec.offsets.clear();
                    value.clear();
                    rec.encode_value(&mut value);
                    if !self
                        .block(BLOCK_OBJ)
                        .add(&rec.prefix, rec.value_type(), &value)
                    {
                        return Err(WriteError::EntryTooLarge);
                    }
                }
            }
            if first.is_none() {
                first = Some(self.block_start);
            }
        }
        self.obj_pos = first.unwrap_or(0);
        self.obj_id_len = id_len as u64;
        self.obj_index_pos = self.finish_section()?;
        Ok(())
    }

    /// Adds a reference record. Records must come in ascending name order
    /// and before every log record.
    pub fn add_ref(&mut self, rec: &RefRecord) -> Result<(), WriteError> {
        if self.in_logs || self.last_ref.as_ref().is_some_and(|l| *l >= rec.name) {
            return Err(WriteError::OutOfOrder);
        }
        if rec.update_index > self.max_update_index {
            return Err(WriteError::UpdateIndex);
        }
        let mut value = std::mem::take(&mut self.scratch);
        value.clear();
        let encoded = rec.encode_value(&mut value, self.min_update_index);
        let result = match encoded {
            Some(()) => self.add(BLOCK_REF, &rec.name, rec.value_type(), &value),
            None => Err(WriteError::UpdateIndex),
        };
        self.scratch = value;
        result?;
        let at = self.block_start;
        let mut note = |id: &rgit_hash::ObjectId| {
            let offsets = self.objects.entry(id.as_bytes().to_vec()).or_default();
            if offsets.last() != Some(&at) {
                offsets.push(at);
            }
        };
        if self.opts.index_objects {
            match &rec.value {
                RefValue::Direct(id) => note(id),
                RefValue::Peeled(id, peeled) => {
                    note(id);
                    note(peeled);
                }
                RefValue::Deletion | RefValue::Symbolic(_) => {}
            }
        }
        self.last_ref = Some(rec.name.clone());
        Ok(())
    }

    /// Adds a log record. Records must come in ascending key order: by
    /// name, then newest update index first.
    pub fn add_log(&mut self, rec: &LogRecord) -> Result<(), WriteError> {
        let key = rec.key();
        if self.last_log.as_ref().is_some_and(|l| *l >= key) {
            return Err(WriteError::OutOfOrder);
        }
        self.finish_refs()?;
        let mut value = std::mem::take(&mut self.scratch);
        value.clear();
        rec.encode_value(&mut value);
        let result = self.add(BLOCK_LOG, &key, rec.value_type(), &value);
        self.scratch = value;
        result?;
        if self.last_log.is_none() {
            self.log_pos = self.block_start;
        }
        self.last_log = Some(key);
        Ok(())
    }

    /// Whether no record was added.
    pub fn is_empty(&self) -> bool {
        self.last_ref.is_none() && self.last_log.is_none()
    }

    /// Completes the table and returns the file's bytes.
    pub fn finish(mut self) -> Result<Vec<u8>, WriteError> {
        self.finish_refs()?;
        if self.last_log.is_some() {
            self.log_index_pos = self.finish_section()?;
        }
        let mut footer = self.header();
        footer.extend_from_slice(&self.ref_index_pos.to_be_bytes());
        footer.extend_from_slice(&((self.obj_pos << 5) | self.obj_id_len).to_be_bytes());
        footer.extend_from_slice(&self.obj_index_pos.to_be_bytes());
        footer.extend_from_slice(&self.log_pos.to_be_bytes());
        footer.extend_from_slice(&self.log_index_pos.to_be_bytes());
        let crc = crc32fast::hash(&footer);
        footer.extend_from_slice(&crc.to_be_bytes());
        self.out.extend_from_slice(&footer);
        Ok(self.out)
    }
}
