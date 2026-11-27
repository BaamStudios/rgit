// SPDX-License-Identifier: MIT
//! `status` with the untracked cache (git-update-index(1) "UNTRACKED
//! CACHE", gitformat-index(5) "Untracked cache").
//!
//! The walk lists the untracked files of the work tree like the plain
//! walk of [`crate::untracked`], but directory by directory through the
//! cache: a directory whose stat data and exclude file are unchanged is
//! not read again, its untracked names come from the cache. In the normal
//! mode an untracked directory is read only far enough to see whether it
//! holds something untracked ("check only"). The behaviour (which
//! directories are recorded, when they are invalidated and the trace2
//! counters) follows the reference git, checked with its test suite.

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_index::{CacheDir, CacheStat, ExcludeFile, Index, Stage, UntrackedCache};
use rgit_worktree::ignore::Excludes;

use crate::untracked::{is_nested_repo, os_path};

/// The counters git reports (trace2 `read_directory` data).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Stats {
    pub node_creation: usize,
    pub gitignore_invalidation: usize,
    pub directory_invalidation: usize,
    pub opendir: usize,
}

/// Stat data of a path as the untracked cache keeps it; `None` when it
/// cannot be read.
fn cache_stat(path: &Path) -> Option<CacheStat> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let s = rgit_worktree::stat::stat_from_metadata(&meta);
    Some(CacheStat([
        s.ctime.secs,
        s.ctime.nsecs,
        s.mtime.secs,
        s.mtime.nsecs,
        s.dev,
        s.ino,
        s.uid,
        s.gid,
        s.size,
    ]))
}

/// The id git records for an exclude file read from the work tree: the
/// blob of its content with a newline added (none for an empty file).
fn file_id(kind: HashKind, data: &[u8]) -> ObjectId {
    let mut d = data.to_vec();
    if !d.is_empty() {
        d.push(b'\n');
    }
    rgit_hash::hash_object(kind, "blob", &d).unwrap_or_else(|e| e.id)
}

/// The global exclude files of the cache.
pub(crate) struct GlobalFiles {
    pub info_exclude: PathBuf,
    pub excludes_file: Option<PathBuf>,
}

/// Refreshes a global exclude file record; true when its content changed.
fn check_global(kind: HashKind, rec: &mut ExcludeFile, path: Option<&Path>) -> bool {
    let stat = path.and_then(cache_stat).unwrap_or_default();
    if stat == rec.stat {
        return false;
    }
    rec.stat = stat;
    let id = match path.and_then(|p| std::fs::read(p).ok()) {
        Some(data) => file_id(kind, &data),
        None => kind.null(),
    };
    let changed = id != rec.id;
    rec.id = id;
    changed
}

/// The walk over one cache.
pub(crate) struct CachedWalk<'a> {
    pub root: &'a Path,
    pub index: &'a Index,
    pub excludes: &'a mut Excludes,
    /// `-uall`: list the files inside untracked directories.
    pub all: bool,
    pub stats: Stats,
    /// The untracked paths, as status shows them.
    pub out: Vec<Vec<u8>>,
}

