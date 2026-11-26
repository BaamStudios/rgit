// SPDX-License-Identifier: MIT

//! Sparse checkout patterns (git-sparse-checkout(1), git-read-tree(1)
//! "SPARSE CHECKOUT", `Documentation/technical/sparse-checkout.adoc`).
//!
//! `$GIT_DIR/info/sparse-checkout` holds gitignore(5) style patterns that
//! select the paths present in the work tree. Two ways to read it:
//!
//! - **Full pattern set** (non-cone mode): the last pattern matching a
//!   path decides (a `!` pattern deselects). A path no pattern matches
//!   takes the decision of its nearest leading directory that one
//!   matches; with no match at all it is outside the checkout.
//! - **Cone mode** (`core.sparseCheckoutCone=true`): the file may only use
//!   the restricted forms `/*` (all of the top level), `!/*/` (no
//!   subdirectories of the top level), `/<dir>/` (everything below
//!   `<dir>`, *recursive*) and `!/<dir>/*/` right after `/<dir>/` (only the
//!   files directly in `<dir>`, *parent*). Matching then only compares
//!   directory names. A file that uses anything else is read as a full
//!   pattern set, with warnings ([`Sparse::parse`]).
//!
//! The cone file is written by [`cone_file`] from a list of recursive
//! directories: their leading directories become parent entries, nested
//! directories are folded into their recursive ancestor, and glob
//! characters in names are escaped with a backslash.

use std::collections::BTreeSet;

use crate::ignore::PatternList;
use crate::messages as msg;

/// Which part of the work tree a sparse checkout selects.
#[derive(Debug, Clone)]
pub struct Sparse {
    kind: Kind,
    icase: bool,
}

#[derive(Debug, Clone)]
enum Kind {
    Cone {
        /// Directories whose whole contents are included ("" is the top).
        recursive: BTreeSet<Vec<u8>>,
        /// Directories whose direct files are included ("" is the top).
        parent: BTreeSet<Vec<u8>>,
    },
    Full(PatternList),
}

/// How a directory relates to a cone-mode checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMatch {
    /// Everything below it is included.
    Included,
    /// Some files below it may be included.
    Partial,
    /// Nothing below it is included.
    Excluded,
}

/// The bytes with a meaning in globs; escaped in cone patterns.
fn needs_escape(b: u8) -> bool {
    matches!(b, b'\\' | b'*' | b'?' | b'[')
}

