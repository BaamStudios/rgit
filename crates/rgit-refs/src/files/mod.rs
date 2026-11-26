// SPDX-License-Identifier: MIT
//! The files backend: loose reference files below `$GIT_DIR` plus
//! `$GIT_DIR/packed-refs` (`gitrepository-layout`).
//!
//! In a linked worktree ([`FilesStore::with_worktree`]) the per-worktree
//! references (`HEAD`, other root refs, `refs/bisect/`, `refs/worktree/`,
//! `refs/rewritten/`) live in the worktree's own git directory and all
//! others in the common directory; `main-worktree/<ref>` and
//! `worktrees/<id>/<ref>` name another worktree's per-worktree references
//! (`git-worktree(1)`, REFS). Case-insensitive file systems (#183) are not
//! handled yet.
//!
//! A repository in the reftable format (`extensions.refStorage`,
//! recognised by its `reftable/` directory) keeps no reference files:
//! [`FilesStore`] then forwards to [`crate::reftable::backend`], so that
//! callers work with either format through the same type.

mod pack;
mod transaction;

pub use pack::PackFilter;
pub(crate) use transaction::check_old;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rgit_fs::{FsyncMethod, SharedRepository, Timeout};
use rgit_hash::HashKind;

use crate::error::{Error, TransactionError};
use crate::loose;
use crate::name::{RefNameFlags, is_root_ref, is_valid_refname};
use crate::packed::PackedRefs;
use crate::store::{
    IterItem, IterOptions, LogAllRefUpdates, MAX_SYMREF_DEPTH, PreparedTransaction, RefHooks,
    RefStore, RefUpdate,
};
use crate::target::{Peeled, Reference, Resolved, Source, Target};

/// Settings of the files backend. Reading them from the configuration is
/// the caller's job.
#[derive(Clone, Copy, Debug)]
pub struct FilesOptions {
    /// `core.logAllRefUpdates`.
    pub log_all_ref_updates: LogAllRefUpdates,
    /// `core.filesRefLockTimeout` (default 100 ms).
    pub ref_lock_timeout: Timeout,
    /// `core.packedRefsTimeout` (default 1000 ms).
    pub packed_refs_timeout: Timeout,
    /// `core.sharedRepository`, applied to new files.
    pub shared: SharedRepository,
    /// `core.fsync` includes `reference`: the method to use.
    pub fsync: Option<FsyncMethod>,
}

impl Default for FilesOptions {
    fn default() -> Self {
        Self {
            log_all_ref_updates: LogAllRefUpdates::True,
            ref_lock_timeout: Timeout::FILES_REF_DEFAULT,
            packed_refs_timeout: Timeout::PACKED_REFS_DEFAULT,
            shared: SharedRepository::Umask,
            fsync: None,
        }
    }
}

/// References stored as files.
#[derive(Clone, Debug)]
pub struct FilesStore {
    git_dir: PathBuf,
    /// The git directory of a linked worktree (per-worktree references).
    worktree_dir: Option<PathBuf>,
    kind: HashKind,
    options: FilesOptions,
    /// The last `packed-refs` read for lookups, shared by clones.
    packed_cache: Arc<Mutex<PackedCache>>,
    /// The repository keeps its references in a reftable stack
    /// ([`crate::reftable::backend`]); the methods forward to it.
    pub(crate) reftable: bool,
}

/// The parsed `packed-refs` with the stamp of the file it came from.
type PackedCache = Option<(PackedStamp, Arc<PackedRefs>)>;

/// What identifies one version of `packed-refs` on disk: a rewrite renames
/// a new file over it, which changes these (as with git's own snapshot of
/// the file, a change that keeps all of them is not noticed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PackedStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    dev: u64,
}

impl PackedStamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            ino: meta.ino(),
            #[cfg(unix)]
            dev: meta.dev(),
        }
    }
}

/// A loose file read from disk.
enum LooseRead {
    Missing,
    /// A directory or something else that is not a reference file.
    NotAFile,
    Broken,
    Value(Target),
}

impl FilesStore {
    /// A store for the repository at `git_dir`.
    pub fn new(git_dir: impl Into<PathBuf>, kind: HashKind) -> Self {
        Self::with_options(git_dir, kind, FilesOptions::default())
    }

    /// A store with explicit options.
    pub fn with_options(
        git_dir: impl Into<PathBuf>,
        kind: HashKind,
        options: FilesOptions,
    ) -> Self {
        let git_dir = git_dir.into();
        Self {
            reftable: crate::reftable::backend::uses_reftable(&git_dir),
            git_dir,
            worktree_dir: None,
            kind,
            options,
            packed_cache: Arc::default(),
        }
    }

