// SPDX-License-Identifier: MIT

//! Parsing the index file.

use rgit_hash::{HashKind, Hasher, ObjectId};

use crate::entry::{Entry, Flags, Mode, Stage, Stat, Time, bits};
use crate::{CacheTree, Error, Index, varint};

/// Options for [`Index::from_bytes`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOptions {
    /// Verify the trailing checksum (a null trailer, written with
    /// `index.skipHash`, is always accepted). Default: off, as git only
    /// verifies it in checks such as `fsck`.
    pub verify_checksum: bool,
    /// Threads for parsing the entries of an index with an offset table
    /// (`IEOT`, `index.threads`): 0 picks one per CPU, 1 parses in the
    /// calling thread only.
    pub threads: usize,
}

impl ReadOptions {
    /// Options that verify the trailing checksum (for `fsck`-like checks).
    pub fn verified() -> Self {
        ReadOptions {
            verify_checksum: true,
            ..ReadOptions::default()
        }
    }
}

pub(crate) const SIGNATURE: &[u8; 4] = b"DIRC";
const HEADER_LEN: usize = 12;
/// ctime, mtime (2 x 32 bit each), dev, ino, mode, uid, gid, size.
const STAT_LEN: usize = 40;

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

pub(crate) fn parse(data: &[u8], kind: HashKind, opts: &ReadOptions) -> Result<Index, Error> {
    parse_split(data, kind, opts, &mut |_| Err(Error::SplitIndex))
}

/// [`parse`] for an index that may be split: `shared` returns the bytes
/// of the shared index with the given checksum.
pub(crate) fn parse_split(
    data: &[u8],
    kind: HashKind,
    opts: &ReadOptions,
    shared: &mut dyn FnMut(&ObjectId) -> Result<Vec<u8>, Error>,
) -> Result<Index, Error> {
    let mut link = None;
    let mut index = parse_one(data, kind, opts, &mut link)?;
    if let Some(link) = link {
        let base = if link.base_id.is_null() {
            Vec::new()
        } else {
            let bytes = shared(&link.base_id)?;
            let shared_index = parse_one(&bytes, kind, opts, &mut None)?;
            if shared_index.checksum != Some(link.base_id) {
                return Err(Error::Corrupt("shared index checksum"));
            }
            shared_index.entries
        };
        let own = std::mem::take(&mut index.entries);
        index.entries = crate::split::merge(own, base.clone(), &link)?;
        index.split = Some(crate::split::Split {
            base_id: link.base_id,
            base,
        });
    }
    Ok(index)
}

fn parse_one(
    data: &[u8],
    kind: HashKind,
    opts: &ReadOptions,
    link: &mut Option<crate::split::Link>,
) -> Result<Index, Error> {
    let hash_len = kind.byte_len();
    if data.len() < HEADER_LEN + hash_len {
        return Err(Error::TooSmall);
    }
    if &data[..4] != SIGNATURE {
        // git prints the signature as read in host byte order.
        return Err(Error::BadSignature(u32::from_ne_bytes([
            data[0], data[1], data[2], data[3],
        ])));
    }
    let version = be32(&data[4..8]);
    if !(2..=4).contains(&version) {
        return Err(Error::BadVersion(version));
    }
    let body_end = data.len() - hash_len;
    let trailer = &data[body_end..];
    let checksum = if trailer.iter().all(|&b| b == 0) {
        None
    } else {
        let id = ObjectId::from_bytes(kind, trailer).map_err(|_| Error::Corrupt("trailer"))?;
        if opts.verify_checksum {
            let mut hasher = Hasher::new(kind);
            hasher.update(&data[..body_end]);
            // A collision attack on the index checksum is just a mismatch.
            let actual = hasher.finalize().unwrap_or_else(|e| e.id);
            if actual != id {
                return Err(Error::BadChecksum);
            }
        }
        Some(id)
    };
    let count = be32(&data[8..12]) as usize;
    let body = &data[..body_end];

    let mut index = Index::new(kind);
    index.version = version;
    index.checksum = checksum;

    let blocks = offset_table(body, kind, count).filter(|b| b.len() > 1);
    let (entries, mut pos) = match blocks {
        Some(blocks) if opts.threads != 1 => {
            parse_threaded(body, kind, version, &blocks, opts.threads)?
        }
        _ => parse_entries(body, HEADER_LEN, count, kind, version, false)?,
    };
    index.entries = entries;

    while pos < body.len() {
        let header = body
            .get(pos..pos + 8)
            .ok_or(Error::Corrupt("truncated extension"))?;
        let sig: [u8; 4] = header[..4].try_into().expect("4 bytes");
        let size = be32(&header[4..]) as usize;
        pos += 8;
        // git ignores an optional extension running past the end of the
        // file, and with it everything after it. Required extensions are
        // refused below anyway.
        if size > body.len() - pos && sig[0].is_ascii_uppercase() {
            rgit_trace::trace!(
                rgit_trace::TRACE,
                "index: {} extension size {size} exceeds the file, ignored",
                String::from_utf8_lossy(&sig)
            );
            index.ignored.push(sig);
            break;
        }
        let ext = &body[pos..(pos + size).min(body.len())];
        pos = (pos + size).min(body.len());
        match &sig {
            b"TREE" => index.tree = CacheTree::parse(ext, kind),
            b"REUC" => index.resolve_undo = Some(crate::index::parse_resolve_undo(ext, kind)),
            b"link" => {
                *link = Some(
                    crate::split::Link::parse(ext, kind).ok_or(Error::Corrupt("link extension"))?,
                );
            }
            b"sdir" => return Err(Error::SparseIndex),
            // Written anew from the entries when asked for.
            b"EOIE" | b"IEOT" => {}
            // Known optional extensions that rgit does not keep yet:
            // fsmonitor data (and an untracked cache it cannot parse).
            b"UNTR" => match crate::UntrackedCache::parse(ext, kind) {
                Some(uc) => index.untracked = Some(uc),
                None => index.dropped.push(sig),
            },
            b"FSMN" => index.dropped.push(sig),
            _ if sig[0].is_ascii_uppercase() => index.ignored.push(sig),
            _ => return Err(Error::UnknownExtension(sig)),
        }
    }
    Ok(index)
}

