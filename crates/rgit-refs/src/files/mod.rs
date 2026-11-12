// SPDX-License-Identifier: MIT
//! The files backend: loose reference files below `$GIT_DIR` plus
//! `$GIT_DIR/packed-refs` (`gitrepository-layout`).
//!
//! Per-worktree references (#104) and case-insensitive file systems (#183)
//! are not handled yet: `$GIT_DIR` is also the common directory.

mod pack;
mod transaction;

pub use pack::PackFilter;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

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
    kind: HashKind,
    options: FilesOptions,
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
        Self {
            git_dir: git_dir.into(),
            kind,
            options,
        }
    }

    /// The repository directory.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The options.
    pub fn options(&self) -> &FilesOptions {
        &self.options
    }

    /// Path of the loose file for `name`.
    pub fn ref_path(&self, name: &str) -> PathBuf {
        self.git_dir.join(name)
    }

    /// Path of `packed-refs`.
    pub fn packed_refs_path(&self) -> PathBuf {
        self.git_dir.join("packed-refs")
    }

    /// Reads and parses `packed-refs`; an absent file is empty.
    pub fn packed_refs(&self) -> Result<PackedRefs, Error> {
        let path = self.packed_refs_path();
        match std::fs::read(&path) {
            Ok(data) => {
                PackedRefs::parse(&data, self.kind).map_err(|source| Error::Packed { path, source })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(PackedRefs::default()),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    fn read_loose(&self, name: &str) -> Result<LooseRead, Error> {
        read_loose_path(&self.ref_path(name), self.kind)
    }

    /// Reads only the loose file of `name` (no packed fallback).
    pub fn read_loose_ref(&self, name: &str) -> Result<Option<Target>, Error> {
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
        dir_rel: &str,
        out: &mut BTreeMap<String, LooseRead>,
    ) -> Result<(), Error> {
        let dir = self.git_dir.join(dir_rel.trim_end_matches('/'));
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
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
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
                self.collect_loose(&rel, out)?;
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
        match self.read_loose(name)? {
            LooseRead::Value(t) => Ok(Some(t)),
            LooseRead::Broken => Err(Error::Broken(name.to_owned())),
            LooseRead::Missing | LooseRead::NotAFile => {
                if !name.starts_with("refs/") {
                    return Ok(None);
                }
                Ok(self
                    .packed_refs()?
                    .find(name)
                    .map(|r| Target::Object(r.oid)))
            }
        }
    }

    fn iter(&self, options: &IterOptions) -> Result<Vec<IterItem>, Error> {
        let packed = self.packed_refs()?;
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
        self.collect_loose(walk_root, &mut loose)?;

        let mut items: BTreeMap<Vec<u8>, IterItem> = BTreeMap::new();
        for entry in packed.refs() {
            if !entry.name.starts_with(prefix) || !entry.name.starts_with("refs/") {
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
            items.insert(entry.name.as_bytes().to_vec(), item);
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
            items.insert(name.into_bytes(), item);
        }
        if options.include_root_refs {
            let entries = std::fs::read_dir(&self.git_dir).map_err(|source| Error::Io {
                path: self.git_dir.clone(),
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
        let prepared = transaction::prepare(self, updates, hooks)?;
        Ok(Box::new(prepared))
    }
}
