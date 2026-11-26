// SPDX-License-Identifier: MIT
//! The merged view of a stack of tables: for every key the record of the
//! newest table wins, and a deletion hides the records of older tables.

use std::sync::Arc;

use super::FormatError;
use super::record::{LogRecord, RefRecord, log_key};
use super::table::{LogIter, RefIter, Table};

/// A sorted stream of records from one table.
pub(crate) trait Source {
    type Record;
    fn next_record(&mut self) -> Result<Option<Self::Record>, FormatError>;
    fn key(record: &Self::Record) -> Vec<u8>;
}

impl Source for RefIter {
    type Record = RefRecord;
    fn next_record(&mut self) -> Result<Option<RefRecord>, FormatError> {
        RefIter::next_record(self)
    }
    fn key(record: &RefRecord) -> Vec<u8> {
        record.name.clone()
    }
}

impl Source for LogIter {
    type Record = LogRecord;
    fn next_record(&mut self) -> Result<Option<LogRecord>, FormatError> {
        LogIter::next_record(self)
    }
    fn key(record: &LogRecord) -> Vec<u8> {
        record.key()
    }
}

/// Merges the streams of several tables, oldest table first.
pub(crate) struct Merged<S: Source> {
    sources: Vec<S>,
    /// The next record of each source with its key.
    heads: Vec<Option<(Vec<u8>, S::Record)>>,
    primed: bool,
}

impl<S: Source> Merged<S> {
    pub fn new(sources: Vec<S>) -> Self {
        let heads = sources.iter().map(|_| None).collect();
        Merged {
            sources,
            heads,
            primed: false,
        }
    }

    fn advance(&mut self, i: usize) -> Result<(), FormatError> {
        self.heads[i] = self.sources[i].next_record()?.map(|r| (S::key(&r), r));
        Ok(())
    }

    /// The next record in key order; of records with the same key only
    /// the one from the newest table (deletions included).
    pub fn next_record(&mut self) -> Result<Option<S::Record>, FormatError> {
        if !self.primed {
            self.primed = true;
            for i in 0..self.sources.len() {
                self.advance(i)?;
            }
        }
        let mut best: Option<usize> = None;
        for (i, head) in self.heads.iter().enumerate() {
            let Some((key, _)) = head else { continue };
            match best {
                // Later sources are newer: they win ties.
                Some(b) if self.heads[b].as_ref().is_some_and(|(k, _)| k < key) => {}
                _ => best = Some(i),
            }
        }
        let Some(best) = best else { return Ok(None) };
        let (key, record) = self.heads[best].take().expect("best head exists");
        self.advance(best)?;
        for i in 0..best {
            if self.heads[i].as_ref().is_some_and(|(k, _)| *k == key) {
                self.advance(i)?;
            }
        }
        Ok(Some(record))
    }
}

/// Merged reference records of `tables` from the first name not less than
/// `from`, deletions included.
pub(crate) fn merged_refs(
    tables: &[Arc<Table>],
    from: &[u8],
) -> Result<Merged<RefIter>, FormatError> {
    let mut sources = Vec::with_capacity(tables.len());
    for t in tables {
        if t.has_refs() {
            sources.push(if from.is_empty() {
                t.refs()
            } else {
                t.seek_ref(from)?
            });
        }
    }
    Ok(Merged::new(sources))
}

/// Merged log records of `tables` from the first key not less than `from`,
/// deletions included.
pub(crate) fn merged_logs(
    tables: &[Arc<Table>],
    from: &[u8],
) -> Result<Merged<LogIter>, FormatError> {
    let mut sources = Vec::with_capacity(tables.len());
    for t in tables {
        if t.has_logs() {
            sources.push(if from.is_empty() {
                t.logs()
            } else {
                t.seek_log(from)?
            });
        }
    }
    Ok(Merged::new(sources))
}

/// The current record of `name`; `None` when it does not exist or was
/// deleted.
pub fn read_ref(tables: &[Arc<Table>], name: &[u8]) -> Result<Option<RefRecord>, FormatError> {
    for t in tables.iter().rev() {
        if !t.has_refs() {
            continue;
        }
        if let Some(rec) = t.seek_ref(name)?.next_record()? {
            if rec.name == name {
                return Ok((!rec.is_deletion()).then_some(rec));
            }
        }
    }
    Ok(None)
}

/// The live references whose name starts with `prefix`, sorted by name.
pub fn refs_with_prefix(
    tables: &[Arc<Table>],
    prefix: &[u8],
) -> Result<Vec<RefRecord>, FormatError> {
    let mut merged = merged_refs(tables, prefix)?;
    let mut out = Vec::new();
    while let Some(rec) = merged.next_record()? {
        if !rec.name.starts_with(prefix) {
            break;
        }
        if !rec.is_deletion() {
            out.push(rec);
        }
    }
    Ok(out)
}

/// The log records of `name`, newest first, without deleted ones. The
/// marker of an empty log (null ids on both sides) is included.
pub fn logs_of(tables: &[Arc<Table>], name: &[u8]) -> Result<Vec<LogRecord>, FormatError> {
    let mut merged = merged_logs(tables, &log_key(name, u64::MAX))?;
    let mut out = Vec::new();
    while let Some(rec) = merged.next_record()? {
        if rec.refname != name {
            break;
        }
        if rec.update.is_some() {
            out.push(rec);
        }
    }
    Ok(out)
}

/// The names that have log records, sorted.
pub fn log_names(tables: &[Arc<Table>]) -> Result<Vec<Vec<u8>>, FormatError> {
    let mut merged = merged_logs(tables, b"")?;
    let mut out: Vec<Vec<u8>> = Vec::new();
    while let Some(rec) = merged.next_record()? {
        if rec.update.is_some() && out.last() != Some(&rec.refname) {
            out.push(rec.refname);
        }
    }
    Ok(out)
}