/// Parses `count` entries starting at `pos`; returns them and the position
/// after the last one. In version 4 the first entry of an offset table
/// `block` strips all of a previous path that is not known here.
fn parse_entries(
    body: &[u8],
    mut pos: usize,
    count: usize,
    kind: HashKind,
    version: u32,
    block: bool,
) -> Result<(Vec<Entry>, usize), Error> {
    let hash_len = kind.byte_len();
    // Every entry takes at least STAT_LEN + hash + 2 bytes (v4: + 2 more).
    let min_entry = STAT_LEN + hash_len + 2;
    let mut entries = Vec::with_capacity(count.min(body.len() / min_entry));
    let mut prev_path: Vec<u8> = Vec::new();
    for i in 0..count {
        let start = pos;
        let fixed = body
            .get(start..start + min_entry)
            .ok_or(Error::Corrupt("truncated entry"))?;
        let w = |i: usize| be32(&fixed[i * 4..]);
        let stat = Stat {
            ctime: Time {
                secs: w(0),
                nsecs: w(1),
            },
            mtime: Time {
                secs: w(2),
                nsecs: w(3),
            },
            dev: w(4),
            ino: w(5),
            uid: w(7),
            gid: w(8),
            size: w(9),
        };
        let mode = Mode(w(6));
        let id = ObjectId::from_bytes(kind, &fixed[STAT_LEN..STAT_LEN + hash_len])
            .map_err(|_| Error::Corrupt("object id"))?;
        let raw_flags = be16(&fixed[STAT_LEN + hash_len..]);
        pos = start + min_entry;
        let mut flags = Flags {
            assume_valid: raw_flags & bits::ASSUME_VALID != 0,
            ..Flags::default()
        };
        if raw_flags & bits::EXTENDED != 0 {
            let ext = be16(
                body.get(pos..pos + 2)
                    .ok_or(Error::Corrupt("truncated entry"))?,
            );
            pos += 2;
            if ext & !bits::EXTENDED_KNOWN != 0 {
                return Err(Error::UnknownEntryFormat(u32::from(ext) << 16));
            }
            flags.skip_worktree = ext & bits::SKIP_WORKTREE != 0;
            flags.intent_to_add = ext & bits::INTENT_TO_ADD != 0;
        }
        let stage = Stage::new(((raw_flags & bits::STAGE_MASK) >> bits::STAGE_SHIFT) as u8)
            .expect("two bits");
        let name_len = usize::from(raw_flags & bits::NAME_MASK);

        let path = if version == 4 {
            let (strip, used) = varint::decode(body.get(pos..).unwrap_or_default())
                .ok_or(Error::Corrupt("path prefix"))?;
            pos += used;
            if strip > prev_path.len() && !(block && i == 0) {
                return Err(Error::Corrupt("path prefix"));
            }
            let keep = prev_path.len().saturating_sub(strip);
            let nul = body[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::Corrupt("unterminated path"))?;
            let mut path = Vec::with_capacity(keep + nul);
            path.extend_from_slice(&prev_path[..keep]);
            path.extend_from_slice(&body[pos..pos + nul]);
            pos += nul + 1;
            prev_path.clone_from(&path);
            path
        } else {
            let rest = &body[pos..];
            let len = if name_len < usize::from(bits::NAME_MASK) {
                if rest.get(name_len) != Some(&0) {
                    return Err(Error::Corrupt("path length"));
                }
                name_len
            } else {
                rest.iter()
                    .position(|&b| b == 0)
                    .ok_or(Error::Corrupt("unterminated path"))?
            };
            let path = rest[..len].to_vec();
            // 1 to 8 NUL bytes pad the entry to a multiple of eight.
            let entry_len = (pos - start + len + 8) & !7;
            if start + entry_len > body.len() {
                return Err(Error::Corrupt("truncated entry"));
            }
            pos = start + entry_len;
            path
        };
        entries.push(Entry {
            stat,
            mode,
            id,
            flags,
            stage,
            path,
            base: 0,
        });
    }
    Ok((entries, pos))
}

