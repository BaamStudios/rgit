// SPDX-License-Identifier: MIT
//! Reflogs of the files backend: one file per reference below `logs/` of
//! the git directory (per-worktree references) or of the common directory.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rgit_fs::SharedRepository;
use rgit_hash::{HashKind, ObjectId};

use super::ReflogError as Error;
use super::entry::{Committer, Entry};
use crate::name::{RefNameFlags, is_per_worktree, is_valid_refname};

/// The reflogs of one worktree's view of a repository.
#[derive(Clone, Debug)]
pub struct FilesReflog {
    git_dir: PathBuf,
    common_dir: PathBuf,
    kind: HashKind,
    shared: SharedRepository,
}

/// `strerror`-style text of an I/O error (without Rust's `(os error N)`).
pub(crate) fn strerror(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(p) => s[..p].to_string(),
        None => s,
    }
}

impl FilesReflog {
    /// Reflogs of the worktree whose git directory is `git_dir`, sharing
    /// `common_dir` (the same directory outside linked worktrees).
    pub fn new(
        git_dir: impl Into<PathBuf>,
        common_dir: impl Into<PathBuf>,
        kind: HashKind,
    ) -> Self {
        FilesReflog {
            git_dir: git_dir.into(),
            common_dir: common_dir.into(),
            kind,
            shared: SharedRepository::default(),
        }
    }

    /// Applies `core.sharedRepository` to newly created log files.
    pub fn with_shared(mut self, shared: SharedRepository) -> Self {
        self.shared = shared;
        self
    }

    /// The object format of the entries.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// The file holding the log of `name`. `main-worktree/<ref>` and
    /// `worktrees/<id>/<ref>` address other worktrees' per-worktree logs.
    pub fn path(&self, name: &str) -> PathBuf {
        if let Some(rest) = name.strip_prefix("main-worktree/") {
            return self.common_dir.join("logs").join(rest);
        }
        if let Some(rest) = name.strip_prefix("worktrees/") {
            if let Some((id, r)) = rest.split_once('/') {
                return self
                    .common_dir
                    .join("worktrees")
                    .join(id)
                    .join("logs")
                    .join(r);
            }
        }
        let base = if is_per_worktree(name) {
            &self.git_dir
        } else {
            &self.common_dir
        };
        base.join("logs").join(name)
    }

    /// Whether `name` has a reflog (a regular file, possibly empty).
    pub fn exists(&self, name: &str) -> bool {
        fs::metadata(self.path(name)).is_ok_and(|m| m.is_file())
    }

    /// The raw content of the log of `name`, `None` if there is none.
    pub fn read_raw(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        let path = self.path(name);
        match fs::read(&path) {
            Ok(d) => Ok(Some(d)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) if path.is_dir() || e.kind() == io::ErrorKind::NotADirectory => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The entries of the log of `name`, oldest first; `None` if there is
    /// no log.
    pub fn read(&self, name: &str) -> io::Result<Option<Vec<Entry>>> {
        Ok(self
            .read_raw(name)?
            .map(|d| Entry::parse_all(&d, self.kind)))
    }

    /// Makes room for a log file at `path`: creates the leading directories
    /// and removes an empty directory tree in its place.
    fn prepare(&self, path: &Path) -> Result<(), Error> {
        if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
            if has_files(path) {
                return Err(Error::LogsUnder(path.to_owned()));
            }
            let _ = fs::remove_dir_all(path);
        }
        if let Some(parent) = path.parent() {
            // A failure shows up as the open error below.
            let _ = fs::create_dir_all(parent);
        }
        Ok(())
    }

    /// Appends a raw line (with its line feed) to the log of `name`. Without
    /// `create`, nothing happens unless the log exists. Returns whether the
    /// line was written.
    pub fn append_line(&self, name: &str, line: &[u8], create: bool) -> Result<bool, Error> {
        let path = self.path(name);
        if create {
            self.prepare(&path)?;
        }
        let existed = path.exists();
        let mut opts = OpenOptions::new();
        opts.append(true).create(create);
        let mut file = match opts.open(&path) {
            Ok(f) => f,
            Err(e)
                if !create
                    && (e.kind() == io::ErrorKind::NotFound
                        || e.kind() == io::ErrorKind::NotADirectory
                        || e.kind() == io::ErrorKind::IsADirectory
                        || path.is_dir()) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(Error::Append(path, strerror(&e))),
        };
        if !existed {
            self.adjust_perm(&path);
        }
        file.write_all(line)
            .map_err(|e| Error::Append(path.clone(), strerror(&e)))?;
        Ok(true)
    }

    /// Appends `entry` to the log of `name` (see [`append_line`](Self::append_line)).
    pub fn append(&self, name: &str, entry: &Entry, create: bool) -> Result<bool, Error> {
        self.append_line(name, &entry.to_line(), create)
    }

    /// Creates an empty log for `name` unless one exists
    /// (`--create-reflog` without an entry).
    pub fn create(&self, name: &str) -> Result<(), Error> {
        self.append_line(name, b"", true).map(|_| ())
    }

    #[cfg(unix)]
    fn adjust_perm(&self, path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        if self.shared == SharedRepository::default() {
            return;
        }
        if let Ok(m) = fs::metadata(path) {
            let mode = m.permissions().mode() & 0o7777;
            let new = self.shared.adjust_file_mode(mode);
            if new != mode {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(new));
            }
        }
    }

