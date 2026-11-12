// SPDX-License-Identifier: MIT
//! `include.path` and `includeIf.<condition>.path`.

use std::path::{Path, PathBuf};

use bstr::{BStr, BString, ByteSlice};

use crate::env::Environment;
use crate::error::Error;
use crate::glob::wildmatch;
use crate::origin::{Origin, path_bytes};

/// Maximum nesting of include files.
pub const MAX_INCLUDE_DEPTH: usize = 10;

/// Facts about the repository that conditional includes depend on.
///
/// Return `None` for anything not known yet (for example during early
/// configuration reading, or outside a repository); the condition is then
/// false.
pub trait RepoFacts {
    /// The git directory (`$GIT_DIR`, or the directory a `.git` file points
    /// to), absolute but with symbolic links not resolved.
    fn git_dir(&self) -> Option<PathBuf>;

    /// The top of the working tree with symbolic links resolved; `None` for
    /// a bare repository.
    fn worktree(&self) -> Option<PathBuf> {
        None
    }

    /// The checked-out branch without `refs/heads/` (also for an unborn
    /// branch); `None` for a detached `HEAD`.
    fn current_branch(&self) -> Option<BString> {
        None
    }
}

/// Simple [`RepoFacts`] holding values.
#[derive(Debug, Clone, Default)]
pub struct StaticRepoFacts {
    /// See [`RepoFacts::git_dir`].
    pub git_dir: Option<PathBuf>,
    /// See [`RepoFacts::worktree`].
    pub worktree: Option<PathBuf>,
    /// See [`RepoFacts::current_branch`].
    pub branch: Option<BString>,
}

impl RepoFacts for StaticRepoFacts {
    fn git_dir(&self) -> Option<PathBuf> {
        self.git_dir.clone()
    }
    fn worktree(&self) -> Option<PathBuf> {
        self.worktree.clone()
    }
    fn current_branch(&self) -> Option<BString> {
        self.branch.clone()
    }
}

/// What an entry asks to include.
pub(crate) enum IncludeKind<'a> {
    /// `include.path`
    Always,
    /// `includeIf.<condition>.path`
    If(&'a BStr),
}

/// Recognize `include.path` and `includeif.<cond>.path` (canonical names).
pub(crate) fn include_kind(name: &BStr) -> Option<IncludeKind<'_>> {
    if name == "include.path" {
        return Some(IncludeKind::Always);
    }
    let rest = name.strip_prefix(b"includeif.")?;
    let cond = rest.strip_suffix(b".path")?;
    Some(IncludeKind::If(cond.as_bstr()))
}

/// A parsed include condition.
pub(crate) enum Condition<'a> {
    GitDir { pattern: &'a [u8], icase: bool },
    Worktree { pattern: &'a [u8], icase: bool },
    OnBranch(&'a [u8]),
    RemoteUrl(&'a [u8]),
    Unknown,
}

pub(crate) fn parse_condition(cond: &BStr) -> Condition<'_> {
    let c: &[u8] = cond.as_ref();
    if let Some(p) = c.strip_prefix(b"gitdir:") {
        Condition::GitDir {
            pattern: p,
            icase: false,
        }
    } else if let Some(p) = c.strip_prefix(b"gitdir/i:") {
        Condition::GitDir {
            pattern: p,
            icase: true,
        }
    } else if let Some(p) = c.strip_prefix(b"worktree:") {
        Condition::Worktree {
            pattern: p,
            icase: false,
        }
    } else if let Some(p) = c.strip_prefix(b"worktree/i:") {
        Condition::Worktree {
            pattern: p,
            icase: true,
        }
    } else if let Some(p) = c.strip_prefix(b"onbranch:") {
        Condition::OnBranch(p)
    } else if let Some(p) = c.strip_prefix(b"hasconfig:remote.*.url:") {
        Condition::RemoteUrl(p)
    } else {
        Condition::Unknown
    }
}

fn realpath(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok()
}

fn join_bytes(prefix: &Path, rest: &[u8]) -> BString {
    let mut out = path_bytes(prefix);
    out.extend_from_slice(rest);
    out
}

/// Expand a `gitdir:`/`worktree:` pattern. Returns the pattern spellings to
/// try: as written and with the expanded prefix's symbolic links resolved.
pub(crate) fn prepare_path_pattern(
    pattern: &[u8],
    origin: &Origin,
    env: &dyn Environment,
) -> Result<Vec<BString>, Error> {
    let mut variants: Vec<BString> = Vec::new();
    if let Some(rest) = pattern.strip_prefix(b"./") {
        let Origin::File(file) = origin else {
            return Err(Error::RelativeIncludeCondition);
        };
        let dir = file.parent().unwrap_or(Path::new(""));
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        let abs = absolute(dir);
        variants.push(join_bytes(&abs, &[b"/", rest].concat()));
        if let Some(real) = realpath(&abs) {
            variants.push(join_bytes(&real, &[b"/", rest].concat()));
        }
    } else if let Some(rest) = pattern.strip_prefix(b"~/") {
        let home = env
            .var_bytes("HOME")
            .filter(|h| !h.is_empty())
            .ok_or_else(|| Error::ExpandIncludePath {
                path: pattern.into(),
            })?;
        let home = crate::value::bytes_to_path(&home);
        variants.push(join_bytes(&home, &[b"/", rest].concat()));
        if let Some(real) = realpath(&home) {
            variants.push(join_bytes(&real, &[b"/", rest].concat()));
        }
    } else if pattern.starts_with(b"/") {
        variants.push(pattern.into());
    } else {
        variants.push([b"**/", pattern].concat().into());
    }
    for v in &mut variants {
        if v.ends_with(b"/") {
            v.extend_from_slice(b"**");
        }
    }
    variants.dedup();
    Ok(variants)
}

/// Make `p` absolute against the current directory without resolving links.
pub(crate) fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let mut out = cwd;
            for c in p.components() {
                match c {
                    std::path::Component::CurDir => {}
                    other => out.push(other),
                }
            }
            out
        }
        Err(_) => p.to_path_buf(),
    }
}

