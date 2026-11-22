// SPDX-License-Identifier: MIT
//! Where a submodule's repository lives.
//!
//! A submodule checked out by `git submodule` has a `.git` file in its work
//! tree (`gitdir: <path>`) pointing to `$GIT_COMMON_DIR/modules/<name>`, an
//! "absorbed" git directory. Old-form submodules and nested repositories
//! added with `git add` have a `.git` directory of their own.

use std::path::{Path, PathBuf};

/// `$GIT_COMMON_DIR/modules/<name>`: the absorbed git directory of the
/// submodule `name` (which must have passed [`crate::check_name`]).
pub fn modules_dir(common_dir: &Path, name: &[u8]) -> PathBuf {
    common_dir.join("modules").join(bytes_to_path(name))
}

fn bytes_to_path(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

/// The git directory of the work tree at `worktree`: `worktree/.git` if it
/// is a directory, or the target of a `gitdir:` file there (relative
/// targets are resolved against `worktree`). `None` if there is neither or
/// the file is malformed. The result is not checked to be a repository.
pub fn git_dir_of(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git);
    }
    if !meta.is_file() {
        return None;
    }
    let data = std::fs::read(&dot_git).ok()?;
    let rest = data.strip_prefix(b"gitdir:")?;
    let target = rest.trim_ascii();
    if target.is_empty() {
        return None;
    }
    let target = bytes_to_path(target);
    Some(if target.is_absolute() {
        target
    } else {
        worktree.join(target)
    })
}

/// Whether the submodule work tree at `worktree` is populated: it has a
/// `.git` that leads to a directory containing `HEAD`.
pub fn is_populated(worktree: &Path) -> bool {
    git_dir_of(worktree).is_some_and(|d| d.join("HEAD").exists())
}

/// Whether `worktree` has an absorbed git directory (a `.git` file rather
/// than a directory).
pub fn is_absorbed(worktree: &Path) -> bool {
    std::fs::symlink_metadata(worktree.join(".git")).is_ok_and(|m| m.is_file())
}
