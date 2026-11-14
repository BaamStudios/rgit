// SPDX-License-Identifier: MIT
//! The untracked and ignored part of the work tree, shared by
//! `ls-files -o/-i`, `status` (untracked files, `--ignored`) and `clean`.
//!
//! [`Walker::walk`] visits the directories that hold tracked files and
//! returns the untracked paths it finds there as [`Node`]s: files, nested
//! repositories and untracked directories. An untracked directory is
//! scanned as a whole ([`Scan`]), so that each command can decide from the
//! summary whether to show it as one `dir/` entry or to list what is inside.
//! The rules of each command are in the functions at the end; they follow
//! git-ls-files(1), git-status(1), git-clean(1) and gitignore(5) and were
//! checked against the `git` binary (differential tests in `tests/`).
//!
//! A directory is only shown as a whole when the pathspec selects the
//! directory itself; otherwise the walk goes on inside it as if it held
//! tracked files.

use std::path::{Path, PathBuf};

use rgit_index::Index;
use rgit_pathspec::{DirInterest, Pathspec};
use rgit_worktree::ignore::Excludes;

use crate::messages::clean as msg;

/// What an untracked path is.
#[derive(Debug)]
pub(crate) enum Kind {
    /// A file, a symbolic link or anything else that is not a directory.
    File,
    /// A directory with a `.git` that makes it a repository of its own.
    Repo,
    /// A directory; `None` when it was not scanned (an excluded directory
    /// whose contents nobody asked for).
    Dir(Option<Scan>),
}

/// An untracked path.
#[derive(Debug)]
pub(crate) struct Node {
    /// Relative to the top of the work tree, without trailing slash.
    pub path: Vec<u8>,
    /// Excluded, by a pattern for the path itself or for a leading
    /// directory.
    pub excluded: bool,
    /// A directory where the index has a file: shown only by the listings
    /// that name files one by one.
    pub shadowing: bool,
    pub kind: Kind,
}

impl Node {
    /// The path with a trailing slash for directories and repositories.
    pub fn shown(&self) -> Vec<u8> {
        let mut p = self.path.clone();
        if !matches!(self.kind, Kind::File) {
            p.push(b'/');
        }
        p
    }
}

/// The contents of an untracked directory and what they amount to.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    pub children: Vec<Node>,
    /// Somewhere below is a file or repository that is not excluded.
    pub untracked: bool,
    /// Like `untracked`, but an empty directory that is not excluded also
    /// counts (and so does this directory when it is empty).
    pub untracked_or_empty: bool,
    /// Somewhere below is an excluded path.
    pub has_excluded: bool,
    /// Somewhere below is a file or repository, excluded or not.
    pub has_files: bool,
}

impl Scan {
    fn summarise(&mut self) {
        self.untracked = false;
        self.untracked_or_empty = self.children.is_empty();
        self.has_excluded = false;
        self.has_files = false;
        for c in &self.children {
            self.has_excluded |= c.excluded;
            match &c.kind {
                Kind::File | Kind::Repo => {
                    self.has_files = true;
                    if !c.excluded {
                        self.untracked = true;
                        self.untracked_or_empty = true;
                    }
                }
                Kind::Dir(Some(s)) => {
                    self.has_files |= s.has_files;
                    self.has_excluded |= s.has_excluded;
                    if !c.excluded {
                        self.untracked |= s.untracked;
                        self.untracked_or_empty |= s.untracked_or_empty;
                    }
                }
                Kind::Dir(None) => {}
            }
        }
    }
}

pub(crate) fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    if rel.is_empty() {
        return root.to_path_buf();
    }
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

/// Whether the directory `dir` is a repository of its own: its `.git` is a
/// repository directory, a `gitdir:` file pointing to one, or a file that
/// cannot be read (better kept than removed).
pub(crate) fn is_nested_repo(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
        return false;
    };
    let ov = rgit_repo::layout::LayoutOverrides::default();
    if meta.is_dir() {
        return rgit_repo::layout::is_git_directory(&dot_git, dir, &ov);
    }
    if !meta.is_file() {
        return false;
    }
    match rgit_repo::layout::read_gitfile(&dot_git, dir, &ov) {
        Ok(_) => true,
        Err(rgit_repo::GitfileError::Read) => std::fs::File::open(&dot_git).is_err(),
        Err(_) => false,
    }
}

