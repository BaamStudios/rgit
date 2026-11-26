// SPDX-License-Identifier: MIT
//! The reftable reference storage format
//! (`Documentation/technical/reftable.adoc`).
//!
//! * [`record`], [`block`], [`table`]: reading one table file.
//! * [`writer`]: writing a table; the same records and options give the
//!   bytes the reference git writes (`tests/reftable_format.rs`).
//! * [`merged`]: the view over several tables, newest record first.
//! * [`stack`]: `tables.list`, its lock, adding tables and compaction.

pub mod backend;
pub mod block;
pub mod merged;
pub mod record;
pub mod stack;
pub mod table;
pub mod writer;

pub use record::{IndexRecord, LogRecord, LogUpdate, ObjRecord, RefRecord, RefValue};
pub use table::{BlockInfo, Table};

/// A table file that cannot be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// The data ends inside a header, block or record.
    #[error("truncated reftable data")]
    Truncated,
    /// The data contradicts the format.
    #[error("corrupt reftable: {0}")]
    Corrupt(&'static str),
}
