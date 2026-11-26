// SPDX-License-Identifier: MIT
//! rerere in the repository (Documentation/git-rerere.adoc,
//! config/rerere.adoc, rerere-options.adoc): [`rgit_rerere`] on the index
//! and work tree, and the hooks the merge commands call.
//!
//! - [`auto`]: the pass after a conflicted merge (merge, cherry-pick,
//!   revert, rebase, am, stash) and before a commit;
//! - [`clear`]: `rerere clear`, run by `rebase` and `am` when they skip or
//!   abort.
//!
//! rerere is on with `rerere.enabled`, or while `rr-cache` exists in the
//! git directory and `rerere.enabled` is not set. `rr-cache` lives in the
//! common directory, `MERGE_RR` in the git directory of the work tree.

use std::path::{Path, PathBuf};

use rgit_diff::merge::{ConflictStyle, MergeOptions};
use rgit_fs::{LockFile, LockOptions};
use rgit_index::{AddOptions, Index, Stage};
use rgit_merge::PathMerge;
use rgit_rerere::{Cache, Conflicted, Host, RrEntry};

use crate::commands::read_tree::lock_failed;
use crate::messages::rerere as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub(crate) fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(rel).as_ref())
    }
}

/// rerere set up for a repository.
pub(crate) struct Rerere {
    pub repo: Repo,
    pub cache: Cache,
    git_dir: PathBuf,
}

impl Rerere {
    /// Opens rerere in the current repository; `None` when it is off.
    /// Creates `rr-cache` when `rerere.enabled` is true.
    pub fn open(ctx: &Context) -> Result<Option<Rerere>, Die> {
        let repo = Repo::open(ctx, Mode::Required)?;
        Rerere::with_repo(repo)
    }

    pub fn with_repo(repo: Repo) -> Result<Option<Rerere>, Die> {
        let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
        let root = repo.setup.cwd.join(&repo.setup.common_dir).join("rr-cache");
        let enabled = match repo.config.bool("rerere.enabled") {
            Ok(Some(b)) => b,
            Ok(None) => root.is_dir(),
            Err(e) => return Err(Die::Fatal(e.to_string())),
        };
        if !enabled {
            return Ok(None);
        }
        if !root.is_dir() {
            std::fs::create_dir_all(&root).map_err(|e| {
                Die::Fatal(format!(
                    "cannot mkdir {}: {}",
                    root.display(),
                    crate::global::strerror(&e)
                ))
            })?;
        }
        Ok(Some(Rerere {
            repo,
            cache: Cache::new(root),
            git_dir,
        }))
    }

    fn merge_rr_path(&self) -> PathBuf {
        self.git_dir.join("MERGE_RR")
    }

    /// Reads `MERGE_RR` (empty when missing).
    pub fn read_merge_rr(&self) -> Result<Vec<RrEntry>, Die> {
        match std::fs::read(self.merge_rr_path()) {
            Ok(data) => rgit_rerere::parse_merge_rr(&data)
                .map_err(|_| Die::Fatal(msg::CORRUPT_MERGE_RR.to_owned())),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Locks `MERGE_RR` for an update.
    pub fn lock(&self) -> Result<LockFile, Die> {
        LockFile::acquire(self.merge_rr_path(), &LockOptions::default()).map_err(lock_failed)
    }

    /// Writes `MERGE_RR` through its lock.
    pub fn write_merge_rr(&self, mut lock: LockFile, rr: &[RrEntry]) -> Result<(), Die> {
        use std::io::Write as _;
        let data = rgit_rerere::format_merge_rr(rr);
        lock.file_mut()
            .write_all(&data)
            .and_then(|()| lock.commit())
            .map_err(|e| {
                Die::Fatal(format!(
                    "unable to write rerere record: {}",
                    crate::global::strerror(&e)
                ))
            })
    }

    /// `rerere.autoUpdate`.
    pub fn autoupdate_config(&self) -> bool {
        self.repo
            .config
            .bool("rerere.autoupdate")
            .ok()
            .flatten()
            .unwrap_or(false)
    }

    pub fn host(&self, ctx: &Context) -> Result<RepoHost<'_>, Die> {
        RepoHost::new(ctx, &self.repo)
    }
}

/// The index and work tree for [`rgit_rerere`].
pub(crate) struct RepoHost<'r> {
    repo: &'r Repo,
    root: PathBuf,
    pub index: Index,
    attrs: std::rc::Rc<dyn PathMerge>,
}