impl CachedWalk<'_> {
    /// Walks the work tree through `uc`, updating it.
    pub fn run(&mut self, uc: &mut UntrackedCache, globals: &GlobalFiles) {
        let kind = self.index.hash_kind();
        let mut changed = check_global(kind, &mut uc.info_exclude, Some(&globals.info_exclude));
        changed |= check_global(
            kind,
            &mut uc.excludes_file,
            globals.excludes_file.as_deref(),
        );
        let root = uc.root.get_or_insert_with(|| CacheDir::new(Vec::new()));
        if changed {
            self.stats.gitignore_invalidation += 1;
            root.invalidate_all();
        }
        self.visit(root, b"", false);
        self.out.sort();
    }

    /// The index entries below `dir/` (any stage), or a gitlink at `dir`.
    fn tracked_dir(&self, dir: &[u8]) -> (bool, bool) {
        let gitlink = self
            .index
            .get(dir, Stage::MERGED)
            .is_some_and(|e| e.mode.is_gitlink());
        (gitlink, !self.index.dir_range(dir).is_empty())
    }

    fn tracked_file(&self, path: &[u8]) -> bool {
        !self.index.path_range(path).is_empty()
    }

    /// The id of the per-directory exclude file of `dir` (empty or ending
    /// in `/`): the index blob when it is tracked and unchanged or not
    /// checked out, the content otherwise; `None` when there is none.
    fn exclude_id(&self, dir: &[u8]) -> Option<ObjectId> {
        let mut path = dir.to_vec();
        path.extend_from_slice(b".gitignore");
        let full = os_path(self.root, &path);
        let entry = self.index.get(&path, Stage::MERGED);
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_file() => {
                if let Some(e) = entry {
                    let s = rgit_worktree::stat::stat_from_metadata(&meta);
                    if s.mtime == e.stat.mtime && s.size == e.stat.size && s.ino == e.stat.ino {
                        return Some(e.id);
                    }
                }
                let data = std::fs::read(&full).ok()?;
                Some(file_id(self.index.hash_kind(), &data))
            }
            _ => entry.filter(|e| e.flags.skip_worktree).map(|e| e.id),
        }
    }

    /// Visits `dir` (empty or ending in `/`) with its cache record `node`;
    /// returns whether it holds something untracked.
    fn visit(&mut self, node: &mut CacheDir, dir: &[u8], check_only: bool) -> bool {
        let Some(stat) = cache_stat(&os_path(self.root, dir)) else {
            node.invalidate();
            node.dirs.clear();
            return false;
        };
        // A directory the file system monitor reported nothing in is
        // trusted as cached.
        if node.valid && !self.index.fsmonitor_has_run() {
            if node.stat != stat {
                self.stats.directory_invalidation += 1;
                node.invalidate();
            } else {
                let id = self.exclude_id(dir);
                if id != node.exclude_id {
                    self.stats.gitignore_invalidation += 1;
                    node.invalidate_all();
                    node.exclude_id = id;
                }
            }
        }
        if node.valid {
            self.reuse(node, dir, check_only)
        } else {
            self.read(node, dir, check_only, stat)
        }
    }

    /// A valid record: its names, with the subdirectories visited again.
    fn reuse(&mut self, node: &mut CacheDir, dir: &[u8], check_only: bool) -> bool {
        let mut names: Vec<Vec<u8>> = Vec::new();
        for name in &node.untracked {
            if name.ends_with(b"/") {
                let bare = &name[..name.len() - 1];
                // Untracked directories with a record are checked below;
                // nested repositories have none.
                if node.child(bare).is_some() && !self.all {
                    continue;
                }
                // git records a directory that replaced a tracked file
                // but does not show it.
                let mut path = dir.to_vec();
                path.extend_from_slice(bare);
                if !self.all && self.tracked_file(&path) {
                    continue;
                }
            }
            names.push(name.clone());
        }
        let mut dirs = std::mem::take(&mut node.dirs);
        for child in &mut dirs {
            let mut path = dir.to_vec();
            path.extend_from_slice(&child.name);
            let (_, tracked) = self.tracked_dir(&path);
            path.push(b'/');
            if tracked || self.all {
                self.visit(child, &path, false);
            } else if self.visit(child, &path, true) && !self.tracked_file(&path[..path.len() - 1])
            {
                let mut n = child.name.clone();
                n.push(b'/');
                names.push(n);
            }
        }
        node.dirs = dirs;
        names.sort();
        node.untracked.clone_from(&names);
        if !check_only {
            self.emit(dir, &names);
        }
        !names.is_empty()
    }

    /// Reads the directory and records what it holds.
    fn read(&mut self, node: &mut CacheDir, dir: &[u8], check_only: bool, stat: CacheStat) -> bool {
        self.stats.opendir += 1;
        let listing = read_sorted(&os_path(self.root, dir));
        let mut old = std::mem::take(&mut node.dirs);
        let mut dirs: Vec<CacheDir> = Vec::new();
        let mut names: Vec<Vec<u8>> = Vec::new();
        let mut id_checked = false;
        for (name, is_dir) in listing {
            let mut path = dir.to_vec();
            path.extend_from_slice(&name);
            if is_dir {
                let (gitlink, tracked) = self.tracked_dir(&path);
                if gitlink {
                    continue;
                }
                // A directory where the index has a file (the file was
                // replaced by it) is not shown as untracked, as without
                // the cache.
                if !tracked && !self.all && self.tracked_file(&path) {
                    continue;
                }
                if !tracked {
                    self.check_exclude_id(node, dir, &mut id_checked, &mut old);
                    if self.excludes.is_excluded(&path, true) {
                        continue;
                    }
                    if is_nested_repo(&os_path(self.root, &path)) {
                        let mut n = name.clone();
                        n.push(b'/');
                        names.push(n);
                        if check_only {
                            break;
                        }
                        continue;
                    }
                }
                let mut child = match old.iter().position(|d| d.name == name) {
                    Some(i) => old.remove(i),
                    None => {
                        self.stats.node_creation += 1;
                        CacheDir::new(name.clone())
                    }
                };
                path.push(b'/');
                let has = self.visit(&mut child, &path, !tracked && !self.all);
                dirs.push(child);
                if !tracked && !self.all && has {
                    let mut n = name.clone();
                    n.push(b'/');
                    names.push(n);
                    if check_only {
                        break;
                    }
                }
            } else {
                if self.tracked_file(&path) {
                    continue;
                }
                self.check_exclude_id(node, dir, &mut id_checked, &mut old);
                if self.excludes.is_excluded(&path, false) {
                    continue;
                }
                names.push(name);
                if check_only {
                    break;
                }
            }
        }
        node.dirs = dirs;
        node.untracked.clone_from(&names);
        node.valid = true;
        node.check_only = check_only;
        node.stat = stat;
        if !check_only {
            self.emit(dir, &names);
        }
        !names.is_empty()
    }

    /// Before the first path of `dir` is checked against the exclude
    /// rules, the directory's exclude file is looked at; a changed one
    /// invalidates what is recorded below.
    fn check_exclude_id(
        &mut self,
        node: &mut CacheDir,
        dir: &[u8],
        checked: &mut bool,
        children: &mut [CacheDir],
    ) {
        if std::mem::replace(checked, true) {
            return;
        }
        let id = self.exclude_id(dir);
        if id != node.exclude_id {
            self.stats.gitignore_invalidation += 1;
            node.exclude_id = id;
            for c in children {
                c.invalidate_all();
            }
        }
    }

    fn emit(&mut self, dir: &[u8], names: &[Vec<u8>]) {
        for n in names {
            let mut p = dir.to_vec();
            p.extend_from_slice(n);
            self.out.push(p);
        }
    }
}

