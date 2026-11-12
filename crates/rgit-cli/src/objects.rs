// SPDX-License-Identifier: MIT
//! Minimal object lookup for commands: the type of an object and unique
//! abbreviations, over loose objects and packs.
//!
//! Interim helper until the object database crate (`rgit-odb`, #14) is on
//! `main`; alternates are not consulted.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use flate2::read::ZlibDecoder;
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_pack::{DeltaBaseCache, ObjectKind, PackSet};

/// The objects of one repository.
pub struct Objects {
    dir: PathBuf,
    packs: PackSet,
    kind: HashKind,
}

impl Objects {
    /// Objects below `dir` (`$GIT_DIR/objects`).
    pub fn open(dir: impl Into<PathBuf>, kind: HashKind) -> Self {
        let dir = dir.into();
        let packs = PackSet::open(dir.join("pack"), kind);
        Objects { dir, packs, kind }
    }

    fn loose_path(&self, hex: &str) -> PathBuf {
        self.dir.join(&hex[..2]).join(&hex[2..])
    }

    /// The type of `id`, `None` if it is missing or unreadable.
    pub fn kind_of(&self, id: &ObjectId) -> Option<ObjectKind> {
        if let Some(k) = self.loose_kind(id) {
            return Some(k);
        }
        let mut cache = DeltaBaseCache::new(0);
        self.packs
            .read(id, &mut cache, &mut |_| {})
            .ok()
            .flatten()
            .map(|o| o.kind)
    }

    fn loose_kind(&self, id: &ObjectId) -> Option<ObjectKind> {
        let file = File::open(self.loose_path(&id.to_hex())).ok()?;
        let mut header = Vec::new();
        let mut z = ZlibDecoder::new(file).take(64);
        let mut byte = [0u8; 1];
        while header.len() < 64 {
            if z.read(&mut byte).ok()? == 0 {
                return None;
            }
            if byte[0] == 0 {
                break;
            }
            header.push(byte[0]);
        }
        let ty = header.split(|&b| b == b' ').next()?;
        match ty {
            b"commit" => Some(ObjectKind::Commit),
            b"tree" => Some(ObjectKind::Tree),
            b"blob" => Some(ObjectKind::Blob),
            b"tag" => Some(ObjectKind::Tag),
            _ => None,
        }
    }

    /// Whether any object other than `id` starts with the first `len` hex
    /// digits of `id`.
    fn ambiguous(&self, id: &ObjectId, hex: &str, len: usize) -> bool {
        if let Ok(prefix) = Prefix::new(*id, len) {
            if self
                .packs
                .lookup_prefix(&prefix, 2)
                .iter()
                .any(|other| other != id)
            {
                return true;
            }
        }
        let Ok(rd) = std::fs::read_dir(self.dir.join(&hex[..2])) else {
            return false;
        };
        rd.flatten().any(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.len() == self.kind.hex_len() - 2
                && name.starts_with(&hex[2..len])
                && name != hex[2..]
        })
    }

    /// The shortest unique abbreviation of `id` with at least `min` digits.
    pub fn abbrev(&self, id: &ObjectId, min: usize) -> String {
        let hex = id.to_hex();
        let full = self.kind.hex_len();
        let mut len = min.clamp(4, full);
        while len < full && self.ambiguous(id, &hex, len) {
            len += 1;
        }
        hex[..len].to_string()
    }
}