impl<'r> RepoHost<'r> {
    pub fn new(ctx: &Context, repo: &'r Repo) -> Result<RepoHost<'r>, Die> {
        let root = repo.setup.cwd.join(repo.work_tree()?);
        let (index, _) = repo.read_index()?;
        let attrs =
            crate::commands::merge::attrs::RepoPathMerge::open(ctx, repo, false, None, None)?;
        Ok(RepoHost {
            repo,
            root,
            index,
            attrs,
        })
    }

    /// The conflict of `path` as a merge of its stages left it, from the
    /// index or, for a resolved path, from the resolve-undo records.
    pub fn conflict_text(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        let mut stages: [Option<(u32, rgit_hash::ObjectId)>; 3] = [None, None, None];
        let entries = self.index.entries_for_path(path);
        if entries.iter().any(|e| e.stage() != Stage::MERGED) {
            for e in entries {
                let s = usize::from(e.stage().get());
                if (1..=3).contains(&s) {
                    stages[s - 1] = Some((e.mode.0, e.id));
                }
            }
        } else {
            let r = self
                .index
                .resolve_undo_records()
                .into_iter()
                .find(|r| r.path == path)?;
            stages = r.stages;
        }
        let blob = |s: &Option<(u32, rgit_hash::ObjectId)>| -> Option<Vec<u8>> {
            match s {
                None => Some(Vec::new()),
                Some((mode, id)) => {
                    if mode & 0o170000 != 0o100000 {
                        return None;
                    }
                    match self.repo.read_object(id)? {
                        rgit_object::Object::Blob(b) => Some(b.data.to_vec()),
                        _ => None,
                    }
                }
            }
        };
        stages[1]?;
        stages[2]?;
        let base = blob(&stages[0])?;
        let ours = blob(&stages[1])?;
        let theirs = blob(&stages[2])?;
        let style = self
            .repo
            .config
            .string("merge.conflictstyle")
            .ok()
            .flatten()
            .and_then(|s| ConflictStyle::from_name(&s.to_string()))
            .unwrap_or_default();
        let opts = MergeOptions {
            style,
            marker_size: self.marker_size(path),
            ours_label: b"ours",
            base_label: b"base",
            theirs_label: b"theirs",
            diff: rgit_diff::merge::merge_diff_options(),
            ..Default::default()
        };
        rgit_diff::merge::merge(&base, &ours, &theirs, &opts)
            .ok()
            .map(|r| r.content)
    }
}

impl Host for RepoHost<'_> {
    fn conflicts(&mut self) -> Vec<Conflicted> {
        let mut out: Vec<Conflicted> = Vec::new();
        for e in self.index.entries() {
            if e.stage() == Stage::MERGED {
                continue;
            }
            if out.last().is_none_or(|c| c.path != e.path()) {
                let stages = self.index.entries_for_path(e.path());
                let is_file = |s: Stage| stages.iter().any(|x| x.stage() == s && x.mode.is_file());
                out.push(Conflicted {
                    path: e.path().to_vec(),
                    trackable: is_file(Stage::OURS) && is_file(Stage::THEIRS),
                });
            }
        }
        out
    }

    fn read(&mut self, path: &[u8]) -> std::io::Result<Vec<u8>> {
        std::fs::read(os_path(&self.root, path))
    }

    fn write(&mut self, path: &[u8], data: &[u8]) -> std::io::Result<()> {
        std::fs::write(os_path(&self.root, path), data)
    }