    /// Per-worktree references of a linked worktree live in `dir` (its git
    /// directory); the store's own directory is the common directory. A
    /// `dir` equal to the common directory changes nothing.
    pub fn with_worktree(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let same = dir == self.git_dir
            || matches!(
                (dir.canonicalize(), self.git_dir.canonicalize()),
                (Ok(a), Ok(b)) if a == b
            );
        self.worktree_dir = (!same).then_some(dir);
        self
    }

    /// The repository (common) directory.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Whether the store belongs to a linked worktree.
    pub(crate) fn linked(&self) -> bool {
        self.worktree_dir.is_some()
    }

    /// The directory holding the loose file of `name` and the name relative
    /// to it.
    pub(crate) fn locate<'n>(&self, name: &'n str) -> (PathBuf, &'n str) {
        if let Some(rest) = name.strip_prefix("main-worktree/") {
            return (self.git_dir.clone(), rest);
        }
        if let Some(rest) = name.strip_prefix("worktrees/") {
            if let Some((id, r)) = rest.split_once('/') {
                return (self.git_dir.join("worktrees").join(id), r);
            }
        }
        match &self.worktree_dir {
            Some(wt) if crate::name::is_per_worktree(name) => (wt.clone(), name),
            _ => (self.git_dir.clone(), name),
        }
    }

    /// Names whose loose files are in another directory than the common
    /// one, which `packed-refs` does not hold for them.
    fn outside_common(&self, name: &str) -> bool {
        name.starts_with("main-worktree/")
            || name.starts_with("worktrees/")
            || (self.linked() && crate::name::is_per_worktree(name))
    }

    /// The options.
    pub fn options(&self) -> &FilesOptions {
        &self.options
    }

    /// Path of the loose file for `name`.
    pub fn ref_path(&self, name: &str) -> PathBuf {
        let (dir, rel) = self.locate(name);
        dir.join(rgit_bstr::to_os(rel))
    }

    /// Path of `packed-refs`.
    pub fn packed_refs_path(&self) -> PathBuf {
        self.git_dir.join("packed-refs")
    }

    /// Reads and parses `packed-refs`; an absent file is empty.
    pub fn packed_refs(&self) -> Result<PackedRefs, Error> {
        if self.reftable {
            return Ok(PackedRefs::default());
        }
        let path = self.packed_refs_path();
        match std::fs::read(&path) {
            Ok(data) => {
                PackedRefs::parse(&data, self.kind).map_err(|source| Error::Packed { path, source })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(PackedRefs::default()),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    /// `packed-refs` for lookups: parsed once and reused while the file
    /// on disk stays the same (a fetch looks up every advertised tag).
    /// Transactions read the file themselves under its lock.
    fn packed_snapshot(&self) -> Result<Arc<PackedRefs>, Error> {
        let path = self.packed_refs_path();
        let stamp = match std::fs::metadata(&path) {
            Ok(meta) => PackedStamp::of(&meta),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.forget_packed();
                return Ok(Arc::default());
            }
            Err(source) => return Err(Error::Io { path, source }),
        };
        if let Ok(cache) = self.packed_cache.lock() {
            if let Some((s, refs)) = cache.as_ref() {
                if *s == stamp {
                    return Ok(Arc::clone(refs));
                }
            }
        }
        // Read after the stat: a file replaced in between is cached with
        // the older stamp and read again next time.
        let refs = Arc::new(self.packed_refs()?);
        if let Ok(mut cache) = self.packed_cache.lock() {
            *cache = Some((stamp, Arc::clone(&refs)));
        }
        Ok(refs)
    }

    /// Drops the cached `packed-refs` (after writing it).
    pub(crate) fn forget_packed(&self) {
        if let Ok(mut cache) = self.packed_cache.lock() {
            *cache = None;
        }
    }

    fn read_loose(&self, name: &str) -> Result<LooseRead, Error> {
        read_loose_path(&self.ref_path(name), self.kind)
    }

    /// The value of the loose file of `name`, `None` when there is no
    /// such file or it holds no reference.
    pub(crate) fn loose_value(&self, name: &str) -> Option<Target> {
        match self.read_loose(name) {
            Ok(LooseRead::Value(t)) => Some(t),
            _ => None,
        }
    }

    /// Reads only the loose file of `name` (no packed fallback).
    pub fn read_loose_ref(&self, name: &str) -> Result<Option<Target>, Error> {
        if self.reftable && !crate::name::is_pseudo_ref(name) {
            return self.rt_read_raw(name);
        }
        match self.read_loose(name)? {
            LooseRead::Value(t) => Ok(Some(t)),
            LooseRead::Broken => Err(Error::Broken(name.to_owned())),
            LooseRead::Missing | LooseRead::NotAFile => Ok(None),
        }
    }

    fn read_raw_with(&self, name: &str, packed: &PackedRefs) -> Result<Option<Target>, Error> {
        match self.read_loose(name)? {
            LooseRead::Value(t) => Ok(Some(t)),
            LooseRead::Broken => Err(Error::Broken(name.to_owned())),
            LooseRead::Missing | LooseRead::NotAFile if self.outside_common(name) => Ok(None),
            LooseRead::Missing | LooseRead::NotAFile => {
                Ok(packed.find(name).map(|r| Target::Object(r.oid)))
            }
        }
    }

    /// Like [`RefStore::resolve`] but with an already loaded `packed-refs`.
    fn resolve_with(&self, name: &str, packed: &PackedRefs) -> Result<Option<Resolved>, Error> {
        let mut chain = vec![name.to_owned()];
        let mut current = name.to_owned();
        for _ in 0..MAX_SYMREF_DEPTH {
            match self.read_raw_with(&current, packed)? {
                None if chain.len() == 1 => return Ok(None),
                None => return Ok(Some(Resolved { chain, oid: None })),
                Some(Target::Object(oid)) => {
                    return Ok(Some(Resolved {
                        chain,
                        oid: Some(oid),
                    }));
                }
                Some(Target::Symbolic(next)) => {
                    chain.push(next.clone());
                    current = next;
                }
            }
        }
        Err(Error::TooDeep(name.to_owned()))
    }

    /// Whether `name` exists as a loose file or packed entry (without
    /// resolving it).
    pub fn exists(&self, name: &str) -> Result<bool, Error> {
        Ok(self.read_raw(name)?.is_some())
    }

    fn collect_loose(
        &self,
        base: &Path,
        dir_rel: &str,
        out: &mut BTreeMap<String, LooseRead>,
    ) -> Result<(), Error> {
        let dir = base.join(rgit_bstr::to_os(dir_rel.trim_end_matches('/')));
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.kind() == io::ErrorKind::NotADirectory =>
            {
                return Ok(());
            }
            Err(source) => return Err(Error::Io { path: dir, source }),
        };
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io {
                path: dir.clone(),
                source,
            })?;
            let file_name = rgit_bstr::from_os_string(entry.file_name());
            let rel = format!("{}/{}", dir_rel.trim_end_matches('/'), file_name);
            let file_type = entry.file_type().map_err(|source| Error::Io {
                path: entry.path(),
                source,
            })?;
            let is_dir = if file_type.is_symlink() {
                std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
                    && !is_ref_symlink(&entry.path())
            } else {
                file_type.is_dir()
            };
            if is_dir {
                self.collect_loose(base, &rel, out)?;
            } else if !file_name.ends_with(".lock") {
                let value = read_loose_path(&entry.path(), self.kind)?;
                if !matches!(value, LooseRead::Missing | LooseRead::NotAFile) {
                    out.insert(rel, value);
                }
            }
        }
        Ok(())
    }

    fn reference_from(
        &self,
        name: String,
        target: Target,
        source: Source,
        packed: &PackedRefs,
    ) -> IterItem {
        let (oid, peeled) = match &target {
            Target::Object(oid) => {
                let peeled = match source {
                    Source::Packed => packed
                        .find(&name)
                        .map(|e| packed.peeled(e))
                        .unwrap_or_default(),
                    Source::Loose => Peeled::Unknown,
                };
                (Some(*oid), peeled)
            }
            Target::Symbolic(_) => match self.resolve_with(&name, packed) {
                Ok(Some(r)) => (r.oid, Peeled::Unknown),
                Ok(None) | Err(Error::TooDeep(_)) => (None, Peeled::Unknown),
                Err(_) => return IterItem::Broken(name),
            },
        };
        IterItem::Ref(Reference {
            name,
            target,
            oid,
            peeled,
            source,
        })
    }
}

