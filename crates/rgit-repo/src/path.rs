// SPDX-License-Identifier: MIT
//! Path helpers for repository setup: resolving symbolic links the way
//! setup reports paths, relative prefixes and `GIT_CEILING_DIRECTORIES`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};

/// Symbolic links followed before giving up with `ELOOP`.
const MAX_SYMLINKS: usize = 40;

/// A path that could not be resolved (`Invalid path '<p>': <reason>`).
#[derive(Debug)]
pub struct InvalidPath {
    /// The path up to and including the component that failed.
    pub path: PathBuf,
    /// The error.
    pub source: io::Error,
}

/// `path` joined to `base` when relative.
pub fn absolute(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Lexical normalisation of an absolute or relative path: removes `.`
/// components, empty components and trailing slashes, and folds `..` into
/// the preceding component (never above the root).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            Component::Normal(n) => {
                out.push(n);
                depth += 1;
            }
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Resolve `path` (relative to `base`) to an absolute path without `.`,
/// `..` or symbolic links. The last component may be missing; a missing
/// component before it is an error naming the path up to that component.
pub fn real_path(path: &Path, base: &Path) -> Result<PathBuf, InvalidPath> {
    let abs = absolute(path, base);
    let mut pending: Vec<OsString> = Vec::new();
    push_components(&abs, &mut pending);
    let mut resolved = root_of(&abs);
    let mut links = 0usize;
    while let Some(name) = pending.pop() {
        if name == "." || name.is_empty() {
            continue;
        }
        if name == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&name);
        match std::fs::symlink_metadata(&candidate) {
            Err(e) if e.kind() == io::ErrorKind::NotFound && only_dots(&pending) => {
                resolved = candidate;
            }
            Err(e) => {
                return Err(InvalidPath {
                    path: candidate,
                    source: e,
                });
            }
            Ok(m) if m.file_type().is_symlink() => {
                links += 1;
                if links > MAX_SYMLINKS {
                    return Err(InvalidPath {
                        path: candidate,
                        source: io::Error::other("Too many levels of symbolic links"),
                    });
                }
                let target = std::fs::read_link(&candidate).map_err(|e| InvalidPath {
                    path: candidate.clone(),
                    source: e,
                })?;
                if target.is_absolute() {
                    resolved = root_of(&target);
                }
                push_components(&target, &mut pending);
            }
            Ok(_) => resolved = candidate,
        }
    }
    Ok(resolved)
}

/// [`real_path`], falling back to lexical normalisation of the absolute
/// path when it cannot be resolved.
pub fn real_path_lossy(path: &Path, base: &Path) -> PathBuf {
    real_path(path, base).unwrap_or_else(|_| normalize(&absolute(path, base)))
}

fn only_dots(pending: &[OsString]) -> bool {
    pending.iter().all(|c| c == "." || c.is_empty())
}

fn root_of(p: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => root.push(c.as_os_str()),
            _ => break,
        }
    }
    if root.as_os_str().is_empty() {
        root.push("/");
    }
    root
}

/// Push the non-root components of `p` so that the first one is popped
/// first.
fn push_components(p: &Path, stack: &mut Vec<OsString>) {
    let comps: Vec<OsString> = p
        .components()
        .filter_map(|c| match c {
            Component::Prefix(_) | Component::RootDir => None,
            Component::CurDir => Some(OsString::from(".")),
            Component::ParentDir => Some(OsString::from("..")),
            Component::Normal(n) => Some(n.to_os_string()),
        })
        .collect();
    stack.extend(comps.into_iter().rev());
}

/// `Some(rel)` when `path` lies strictly inside `dir` (both absolute and
/// normalised); `rel` uses `/` and has no trailing slash.
pub fn strictly_inside<'a>(path: &'a Path, dir: &Path) -> Option<&'a Path> {
    let rel = path.strip_prefix(dir).ok()?;
    if rel.as_os_str().is_empty() {
        None
    } else {
        Some(rel)
    }
}

/// The prefix of `cwd` inside `top`: `rel/` when `cwd` is strictly inside,
/// otherwise `None` (equal, or outside).
pub fn prefix_of(cwd: &Path, top: &Path) -> Option<OsString> {
    let rel = strictly_inside(cwd, top)?;
    let mut s = OsString::new();
    for (i, c) in rel.components().enumerate() {
        if i > 0 {
            s.push("/");
        }
        s.push(c.as_os_str());
    }
    s.push("/");
    Some(s)
}

/// The deepest entry of `GIT_CEILING_DIRECTORIES` that is a proper
/// ancestor of `cwd`. Relative entries are ignored; entries are resolved
/// through symbolic links until an empty entry is seen.
pub fn ceiling_for(cwd: &Path, ceilings: &OsStr) -> Option<PathBuf> {
    let mut resolve = true;
    let mut best: Option<PathBuf> = None;
    for entry in split_colon(ceilings) {
        if entry.is_empty() {
            resolve = false;
            continue;
        }
        let p = Path::new(&entry);
        if !p.is_absolute() {
            continue;
        }
        let dir = if resolve {
            match real_path(p, Path::new("/")) {
                Ok(r) => r,
                Err(_) => continue,
            }
        } else {
            normalize(p)
        };
        if strictly_inside(cwd, &dir).is_some()
            && best
                .as_ref()
                .is_none_or(|b| dir.as_os_str().len() > b.as_os_str().len())
        {
            best = Some(dir);
        }
    }
    best
}

#[cfg(unix)]
fn split_colon(s: &OsStr) -> Vec<OsString> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes()
        .split(|&b| b == b':')
        .map(|b| OsStr::from_bytes(b).to_os_string())
        .collect()
}

#[cfg(not(unix))]
fn split_colon(s: &OsStr) -> Vec<OsString> {
    std::env::split_paths(s)
        .map(PathBuf::into_os_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize(Path::new("/a/./b/../c/")), Path::new("/a/c"));
        assert_eq!(normalize(Path::new("/../a")), Path::new("/a"));
        assert_eq!(normalize(Path::new("a/../../b")), Path::new("../b"));
        assert_eq!(normalize(Path::new("./")), Path::new("."));
    }

    #[test]
    fn prefixes() {
        assert_eq!(
            prefix_of(Path::new("/a/b/c"), Path::new("/a")),
            Some("b/c/".into())
        );
        assert_eq!(prefix_of(Path::new("/a"), Path::new("/a")), None);
        assert_eq!(prefix_of(Path::new("/ab"), Path::new("/a")), None);
        assert_eq!(
            prefix_of(Path::new("/a"), Path::new("/")),
            Some("a/".into())
        );
    }

    #[test]
    fn real_path_missing_last() {
        let tmp = std::env::temp_dir();
        let base = real_path(&tmp, Path::new("/")).unwrap();
        let p = real_path(Path::new("no-such-rgit-dir"), &base).unwrap();
        assert_eq!(p, base.join("no-such-rgit-dir"));
        let e = real_path(Path::new("no-such-rgit-dir/x"), &base).unwrap_err();
        assert_eq!(e.path, base.join("no-such-rgit-dir"));
    }

    #[test]
    fn ceilings() {
        let c = ceiling_for(Path::new("/a/b/c"), OsStr::new(":/a:/a/b:/a/b/c:rel"));
        assert_eq!(c.as_deref(), Some(Path::new("/a/b")));
        assert_eq!(ceiling_for(Path::new("/a"), OsStr::new(":/a/")), None);
        assert_eq!(
            ceiling_for(Path::new("/a"), OsStr::new(":/")).as_deref(),
            Some(Path::new("/"))
        );
    }
}
