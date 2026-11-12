// SPDX-License-Identifier: MIT
//! Reading refs for object names: [`rgit_refs::files::FilesStore`] on the
//! common directory, per-worktree refs (`HEAD`, `refs/bisect/`, ...) from
//! the worktree's git directory, and reflogs (read here until #30).

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, MAX_SYMREF_DEPTH, RefStore, Target};
use rgit_repo::Setup;

/// Names that cannot leave the ref directories (no empty, `.` or `..`
/// components).
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
        && !name.contains('\\')
}

/// Read access to the refs of a repository.
#[derive(Debug, Clone)]
pub struct Refs {
    git_dir: PathBuf,
    common_dir: PathBuf,
    hash: HashKind,
    common: FilesStore,
    worktree: FilesStore,
}

impl Refs {
    pub fn new(setup: &Setup) -> Refs {
        let hash = setup.format.object_format;
        Refs {
            git_dir: setup.git_dir.clone(),
            common_dir: setup.common_dir.clone(),
            hash,
            common: FilesStore::new(&setup.common_dir, hash),
            worktree: FilesStore::new(&setup.git_dir, hash),
        }
    }

    /// Refs that live in the worktree's git directory rather than the
    /// common directory (`gitrepository-layout(5)`).
    fn per_worktree(name: &str) -> bool {
        !name.starts_with("refs/")
            || name.starts_with("refs/bisect/")
            || name.starts_with("refs/worktree/")
            || name.starts_with("refs/rewritten/")
    }

    fn store_for(&self, name: &str) -> &FilesStore {
        if Self::per_worktree(name) {
            &self.worktree
        } else {
            &self.common
        }
    }

    fn dir_for(&self, name: &str) -> &Path {
        if Self::per_worktree(name) {
            &self.git_dir
        } else {
            &self.common_dir
        }
    }

    fn read_raw(&self, name: &str) -> Option<Target> {
        if !valid_name(name) {
            return None;
        }
        self.store_for(name).read_raw(name).ok().flatten()
    }

    /// Resolve `name` to an object id, following symbolic refs.
    pub fn resolve(&self, name: &str) -> Option<ObjectId> {
        let mut cur = name.to_string();
        for _ in 0..=MAX_SYMREF_DEPTH {
            match self.read_raw(&cur)? {
                Target::Object(oid) => return Some(oid),
                Target::Symbolic(next) => cur = next,
            }
        }
        None
    }

    /// The ref `HEAD` points to, if it is symbolic.
    pub fn head_target(&self) -> Option<String> {
        match self.read_raw("HEAD")? {
            Target::Symbolic(t) => Some(t),
            Target::Object(_) => None,
        }
    }

    /// All refs below `prefix` (which ends in `/`) with their ids, sorted.
    pub fn list(&self, prefix: &str) -> Vec<(String, ObjectId)> {
        let items = self
            .store_for(prefix)
            .iter(&IterOptions::prefix(prefix))
            .unwrap_or_default();
        items
            .into_iter()
            .filter_map(|item| match item {
                IterItem::Ref(r) => {
                    let oid = self.resolve(&r.name)?;
                    Some((r.name, oid))
                }
                _ => None,
            })
            .collect()
    }

    /// The new ids recorded in the reflog of `name`, oldest first.
    pub fn reflog(&self, name: &str) -> Option<Vec<ObjectId>> {
        if !valid_name(name) {
            return None;
        }
        let file = self.dir_for(name).join("logs").join(name);
        let data = std::fs::read(file).ok()?;
        let hex_len = self.hash.hex_len();
        let mut out = Vec::new();
        for line in data.split(|&b| b == b'\n') {
            if line.len() < 2 * hex_len + 1 {
                continue;
            }
            if let Ok(oid) = ObjectId::from_hex(self.hash, &line[hex_len + 1..2 * hex_len + 1]) {
                out.push(oid);
            }
        }
        Some(out)
    }
}