/// A legacy symbolic link whose target starts with `refs/` is a symbolic
/// reference (`git-update-ref`, NOTES).
fn is_ref_symlink(path: &Path) -> bool {
    std::fs::read_link(path).is_ok_and(|t| t.to_str().is_some_and(|s| s.starts_with("refs/")))
}

fn read_loose_path(path: &Path, kind: HashKind) -> Result<LooseRead, Error> {
    if let Ok(target) = std::fs::read_link(path) {
        if let Some(s) = target.to_str() {
            if s.starts_with("refs/") {
                return Ok(if is_valid_refname(s.as_bytes(), RefNameFlags::ONELEVEL) {
                    LooseRead::Value(Target::Symbolic(s.to_owned()))
                } else {
                    LooseRead::Broken
                });
            }
        }
    }
    match std::fs::read(path) {
        Ok(data) => Ok(match loose::parse(&data, kind) {
            Some(t) => LooseRead::Value(t),
            None => LooseRead::Broken,
        }),
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.kind() == io::ErrorKind::NotADirectory =>
        {
            Ok(LooseRead::Missing)
        }
        Err(e) if e.kind() == io::ErrorKind::IsADirectory => Ok(LooseRead::NotAFile),
        Err(source) => {
            if path.is_dir() {
                Ok(LooseRead::NotAFile)
            } else {
                Err(Error::Io {
                    path: path.to_owned(),
                    source,
                })
            }
        }
    }
}

