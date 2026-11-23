// SPDX-License-Identifier: MIT
//! Object lookup for commands: the type of an object and unique
//! abbreviations, over loose objects ([`rgit_odb`]) and packs
//! ([`rgit_pack::PackSet`]) of the repository and its alternates.
//!
//! Interim glue until the object database serves packs itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectKind;
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_pack::{DeltaBaseCache, PackSet};

/// The objects of one repository.
pub struct Objects {
    /// Loose objects and packs per object directory, the repository's
    /// own first, then its alternates.
    layers: Vec<(ObjectDatabase, PackSet)>,
    kind: HashKind,
}

impl Objects {
    /// Objects below `dir` (`$GIT_DIR/objects`) and its alternates.
    pub fn open(dir: impl Into<PathBuf>, kind: HashKind) -> Self {
        let dir = dir.into();
        let layer = |d: &std::path::Path| {
            (
                ObjectDatabase::open(d, kind, LooseOptions::default()),
                PackSet::open(d.join("pack"), kind),
            )
        };
        let mut layers = vec![layer(&dir)];
        layers.extend(rgit_odb::resolve_alternates(&dir).iter().map(|d| layer(d)));
        Objects { layers, kind }
    }

    /// The type of `id`, `None` if it is missing or unreadable.
    pub fn kind_of(&self, id: &ObjectId) -> Option<ObjectKind> {
        for (loose, packs) in &self.layers {
            if let Ok(Some(h)) = loose.header(id) {
                return Some(h.kind);
            }
            let mut cache = DeltaBaseCache::new(0);
            if let Ok(Some(object)) = packs.read(id, &mut cache, &mut |_| {}) {
                return Some(match object.kind {
                    rgit_pack::ObjectKind::Commit => ObjectKind::Commit,
                    rgit_pack::ObjectKind::Tree => ObjectKind::Tree,
                    rgit_pack::ObjectKind::Blob => ObjectKind::Blob,
                    rgit_pack::ObjectKind::Tag => ObjectKind::Tag,
                });
            }
        }
        None
    }

    /// Whether any object other than `id` starts with the first `len` hex
    /// digits of `id`.
    fn ambiguous(&self, id: &ObjectId, len: usize) -> bool {
        let Ok(prefix) = Prefix::new(*id, len) else {
            return false;
        };
        let mut other = false;
        for (loose, packs) in &self.layers {
            if packs.lookup_prefix(&prefix, 2).iter().any(|o| o != id) {
                return true;
            }
            let _ = loose
                .loose()
                .for_each_prefix_match(&prefix, &mut |o| other |= o != *id);
            if other {
                return true;
            }
        }
        false
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

/// An object kept by [`Recent`].
type Kept = (ObjectKind, Rc<[u8]>);

/// The objects read last, by id: `log -p` reads every commit several
/// times (walk, entry, parent tree) and most trees twice (as the new side
/// of one commit's diff and the old side of the next). Two generations
/// bound the memory: when the current one is full it becomes the old one
/// and the previous old one is dropped.
pub struct Recent {
    cur: HashMap<ObjectId, Kept>,
    old: HashMap<ObjectId, Kept>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Recent {
    /// A cache whose generations hold up to `max_entries` objects or
    /// `max_bytes` bytes of content.
    pub fn new(max_entries: usize, max_bytes: usize) -> Recent {
        Recent {
            cur: HashMap::new(),
            old: HashMap::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    /// The object `id`, if it was read recently.
    pub fn get(&mut self, id: &ObjectId) -> Option<Kept> {
        if let Some(v) = self.cur.get(id) {
            return Some(v.clone());
        }
        let v = self.old.remove(id)?;
        self.insert(*id, v.0, v.1.clone());
        Some(v)
    }

    /// Remembers object `id`.
    pub fn insert(&mut self, id: ObjectId, kind: ObjectKind, data: Rc<[u8]>) {
        if data.len() > self.max_bytes {
            return;
        }
        if self.cur.len() >= self.max_entries || self.bytes + data.len() > self.max_bytes {
            self.old = std::mem::take(&mut self.cur);
            self.bytes = 0;
        }
        self.bytes += data.len();
        self.cur.insert(id, (kind, data));
    }
}
