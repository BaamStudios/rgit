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
}

impl ReadOptions {
    /// Options that verify the trailing checksum (for `fsck`-like checks).
    pub fn verified() -> Self {
        ReadOptions {
            verify_checksum: true,
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
    // Every entry takes at least STAT_LEN + hash + 2 bytes (v4: + 2 more).
    let min_entry = STAT_LEN + hash_len + 2;
    index.entries.reserve(count.min(body.len() / min_entry));

    let mut pos = HEADER_LEN;
    let mut prev_path: Vec<u8> = Vec::new();
    for _ in 0..count {
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
            let (strip, used) =
                varint::decode(&body[pos..]).ok_or(Error::Corrupt("path prefix"))?;
            pos += used;
            if strip > prev_path.len() {
                return Err(Error::Corrupt("path prefix"));
            }
            let nul = body[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::Corrupt("unterminated path"))?;
            let mut path = Vec::with_capacity(prev_path.len() - strip + nul);
            path.extend_from_slice(&prev_path[..prev_path.len() - strip]);
            path.extend_from_slice(&body[pos..pos + nul]);
            pos += nul + 1;
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
        if version == 4 {
            prev_path.clone_from(&path);
        }
        index.entries.push(Entry {
            stat,
            mode,
            id,
            flags,
            stage,
            path,
        });
    }

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
            b"REUC" => index.resolve_undo = Some(ext.to_vec()),
            b"link" => return Err(Error::SplitIndex),
            b"sdir" => return Err(Error::SparseIndex),
            // Known optional extensions that rgit does not keep yet: the
            // untracked cache, fsmonitor data and the two offset tables.
            b"UNTR" | b"FSMN" | b"EOIE" | b"IEOT" => index.dropped.push(sig),
            _ if sig[0].is_ascii_uppercase() => index.ignored.push(sig),
            _ => return Err(Error::UnknownExtension(sig)),
        }
    }
    Ok(index)
}
