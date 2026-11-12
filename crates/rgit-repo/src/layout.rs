// SPDX-License-Identifier: MIT
//! What counts as a repository directory, and `.git` files
//! (`gitdir: <path>`), following `gitrepository-layout(5)`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::GitfileError;
use crate::path;

/// Overrides that change where a repository keeps its shared parts.
#[derive(Debug, Clone, Default)]
pub struct LayoutOverrides {
    /// `GIT_COMMON_DIR`, relative to the current directory.
    pub common_dir: Option<PathBuf>,
    /// `GIT_OBJECT_DIRECTORY`, relative to the current directory.
    pub object_dir: Option<PathBuf>,
}

/// Whether `dir` looks like a repository: a valid `HEAD`, an object
/// directory and a `refs` directory (the last two in the common directory).
pub fn is_git_directory(dir: &Path, cwd: &Path, ov: &LayoutOverrides) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let common = match &ov.common_dir {
        Some(c) => path::absolute(c, cwd),
        None => common_dir_of(dir).unwrap_or_else(|| dir.to_path_buf()),
    };
    let objects = match &ov.object_dir {
        Some(o) => path::absolute(o, cwd),
        None => common.join("objects"),
    };
    if !objects.exists() || !common.join("refs").exists() {
        return false;
    }
    valid_head(&dir.join("HEAD"))
}

/// The common directory named by `<dir>/commondir`, resolved against
/// `dir` when relative. `None` without such a file.
pub fn common_dir_of(dir: &Path) -> Option<PathBuf> {
    let data = std::fs::read(dir.join("commondir")).ok()?;
    let text = trim_end(&data);
    if text.is_empty() {
        return None;
    }
    let p = bytes_to_path(text);
    Some(if p.is_absolute() { p } else { dir.join(p) })
}

/// `HEAD` is a symbolic link into `refs/`, a `ref: refs/...` file, or a
/// file starting with a full hexadecimal object id.
fn valid_head(head: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(head) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return std::fs::read_link(head)
            .map(|t| t.starts_with("refs"))
            .unwrap_or(false);
    }
    if !meta.is_file() {
        return false;
    }
    let Ok(data) = std::fs::read(head) else {
        return false;
    };
    if let Some(rest) = data.strip_prefix(b"ref:") {
        let rest = trim_start(rest);
        return rest.starts_with(b"refs/");
    }
    let hex = data.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    hex >= rgit_hash::HashKind::Sha1.hex_len()
}

/// Read a `.git` file and return the absolute, resolved repository path it
/// points to. `base` resolves a relative `gitfile`.
pub fn read_gitfile(
    gitfile: &Path,
    base: &Path,
    ov: &LayoutOverrides,
) -> Result<PathBuf, GitfileError> {
    let abs = path::absolute(gitfile, base);
    let meta = std::fs::metadata(&abs).map_err(|_| GitfileError::Read)?;
    if !meta.is_file() {
        return Err(GitfileError::NotRegular);
    }
    let data = std::fs::read(&abs).map_err(|_| GitfileError::Read)?;
    let Some(rest) = data.strip_prefix(b"gitdir: ") else {
        return Err(GitfileError::InvalidFormat);
    };
    let target = trim_end(rest);
    if target.is_empty() {
        return Err(GitfileError::NoPath);
    }
    let target = bytes_to_path(target);
    let dir = abs.parent().unwrap_or(Path::new("/"));
    let target = path::absolute(&target, dir);
    if !is_git_directory(&target, base, ov) {
        return Err(GitfileError::NotARepository);
    }
    Ok(path::real_path_lossy(&target, dir))
}

fn trim_end(b: &[u8]) -> &[u8] {
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    &b[..end]
}

fn trim_start(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    &b[start..]
}

#[cfg(unix)]
pub(crate) fn bytes_to_path(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(b))
}

#[cfg(not(unix))]
pub(crate) fn bytes_to_path(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}