/// The blocks (offset, entry count) of the Index Entry Offset Table, found
/// through the End Of Index Entries extension, if both are present and
/// consistent with the file.
fn offset_table(body: &[u8], kind: HashKind, count: usize) -> Option<Vec<(usize, usize)>> {
    let hash_len = kind.byte_len();
    // EOIE is the last extension: header, 32-bit offset, hash.
    let eoie_len = 8 + 4 + hash_len;
    let eoie = body.len().checked_sub(eoie_len)?;
    let ext = &body[eoie..];
    if &ext[..4] != b"EOIE" || be32(&ext[4..8]) as usize != 4 + hash_len {
        return None;
    }
    let entries_end = be32(&ext[8..12]) as usize;
    if !(HEADER_LEN..=eoie).contains(&entries_end) {
        return None;
    }
    // The hash covers the signature and size of every extension before it.
    let mut hasher = Hasher::new(kind);
    let mut pos = entries_end;
    let mut ieot = None;
    while pos < eoie {
        let header = body.get(pos..pos + 8)?;
        let size = be32(&header[4..]) as usize;
        hasher.update(header);
        if &header[..4] == b"IEOT" {
            ieot = body.get(pos + 8..pos + 8 + size);
        }
        pos = pos.checked_add(8 + size)?;
    }
    let id = hasher.finalize().unwrap_or_else(|e| e.id);
    if pos != eoie || id.as_bytes() != &ext[12..] {
        return None;
    }
    let ieot = ieot?;
    if ieot.len() < 4 || be32(ieot) != 1 || (ieot.len() - 4) % 8 != 0 {
        return None;
    }
    let blocks: Vec<(usize, usize)> = ieot[4..]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| (be32(c) as usize, be32(&c[4..]) as usize))
        .collect();
    let total: usize = blocks.iter().map(|b| b.1).sum();
    let ordered = blocks.windows(2).all(|w| w[0].0 < w[1].0);
    (total == count && ordered && blocks.first()?.0 == HEADER_LEN).then_some(blocks)
}

/// Parses the entries block by block on several threads.
fn parse_threaded(
    body: &[u8],
    kind: HashKind,
    version: u32,
    blocks: &[(usize, usize)],
    threads: usize,
) -> Result<(Vec<Entry>, usize), Error> {
    let threads = match threads {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
    .clamp(1, blocks.len());
    let per_thread = blocks.len().div_ceil(threads);
    type Part = Result<(Vec<Entry>, usize), Error>;
    let results: Vec<Part> = std::thread::scope(|s| {
        let handles: Vec<_> = blocks
            .chunks(per_thread)
            .map(|chunk| {
                s.spawn(move || -> Part {
                    let mut out = Vec::new();
                    let mut end = 0;
                    for &(offset, count) in chunk {
                        let (mut entries, e) =
                            parse_entries(body, offset, count, kind, version, true)?;
                        out.append(&mut entries);
                        end = e;
                    }
                    Ok((out, end))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Err(Error::Corrupt("entry block"))))
            .collect()
    });
    let mut entries = Vec::new();
    let mut end = HEADER_LEN;
    for r in results {
        let (mut part, e) = r?;
        entries.append(&mut part);
        end = e;
    }
    Ok((entries, end))
}
