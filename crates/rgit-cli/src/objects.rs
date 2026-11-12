// SPDX-License-Identifier: MIT
//! Object lookup for commands: the type of an object and unique
//! abbreviations, over loose objects ([`rgit_odb`]) and packs
//! ([`rgit_pack::PackSet`]).
//!
//! Interim glue until the object database serves packs itself; alternates
//! are not consulted.

use std::path::PathBuf;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectKind;
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_pack::{DeltaBaseCache, PackSet};

/// The objects of one repository.
pub struct Objects {
    loose: ObjectDatabase,
    packs: PackSet,
    kind: HashKind,
}

impl Objects {
    /// Objects below `dir` (`$GIT_DIR/objects`).
    pub fn open(dir: impl Into<PathBuf>, kind: HashKind) -> Self {
        let dir = dir.into();
        let packs = PackSet::open(dir.join("pack"), kind);
        let loose = ObjectDatabase::open(&dir, kind, LooseOptions::default());
        Objects { loose, packs, kind }
    }

    /// The type of `id`, `None` if it is missing or unreadable.
    pub fn kind_of(&self, id: &ObjectId) -> Option<ObjectKind> {
        if let Ok(Some(h)) = self.loose.header(id) {
            return Some(h.kind);
        }
        let mut cache = DeltaBaseCache::new(0);
        let object = self.packs.read(id, &mut cache, &mut |_| {}).ok()??;
        Some(match object.kind {
            rgit_pack::ObjectKind::Commit => ObjectKind::Commit,
            rgit_pack::ObjectKind::Tree => ObjectKind::Tree,
            rgit_pack::ObjectKind::Blob => ObjectKind::Blob,
            rgit_pack::ObjectKind::Tag => ObjectKind::Tag,
        })
    }

    /// Whether any object other than `id` starts with the first `len` hex
    /// digits of `id`.
    fn ambiguous(&self, id: &ObjectId, len: usize) -> bool {
        let Ok(prefix) = Prefix::new(*id, len) else {
            return false;
        };
        if self
            .packs
            .lookup_prefix(&prefix, 2)
            .iter()
            .any(|other| other != id)
        {
            return true;
        }
        let mut other = false;
        let _ = self
            .loose
            .loose()
            .for_each_prefix_match(&prefix, &mut |o| other |= o != *id);
        other
    }

    /// The shortest unique abbreviation of `id` with at least `min` digits.
    pub fn abbrev(&self, id: &ObjectId, min: usize) -> String {
        let full = self.kind.hex_len();
        let mut len = min.clamp(4, full);
        while len < full && self.ambiguous(id, len) {
            len += 1;
        }
        id.to_hex_with_len(len)
    }
}
