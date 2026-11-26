// SPDX-License-Identifier: MIT
//! [`Repository::status`]: what `git status` shows, as data.
//!
//! Three comparisons: the tree of `HEAD` with the index (what the next
//! commit changes), the index with the work tree (what is not staged),
//! and the work tree with the index and the ignore rules (what is not
//! tracked). Written from `Documentation/git-status.adoc` (short format)
//! and checked against `git status --porcelain` in the differential tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use bstr::BString;
use rgit_config::Environment;
use rgit_hash::ObjectId;
use rgit_index::{Index, Stage};
use rgit_object::{ObjectKind, TreeIter};
use rgit_worktree::ignore::{Excludes, default_excludes_file};
use rgit_worktree::stat::{CheckStat, Checker, StatOptions, index_mtime};

use crate::convert::Filter;
use crate::error::Diagnostic;
use crate::{Cancel, Error, Head, Repository, Result, messages};

/// How one side of a path differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Change {
    /// No difference.
    Unmodified,
    /// The path is new.
    Added,
    /// Content or executable bit differ.
    Modified,
    /// The path is gone.
    Deleted,
    /// A file became a symbolic link or a submodule, or the reverse.
    TypeChanged,
}

impl Change {
    fn letter(self) -> u8 {
        match self {
            Change::Unmodified => b' ',
            Change::Added => b'A',
            Change::Modified => b'M',
            Change::Deleted => b'D',
            Change::TypeChanged => b'T',
        }
    }
}

/// How the two sides of an unfinished merge left a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Conflict {
    /// Both sides deleted it (`DD`).
    BothDeleted,
    /// We added it (`AU`).
    AddedByUs,
    /// They deleted it, we changed it (`UD`).
    DeletedByThem,
    /// They added it (`UA`).
    AddedByThem,
    /// We deleted it, they changed it (`DU`).
    DeletedByUs,
    /// Both sides added it (`AA`).
    BothAdded,
    /// Both sides changed it (`UU`).
    BothModified,
}

impl Conflict {
    /// From the stages present in the index (bit 0: base, bit 1: ours,
    /// bit 2: theirs).
    fn from_stages(mask: u8) -> Conflict {
        match mask {
            0b001 => Conflict::BothDeleted,
            0b010 => Conflict::AddedByUs,
            0b011 => Conflict::DeletedByThem,
            0b100 => Conflict::AddedByThem,
            0b101 => Conflict::DeletedByUs,
            0b110 => Conflict::BothAdded,
            _ => Conflict::BothModified,
        }
    }

    fn code(self) -> [u8; 2] {
        *match self {
            Conflict::BothDeleted => b"DD",
            Conflict::AddedByUs => b"AU",
            Conflict::DeletedByThem => b"UD",
            Conflict::AddedByThem => b"UA",
            Conflict::DeletedByUs => b"DU",
            Conflict::BothAdded => b"AA",
            Conflict::BothModified => b"UU",
        }
    }
}

/// The state of one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum State {
    /// A tracked path that differs on at least one side.
    Tracked {
        /// `HEAD` compared with the index: what a commit would record.
        index: Change,
        /// The index compared with the work tree: what is not staged. A
        /// path added with `git add --intent-to-add` is [`Change::Added`]
        /// here.
        worktree: Change,
    },
    /// A path with an unresolved merge conflict.
    Conflicted(Conflict),
    /// A file (or, collapsed, a directory: the path then ends in `/`)
    /// that is neither tracked nor ignored.
    Untracked,
}

/// One line of a status.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StatusEntry {
    /// The path relative to the top of the work tree, with `/` between
    /// components. A byte string: file names need not be UTF-8.
    pub path: BString,
    /// What is the matter with it.
    pub state: State,
}

impl StatusEntry {
    /// The two status letters of `git status --short` (`XY`): `M `, ` M`,
    /// `A `, `??`, `UU`, ...
    #[must_use]
    pub fn code(&self) -> [u8; 2] {
        match self.state {
            State::Tracked { index, worktree } => [index.letter(), worktree.letter()],
            State::Conflicted(conflict) => conflict.code(),
            State::Untracked => *b"??",
        }
    }
}

/// Which untracked files a status lists (`--untracked-files=<mode>`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UntrackedFiles {
    /// None; the work tree is not searched for them.
    No,
    /// Untracked files, and a directory without tracked files as one
    /// entry.
    #[default]
    Normal,
    /// Every untracked file, also inside untracked directories.
    All,
}

/// What [`Repository::status`] computes.
#[derive(Debug, Clone, Default)]
pub struct StatusOptions {
    untracked: UntrackedFiles,
    cancel: Option<Cancel>,
}