    fn marker_size(&mut self, path: &[u8]) -> usize {
        self.attrs
            .driver(path, false)
            .1
            .unwrap_or(rgit_diff::merge::DEFAULT_MARKER_SIZE)
    }

    fn stage(&mut self, paths: &[Vec<u8>]) -> Result<(), String> {
        let (mut index, mtime) = self.repo.read_index().map_err(die_text)?;
        let checker = self.repo.checker(mtime).map_err(die_text)?;
        for p in paths {
            let old: Vec<_> = index.entries_for_path(p).to_vec();
            let base = old
                .iter()
                .find(|e| e.stage() == Stage::OURS)
                .or(old.first());
            let entry = crate::commands::add::entry_for(
                self.repo, &checker, &self.root, p, base, false, true,
            )
            .map_err(|e| match e {
                crate::commands::add::EntryError::Read(s) => s,
                crate::commands::add::EntryError::Fatal(d) => die_text(d),
            })?;
            let Some(mut entry) = entry else {
                continue;
            };
            entry.flags = Default::default();
            let mut stages: [Option<(u32, rgit_hash::ObjectId)>; 3] = [None, None, None];
            for e in &old {
                let s = usize::from(e.stage().get());
                if (1..=3).contains(&s) {
                    stages[s - 1] = Some((e.mode.0, e.id));
                }
            }
            index.remove_path(p);
            if stages.iter().any(Option::is_some) {
                index.record_resolve_undo(p, stages);
            }
            index
                .add(
                    entry,
                    AddOptions {
                        ok_to_replace: true,
                        ..Default::default()
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        let path = self.repo.setup.cwd.join(self.repo.index_path());
        let skip_hash = self
            .repo
            .config
            .bool("index.skiphash")
            .ok()
            .flatten()
            .unwrap_or(false);
        index
            .write_file(
                &path,
                &self.repo.index_lock_options(),
                &rgit_index::WriteOptions { skip_hash },
            )
            .map_err(|e| e.to_string())?;
        self.index = index;
        Ok(())
    }

    fn report(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

fn die_text(d: Die) -> String {
    match d {
        Die::Fatal(s) | Die::Error(s, _) => s,
        _ => String::new(),
    }
}

/// The rerere pass after a conflicted merge or before a commit:
/// `autoupdate` is `--[no-]rerere-autoupdate` (`None`: `rerere.autoUpdate`).
/// Does nothing when rerere is off.
pub(crate) fn auto(ctx: &Context, autoupdate: Option<bool>) -> Result<(), Die> {
    let Some(rr) = Rerere::open(ctx)? else {
        return Ok(());
    };
    rr.run(ctx, autoupdate)
}

impl Rerere {
    /// One pass of [`rgit_rerere::rerere`].
    pub fn run(&self, ctx: &Context, autoupdate: Option<bool>) -> Result<(), Die> {
        let lock = self.lock()?;
        let mut entries = self.read_merge_rr()?;
        let mut host = self.host(ctx)?;
        let autoupdate = autoupdate.unwrap_or_else(|| self.autoupdate_config());
        rgit_rerere::rerere(
            &self.cache,
            &mut entries,
            &mut host,
            self.repo.hash,
            autoupdate,
        )
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
        self.write_merge_rr(lock, &entries)
    }

    /// `rerere clear`.
    pub fn clear(&self) -> Result<(), Die> {
        let lock = self.lock()?;
        let entries = self.read_merge_rr()?;
        rgit_rerere::clear(&self.cache, &entries);
        let _ = std::fs::remove_file(self.merge_rr_path());
        lock.rollback();
        Ok(())
    }
}

/// `rerere clear` (rebase and am when they skip or abort).
pub(crate) fn clear(ctx: &Context) -> Result<(), Die> {
    match Rerere::open(ctx)? {
        Some(rr) => rr.clear(),
        None => Ok(()),
    }
}

/// The `--rerere-autoupdate` option value of a command line: `-1` unset,
/// else a boolean.
pub(crate) fn flag(v: i32) -> Option<bool> {
    match v {
        -1 => None,
        v => Some(v != 0),
    }
}
