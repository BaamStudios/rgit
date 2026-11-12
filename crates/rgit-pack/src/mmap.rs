// SPDX-License-Identifier: MIT

//! Read-only file contents, memory mapped where possible.
//!
//! This module holds the crate's only `unsafe` code (ADR 0003).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// The bytes of a file: memory mapped, or read into memory when mapping is
/// not possible (empty files, file systems without mmap support) or when the
/// data comes from memory in the first place.
pub(crate) enum FileData {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl FileData {
    /// Map `path` read-only, falling back to reading it.
    pub(crate) fn open(path: &Path) -> io::Result<FileData> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if len > 0 {
            // SAFETY: the mapping is read-only and private to this process.
            // Pack and index files are immutable once written: git and rgit
            // write them under a temporary name and rename them into place,
            // so their content never changes while mapped. A file truncated
            // by another process would raise SIGBUS on access, the same
            // assumption every git implementation that maps packs makes.
            // All offsets taken from the mapped data are bounds-checked
            // against `len()` before they are used.
            if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
                return Ok(FileData::Mapped(map));
            }
        }
        let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        file.read_to_end(&mut buf)?;
        Ok(FileData::Owned(buf))
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            FileData::Mapped(m) => m,
            FileData::Owned(v) => v,
        }
    }
}