/// Per-directory exclude files (`name`, usually `.gitignore`) that are
/// marked skip-worktree in the index are read from the index, not from the
/// work tree.
pub(crate) fn use_skip_worktree_ignores(
    repo: &crate::repository::Repo,
    index: &Index,
    name: &[u8],
    ex: &mut Excludes,
) {
    for e in index.entries() {
        if !e.flags.skip_worktree || e.stage().is_conflict() {
            continue;
        }
        let path = e.path();
        let (dir, base) = match path.iter().rposition(|&b| b == b'/') {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => (&path[..0], path),
        };
        if name != base {
            continue;
        }
        if let Some(rgit_object::Object::Blob(b)) = repo.read_object(&e.id) {
            ex.set_per_directory_contents(dir.to_vec(), b.data);
        }
    }
}

/// Sorted directory entries without `.git`: name and whether it is a
/// directory (symbolic links are not followed).
fn read_sorted(dir: &Path) -> std::io::Result<Vec<(Vec<u8>, bool)>> {
    let mut out: Vec<(Vec<u8>, bool)> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().as_encoded_bytes().to_vec();
            if name == b".git" {
                return None;
            }
            let is_dir = e.file_type().ok()?.is_dir();
            Some((name, is_dir))
        })
        .collect();
    out.sort();
    Ok(out)
}

/// The work tree walk.
pub(crate) struct Walker<'a> {
    pub root: &'a Path,
    pub index: &'a Index,
    pub excludes: Option<&'a mut Excludes>,
    pub pathspec: Option<&'a Pathspec>,
    /// Also scan excluded directories (for commands that show ignored
    /// paths one by one or need to know whether they hold files).
    pub scan_excluded: bool,
}

impl Walker<'_> {
    fn excluded(&mut self, path: &[u8], is_dir: bool) -> bool {
        match self.excludes.as_deref_mut() {
            Some(ex) => ex.is_excluded(path, is_dir),
            None => false,
        }
    }

    fn selects(&self, path: &[u8]) -> bool {
        self.pathspec.is_none_or(|ps| ps.match_path(path).is_some())
    }

    fn selects_dir(&self, path: &[u8]) -> bool {
        let mut d = path.to_vec();
        d.push(b'/');
        self.selects(&d)
    }

    fn interest(&self, path: &[u8]) -> DirInterest {
        self.pathspec
            .map_or(DirInterest::All, |ps| ps.dir_interest(path))
    }

    /// Walks the whole work tree; the untracked paths found in directories
    /// that hold tracked files (or that the pathspec does not select as a
    /// whole), in walk order.
    pub fn walk(&mut self) -> Vec<Node> {
        let mut out = Vec::new();
        self.walk_dir(b"", false, &mut out);
        out
    }

    fn walk_dir(&mut self, dir: &[u8], parent_excluded: bool, out: &mut Vec<Node>) {
        let Ok(entries) = read_sorted(&os_path(self.root, dir)) else {
            return;
        };
        for (name, is_dir) in entries {
            let mut path = dir.to_vec();
            path.extend_from_slice(&name);
            if !is_dir {
                if !self.index.path_range(&path).is_empty() || !self.selects(&path) {
                    continue;
                }
                let excluded = parent_excluded || self.excluded(&path, false);
                out.push(Node {
                    excluded,
                    path,
                    shadowing: false,
                    kind: Kind::File,
                });
                continue;
            }
            let at_path = self.index.entries_for_path(&path);
            if at_path.iter().any(|e| e.mode.is_gitlink()) {
                continue;
            }
            let shadowing = !at_path.is_empty();
            if self.interest(&path) == DirInterest::None {
                continue;
            }
            let excluded = parent_excluded || self.excluded(&path, true);
            let tracked_below = !self.index.dir_range(&path).is_empty();
            if !tracked_below && is_nested_repo(&os_path(self.root, &path)) {
                if self.selects_dir(&path) {
                    out.push(Node {
                        path,
                        excluded,
                        shadowing,
                        kind: Kind::Repo,
                    });
                }
                continue;
            }
            if tracked_below || (!shadowing && !self.selects_dir(&path)) {
                path.push(b'/');
                self.walk_dir(&path, excluded, out);
                continue;
            }
            let scan = (!excluded || self.scan_excluded).then(|| self.scan(&path, excluded));
            out.push(Node {
                path,
                excluded,
                shadowing,
                kind: Kind::Dir(scan),
            });
        }
    }

    /// Scans the untracked directory `dir` (no trailing slash).
    fn scan(&mut self, dir: &[u8], excluded: bool) -> Scan {
        let mut scan = Scan::default();
        let entries = match read_sorted(&os_path(self.root, dir)) {
            Ok(e) => e,
            Err(e) => {
                let mut shown = String::from_utf8_lossy(dir).into_owned();
                shown.push('/');
                eprintln!(
                    "warning: {}",
                    msg::could_not_open_dir(&shown, &crate::global::strerror(&e))
                );
                Vec::new()
            }
        };
        for (name, is_dir) in entries {
            let mut path = dir.to_vec();
            path.push(b'/');
            path.extend_from_slice(&name);
            let child_excluded = excluded || self.excluded(&path, is_dir);
            let kind = if !is_dir {
                Kind::File
            } else if is_nested_repo(&os_path(self.root, &path)) {
                Kind::Repo
            } else if !child_excluded || self.scan_excluded {
                Kind::Dir(Some(self.scan(&path, child_excluded)))
            } else {
                Kind::Dir(None)
            };
            scan.children.push(Node {
                path,
                excluded: child_excluded,
                shadowing: false,
                kind,
            });
        }
        scan.summarise();
        scan
    }
}

