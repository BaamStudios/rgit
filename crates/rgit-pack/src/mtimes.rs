// SPDX-License-Identifier: MIT

//! `pack-*.mtimes`: the per-object modification times of a cruft pack
//! (`Documentation/gitformat-pack.adoc`, "pack-*.mtimes files").
//!
//! Layout (numbers in network byte order): the magic `MTME`, version 1,
//! the hash function id (1 SHA-1, 2 SHA-256), one 4-byte time (epoch
//! seconds) per object in index order, then the pack's checksum and a
//! checksum of everything before it.

use std::path::Path;

use rgit_hash::{HashKind, Hasher, ObjectId};

use crate::Error;

const MAGIC: &[u8; 4] = b"MTME";
const HEADER_LEN: usize = 12;

fn hash_id(kind: HashKind) -> u32 {
    match kind {
        HashKind::Sha1 => 1,
        HashKind::Sha256 => 2,
    }
}

/// The content of a `.mtimes` file for a pack with checksum `pack` whose
/// objects, in index order (sorted by id), have the times `mtimes`.
pub fn mtimes_bytes(kind: HashKind, pack: &ObjectId, mtimes: &[u32]) -> Vec<u8> {
    let h = kind.byte_len();
    let mut out = Vec::with_capacity(HEADER_LEN + 4 * mtimes.len() + 2 * h);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&hash_id(kind).to_be_bytes());
    for t in mtimes {
        out.extend_from_slice(&t.to_be_bytes());
    }
    out.extend_from_slice(pack.as_bytes());
    let mut hasher = Hasher::new(kind);
    hasher.update(&out);
    // A collision in our own output cannot be acted upon; the digest is
    // still the right trailer.
    let sum = match hasher.finalize() {
        Ok(s) => s,
        Err(e) => e.id,
    };
    out.extend_from_slice(sum.as_bytes());
    out
}

/// A parsed `.mtimes` file.
pub struct Mtimes {
    data: Vec<u8>,
    count: u32,
}

impl std::fmt::Debug for Mtimes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mtimes")
            .field("count", &self.count)
            .finish()
    }
}

impl Mtimes {
    /// Read and check `path` for a pack of `count` objects.
    pub fn open(path: &Path, kind: HashKind, count: u32) -> Result<Mtimes, Error> {
        let bad = |what: &'static str| Error::Mtimes {
            path: path.to_path_buf(),
            what,
        };
        let data = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        let h = kind.byte_len();
        if data.len() < HEADER_LEN || &data[..4] != MAGIC {
            return Err(bad("has unknown signature"));
        }
        let word =
            |at: usize| u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        if word(4) != 1 {
            return Err(bad("has unsupported version"));
        }
        if word(8) != hash_id(kind) {
            return Err(bad("has unsupported hash id"));
        }
        if data.len() != HEADER_LEN + 4 * count as usize + 2 * h {
            return Err(bad("is too small"));
        }
        Ok(Mtimes { data, count })
    }

    /// Number of entries.
    pub fn len(&self) -> u32 {
        self.count
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The time of the object at index position `pos`.
    pub fn get(&self, pos: u32) -> Option<u32> {
        if pos >= self.count {
            return None;
        }
        let at = HEADER_LEN + 4 * pos as usize;
        Some(u32::from_be_bytes([
            self.data[at],
            self.data[at + 1],
            self.data[at + 2],
            self.data[at + 3],
        ]))
    }

    /// The pack checksum recorded in the trailer.
    pub fn pack_checksum(&self, kind: HashKind) -> &[u8] {
        let h = kind.byte_len();
        let end = self.data.len() - h;
        &self.data[end - h..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let kind = HashKind::Sha1;
        let pack = rgit_hash::hash(kind, b"pack").unwrap();
        let bytes = mtimes_bytes(kind, &pack, &[1, 2, 0xffff_fff0]);
        assert_eq!(&bytes[..12], b"MTME\0\0\0\x01\0\0\0\x01");
        assert_eq!(bytes.len(), 12 + 12 + 40);
        let dir = std::env::temp_dir().join(format!("rgit-mtimes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pack-x.mtimes");
        std::fs::write(&path, &bytes).unwrap();
        let m = Mtimes::open(&path, kind, 3).unwrap();
        assert_eq!(m.get(0), Some(1));
        assert_eq!(m.get(2), Some(0xffff_fff0));
        assert_eq!(m.get(3), None);
        assert_eq!(m.pack_checksum(kind), pack.as_bytes());
        assert!(Mtimes::open(&path, kind, 2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_pack() {
        let kind = HashKind::Sha1;
        let pack = rgit_hash::hash(kind, b"").unwrap();
        let bytes = mtimes_bytes(kind, &pack, &[]);
        assert_eq!(bytes.len(), 52);
    }
}
