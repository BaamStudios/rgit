// SPDX-License-Identifier: MIT

//! All packs of an object directory (`objects/pack/`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use rgit_hash::{HashKind, ObjectId, Prefix};

use crate::midx::{MidxChain, OpenError};
use crate::{DeltaBaseCache, Error, ExternalBase, Object, Pack};

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
    /// The multi-pack index, when one is used (`core.multiPackIndex`).
    midx: Option<MidxChain>,
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
            midx: None,
        };
        set.rescan();
        set
    }

    /// [`open`](PackSet::open), also loading the multi-pack index of the
    /// directory. Objects are then looked up in the copy the MIDX selected
    /// first. Problems that make the MIDX unusable are appended to `errors`
    /// (print each as `error: <text>`); `Err` is a problem git dies on.
    pub fn open_with_midx(
        dir: impl Into<PathBuf>,
        kind: HashKind,
        errors: &mut Vec<String>,
    ) -> Result<PackSet, OpenError> {
        let mut set = PackSet::open(dir, kind);
        set.midx = MidxChain::open(&set.dir, kind, errors)?;
        if let Some(chain) = &set.midx {
            // git notices listed packs with a broken index when it looks up
            // an object through the MIDX; this reports them once.
            let packs = set.packs();
            for layer in chain.layers() {
                for name in layer.pack_names() {
                    let idx = set.dir.join(name);
                    if idx.exists() && !packs.iter().any(|p| p.index().path() == idx) {
                        errors.push(
                            Error::IndexUnavailable {
                                path: idx.with_extension("pack"),
                            }
                            .to_string(),
                        );
                    }
                }
            }
        }
        Ok(set)
    }

    /// The multi-pack index in use, if any.
    pub fn midx(&self) -> Option<&MidxChain> {
        self.midx.as_ref()
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
                // Like git, any `<name>.idx` next to a `<name>.pack` is a
                // pack, not only `pack-<hash>.idx` (t5300 copies packs as
                // `test-1-<hash>.idx`).
                if !name.ends_with(".idx") {
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

    /// Instead of writing `id` again: if a pack other than a cruft pack
    /// has it, update that `.pack` file's modification time (so that it
    /// counts as recent) and return `true`. A copy in a cruft pack does
    /// not count, because its age is recorded per object in the `.mtimes`
    /// file: the caller writes a loose copy instead, which makes the object
    /// recent (gitformat-pack(5), "cruft packs").
    pub fn freshen(&self, id: &ObjectId) -> bool {
        if !self.contains(id) && !(self.rescan_if_changed() && self.contains(id)) {
            return false;
        }
        for pack in self.packs() {
            if pack.contains(id) && !pack.is_cruft() {
                if let Ok(f) = std::fs::File::open(pack.path()) {
                    let _ = f.set_modified(SystemTime::now());
                }
                return true;
            }
        }
        false
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
        self.read_with_fallback(id, cache, report, &mut |_| None)
    }

    /// [`read`](PackSet::read), with `fallback` asked for delta bases that
    /// no pack can deliver (e.g. a loose copy of a corrupt packed base).
    pub fn read_with_fallback(
        &self,
        id: &ObjectId,
        cache: &mut DeltaBaseCache,
        report: Report<'_>,
        fallback: ExternalBase<'_>,
    ) -> Result<Option<Object>, Error> {
        match self.read_once(id, cache, report, 0, fallback)? {
            Some(o) => Ok(Some(o)),
            None if self.rescan_if_changed() => self.read_once(id, cache, report, 0, fallback),
            None => Ok(None),
        }
    }

    fn read_once(
        &self,
        id: &ObjectId,
        cache: &mut DeltaBaseCache,
        report: Report<'_>,
        depth: usize,
        fallback: ExternalBase<'_>,
    ) -> Result<Option<Object>, Error> {
        let packs = self.packs();
        let mut first_failure: Option<PathBuf> = None;
        if let Some((pack, offset)) = self.midx_location(id, &packs, report) {
            let mut external = |base: &ObjectId| -> Option<Object> {
                if depth >= 64 {
                    return None;
                }
                let mut tmp = DeltaBaseCache::new(0);
                match self.read_once(base, &mut tmp, &mut |_| {}, depth + 1, &mut |_| None) {
                    Ok(Some(o)) => Some(o),
                    _ => fallback(base),
                }
            };
            match pack.read_at(offset, cache, &mut external) {
                Ok(o) => return Ok(Some(o)),
                Err(e) => {
                    report(&e);
                    first_failure.get_or_insert_with(|| pack.path().to_path_buf());
                }
            }
        }
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
                    match self.read_once(base, &mut tmp, &mut |_| {}, depth + 1, &mut |_| None) {
                        Ok(Some(o)) => Some(o),
                        _ => fallback(base),
                    }
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

    /// The pack and offset the multi-pack index selected for `id`, if the
    /// MIDX lists it and that pack is loaded. A listed pack whose index
    /// cannot be loaded is reported (`packfile ... index unavailable`).
    fn midx_location(
        &self,
        id: &ObjectId,
        packs: &[Arc<Pack>],
        report: Report<'_>,
    ) -> Option<(Arc<Pack>, u64)> {
        let (layer, pos) = self.midx.as_ref()?.lookup(id)?;
        let pack_id = layer.pack_id(pos).ok()?;
        let offset = layer.offset(pos).ok()?;
        let name = layer.pack_name(pack_id)?;
        let idx = self.dir.join(name);
        if let Some(p) = packs.iter().find(|p| p.index().path() == idx) {
            return Some((p.clone(), offset));
        }
        if idx.exists() {
            report(&Error::IndexUnavailable {
                path: idx.with_extension("pack"),
            });
        }
        None
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