impl RefStore for FilesStore {
    fn hash_kind(&self) -> HashKind {
        self.kind
    }

    fn read_raw(&self, name: &str) -> Result<Option<Target>, Error> {
        // `FETCH_HEAD` and `MERGE_HEAD` are files in every format.
        if self.reftable && !crate::name::is_pseudo_ref(name) {
            return self.rt_read_raw(name);
        }
        match self.read_loose(name)? {
            LooseRead::Value(t) => Ok(Some(t)),
            LooseRead::Broken => Err(Error::Broken(name.to_owned())),
            LooseRead::Missing | LooseRead::NotAFile => {
                if !name.starts_with("refs/") || self.outside_common(name) {
                    return Ok(None);
                }
                Ok(self
                    .packed_snapshot()?
                    .find(name)
                    .map(|r| Target::Object(r.oid)))
            }
        }
    }

    fn iter(&self, options: &IterOptions) -> Result<Vec<IterItem>, Error> {
        if self.reftable {
            return self.rt_iter(options);
        }
        let packed = self.packed_snapshot()?;
        let prefix = options.prefix.as_str();
        let mut loose = BTreeMap::new();
        // Walk only the directory that can contain the prefix.
        let walk_root = if prefix.starts_with("refs/") {
            match prefix.rfind('/') {
                Some(i) => &prefix[..i],
                None => "refs",
            }
        } else {
            "refs"
        };
        self.collect_loose(&self.git_dir, walk_root, &mut loose)?;
        if let Some(wt) = &self.worktree_dir {
            // The common directory's per-worktree references belong to the
            // main worktree; this worktree's own are in its git directory.
            loose.retain(|name, _| !crate::name::is_per_worktree(name));
            for dir in ["refs/bisect", "refs/worktree", "refs/rewritten"] {
                if dir.starts_with(walk_root) || walk_root.starts_with(dir) {
                    let root = if walk_root.len() > dir.len() {
                        walk_root
                    } else {
                        dir
                    };
                    self.collect_loose(wt, root, &mut loose)?;
                }
            }
        }

        let mut items: BTreeMap<Vec<u8>, IterItem> = BTreeMap::new();
        for entry in packed.refs() {
            if !entry.name.starts_with(prefix)
                || !entry.name.starts_with("refs/")
                || self.outside_common(&entry.name)
            {
                continue;
            }
            let item = if is_valid_refname(entry.name.as_bytes(), RefNameFlags::STRICT) {
                self.reference_from(
                    entry.name.clone(),
                    Target::Object(entry.oid),
                    Source::Packed,
                    &packed,
                )
            } else {
                IterItem::BadName(entry.name.clone())
            };
            items.insert(rgit_bstr::to_bytes(&entry.name).into_owned(), item);
        }
        for (name, value) in loose {
            if !name.starts_with(prefix) {
                continue;
            }
            let item = if !is_valid_refname(name.as_bytes(), RefNameFlags::STRICT) {
                IterItem::BadName(name.clone())
            } else {
                match value {
                    LooseRead::Value(t) => {
                        self.reference_from(name.clone(), t, Source::Loose, &packed)
                    }
                    _ => IterItem::Broken(name.clone()),
                }
            };
            items.insert(rgit_bstr::to_bytes(&name).into_owned(), item);
        }
        if options.include_root_refs {
            let root = self.worktree_dir.as_ref().unwrap_or(&self.git_dir);
            let entries = std::fs::read_dir(root).map_err(|source| Error::Io {
                path: root.clone(),
                source,
            })?;
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if !is_root_ref(&name) || !name.starts_with(prefix) {
                    continue;
                }
                match read_loose_path(&entry.path(), self.kind)? {
                    LooseRead::Value(t) => {
                        let item = self.reference_from(name.clone(), t, Source::Loose, &packed);
                        items.insert(name.into_bytes(), item);
                    }
                    LooseRead::Broken => {
                        items.insert(name.clone().into_bytes(), IterItem::Broken(name));
                    }
                    _ => {}
                }
            }
        }
        Ok(items.into_values().collect())
    }

    fn prepare<'s>(
        &'s self,
        updates: Vec<RefUpdate>,
        hooks: &mut dyn RefHooks,
    ) -> Result<Box<dyn PreparedTransaction + 's>, TransactionError> {
        if self.reftable {
            let prepared = crate::reftable::backend::prepare(self, updates, hooks)?;
            return Ok(Box::new(prepared));
        }
        let prepared = transaction::prepare(self, updates, hooks)?;
        Ok(Box::new(prepared))
    }
}