/// Sorts paths the way git lists them (byte order of the whole path, a
/// directory with its trailing slash).
pub(crate) fn sort_paths(paths: &mut Vec<Vec<u8>>) {
    paths.sort();
    paths.dedup();
}

// ---------------------------------------------------------------------
// ls-files -o [-i] [--directory] [--no-empty-directory]

/// What `ls-files --others` shows; `ignored` selects the excluded paths
/// (`-i`) instead of the others.
pub(crate) fn ls_files_others(
    roots: &[Node],
    ignored: bool,
    directory: bool,
    no_empty: bool,
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for n in roots.iter().filter(|n| !(directory && n.shadowing)) {
        ls_node(n, ignored, directory, no_empty, &mut out);
    }
    sort_paths(&mut out);
    out
}

fn ls_node(n: &Node, ignored: bool, directory: bool, no_empty: bool, out: &mut Vec<Vec<u8>>) {
    match &n.kind {
        Kind::File | Kind::Repo => {
            if n.excluded == ignored {
                out.push(n.shown());
            }
        }
        Kind::Dir(scan) => {
            if !directory {
                if let Some(s) = scan {
                    for c in &s.children {
                        ls_node(c, ignored, false, no_empty, out);
                    }
                }
                return;
            }
            if !ignored {
                let show = !n.excluded && scan.as_ref().is_some_and(|s| !no_empty || s.untracked);
                if show {
                    out.push(n.shown());
                }
                return;
            }
            let has_files = scan.as_ref().is_some_and(|s| s.has_files);
            if n.excluded {
                if !no_empty || has_files {
                    out.push(n.shown());
                }
                return;
            }
            let Some(s) = scan else { return };
            if no_empty {
                if !s.untracked && s.has_files {
                    out.push(n.shown());
                }
                return;
            }
            if !s.untracked_or_empty {
                out.push(n.shown());
            }
            for c in &s.children {
                ls_node(c, true, true, false, out);
            }
        }
    }
}

// ---------------------------------------------------------------------
// status

/// `status --ignored=<mode>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IgnoredMode {
    No,
    Traditional,
    Matching,
}

/// The untracked and the ignored paths, each sorted.
pub(crate) type StatusLists = (Vec<Vec<u8>>, Vec<Vec<u8>>);

/// The untracked and the ignored paths `status` shows; `all` is
/// `--untracked-files=all`.
pub(crate) fn status_lists(roots: &[Node], all: bool, ignored: IgnoredMode) -> StatusLists {
    let mut untracked = Vec::new();
    let mut ign = Vec::new();
    for n in roots.iter().filter(|n| all || !n.shadowing) {
        status_untracked(n, all, &mut untracked);
        match ignored {
            IgnoredMode::No => {}
            IgnoredMode::Traditional if all => all_excluded_files(n, &mut ign),
            IgnoredMode::Traditional => status_traditional(n, &mut ign),
            IgnoredMode::Matching => status_matching(n, &mut ign),
        }
    }
    sort_paths(&mut untracked);
    sort_paths(&mut ign);
    (untracked, ign)
}