impl StatusOptions {
    /// The defaults of `git status`.
    #[must_use]
    pub fn new() -> StatusOptions {
        StatusOptions::default()
    }

    /// Which untracked files to list.
    #[must_use]
    pub fn untracked_files(mut self, mode: UntrackedFiles) -> StatusOptions {
        self.untracked = mode;
        self
    }

    /// Stop with [`Error::Cancelled`] once `cancel` is cancelled.
    #[must_use]
    pub fn cancel(mut self, cancel: Cancel) -> StatusOptions {
        self.cancel = Some(cancel);
        self
    }
}

/// The result of [`Repository::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Status {
    /// Where `HEAD` points.
    pub head: Head,
    /// The paths that are not clean, in the order of `git status
    /// --short`: tracked and conflicted paths sorted by name, then the
    /// untracked ones sorted by name.
    pub entries: Vec<StatusEntry>,
}

impl Status {
    /// Nothing to commit and nothing untracked.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Mode and object of a path in a tree or in the index.
type Side = (u32, ObjectId);

/// The mode as git compares it: permission bits of files reduced to the
/// executable bit.
fn canonical_mode(mode: u32) -> u32 {
    if mode & 0o170_000 == 0o100_000 {
        if mode & 0o100 != 0 {
            0o100_755
        } else {
            0o100_644
        }
    } else {
        mode & 0o170_000
    }
}

fn compare(old: Option<&Side>, new: Option<&Side>) -> Change {
    match (old, new) {
        (None, None) => Change::Unmodified,
        (None, Some(_)) => Change::Added,
        (Some(_), None) => Change::Deleted,
        (Some(a), Some(b)) => {
            if a.0 & 0o170_000 != b.0 & 0o170_000 {
                Change::TypeChanged
            } else if a != b {
                Change::Modified
            } else {
                Change::Unmodified
            }
        }
    }
}

#[cfg(unix)]
fn os_path(top: &Path, rel: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    top.join(std::ffi::OsStr::from_bytes(rel))
}

#[cfg(not(unix))]
fn os_path(top: &Path, rel: &[u8]) -> PathBuf {
    top.join(String::from_utf8_lossy(rel).as_ref())
}

fn stat_options(repo: &Repository) -> StatOptions {
    let flag = |key: &str| repo.config.bool(key).ok().flatten().unwrap_or(true);
    StatOptions {
        trust_ctime: flag("core.trustctime"),
        check_stat: match repo.config.string("core.checkstat") {
            Ok(Some(v)) => CheckStat::from_config(v.as_ref()).unwrap_or_default(),
            _ => CheckStat::default(),
        },
        file_mode: flag("core.filemode"),
        symlinks: flag("core.symlinks"),
        ..StatOptions::default()
    }
}

/// Searches the work tree for files that are neither in the index nor
/// ignored.
struct UntrackedWalk<'a> {
    top: &'a Path,
    index: &'a Index,
    excludes: Excludes,
    all: bool,
    cancel: Option<&'a Cancel>,
    out: Vec<BString>,
    warnings: Vec<Diagnostic>,
}

impl UntrackedWalk<'_> {
    /// Visit the directory `rel` (empty or ending in `/`). With `emit`
    /// the untracked paths are collected; without, the search stops at the
    /// first one. Returns whether there is any.
    fn walk(&mut self, rel: &mut Vec<u8>, emit: bool) -> Result<bool> {
        let dir = os_path(self.top, rel);
        let mut names: Vec<(Vec<u8>, bool)> = Vec::new();
        let listing = match std::fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(e) => {
                self.warnings
                    .push(Diagnostic::warning(Error::io(dir, e).to_string()));
                return Ok(false);
            }
        };
        for entry in listing {
            let entry = entry.map_err(|e| Error::io(&dir, e))?;
            // The type of the entry itself: a symbolic link to a directory
            // is a file here.
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            names.push((entry.file_name().as_encoded_bytes().to_vec(), is_dir));
        }
        names.sort();
        let mut found = false;
        for (name, is_dir) in names {
            if let Some(cancel) = self.cancel {
                cancel.check()?;
            }
            if name == b".git" {
                continue;
            }
            let len = rel.len();
            rel.extend_from_slice(&name);
            if is_dir {
                found |= self.directory(rel, emit)?;
            } else if self.index.entries_for_path(rel).is_empty()
                && !self.excludes.is_excluded(rel, false)
            {
                if emit {
                    self.out.push(rel.as_slice().into());
                }
                found = true;
            }
            rel.truncate(len);
            if found && !emit {
                break;
            }
        }
        Ok(found)
    }

    /// The directory `rel` (no trailing `/`): a submodule, an ignored
    /// directory, one with tracked files (searched), another repository
    /// (one entry) or an untracked directory (one entry, or its files).
    fn directory(&mut self, rel: &mut Vec<u8>, emit: bool) -> Result<bool> {
        if !self.index.entries_for_path(rel).is_empty() || self.excludes.is_excluded(rel, true) {
            return Ok(false);
        }
        let has_tracked = !self.index.dir_range(rel).is_empty();
        let nested_repository = !has_tracked && os_path(self.top, rel).join(".git").exists();
        rel.push(b'/');
        if has_tracked || (self.all && !nested_repository) {
            return self.walk(rel, emit);
        }
        let found = nested_repository || self.walk(rel, false)?;
        if found && emit {
            self.out.push(rel.as_slice().into());
        }
        Ok(found)
    }
}

