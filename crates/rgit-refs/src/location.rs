// SPDX-License-Identifier: MIT
//! Repositories that keep their references outside the git directory.
//!
//! `extensions.refStorage = <format>://<directory>` (or the environment
//! variable `GIT_REFERENCE_BACKEND`, `Documentation/config/extensions.adoc`)
//! moves the references of a repository into `<directory>`; the git
//! directory then holds only stand-ins. Linked worktrees keep their
//! per-worktree references in `<directory>/worktrees/<id>`.
//!
//! The repository setup registers the location with [`set_ref_dir`]; every
//! reference store and root reference helper of this crate opened for the
//! common directory (or for the git directory of one of its worktrees)
//! then works in the registered directory instead ([`resolve`]).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Common directory to reference directory, both absolute.
static DIRS: Mutex<Vec<(PathBuf, PathBuf)>> = Mutex::new(Vec::new());

/// Content of the file `refs/heads` in a git directory whose references
/// are elsewhere.
pub const HEADS_STAND_IN: &str = crate::messages::ALTERNATE_REFS_STAND_IN;

fn normalize(dir: &Path) -> PathBuf {
    if let Ok(p) = rgit_fs::path::canonicalize(dir) {
        return p;
    }
    let abs = if rgit_fs::path::is_absolute(dir) {
        dir.to_path_buf()
    } else {
        rgit_fs::path::join(rgit_fs::path::current_dir().unwrap_or_default(), dir)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The references of the repository with the common directory
/// `common_dir` are kept in `ref_dir`.
pub fn set_ref_dir(common_dir: &Path, ref_dir: &Path) {
    let (common, refs) = (normalize(common_dir), normalize(ref_dir));
    let mut dirs = DIRS.lock().unwrap_or_else(|e| e.into_inner());
    dirs.retain(|(c, _)| *c != common);
    if common != refs {
        dirs.push((common, refs));
    }
}

/// The directory that holds the references of the git directory `dir`:
/// `dir` itself, or for a registered repository its reference directory
/// (`<refdir>/worktrees/<id>` for a linked worktree).
pub fn resolve(dir: &Path) -> PathBuf {
    let dirs = DIRS.lock().unwrap_or_else(|e| e.into_inner());
    if dirs.is_empty() {
        return dir.to_path_buf();
    }
    let n = normalize(dir);
    for (common, refs) in dirs.iter() {
        if n == *common {
            return refs.clone();
        }
        if let (Some(parent), Some(id)) = (n.parent(), n.file_name())
            && parent.parent() == Some(common.as_path())
            && parent.file_name() == Some(std::ffi::OsStr::new("worktrees"))
        {
            return rgit_fs::path::join(rgit_fs::path::join(refs, "worktrees"), id);
        }
    }
    dir.to_path_buf()
}

/// Whether the references of `dir` are kept elsewhere ([`resolve`]).
pub fn is_redirected(dir: &Path) -> bool {
    resolve(dir) != dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_common_and_worktree_dirs() {
        let base = std::env::temp_dir().join(format!("rgit-location-{}", std::process::id()));
        let common = base.join("repo/.git");
        let refs = base.join("refdir");
        std::fs::create_dir_all(common.join("worktrees/wt")).unwrap();
        std::fs::create_dir_all(&refs).unwrap();
        let refs = rgit_fs::path::canonicalize(&refs).unwrap();
        set_ref_dir(&common, &refs);
        assert_eq!(resolve(&common), refs);
        assert_eq!(
            resolve(&common.join("worktrees/wt")),
            refs.join("worktrees/wt")
        );
        let other = base.join("other");
        assert_eq!(resolve(&other), other);
        assert!(is_redirected(&common));
        set_ref_dir(&common, &common);
        assert_eq!(resolve(&common), common);
        let _ = std::fs::remove_dir_all(&base);
    }
}
