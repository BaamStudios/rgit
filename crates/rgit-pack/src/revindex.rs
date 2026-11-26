// SPDX-License-Identifier: MIT

//! Reverse indexes (`pack-*.rev`, `Documentation/gitformat-pack.adoc`):
//! the index positions of a pack's objects in pack (offset) order, with
//! the pack checksum and a checksum of the file in the trailer.

use std::path::Path;

use rgit_hash::{HashKind, Hasher};

use crate::PackIndex;
use crate::messages as msg;

/// The hash function id of a `.rev` (1: SHA-1, 2: SHA-256).
pub fn hash_id(kind: HashKind) -> u32 {
    match kind {
        HashKind::Sha1 => 1,
        _ => 2,
    }
}

/// Whether the last hash of `data` is the checksum of everything before
/// it.
pub fn trailer_ok(data: &[u8], kind: HashKind) -> bool {
    let h = kind.byte_len();
    if data.len() < h {
        return false;
    }
    let (body, sum) = data.split_at(data.len() - h);
    let mut hasher = Hasher::new(kind);
    hasher.update(body);
    hasher.finalize().is_ok_and(|s| s.as_bytes() == sum)
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

/// Check the `.rev` file at `path` against `index`. `Err`: the file cannot
/// be loaded (git's message); `Ok`: the problems found (git's messages),
/// empty when it is good.
pub fn verify(path: &Path, index: &PackIndex) -> Result<Vec<String>, String> {
    let shown = path.display().to_string();
    let data = std::fs::read(path).map_err(|e| format!("could not read '{shown}': {e}"))?;
    let kind = index.hash_kind();
    let h = kind.byte_len();
    let n = index.len() as usize;
    if data.len() < 12 + 2 * h {
        return Err(msg::revindex_too_small(&shown));
    }
    if &data[..4] != b"RIDX" {
        return Err(msg::revindex_unknown_signature(&shown));
    }
    let version = be32(&data, 4);
    if version != 1 {
        return Err(msg::revindex_unsupported_version(&shown, version));
    }
    let id = be32(&data, 8);
    if id != 1 && id != 2 {
        return Err(msg::revindex_unsupported_hash_id(&shown, id));
    }
    if data.len() != 12 + 4 * n + 2 * h {
        return Err(msg::revindex_corrupt(&shown));
    }
    let mut problems = Vec::new();
    if !trailer_ok(&data, kind) {
        problems.push(msg::REVINDEX_INVALID_CHECKSUM.to_owned());
    }
    let mut by_offset: Vec<(u64, u32)> = (0..index.len())
        .filter_map(|p| index.offset_at(p).ok().map(|o| (o, p)))
        .collect();
    by_offset.sort_unstable();
    for (i, &(_, want)) in by_offset.iter().enumerate() {
        let got = be32(&data, 12 + 4 * i);
        if got != want {
            problems.push(msg::revindex_invalid_position(i, want, got));
            break;
        }
    }
    Ok(problems)
}
