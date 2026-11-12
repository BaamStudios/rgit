// SPDX-License-Identifier: MIT

//! Thin pack completion (`--fix-thin`): append missing `REF_DELTA` bases
//! from the object database, then rewrite the header and the checksum.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use rgit_hash::{Hasher, ObjectId};

use super::resolve::{Ctx, Graph};
use super::{Base, Checker, Entry, Error, ObjectDb, Options, Progress, Slots};

/// The state thin completion works on.
pub(crate) struct Thin<'a> {
    /// The pack as received (without trailer).
    pub pack: &'a [u8],
    pub dest: &'a mut File,
    pub entries: &'a mut Vec<Entry>,
    pub slots: &'a mut Slots,
    pub graph: &'a mut Graph,
    /// Offset of the received trailer, where appending starts.
    pub entries_end: u64,
    pub version: u32,
}

pub(crate) struct Completed {
    pub appended: u64,
    pub checksum: ObjectId,
    pub pack_len: u64,
}

/// Encode an entry header for a whole object.
fn entry_header(ty: u8, mut size: u64) -> Vec<u8> {
    let mut out = vec![(ty << 4) | (size & 0x0f) as u8];
    size >>= 4;
    while size != 0 {
        *out.last_mut().expect("not empty") |= 0x80;
        out.push((size & 0x7f) as u8);
        size >>= 7;
    }
    out
}

pub(crate) fn complete(
    t: Thin<'_>,
    db: &dyn ObjectDb,
    opts: &Options,
    checker: &Checker<'_>,
    resolved: &mut u64,
    deltas: u64,
    progress: Option<&mut (dyn Progress + 'static)>,
) -> Result<Completed, Error> {
    let mut end = t.entries_end;
    let mut appended = 0u64;
    let mut tried: HashSet<ObjectId> = HashSet::new();
    let level = if opts.compression_level < 0 {
        Compression::default()
    } else {
        Compression::new(opts.compression_level.clamp(0, 9) as u32)
    };
    let mut progress = progress;
    t.dest.seek(SeekFrom::Start(end)).map_err(Error::Write)?;
    for (child, base) in t.graph.unclaimed_refs() {
        if *resolved == deltas {
            break;
        }
        if t.slots[child as usize].get().is_some() || !tried.insert(base) {
            continue;
        }
        let obj = match db.read(&base) {
            Ok(Some(o)) => o,
            Ok(None) => continue,
            Err(msg) => return Err(Error::Hook(msg)),
        };
        // Append the base as a whole object.
        let mut bytes = entry_header(obj.kind.pack_type(), obj.data.len() as u64);
        let data_offset = end + bytes.len() as u64;
        let mut z = ZlibEncoder::new(Vec::new(), level);
        z.write_all(&obj.data).map_err(Error::Write)?;
        bytes.extend_from_slice(&z.finish().map_err(Error::Write)?);
        t.dest.write_all(&bytes).map_err(Error::Write)?;
        let idx = t.entries.len() as u32;
        t.entries.push(Entry {
            offset: end,
            data_offset,
            size: obj.data.len() as u64,
            crc32: crc32fast::hash(&bytes),
            base: Base::None(obj.kind),
        });
        let slot = OnceLock::new();
        let _ = slot.set((base, obj.kind));
        t.slots.push(slot);
        t.graph.grow(t.entries.len());
        end += bytes.len() as u64;
        appended += 1;

        let ctx = Ctx {
            pack: t.pack,
            entries: t.entries,
            slots: t.slots,
            graph: t.graph,
            hash: opts.hash,
            checker,
            budget: opts.memory_limit.max(1 << 20),
            abort: AtomicBool::new(false),
            error: Mutex::new(None),
            resolved: AtomicU64::new(*resolved),
            progress: progress.as_deref_mut().map(Mutex::new),
        };
        ctx.resolve_tree(idx, Some(obj.data))?;
        *resolved = ctx.resolved.load(Ordering::Acquire);
    }
    if appended == 0 {
        return Ok(Completed {
            appended,
            checksum: rgit_hash::HashKind::null(opts.hash),
            pack_len: end,
        });
    }

    // New object count, then the checksum over the whole file.
    let count = u32::try_from(t.entries.len()).map_err(|_| Error::OffsetTooLarge)?;
    let mut header = [0u8; 12];
    header[..4].copy_from_slice(b"PACK");
    header[4..8].copy_from_slice(&t.version.to_be_bytes());
    header[8..].copy_from_slice(&count.to_be_bytes());
    t.dest.seek(SeekFrom::Start(0)).map_err(Error::Write)?;
    t.dest.write_all(&header).map_err(Error::Write)?;
    t.dest.flush().map_err(Error::Write)?;
    t.dest.seek(SeekFrom::Start(0)).map_err(Error::Write)?;
    let mut hasher = Hasher::new(opts.hash);
    let mut left = end;
    let mut buf = vec![0u8; 128 * 1024];
    while left > 0 {
        let n = (buf.len() as u64).min(left) as usize;
        t.dest.read_exact(&mut buf[..n]).map_err(Error::Read)?;
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    let checksum = hasher.finalize().map_err(|_| Error::ChecksumMismatch)?;
    t.dest.seek(SeekFrom::Start(end)).map_err(Error::Write)?;
    t.dest
        .write_all(checksum.as_bytes())
        .map_err(Error::Write)?;
    t.dest.flush().map_err(Error::Write)?;
    Ok(Completed {
        appended,
        checksum,
        pack_len: end + opts.hash.byte_len() as u64,
    })
}
