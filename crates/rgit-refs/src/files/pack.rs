// SPDX-License-Identifier: MIT
//! `pack-refs`: move loose references into `packed-refs`
//! (Documentation/git-pack-refs.adoc).
//!
//! `packed-refs` is locked and rewritten with every packed entry plus the
//! selected loose references (peeled values computed through the caller),
//! then each packed loose file is removed under its own lock if it still
//! holds the packed value, and empty parent directories below
//! `refs/<x>/` are removed.

use std::collections::BTreeMap;
use std::io::Write;

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;

use super::{FilesStore, LooseRead, read_loose_path};
use crate::error::TransactionError;
use crate::name::{RefNameFlags, is_valid_refname};
use crate::packed::PackedRef;
use crate::target::Target;

/// Which loose references `pack-refs` moves.
pub trait PackFilter {
    /// Whether the loose reference `name` is packed.
    fn include(&self, name: &str) -> bool;
}

impl<F: Fn(&str) -> bool> PackFilter for F {
    fn include(&self, name: &str) -> bool {
        self(name)
    }
}

impl FilesStore {
    /// Packs the loose references selected by `filter` into `packed-refs`
    /// and, with `prune`, removes their loose files. `peel` returns the
    /// peeled object of an annotated tag (and `None` for other objects).
    pub fn pack_refs(
        &self,
        filter: &dyn PackFilter,
        prune: bool,
        peel: &dyn Fn(&ObjectId) -> Option<ObjectId>,
    ) -> Result<(), TransactionError> {
        let opts = LockOptions::new()
            .timeout(self.options.packed_refs_timeout)
            .shared(self.options.shared)
            .fsync(self.options.fsync);
        let mut lock = LockFile::acquire(self.packed_refs_path(), &opts)
            .map_err(TransactionError::PackedLock)?;
        let mut packed = self.packed_refs()?;

        let mut loose = BTreeMap::new();
        self.collect_loose("refs", &mut loose)?;
        let mut moved: Vec<(String, ObjectId)> = Vec::new();
        for (name, value) in loose {
            let LooseRead::Value(Target::Object(oid)) = value else {
                continue;
            };
            if !is_valid_refname(name.as_bytes(), RefNameFlags::STRICT) || !filter.include(&name) {
                continue;
            }
            packed.upsert(PackedRef {
                name: name.clone(),
                oid,
                peeled: None,
            });
            moved.push((name, oid));
        }
        let entries: Vec<PackedRef> = packed
            .refs()
            .iter()
            .map(|r| PackedRef {
                name: r.name.clone(),
                oid: r.oid,
                peeled: peel(&r.oid),
            })
            .collect();
        let packed = crate::packed::PackedRefs::from_refs(entries);
        let path = lock.lock_path().to_owned();
        lock.write_all(&packed.serialize())
            .and_then(|()| lock.commit())
            .map_err(|source| TransactionError::Io { path, source })?;

        if !prune {
            return Ok(());
        }
        let ref_opts = LockOptions::new()
            .timeout(self.options.ref_lock_timeout)
            .follow_symlinks(false)
            .shared(self.options.shared);
        for (name, oid) in moved {
            let file = self.ref_path(&name);
            let Ok(ref_lock) = LockFile::acquire(&file, &ref_opts) else {
                continue;
            };
            if matches!(
                read_loose_path(&file, self.kind),
                Ok(LooseRead::Value(Target::Object(o))) if o == oid
            ) {
                let _ = std::fs::remove_file(&file);
            }
            ref_lock.rollback();
            super::transaction::prune_empty_parents(&self.git_dir, &file);
        }
        Ok(())
    }
}
