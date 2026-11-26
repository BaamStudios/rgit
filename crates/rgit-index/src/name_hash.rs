// SPDX-License-Identifier: MIT

//! Case-insensitive lookup of index paths and of their leading directories
//! (`core.ignoreCase`, git-config(1)).
//!
//! On a case-insensitive file system `Makefile` and `makefile` name the same
//! file. With `core.ignoreCase` the commands that compare the work tree with
//! the index therefore ask whether a path is tracked *under any spelling*,
//! and new entries take the spelling the index already uses for their
//! directory. Only ASCII letters fold, as the reference git behaves.

use std::collections::HashMap;

use crate::index::Index;

/// The paths of an index and their leading directories, by folded name.
///
/// The table owns its names: it stays usable while the index it was built
/// from changes, and [`NameHash::insert`] records paths added meanwhile.
#[derive(Debug, Default)]
pub struct NameHash {
    /// Folded path → its spellings in index order.
    files: HashMap<Box<[u8]>, Vec<Box<[u8]>>>,
    /// Folded directory (no trailing slash) → its first spelling.
    dirs: HashMap<Box<[u8]>, Box<[u8]>>,
}

fn fold(path: &[u8]) -> Box<[u8]> {
    path.to_ascii_lowercase().into_boxed_slice()
}

impl NameHash {
    /// The names of every entry of `index` (all stages).
    pub fn new(index: &Index) -> NameHash {
        let mut names = NameHash {
            files: HashMap::with_capacity(index.len()),
            dirs: HashMap::new(),
        };
        let mut last: &[u8] = b"";
        for e in index.entries() {
            if e.path() == last && !last.is_empty() {
                continue;
            }
            last = e.path();
            names.insert(e.path());
        }
        names
    }

    /// Records `path` and its leading directories.
    pub fn insert(&mut self, path: &[u8]) {
        let spellings = self.files.entry(fold(path)).or_default();
        if !spellings.iter().any(|s| &**s == path) {
            spellings.push(path.into());
        }
        // Deepest directory first: once one is known, so are its parents.
        let mut end = path.len();
        while let Some(slash) = path[..end].iter().rposition(|&b| b == b'/') {
            let dir = &path[..slash];
            let key = fold(dir);
            if self.dirs.contains_key(&key) {
                break;
            }
            self.dirs.insert(key, dir.into());
            end = slash;
        }
    }

    /// The spelling under which `path` is in the index: `path` itself if it
    /// is there, else the first entry that differs only in case.
    pub fn file(&self, path: &[u8]) -> Option<&[u8]> {
        let spellings = self.files.get(&fold(path))?;
        spellings
            .iter()
            .find(|s| &***s == path)
            .or_else(|| spellings.first())
            .map(|s| &**s)
    }

    /// True if an entry differs from `path` at most in case.
    pub fn has_file(&self, path: &[u8]) -> bool {
        self.files.contains_key(&fold(path))
    }

    /// Every spelling of `path` in the index.
    pub fn spellings(&self, path: &[u8]) -> impl Iterator<Item = &[u8]> {
        self.files
            .get(&fold(path))
            .into_iter()
            .flat_map(|v| v.iter().map(|s| &**s))
    }

    /// The spelling the index uses for directory `dir` (trailing slashes are
    /// ignored), if entries live below a directory of that name in any case.
    pub fn dir(&self, dir: &[u8]) -> Option<&[u8]> {
        let dir = dir.strip_suffix(b"/").unwrap_or(dir);
        self.dirs.get(&fold(dir)).map(|s| &**s)
    }

    /// `path` with its leading directories spelled as the index spells them:
    /// the deepest leading directory that exists in any case is replaced by
    /// the existing spelling, the rest of the path is kept.
    pub fn adjust_dirname(&self, path: &[u8]) -> Vec<u8> {
        let mut end = path.len();
        while let Some(slash) = path[..end].iter().rposition(|&b| b == b'/') {
            if let Some(known) = self.dir(&path[..slash]) {
                let mut out = known.to_vec();
                out.extend_from_slice(&path[slash..]);
                return out;
            }
            end = slash;
        }
        path.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(paths: &[&str]) -> NameHash {
        let mut n = NameHash::default();
        for p in paths {
            n.insert(p.as_bytes());
        }
        n
    }

    #[test]
    fn files_fold_ascii_only() {
        let n = names(&["Makefile", "src/Main.rs", "\u{c4}"]);
        assert_eq!(n.file(b"makefile"), Some(&b"Makefile"[..]));
        assert_eq!(n.file(b"SRC/MAIN.RS"), Some(&b"src/Main.rs"[..]));
        assert!(n.has_file("\u{c4}".as_bytes()));
        assert!(!n.has_file("\u{e4}".as_bytes()));
        assert!(!n.has_file(b"src"));
    }

    #[test]
    fn exact_spelling_wins() {
        let n = names(&["A", "a"]);
        assert_eq!(n.file(b"a"), Some(&b"a"[..]));
        assert_eq!(n.file(b"A"), Some(&b"A"[..]));
        assert_eq!(n.spellings(b"a").count(), 2);
    }

    #[test]
    fn directories_and_adjusting() {
        let n = names(&["Dir/Sub/a", "other"]);
        assert_eq!(n.dir(b"dir"), Some(&b"Dir"[..]));
        assert_eq!(n.dir(b"DIR/SUB/"), Some(&b"Dir/Sub"[..]));
        assert_eq!(n.dir(b"other"), None);
        assert_eq!(n.adjust_dirname(b"dir/sub/b"), b"Dir/Sub/b");
        assert_eq!(n.adjust_dirname(b"dir/NEW/b"), b"Dir/NEW/b");
        assert_eq!(n.adjust_dirname(b"new/b"), b"new/b");
        assert_eq!(n.adjust_dirname(b"file"), b"file");
    }
}