/// Escapes glob characters in a directory name for a cone pattern.
#[must_use]
pub fn escape(dir: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(dir.len());
    for &b in dir {
        if needs_escape(b) {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// Removes backslash escapes; `None` if the text has an unescaped glob
/// character (it is no plain directory name).
fn unescape(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'\\' => {
                let next = *text.get(i + 1)?;
                out.push(next);
                i += 2;
            }
            b'*' | b'?' | b'[' => return None,
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Some(out)
}

/// Every leading directory of `path` ("" for the top level first).
fn leading_dirs(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::once(&path[..0]).chain(
        path.iter()
            .enumerate()
            .filter(|(_, b)| **b == b'/')
            .map(|(i, _)| &path[..i]),
    )
}

fn lower(s: &[u8]) -> Vec<u8> {
    s.to_ascii_lowercase()
}

/// Reads a cone-mode file; `Err` with the warnings when it uses other
/// forms.
/// The recursive and the parent directories of a cone-mode file.
type ConeSets = (BTreeSet<Vec<u8>>, BTreeSet<Vec<u8>>);

fn parse_cone(data: &[u8]) -> Result<ConeSets, Vec<String>> {
    let mut recursive = BTreeSet::new();
    let mut parent = BTreeSet::new();
    // The negative patterns seen, as text without `!` and the trailing
    // slash (`/<dir>/*`).
    let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = trim_end_spaces(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let (negative, body) = match line.strip_prefix(b"!") {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let shown = body.strip_suffix(b"/").unwrap_or(body);
        let shown_s = String::from_utf8_lossy(shown).into_owned();
        if negative {
            // `!/<dir>/*/` (or `!/*/` for the top level).
            let dir = body
                .strip_prefix(b"/")
                .and_then(|b| b.strip_suffix(b"*/"))
                .and_then(|d| {
                    if d.is_empty() {
                        Some(Vec::new())
                    } else {
                        d.strip_suffix(b"/").and_then(unescape)
                    }
                });
            let Some(dir) = dir else {
                return Err(vec![msg::unrecognized_pattern(&shown_s)]);
            };
            if !recursive.contains(&dir) {
                return Err(vec![msg::unrecognized_negative_pattern(&shown_s)]);
            }
            recursive.remove(&dir);
            parent.insert(dir);
            seen.insert(shown.to_vec());
            continue;
        }
        if body == b"/*" {
            recursive.insert(Vec::new());
            continue;
        }
        let dir = body
            .strip_prefix(b"/")
            .and_then(|b| b.strip_suffix(b"/"))
            .filter(|d| !d.is_empty())
            .and_then(unescape);
        let Some(dir) = dir else {
            return Err(vec![msg::unrecognized_pattern(&shown_s)]);
        };
        // A negative pattern `!/<dir>/*/` is remembered as `/<dir>/*`, so
        // a directory named `*` repeats it. Repeated directories are fine.
        let mut key = b"/".to_vec();
        key.extend_from_slice(&dir);
        if seen.contains(&key) {
            return Err(vec![msg::pattern_repeated(&shown_s)]);
        }
        if !parent.contains(&dir) {
            recursive.insert(dir);
        }
    }
    Ok((recursive, parent))
}

fn trim_end_spaces(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && line[end - 1] == b' ' && !(end > 1 && line[end - 2] == b'\\') {
        end -= 1;
    }
    &line[..end]
}

impl Sparse {
    /// Reads the contents of a sparse-checkout file. With `cone` the file
    /// is first read as cone patterns; when that fails the returned
    /// warnings (without `warning: `) explain why and the full pattern set
    /// is used. `icase` is `core.ignoreCase`.
    #[must_use]
    pub fn parse(data: &[u8], cone: bool, icase: bool) -> (Sparse, Vec<String>) {
        let mut warnings = Vec::new();
        if cone {
            match parse_cone(data) {
                Ok((mut recursive, mut parent)) => {
                    if icase {
                        recursive = recursive.iter().map(|d| lower(d)).collect();
                        parent = parent.iter().map(|d| lower(d)).collect();
                    }
                    return (
                        Sparse {
                            kind: Kind::Cone { recursive, parent },
                            icase,
                        },
                        warnings,
                    );
                }
                Err(w) => {
                    warnings.extend(w);
                    warnings.push(msg::DISABLING_CONE.to_owned());
                }
            }
        }
        let list = PatternList::from_bytes(data, None, Vec::new());
        (
            Sparse {
                kind: Kind::Full(list),
                icase,
            },
            warnings,
        )
    }

    /// A cone-mode checkout of the recursive directories `dirs` (the top
    /// level files are always included).
    #[must_use]
    pub fn from_cone_dirs<I, D>(dirs: I, icase: bool) -> Sparse
    where
        I: IntoIterator<Item = D>,
        D: AsRef<[u8]>,
    {
        let data = cone_file(dirs);
        let (s, _) = Sparse::parse(&data, true, icase);
        s
    }

    /// Whether the patterns were read in cone mode.
    #[must_use]
    pub fn is_cone(&self) -> bool {
        matches!(self.kind, Kind::Cone { .. })
    }

    /// The recursive directories of a cone-mode checkout, sorted
    /// (`sparse-checkout list`); `None` for a full pattern set.
    #[must_use]
    pub fn cone_dirs(&self) -> Option<Vec<Vec<u8>>> {
        match &self.kind {
            Kind::Cone { recursive, .. } => Some(
                recursive
                    .iter()
                    .filter(|d| !d.is_empty())
                    .cloned()
                    .collect(),
            ),
            Kind::Full(_) => None,
        }
    }

    /// Whether the file (or symbolic link, or submodule) `path` belongs to
    /// the checkout.
    #[must_use]
    pub fn includes(&self, path: &[u8]) -> bool {
        match &self.kind {
            Kind::Cone { recursive, parent } => {
                let owned;
                let path = if self.icase {
                    owned = lower(path);
                    owned.as_slice()
                } else {
                    path
                };
                let mut last = &path[..0];
                for d in leading_dirs(path) {
                    if recursive.contains(d) {
                        return true;
                    }
                    last = d;
                }
                parent.contains(last)
            }
            Kind::Full(list) => {
                if let Some(p) = list.last_match(path, false, self.icase) {
                    return !p.is_negative();
                }
                let mut end = path.len();
                while let Some(pos) = path[..end].iter().rposition(|&b| b == b'/') {
                    if let Some(p) = list.last_match(&path[..pos], true, self.icase) {
                        return !p.is_negative();
                    }
                    end = pos;
                }
                false
            }
        }
    }

    /// How the directory `dir` (no trailing slash) relates to a cone-mode
    /// checkout; `None` for a full pattern set.
    #[must_use]
    pub fn dir_match(&self, dir: &[u8]) -> Option<DirMatch> {
        let Kind::Cone { recursive, parent } = &self.kind else {
            return None;
        };
        let owned;
        let dir = if self.icase {
            owned = lower(dir);
            owned.as_slice()
        } else {
            dir
        };
        if leading_dirs(dir).any(|d| recursive.contains(d)) || recursive.contains(dir) {
            return Some(DirMatch::Included);
        }
        if parent.contains(dir) {
            return Some(DirMatch::Partial);
        }
        let mut below = dir.to_vec();
        below.push(b'/');
        let inside = |set: &BTreeSet<Vec<u8>>| {
            set.range(below.clone()..)
                .next()
                .is_some_and(|d| d.starts_with(&below))
        };
        if inside(recursive) || inside(parent) {
            Some(DirMatch::Partial)
        } else {
            Some(DirMatch::Excluded)
        }
    }
}

/// The contents of a cone-mode sparse-checkout file selecting the
/// directories `dirs` (without leading or trailing slashes, unescaped).
#[must_use]
pub fn cone_file<I, D>(dirs: I) -> Vec<u8>
where
    I: IntoIterator<Item = D>,
    D: AsRef<[u8]>,
{
    let all: BTreeSet<Vec<u8>> = dirs
        .into_iter()
        .map(|d| d.as_ref().to_vec())
        .filter(|d| !d.is_empty())
        .collect();
    // Drop directories below another selected one.
    let recursive: BTreeSet<Vec<u8>> = all
        .iter()
        .filter(|d| !leading_dirs(d).skip(1).any(|p| all.contains(p)))
        .cloned()
        .collect();
    let mut parents: BTreeSet<Vec<u8>> = BTreeSet::new();
    for d in &recursive {
        for p in leading_dirs(d).skip(1) {
            if !recursive.contains(p) {
                parents.insert(p.to_vec());
            }
        }
    }
    let mut out = b"/*\n!/*/\n".to_vec();
    for p in &parents {
        let e = escape(p);
        out.push(b'/');
        out.extend_from_slice(&e);
        out.extend_from_slice(b"/\n!/");
        out.extend_from_slice(&e);
        out.extend_from_slice(b"/*/\n");
    }
    for d in &recursive {
        out.push(b'/');
        out.extend_from_slice(&escape(d));
        out.extend_from_slice(b"/\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cone(data: &str) -> Sparse {
        let (s, w) = Sparse::parse(data.as_bytes(), true, false);
        assert!(w.is_empty(), "{w:?}");
        s
    }

    #[test]
    fn writes_cone_file() {
        let f = cone_file(["foo/bar/baz", "a/b/c", "foo/bar/baz", "a/b"]);
        assert_eq!(
            String::from_utf8(f).unwrap(),
            "/*\n!/*/\n/a/\n!/a/*/\n/foo/\n!/foo/*/\n/foo/bar/\n!/foo/bar/*/\n/a/b/\n/foo/bar/baz/\n"
        );
        let f = cone_file(["deep", "deep/deeper1/deepest"]);
        assert_eq!(String::from_utf8(f).unwrap(), "/*\n!/*/\n/deep/\n");
        let f = cone_file(["zglob[!a]?", "zbad\\dir"]);
        assert_eq!(
            String::from_utf8(f).unwrap(),
            "/*\n!/*/\n/zbad\\\\dir/\n/zglob\\[!a]\\?/\n"
        );
    }

    #[test]
    fn cone_matching() {
        let s = cone("/*\n!/*/\n/deep/\n!/deep/*/\n/deep/deeper1/\n");
        assert!(s.includes(b"a"));
        assert!(s.includes(b"deep/a"));
        assert!(s.includes(b"deep/deeper1/x/y"));
        assert!(!s.includes(b"deep/deeper2/a"));
        assert!(!s.includes(b"folder1/a"));
        assert_eq!(s.dir_match(b"folder1"), Some(DirMatch::Excluded));
        assert_eq!(s.dir_match(b"deep"), Some(DirMatch::Partial));
        assert_eq!(s.dir_match(b"deep/deeper1"), Some(DirMatch::Included));
        assert_eq!(s.dir_match(b"deep/deeper2"), Some(DirMatch::Excluded));
        assert_eq!(s.cone_dirs().unwrap(), vec![b"deep/deeper1".to_vec()]);
        let all = cone("/*\n");
        assert!(all.includes(b"x/y/z"));
    }

    #[test]
    fn cone_rejections() {
        for (data, warning) in [
            ("/*\n!/*/\n/f1/**\n", "unrecognized pattern: '/f1/**'"),
            ("/*\n!/*/\n/\n", "unrecognized pattern: ''"),
            (
                "/*\n!/*/\n!/deep/deeper/*/\n",
                "unrecognized negative pattern: '/deep/deeper/*'",
            ),
            (
                "/*\n!/*/\n/foo/\n!/foo/*/\n/foo/\\*/\n",
                "your sparse-checkout file may have issues: pattern '/foo/\\*' is repeated",
            ),
            ("/deep/d1/a\n", "unrecognized pattern: '/deep/d1/a'"),
        ] {
            let (s, w) = Sparse::parse(data.as_bytes(), true, false);
            assert!(!s.is_cone(), "{data}");
            assert_eq!(w, [warning, "disabling cone pattern matching"], "{data}");
        }
    }

    #[test]
    fn full_patterns() {
        let full = |d: &str| Sparse::parse(d.as_bytes(), false, false).0;
        let s = full("sub\n!sub/added\n");
        assert!(!s.includes(b"init.t"));
        assert!(!s.includes(b"sub/added"));
        assert!(s.includes(b"sub/addedtoo"));
        let s = full("/*\n!sub\nsub/added\n");
        assert!(s.includes(b"init.t"));
        assert!(s.includes(b"sub/added"));
        assert!(!s.includes(b"sub/addedtoo"));
        assert!(s.includes(b"subsub/added"));
        let s = full("/*\n!/*/\n*folder*\n");
        assert!(s.includes(b"a"));
        assert!(s.includes(b"folder1/a"));
        assert!(!s.includes(b"deep/a"));
        assert!(!full("\n").includes(b"a"));
    }
}
