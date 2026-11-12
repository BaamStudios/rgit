// SPDX-License-Identifier: MIT

//! Serializing the index file.

use rgit_hash::Hasher;

use crate::entry::bits;
use crate::read::SIGNATURE;
use crate::{Index, varint};

/// Options for [`Index::to_bytes`].
#[derive(Clone, Copy, Debug, Default)]
pub struct WriteOptions {
    /// Write a null trailer instead of the checksum (`index.skipHash`).
    pub skip_hash: bool,
}

pub(crate) fn serialize(index: &Index, opts: &WriteOptions) -> Vec<u8> {
    let kind = index.kind;
    let version = index.write_version();
    let mut out = Vec::with_capacity(12 + index.entries.len() * 80);
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(index.entries.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );

    let mut prev_path: &[u8] = &[];
    for e in &index.entries {
        let start = out.len();
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
        flags |= u16::try_from(e.path.len())
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
            let common = prev_path
                .iter()
                .zip(&e.path)
                .take_while(|(a, b)| a == b)
                .count();
            varint::encode(prev_path.len() - common, &mut out);
            out.extend_from_slice(&e.path[common..]);
            out.push(0);
            prev_path = &e.path;
        } else {
            out.extend_from_slice(&e.path);
            let len = out.len() - start;
            let padded = (len + 8) & !7;
            out.resize(start + padded, 0);
        }
    }

    if let Some(tree) = &index.tree {
        let mut data = Vec::new();
        tree.write(&mut data);
        push_extension(&mut out, b"TREE", &data);
    }
    if let Some(reuc) = &index.resolve_undo {
        push_extension(&mut out, b"REUC", reuc);
    }

    if opts.skip_hash {
        out.resize(out.len() + kind.byte_len(), 0);
    } else {
        let mut hasher = Hasher::new(kind);
        hasher.update(&out);
        let id = hasher.finalize().unwrap_or_else(|e| e.id);
        out.extend_from_slice(id.as_bytes());
    }
    out
}

fn push_extension(out: &mut Vec<u8>, sig: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(sig);
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(data);
}