impl Repository {
    /// The entries of tree `id` and of the trees below it, by full path.
    fn flatten_tree(
        &self,
        id: &ObjectId,
        prefix: &mut Vec<u8>,
        out: &mut BTreeMap<Vec<u8>, Side>,
        cancel: Option<&Cancel>,
    ) -> Result<()> {
        if let Some(cancel) = cancel {
            cancel.check()?;
        }
        let hash = self.object_format();
        let data = match self.read_kind(id, ObjectKind::Tree) {
            Ok(data) => data,
            // The empty tree exists without being stored.
            Err(Error::ObjectNotFound { .. }) if *id == hash.empty_tree() => Vec::new(),
            Err(e) => return Err(e),
        };
        for entry in TreeIter::new(hash, &data) {
            let entry = entry.map_err(|_| Error::corrupt_message(messages::bad_tree(id)))?;
            let len = prefix.len();
            prefix.extend_from_slice(entry.name);
            if entry.is_tree() {
                prefix.push(b'/');
                self.flatten_tree(&entry.oid, prefix, out, cancel)?;
            } else {
                out.insert(prefix.clone(), (canonical_mode(entry.mode_bits), entry.oid));
            }
            prefix.truncate(len);
        }
        Ok(())
    }

    /// The status of the work tree: staged changes, unstaged changes and
    /// untracked files.
    ///
    /// ```no_run
    /// # fn main() -> rgit::Result<()> {
    /// let repo = rgit::Repository::open(".")?;
    /// let status = repo.status(rgit::StatusOptions::new())?;
    /// if status.is_clean() {
    ///     println!("nothing to commit, working tree clean");
    /// }
    /// for entry in &status.entries {
    ///     let code = entry.code();
    ///     println!("{}{} {}", code[0] as char, code[1] as char, entry.path);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The index file is not rewritten (the `git` command refreshes the
    /// cached file times as a side effect; the library reads only).
    ///
    /// Limits of this version, each an item of ADR 0005: renames are
    /// reported as a deletion and an addition (`git status
    /// --no-renames`); a submodule is reported when its checked-out
    /// commit differs from the index, not for changes inside it; filter
    /// drivers are not run (see the errors).
    ///
    /// # Errors
    /// [`Error::BareRepository`] without a work tree;
    /// [`Error::Unsupported`] if a file has to be read to be compared
    /// (its cached file times do not prove it unchanged) and its content
    /// goes through a `filter.<name>.clean` or `.process` driver;
    /// [`Error::Corrupt`] for a damaged index or tree;
    /// [`Error::Io`] if a tracked file cannot be examined.
    pub fn status(&self, options: StatusOptions) -> Result<Status> {
        let top = self.require_work_dir()?.to_path_buf();
        let cancel = options.cancel.as_ref();
        let head = self.head()?;
        let index = Rc::new(self.read_index()?);

        let mut head_tree = BTreeMap::new();
        if let Some(id) = head.id() {
            let tree = self.parse_commit(&id)?.tree;
            self.flatten_tree(&tree, &mut Vec::new(), &mut head_tree, cancel)?;
        }

        // The index side of the first comparison, and the conflicts.
        let mut staged: BTreeMap<Vec<u8>, Side> = BTreeMap::new();
        let mut conflicts: BTreeMap<Vec<u8>, u8> = BTreeMap::new();
        for entry in index.entries() {
            let stage = entry.stage().get();
            if stage != 0 {
                *conflicts.entry(entry.path().to_vec()).or_default() |= 1 << (stage - 1);
            } else if entry.mode.is_dir() {
                // A directory of a sparse index stands for its tree.
                let mut prefix = entry.path().to_vec();
                if prefix.last() != Some(&b'/') {
                    prefix.push(b'/');
                }
                self.flatten_tree(&entry.id, &mut prefix, &mut staged, cancel)?;
            } else if !entry.flags.intent_to_add {
                staged.insert(
                    entry.path().to_vec(),
                    (canonical_mode(entry.mode.0), entry.id),
                );
            }
        }

        let mut tracked: BTreeMap<Vec<u8>, State> = BTreeMap::new();
        for path in head_tree.keys().chain(staged.keys()) {
            if conflicts.contains_key(path) || tracked.contains_key(path) {
                continue;
            }
            let change = compare(head_tree.get(path), staged.get(path));
            if change != Change::Unmodified {
                tracked.insert(
                    path.clone(),
                    State::Tracked {
                        index: change,
                        worktree: Change::Unmodified,
                    },
                );
            }
        }
        for (path, mask) in &conflicts {
            tracked.insert(
                path.clone(),
                State::Conflicted(Conflict::from_stages(*mask)),
            );
        }

        // The index compared with the work tree.
        let filter = Rc::new(Filter::new(self, Rc::clone(&index))?);
        let index_file = self.git_dir().join("index");
        let checker = Checker::new(
            &top,
            self.object_format(),
            stat_options(self),
            index_mtime(&index_file).map_err(|e| Error::io(&index_file, e))?,
        )
        .with_filter(Rc::clone(&filter) as Rc<dyn rgit_worktree::stat::ContentFilter>);
        for entry in index.entries() {
            if entry.stage() != Stage::MERGED || entry.mode.is_dir() {
                continue;
            }
            if let Some(cancel) = cancel {
                cancel.check()?;
            }
            use rgit_worktree::stat::Status as Checked;
            let checked = checker
                .check(entry)
                .map_err(|e| Error::io(os_path(&top, entry.path()), e))?;
            let mut change = match checked {
                Checked::Unchanged | Checked::StatOnly => Change::Unmodified,
                Checked::Modified if entry.flags.intent_to_add => Change::Added,
                Checked::Modified => Change::Modified,
                Checked::TypeChanged => Change::TypeChanged,
                Checked::Deleted => Change::Deleted,
            };
            if change == Change::Unmodified && entry.mode.is_gitlink() {
                let checked_out = self.checked_out_commit(&os_path(&top, entry.path()));
                if checked_out.is_some_and(|id| id != entry.id) {
                    change = Change::Modified;
                }
            }
            if change == Change::Unmodified {
                continue;
            }
            let state = tracked
                .entry(entry.path().to_vec())
                .or_insert(State::Tracked {
                    index: Change::Unmodified,
                    worktree: change,
                });
            if let State::Tracked { worktree, .. } = state {
                *worktree = change;
            }
        }
        if let Some(path) = filter.unsupported() {
            return Err(Error::Unsupported {
                feature: "filter drivers (filter.<name>.clean, filter.<name>.process)",
                detail: path.to_string(),
            });
        }

        let mut entries: Vec<StatusEntry> = tracked
            .into_iter()
            .map(|(path, state)| StatusEntry {
                path: path.into(),
                state,
            })
            .collect();

        if options.untracked != UntrackedFiles::No {
            let mut untracked = self.untracked(&top, &index, options.untracked, cancel)?;
            untracked.sort();
            entries.extend(untracked.into_iter().map(|path| StatusEntry {
                path,
                state: State::Untracked,
            }));
        }
        Ok(Status { head, entries })
    }