    #[cfg(not(unix))]
    fn adjust_perm(&self, _path: &Path) {}

    /// Removes the log of `name` and the directories left empty by that.
    pub fn delete(&self, name: &str) -> io::Result<()> {
        let path = self.path(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.prune_parents(name, &path);
        Ok(())
    }

    fn logs_root(&self, name: &str, path: &Path) -> PathBuf {
        // The `logs` directory the file is in: walk up as many components
        // as the name has.
        let depth = name.split('/').count();
        let mut root = path.to_path_buf();
        for _ in 0..depth {
            root.pop();
        }
        root
    }

    /// Removes directories left empty below `logs/`, keeping the top two
    /// levels (`logs/refs/tags/` stays).
    fn prune_parents(&self, name: &str, path: &Path) {
        let root = self.logs_root(name, path);
        let mut dir = path.parent();
        while let Some(d) = dir {
            let depth = d.strip_prefix(&root).map_or(0, |r| r.components().count());
            if depth <= 2 || fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }

    /// Moves the log of `from` to `to` (branch rename). Nothing happens if
    /// `from` has no log.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let src = self.path(from);
        if !fs::metadata(&src).is_ok_and(|m| m.is_file()) {
            return Ok(());
        }
        let dst = self.path(to);
        // Move out of the way first: `to` may live below `from`
        // (`a` -> `a/b`) or the other way round.
        let tmp = self.common_dir.join("logs").join(".tmp-renamed-log");
        let _ = fs::create_dir_all(tmp.parent().unwrap_or(&self.common_dir));
        fs::rename(&src, &tmp)
            .map_err(|e| Error::Rename(src.clone(), tmp.clone(), strerror(&e)))?;
        self.prune_parents(from, &src);
        if let Err(e) = self.prepare(&dst) {
            let _ = fs::rename(&tmp, &src);
            return Err(e);
        }
        fs::rename(&tmp, &dst).map_err(|e| {
            let _ = fs::rename(&tmp, &src);
            Error::Rename(src.clone(), dst.clone(), strerror(&e))
        })
    }

    /// Every reference with a reflog in this worktree's view, sorted:
    /// shared logs from the common directory plus this worktree's own.
    /// Files whose names are not valid reference names (lock files, ...)
    /// and symbolic links are skipped.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        let same = same_dir(&self.git_dir, &self.common_dir);
        walk(&self.common_dir.join("logs"), "", &mut |name| {
            if same || !is_per_worktree(name) {
                out.push(name.to_string());
            }
        })?;
        if !same {
            walk(&self.git_dir.join("logs"), "", &mut |name| {
                if is_per_worktree(name) {
                    out.push(name.to_string());
                }
            })?;
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Builds an entry by `committer` and appends it (see
    /// [`append`](Self::append)). The message is normalised.
    pub fn log_update(
        &self,
        name: &str,
        old: ObjectId,
        new: ObjectId,
        committer: &Committer,
        message: &[u8],
        create: bool,
    ) -> Result<bool, Error> {
        self.append(name, &Entry::new(old, new, committer, message), create)
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// Whether the directory tree below `dir` contains anything but directories.
fn has_files(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                if has_files(&e.path()) {
                    return true;
                }
            }
            _ => return true,
        }
    }
    false
}

fn walk(dir: &Path, prefix: &str, f: &mut dyn FnMut(&str)) -> io::Result<()> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Some(fname) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let name = format!("{prefix}{fname}");
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            walk(&e.path(), &format!("{name}/"), f)?;
        } else if t.is_file() && is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL) {
            f(&name);
        }
    }
    Ok(())
}
