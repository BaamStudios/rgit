// SPDX-License-Identifier: MIT

//! Serializing the index file.

use rgit_hash::{HashKind, Hasher};

use crate::entry::{Entry, bits};
use crate::read::SIGNATURE;
use crate::{Index, varint};

/// Options for [`Index::to_bytes`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WriteOptions {
    /// Write a null trailer instead of the checksum (`index.skipHash`).
    pub skip_hash: bool,
    /// The version of an index that has none yet (a new index, see
    /// [`Index::new`]): 4, or 2 and 3 (and 0) for "2, or 3 when an entry
    /// needs extended flags" (`index.version`, `GIT_INDEX_VERSION`,
    /// `feature.manyFiles`).
    pub default_version: u32,
    /// Write the End Of Index Entries extension (`EOIE`,
    /// `index.recordEndOfIndexEntries`).
    pub end_of_entries: bool,
    /// Write the Index Entry Offset Table (`IEOT`,
    /// `index.recordOffsetTable`) for this many loading threads, see
    /// [`OffsetTable`].
    pub offset_table: OffsetTable,
    /// Split index settings (used by [`Index::write_file`] and
    /// [`Index::write_locked`]; [`Index::to_bytes`] always writes a whole
    /// index).
    pub split: crate::SplitOptions,
}

/// How many blocks the Index Entry Offset Table splits the entries into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OffsetTable {
    /// No table.
    #[default]
    None,
    /// As many blocks as loading threads would be started for the index
    /// (`index.threads` true or 0): one per 10000 entries, at most one
    /// less than the number of CPUs.
    Auto,
    /// A fixed number of threads (`index.threads=<n>`).
    Threads(usize),
}

/// Entries per loading thread when the number of threads is automatic.
const ENTRIES_PER_THREAD: usize = 10000;

impl OffsetTable {
    /// The number of entries per block for `entries` entries, or `None`
    /// when no table is written (fewer than two blocks).
    fn block_size(self, entries: usize) -> Option<usize> {
        let threads = match self {
            OffsetTable::None => return None,
            OffsetTable::Auto => {
                let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
                (entries / ENTRIES_PER_THREAD).min(cpus.saturating_sub(1).max(1))
            }
            OffsetTable::Threads(n) => n.min(entries),
        };
        (threads > 1).then(|| entries.div_ceil(threads))
    }
}