fn status_untracked(n: &Node, all: bool, out: &mut Vec<Vec<u8>>) {
    if n.excluded {
        return;
    }
    match &n.kind {
        Kind::File | Kind::Repo => out.push(n.shown()),
        Kind::Dir(Some(s)) if all => {
            for c in &s.children {
                status_untracked(c, true, out);
            }
        }
        Kind::Dir(Some(s)) => {
            if s.untracked {
                out.push(n.shown());
            }
        }
        Kind::Dir(None) => {}
    }
}

/// Every excluded file below `n` (or `n` itself), one by one.
fn all_excluded_files(n: &Node, out: &mut Vec<Vec<u8>>) {
    match &n.kind {
        Kind::File | Kind::Repo => {
            if n.excluded {
                out.push(n.shown());
            }
        }
        Kind::Dir(Some(s)) => {
            for c in &s.children {
                all_excluded_files(c, out);
            }
        }
        Kind::Dir(None) => {}
    }
}

fn status_traditional(n: &Node, out: &mut Vec<Vec<u8>>) {
    match &n.kind {
        Kind::File | Kind::Repo => {
            if n.excluded {
                out.push(n.shown());
            }
        }
        Kind::Dir(scan) => {
            let Some(s) = scan else { return };
            if n.excluded || !s.untracked {
                if s.has_files {
                    out.push(n.shown());
                }
                return;
            }
            for c in &s.children {
                status_traditional(c, out);
            }
        }
    }
}

fn status_matching(n: &Node, out: &mut Vec<Vec<u8>>) {
    match &n.kind {
        Kind::File | Kind::Repo => {
            if n.excluded {
                out.push(n.shown());
            }
        }
        Kind::Dir(scan) => {
            if n.excluded {
                out.push(n.shown());
                return;
            }
            if let Some(s) = scan {
                for c in &s.children {
                    status_matching(c, out);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// clean

/// A path `clean` removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanItem {
    /// Relative to the top, without trailing slash.
    pub path: Vec<u8>,
    pub is_dir: bool,
}

/// What `clean` removes. `only_ignored` is `-X`; `dirs` is `-d` (or a
/// pathspec was given); `force_repos` is a second `-f`.
pub(crate) fn clean_items(
    roots: &[Node],
    only_ignored: bool,
    dirs: bool,
    force_repos: bool,
) -> Vec<CleanItem> {
    let mut out = Vec::new();
    for n in roots.iter().filter(|n| !n.shadowing) {
        if only_ignored {
            clean_ignored(n, force_repos, &mut out);
        } else {
            clean_untracked(n, dirs, force_repos, true, &mut out);
        }
    }
    if !dirs {
        out.retain(|i| !i.is_dir);
    }
    out.sort_by_key(item_key);
    out.dedup();
    out
}

fn item_key(i: &CleanItem) -> Vec<u8> {
    let mut k = i.path.clone();
    if i.is_dir {
        k.push(b'/');
    }
    k
}

fn item(n: &Node, is_dir: bool) -> CleanItem {
    CleanItem {
        path: n.path.clone(),
        is_dir,
    }
}

fn clean_untracked(n: &Node, dirs: bool, force_repos: bool, root: bool, out: &mut Vec<CleanItem>) {
    if n.excluded {
        return;
    }
    match &n.kind {
        Kind::File => out.push(item(n, false)),
        Kind::Repo => {
            if force_repos {
                out.push(item(n, true));
            }
        }
        Kind::Dir(scan) => {
            if root && !dirs {
                return;
            }
            let Some(s) = scan else { return };
            if !s.has_excluded {
                out.push(item(n, true));
                return;
            }
            for c in &s.children {
                clean_untracked(c, dirs, force_repos, false, out);
            }
        }
    }
}

fn clean_ignored(n: &Node, force_repos: bool, out: &mut Vec<CleanItem>) {
    match &n.kind {
        Kind::File => {
            if n.excluded {
                out.push(item(n, false));
            }
        }
        Kind::Repo => {
            if n.excluded && force_repos {
                out.push(item(n, true));
            }
        }
        Kind::Dir(scan) => {
            if n.excluded {
                out.push(item(n, true));
                return;
            }
            let Some(s) = scan else { return };
            if !s.untracked_or_empty {
                out.push(item(n, true));
                return;
            }
            for c in &s.children {
                clean_ignored(c, force_repos, out);
            }
        }
    }
}
