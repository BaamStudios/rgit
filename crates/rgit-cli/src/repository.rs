// SPDX-License-Identifier: MIT
//! A set-up repository for plumbing that lists trees and the index:
//! [`crate::repo_objects::Repo`] (configuration, object database with
//! packs and alternates) plus revision lookup, abbreviations and output
//! settings.

use std::cell::OnceCell;
use std::ops::Deref;

use std::path::PathBuf;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_index::{Index, ReadOptions, Time};
use rgit_object::{Object, ObjectKind};
use rgit_odb::ObjectDatabase;
use rgit_worktree::stat::{CheckStat, Checker, StatOptions, index_mtime};

use crate::repo::{self, Mode};
use crate::repo_objects;
use crate::revision::Resolver;
use crate::{Context, Die};

/// Shortest abbreviation git prints.
pub const MIN_ABBREV: usize = 4;

/// The pathspec switches from the environment and the global options.
pub fn pathspec_globals(ctx: &Context) -> rgit_pathspec::GlobalFlags {
    let mut g = rgit_pathspec::GlobalFlags::from_env().unwrap_or_default();
    g.literal |= ctx.literal_pathspecs;
    g.glob |= ctx.glob_pathspecs;
    g.noglob |= ctx.noglob_pathspecs;
    g.icase |= ctx.icase_pathspecs;
    g
}

/// The commit checked out in the submodule (or nested repository) at the
/// directory `dir`: its `.git` directory or `gitdir:` file, then `HEAD`.
pub fn submodule_head(dir: &std::path::Path, hash: HashKind) -> Option<rgit_hash::ObjectId> {
    use rgit_refs::RefStore;
    let dot_git = dir.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    let git_dir = if meta.is_dir() {
        dot_git
    } else {
        let data = std::fs::read(&dot_git).ok()?;
        let rest = data.strip_prefix(b"gitdir:")?;
        let target = std::str::from_utf8(rest).ok()?.trim();
        dir.join(target)
    };
    rgit_refs::files::FilesStore::new(git_dir, hash)
        .resolve_oid("HEAD")
        .ok()
        .flatten()
}

/// A repository opened by a command.
pub struct Repo {
    inner: repo_objects::Repo,
    auto_abbrev: OnceCell<usize>,
}

impl Deref for Repo {
    type Target = repo_objects::Repo;

    fn deref(&self) -> &repo_objects::Repo {
        &self.inner
    }
}

impl Repo {
    /// Set up the repository and open its configuration and objects.
    pub fn open(ctx: &Context, mode: Mode) -> Result<Repo, Die> {
        let setup = repo::setup(ctx, mode)?;
        Ok(Repo {
            inner: repo_objects::Repo::open(ctx, setup)?,
            auto_abbrev: OnceCell::new(),
        })
    }

    /// The object database (loose objects, packs, alternates).
    pub fn odb(&self) -> &ObjectDatabase {
        &self.inner.odb
    }

    /// Resolve an object name (`gitrevisions(7)`); `None` when it does not
    /// name an object. Errors git reports on the way (`expected tree type,
    /// ...`) are printed.
    pub fn resolve(&mut self, name: &str) -> Option<ObjectId> {
        Resolver::new(&mut self.inner)
            .resolve(name)
            .ok()
            .map(|r| r.oid)
    }

    /// Peel tags and commits down to an object of type `kind`.
    pub fn peel_to(&mut self, id: ObjectId, kind: ObjectKind) -> Option<ObjectId> {
        Resolver::new(&mut self.inner).peel_to(id, kind, None)
    }

    /// Read and parse an object.
    pub fn read_object(&self, id: &ObjectId) -> Option<Object> {
        self.inner.odb.read_object(id).ok().flatten()
    }

    /// The current directory relative to the top of the work tree, with a
    /// trailing `/`, or empty.
    pub fn prefix(&self) -> Vec<u8> {
        self.setup
            .prefix
            .as_ref()
            .map(|p| p.as_encoded_bytes().to_vec())
            .unwrap_or_default()
    }