/// The entries of `dir` (name, is a directory), sorted, without `.git`.
fn read_sorted(dir: &Path) -> Vec<(Vec<u8>, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(Vec<u8>, bool)> = rd
        .flatten()
        .filter_map(|e| {
            let name = rgit_worktree::precompose::file_name(&e.file_name());
            if name == b".git" {
                return None;
            }
            let is_dir = e.file_type().ok()?.is_dir();
            Some((name, is_dir))
        })
        .collect();
    out.sort();
    out
}

/// `update-index --test-untracked-cache`: checks that directory mtimes
/// change when entries are added and removed; 0 if they do, 1 if not.
pub(crate) fn test_mtime(root: &Path) -> i32 {
    use crate::messages::update_index as msg;
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = write!(err, "{}", msg::testing_mtime(&root.display().to_string()));
    let dir = root.join(format!("mtime-test-{}", std::process::id()));
    if std::fs::create_dir(&dir).is_err() {
        let _ = writeln!(err);
        return 1;
    }
    let stat = |p: &Path| cache_stat(p).unwrap_or_default();
    let mut ok = true;
    let mut step = |ok: &mut bool, f: &dyn Fn() -> std::io::Result<()>| {
        let before = stat(&dir);
        // A directory mtime with a coarse clock needs time to move on.
        std::thread::sleep(std::time::Duration::from_millis(10));
        if f().is_err() || stat(&dir) == before {
            *ok = false;
        }
        let _ = write!(err, ".");
    };
    let file = dir.join("newfile");
    let sub = dir.join("new-dir");
    step(&mut ok, &|| std::fs::write(&file, b""));
    step(&mut ok, &|| std::fs::create_dir(&sub));
    step(&mut ok, &|| std::fs::rename(&file, dir.join("renamed")));
    step(&mut ok, &|| std::fs::rename(&sub, dir.join("renamed-dir")));
    step(&mut ok, &|| std::fs::remove_file(dir.join("renamed")));
    step(&mut ok, &|| std::fs::remove_dir(dir.join("renamed-dir")));
    let _ = std::fs::remove_dir_all(&dir);
    if ok {
        let _ = writeln!(err, " OK");
        0
    } else {
        let _ = writeln!(err);
        1
    }
}