    fn untracked(
        &self,
        top: &Path,
        index: &Index,
        mode: UntrackedFiles,
        cancel: Option<&Cancel>,
    ) -> Result<Vec<BString>> {
        let env: &dyn Environment = &*self.env;
        let mut excludes = Excludes::new(top);
        excludes.set_ignore_case(
            self.config
                .bool("core.ignorecase")
                .ok()
                .flatten()
                .unwrap_or(false),
        );
        let excludes_file = match self.config.path("core.excludesfile", env) {
            Ok(Some(path)) => Some(path),
            _ => default_excludes_file(|name| env.var(name)),
        };
        let info_exclude = self.common_dir().join("info").join("exclude");
        excludes.add_standard(
            excludes_file
                .as_deref()
                .map(|p| (p, p.display().to_string().into_bytes())),
            (&info_exclude, b".git/info/exclude".to_vec()),
        );
        let mut walk = UntrackedWalk {
            top,
            index,
            excludes,
            all: mode == UntrackedFiles::All,
            cancel,
            out: Vec::new(),
            warnings: Vec::new(),
        };
        walk.walk(&mut Vec::new(), true)?;
        let ignore_warnings = walk.excludes.take_warnings();
        self.push_diagnostics(
            walk.warnings
                .into_iter()
                .chain(ignore_warnings.into_iter().map(Diagnostic::warning)),
        );
        Ok(walk.out)
    }
}