/// What a file written by [`write`] holds.
pub(crate) enum Content<'a> {
    /// The whole index.
    Whole,
    /// A shared index: all entries, no extensions.
    Shared,
    /// A split index: these entries (`true`: written without a name) and
    /// the `link` data.
    Split(Vec<(&'a Entry, bool)>, Vec<u8>),
}

pub(crate) fn serialize(index: &Index, opts: &WriteOptions) -> Vec<u8> {
    write(index, opts, Content::Whole)
}

pub(crate) fn write(index: &Index, opts: &WriteOptions, content: Content<'_>) -> Vec<u8> {
    let kind = index.kind;
    let version = index.write_version(opts);
    let is_shared = matches!(content, Content::Shared);
    let (entries, link): (Vec<(&Entry, bool)>, Option<Vec<u8>>) = match content {
        Content::Whole | Content::Shared => {
            (index.entries.iter().map(|e| (e, false)).collect(), None)
        }
        Content::Split(own, link) => (own, Some(link)),
    };
    let mut out = Vec::with_capacity(12 + entries.len() * 80);
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(entries.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );

    // The offset table only for a whole index.
    let block_size = if link.is_none() && !is_shared {
        opts.offset_table.block_size(entries.len())
    } else {
        None
    };
    // (offset, count) per block of the offset table.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut prev_path: &[u8] = &[];
    for (i, &(e, nameless)) in entries.iter().enumerate() {
        let name: &[u8] = if nameless { b"" } else { &e.path };
        let start = out.len();
        let block_start = block_size.is_some_and(|n| i % n == 0);
        if block_start {
            blocks.push((start, 0));
        }
        if let Some(b) = blocks.last_mut() {
            b.1 += 1;
        }
        let s = &e.stat;
        for v in [
            s.ctime.secs,
            s.ctime.nsecs,
            s.mtime.secs,
            s.mtime.nsecs,
            s.dev,
            s.ino,
            e.mode.0,
            s.uid,
            s.gid,
            s.size,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(e.id.as_bytes());
        let extended = version >= 3 && e.flags.needs_extended();
        let mut flags = u16::from(e.stage.get()) << bits::STAGE_SHIFT;
        flags |= u16::try_from(name.len())
            .unwrap_or(bits::NAME_MASK)
            .min(bits::NAME_MASK);
        if e.flags.assume_valid {
            flags |= bits::ASSUME_VALID;
        }
        if extended {
            flags |= bits::EXTENDED;
        }
        out.extend_from_slice(&flags.to_be_bytes());
        if extended {
            out.extend_from_slice(&e.flags.extended_bits().to_be_bytes());
        }
        if version == 4 {
            // A block of the offset table starts with a whole path, so
            // that it can be parsed on its own: all of the previous path
            // is stripped.
            let common = if block_start {
                0
            } else {
                prev_path
                    .iter()
                    .zip(name)
                    .take_while(|(a, b)| a == b)
                    .count()
            };
            varint::encode(prev_path.len() - common, &mut out);
            out.extend_from_slice(&name[common..]);
            out.push(0);
            prev_path = name;
        } else {
            out.extend_from_slice(name);
            let len = out.len() - start;
            let padded = (len + 8) & !7;
            out.resize(start + padded, 0);
        }
    }
    let entries_end = out.len();

    // (signature, size) of every extension, for the EOIE hash.
    let mut written: Vec<([u8; 4], u32)> = Vec::new();
    if !blocks.is_empty() {
        let mut data = Vec::with_capacity(4 + blocks.len() * 8);
        data.extend_from_slice(&1u32.to_be_bytes());
        for (offset, count) in &blocks {
            data.extend_from_slice(&be32(*offset));
            data.extend_from_slice(&be32(*count));
        }
        push_extension(&mut out, &mut written, b"IEOT", &data);
    }
    if let Some(link) = &link {
        push_extension(&mut out, &mut written, b"link", link);
    }
    if is_shared {
        trailer(&mut out, kind, false);
        return out;
    }
    // Like git, no cache tree while an entry has a null id
    // (GIT_ALLOW_NULL_SHA1): the trees could not be written.
    let null_entry = || index.entries.iter().any(|e| e.id.is_null());
    if let Some(tree) = index.tree.as_ref().filter(|_| !null_entry()) {
        let mut data = Vec::new();
        tree.write(&mut data);
        push_extension(&mut out, &mut written, b"TREE", &data);
    }
    if let Some(records) = &index.resolve_undo {
        let mut data = Vec::new();
        crate::index::write_resolve_undo(records, &mut data);
        push_extension(&mut out, &mut written, b"REUC", &data);
    }
    if let Some(uc) = &index.untracked {
        let mut data = Vec::new();
        uc.write(&mut data);
        push_extension(&mut out, &mut written, b"UNTR", &data);
    }
    if opts.end_of_entries {
        let mut hasher = Hasher::new(kind);
        for (sig, size) in &written {
            hasher.update(sig);
            hasher.update(&size.to_be_bytes());
        }
        let mut data = Vec::with_capacity(4 + kind.byte_len());
        data.extend_from_slice(&be32(entries_end));
        data.extend_from_slice(finish(hasher).as_bytes());
        push_extension(&mut out, &mut Vec::new(), b"EOIE", &data);
    }

    trailer(&mut out, kind, opts.skip_hash);
    out
}

fn be32(n: usize) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes()
}

fn finish(hasher: Hasher) -> rgit_hash::ObjectId {
    // A collision attack on an index checksum only means a mismatch.
    hasher.finalize().unwrap_or_else(|e| e.id)
}

/// Appends the trailing checksum (or a null one).
pub(crate) fn trailer(out: &mut Vec<u8>, kind: HashKind, skip_hash: bool) {
    if skip_hash {
        out.resize(out.len() + kind.byte_len(), 0);
    } else {
        let mut hasher = Hasher::new(kind);
        hasher.update(out);
        let id = finish(hasher);
        out.extend_from_slice(id.as_bytes());
    }
}

fn push_extension(
    out: &mut Vec<u8>,
    written: &mut Vec<([u8; 4], u32)>,
    sig: &[u8; 4],
    data: &[u8],
) {
    let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(sig);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(data);
    written.push((*sig, size));
}