/// Does `path` (as given and with links resolved) match one of the patterns?
pub(crate) fn path_matches(patterns: &[BString], path: &Path, icase: bool) -> bool {
    let abs = absolute(path);
    let mut texts = vec![path_bytes(&abs)];
    if let Some(real) = realpath(&abs) {
        texts.push(path_bytes(&real));
    }
    patterns
        .iter()
        .any(|p| texts.iter().any(|t| wildmatch(p, t, icase)))
}

/// `onbranch:` matching.
pub(crate) fn branch_matches(pattern: &[u8], branch: &[u8]) -> bool {
    let mut p = BString::from(pattern);
    if p.ends_with(b"/") {
        p.extend_from_slice(b"**");
    }
    wildmatch(&p, branch, false)
}

/// `hasconfig:remote.*.url:` matching of one URL.
pub(crate) fn url_matches(pattern: &[u8], url: &[u8]) -> bool {
    wildmatch(pattern, url, false)
}

/// Is this canonical name a `remote.<name>.url`?
pub(crate) fn is_remote_url(name: &BStr) -> bool {
    match crate::key::split_canonical(name) {
        (section, Some(_), key) => section == "remote" && key == "url",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn patterns() {
        let env = MapEnv::new([("HOME", "/nonexistent-home")]);
        let o = Origin::File("/etc/x/config".into());
        let p = |s: &str| prepare_path_pattern(s.as_bytes(), &o, &env).unwrap();
        assert_eq!(p("foo/"), vec![BString::from("**/foo/**")]);
        assert_eq!(p("/a/b"), vec![BString::from("/a/b")]);
        assert_eq!(p("~/foo/"), vec![BString::from("/nonexistent-home/foo/**")]);
        assert_eq!(p("")[0], "**/**");
        assert_eq!(p("./r/.git")[0], "/etc/x/r/.git");
        assert!(matches!(
            prepare_path_pattern(b"./x", &Origin::CommandLine, &env),
            Err(Error::RelativeIncludeCondition)
        ));
    }

    #[test]
    fn kinds() {
        assert!(matches!(
            include_kind(b"include.path".as_bstr()),
            Some(IncludeKind::Always)
        ));
        assert!(matches!(
            include_kind(b"includeif.gitdir:x.path".as_bstr()),
            Some(IncludeKind::If(c)) if c == "gitdir:x"
        ));
        assert!(include_kind(b"include.x.path".as_bstr()).is_none());
        assert!(is_remote_url(b"remote.origin.url".as_bstr()));
        assert!(!is_remote_url(b"remote.url".as_bstr()));
        assert!(branch_matches(b"foo-dir/", b"foo-dir/a/b/c"));
        assert!(!branch_matches(b"", b"main"));
    }
}
