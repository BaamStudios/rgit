// SPDX-License-Identifier: MIT

//! All packs of an object directory (`objects/pack/`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use rgit_hash::{HashKind, ObjectId, Prefix};

use crate::{DeltaBaseCache, Error, Object, Pack};

/// The packs in one `objects/pack` directory.
///
/// Packs are searched newest first (by `.pack` modification time), so
/// objects from recent fetches are found quickly. When an object is not
/// found and the directory changed since the last scan, it is scanned
/// again, because another process may have repacked meanwhile.
///
/// A pack whose `.idx` cannot be parsed is left out (its error is kept in
/// [`PackSet::scan_errors`]). A pack whose `.pack` cannot be opened is
/// treated as not containing any object; its error is reported.
pub struct PackSet {
    dir: PathBuf,
    kind: HashKind,
    state: RwLock<State>,
}

#[derive(Default)]
struct State {
    packs: Vec<Arc<Pack>>,
    errors: Vec<Error>,
    /// Modification time of the directory at the last scan.
    dir_mtime: Option<SystemTime>,
}

/// Callback for errors that do not stop a lookup: a corrupt copy of an
/// object when another copy may still be found, or a pack that fails to
/// open. git prints these as `error: <message>`.
pub type Report<'a> = &'a mut dyn FnMut(&Error);

impl PackSet {
    /// Scan `dir` (usually `<git-dir>/objects/pack`). A missing directory
    /// gives an empty set.
    pub fn open(dir: impl Into<PathBuf>, kind: HashKind) -> PackSet {
        let set = PackSet {
            dir: dir.into(),
            kind,
            state: RwLock::new(State::default()),
        };
        set.rescan();
        set
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Hash algorithm of the repository.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// The packs, newest first.
    pub fn packs(&self) -> Vec<Arc<Pack>> {
        self.state.read().expect("lock").packs.clone()
    }

    /// Errors of `.idx` files that could not be opened in the last scan.
    pub fn scan_errors(&self) -> Vec<Error> {
        self.state.read().expect("lock").errors.clone()
    }

    /// Scan the directory again: open new packs, forget removed ones, keep
    /// the already opened ones. Returns whether the set changed.
    pub fn rescan(&self) -> bool {
        let dir_mtime = dir_mtime(&self.dir);
        let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("pack-") || !name.ends_with(".idx") {
                    continue;
                }
                let pack = p.with_extension("pack");
                let Ok(meta) = std::fs::metadata(&pack) else {
                    continue;
                };
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                found.push((mtime, p));
            }
        }
        // Newest first; ties by name for a stable order.
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        let mut state = self.state.write().expect("lock");
        let mut old: HashMap<PathBuf, Arc<Pack>> = state
            .packs
            .drain(..)
            .map(|p| (p.index().path().to_path_buf(), p))
            .collect();
        let before: BTreeSet<PathBuf> = old.keys().cloned().collect();
        let mut packs = Vec::with_capacity(found.len());
        let mut errors = Vec::new();
        for (_, idx) in found {
            if let Some(p) = old.remove(&idx) {
                packs.push(p);
                continue;
            }
            match Pack::open(&idx, self.kind) {
                Ok(p) => packs.push(Arc::new(p)),
                Err(e) => errors.push(e),
            }
        }
        let after: BTreeSet<PathBuf> = packs
            .iter()
            .map(|p| p.index().path().to_path_buf())
            .collect();
        state.packs = packs;
        state.errors = errors;
        state.dir_mtime = dir_mtime;
        before != after
    }

    /// [`rescan`](PackSet::rescan) if the directory's modification time
    /// changed since the last scan (a single `stat` otherwise).
    pub fn rescan_if_changed(&self) -> bool {
        if dir_mtime(&self.dir) == self.state.read().expect("lock").dir_mtime {
            return false;
        }
        self.rescan()
    }

    /// Whether any pack lists `id` (without reading it).
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.state
            .read()
            .expect("lock")
            .packs
            .iter()
            .any(|p| p.contains(id))
    }

    /// Read `id` from the first pack that has a readable copy.
    ///
    /// - `Ok(None)`: no pack lists it, also after
    ///   [`rescan_if_changed`](PackSet::rescan_if_changed).
    /// - `Err(Error::Corrupt)`: packs list it but no copy can be read; the
    ///   reasons were passed to `report`. The caller may still find a loose
    ///   copy.
    ///
    /// `REF_DELTA` bases missing from a pack are looked up in the other
    /// packs.
    pub fn read(
        &self,
        id: &ObjectId,
        cache: &mut DeltaBaseCache,
        report: Report<'_>,
    ) -> Result<Option<Object>, Error> {
        match self.read_once(id, cache, report, 0)? {
            Some(o) => Ok(Some(o)),
            None if self.rescan_if_changed() => self.read_once(id, cache, report, 0),
            None => Ok(None),
        }
    }

    fn read_once(
        &self,
        id: &ObjectId,
        cache: &mut DeltaBaseCache,
        report: Report<'_>,
        depth: usize,
    ) -> Result<Option<Object>, Error> {
        let packs = self.packs();
        let mut first_failure: Option<PathBuf> = None;
        for pack in &packs {
            let Some(pos) = pack.index().lookup(id) else {
                continue;
            };
            if let Err(e) = pack.data() {
                // An unusable pack hides its objects instead of making them
                // corrupt.
                report(&e);
                continue;
            }
            let result = pack.index().offset_at(pos).and_then(|offset| {
                let mut external = |base: &ObjectId| -> Option<Object> {
                    if depth >= 64 {
                        return None;
                    }
                    let mut tmp = DeltaBaseCache::new(0);
                    self.read_once(base, &mut tmp, &mut |_| {}, depth + 1)
                        .ok()
                        .flatten()
                };
                pack.read_at(offset, cache, &mut external)
            });
            match result {
                Ok(o) => return Ok(Some(o)),
                Err(e) => {
                    report(&e);
                    first_failure.get_or_insert_with(|| pack.path().to_path_buf());
                }
            }
        }
        match first_failure {
            Some(path) => Err(Error::Corrupt { id: *id, path }),
            None => Ok(None),
        }
    }

    /// All ids starting with `prefix` across all packs, sorted, without
    /// duplicates, at most `limit` of them.
    pub fn lookup_prefix(&self, prefix: &Prefix, limit: usize) -> Vec<ObjectId> {
        let mut out = BTreeSet::new();
        for pack in self.packs() {
            let idx = pack.index();
            for pos in idx.lookup_prefix(prefix) {
                out.insert(idx.id_at(pos));
            }
        }
        out.into_iter().take(limit).collect()
    }

    /// Every id in every pack, sorted, without duplicates.
    pub fn all_ids(&self) -> Vec<ObjectId> {
        let mut out = BTreeSet::new();
        for pack in self.packs() {
            out.extend(pack.index().ids());
        }
        out.into_iter().collect()
    }
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

impl std::fmt::Debug for PackSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackSet")
            .field("dir", &self.dir)
            .field("packs", &self.packs().len())
            .finish()
    }
}