    /// A command line path relative to the current directory (or absolute)
    /// as a path relative to the top of the work tree: `.` and empty
    /// components dropped, `..` resolved, a trailing `/` kept. The error is
    /// the path when it lies outside the work tree.
    pub fn prefix_path(&self, arg: &[u8]) -> Result<Vec<u8>, ()> {
        let mut full: Vec<u8> = Vec::new();
        if arg.first() == Some(&b'/') {
            let top = self
                .setup
                .work_tree
                .as_ref()
                .map(|w| w.as_os_str().as_encoded_bytes().to_vec())
                .ok_or(())?;
            let rest = arg.strip_prefix(top.as_slice()).ok_or(())?;
            if !(rest.is_empty() || rest.starts_with(b"/")) {
                return Err(());
            }
            full.extend_from_slice(rest);
        } else {
            full.extend_from_slice(&self.prefix());
            full.extend_from_slice(arg);
        }
        let trailing = full.ends_with(b"/") && !full.is_empty();
        let mut parts: Vec<&[u8]> = Vec::new();
        for c in full.split(|&b| b == b'/') {
            match c {
                b"" | b"." => {}
                b".." => {
                    parts.pop().ok_or(())?;
                }
                c => parts.push(c),
            }
        }
        let mut out = parts.join(&b'/');
        if trailing && !out.is_empty() {
            out.push(b'/');
        }
        Ok(out)
    }

    fn config_bool(&self, key: &str, default: bool) -> bool {
        self.config.bool(key).ok().flatten().unwrap_or(default)
    }

    /// What stat comparisons trust (`core.trustCtime`, `core.checkStat`,
    /// `core.fileMode`, `core.symlinks`).
    pub fn stat_options(&self) -> StatOptions {
        let check_stat = match self.config.string("core.checkstat") {
            Ok(Some(v)) => CheckStat::from_config(v.as_ref()).unwrap_or_default(),
            _ => CheckStat::default(),
        };
        StatOptions {
            trust_ctime: self.config_bool("core.trustctime", true),
            check_stat,
            file_mode: self.config_bool("core.filemode", true),
            symlinks: self.config_bool("core.symlinks", true),
            ..StatOptions::default()
        }
    }

    /// The index file (`$GIT_INDEX_FILE` or `<git-dir>/index`).
    pub fn index_path(&self) -> PathBuf {
        rgit_index::index_path(&self.setup.git_dir)
    }

    /// Read the index; also returns the index file's mtime for the racy-git
    /// check.
    pub fn read_index(&self) -> Result<(Index, Option<Time>), Die> {
        let path = self.index_path();
        let mtime = index_mtime(&path).ok().flatten();
        let index = Index::read_file(&path, self.hash, &ReadOptions::default())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok((index, mtime))
    }

    /// The top of the work tree, or git's error for commands that need one.
    pub fn work_tree(&self) -> Result<PathBuf, Die> {
        self.setup
            .require_work_tree()
            .map(|p| p.to_path_buf())
            .map_err(repo::die)
    }

    /// A checker comparing index entries with the work tree.
    pub fn checker(&self, index_mtime: Option<Time>) -> Result<Checker, Die> {
        Ok(Checker::new(
            self.work_tree()?,
            self.hash,
            self.stat_options(),
            index_mtime,
        ))
    }

    /// `core.quotePath` (default true).
    pub fn quote_path(&self) -> bool {
        self.config
            .bool("core.quotepath")
            .ok()
            .flatten()
            .unwrap_or(true)
    }

    /// The default abbreviation length: `core.abbrev`, else computed from
    /// the number of objects.
    pub fn default_abbrev(&self) -> usize {
        *self
            .auto_abbrev
            .get_or_init(|| crate::revision::default_abbrev(&self.inner))
    }

    /// `id` abbreviated to at least `len` digits (full when `len` is 0 or
    /// at least the full length), unique among all objects.
    pub fn abbrev(&self, id: &ObjectId, len: usize) -> String {
        let hex = id.to_hex();
        if len == 0 || len >= hex.len() {
            return hex;
        }
        let mut len = len.max(MIN_ABBREV);
        while len < hex.len() {
            let Ok(prefix) = Prefix::new(*id, len) else {
                break;
            };
            let mut other = false;
            for store in self.odb().stores() {
                let _ = store.for_each_prefix_match(&prefix, &mut |o| other |= o != *id);
                if other {
                    break;
                }
            }
            if !other {
                break;
            }
            len += 1;
        }
        hex[..len].to_string()
    }
}
