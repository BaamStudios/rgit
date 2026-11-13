// SPDX-License-Identifier: MIT

//! Comparing index entries with the work tree.
//!
//! Every index entry caches the `lstat(2)` data of its file from when it
//! was last staged or refreshed. A file whose current data equals the
//! cached data is taken as unchanged without reading it. What is compared
//! (git-config(1) `core.*`, and the racy-git technical note):
//!
//! | Field | Compared |
//! |---|---|
//! | file type | always (regular file, symbolic link, directory for a gitlink); with `core.symlinks=false` a regular file may stand for a symlink entry |
//! | executable bit | with `core.fileMode=true` (the default) |
//! | mtime seconds | always |
//! | ctime seconds | with `core.trustCtime=true` (the default) |
//! | uid, gid, inode | with `core.checkStat=default` |
//! | size (32 bits) | always |
//! | nanoseconds | only with [`StatOptions::nsec`]; off by default, as in git builds without `USE_NSEC` (the default on Linux) |
//! | device | only with [`StatOptions::dev`]; off by default (`USE_STDEV`) |
//!
//! The stat fields rgit records ([`stat_from_metadata`]) are the same
//! `lstat` fields git records, so git and rgit can share an index without
//! either re-hashing files the other refreshed.
//!
//! **Racy git.** A file modified within the timestamp granularity of its
//! last staging keeps its cached data. Such an entry is *racily clean*
//! when its mtime is not older than the index file's own mtime
//! ([`is_racy`]); its content is then compared ([`Checker`]). Before an
//! index is written, racily clean entries whose content differs get their
//! cached size set to 0 ([`smudge_racy_entries`]), so the mismatch survives
//! later index writes that advance the index timestamp.

use std::cell::{Cell, RefCell};
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId, hash_object};
use rgit_index::{Entry, Index, Mode, Stat, Time};

use crate::os;

/// `core.checkStat`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CheckStat {
    /// `default`: all fields listed in the module documentation.
    #[default]
    Default,
    /// `minimal`: only whole seconds of mtime (and ctime) and the size.
    Minimal,
}

impl CheckStat {
    /// Parses a `core.checkStat` value; `None` for an unknown value (git
    /// ignores those).
    #[must_use]
    pub fn from_config(value: &[u8]) -> Option<CheckStat> {
        match value {
            b"default" => Some(CheckStat::Default),
            b"minimal" => Some(CheckStat::Minimal),
            _ => None,
        }
    }
}

/// What to compare; see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatOptions {
    /// `core.trustCtime` (default true).
    pub trust_ctime: bool,
    /// `core.checkStat`.
    pub check_stat: CheckStat,
    /// `core.fileMode` (default true): compare the executable bit.
    pub file_mode: bool,
    /// `core.symlinks` (default true): with false, a regular file stands in
    /// for a symbolic link entry.
    pub symlinks: bool,
    /// Compare nanoseconds of mtime and ctime, and use them for the racy
    /// check (git's `USE_NSEC` build option, off by default).
    pub nsec: bool,
    /// Compare the device number (git's `USE_STDEV`, off by default).
    pub dev: bool,
}

impl Default for StatOptions {
    fn default() -> Self {
        StatOptions {
            trust_ctime: true,
            check_stat: CheckStat::Default,
            file_mode: true,
            symlinks: true,
            nsec: false,
            dev: false,
        }
    }
}

/// Differences between cached and current stat data (a bit set).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Changes(u8);

impl Changes {
    /// Nothing differs.
    pub const NONE: Changes = Changes(0);
    /// Modification time.
    pub const MTIME: Changes = Changes(1);
    /// Inode change time.
    pub const CTIME: Changes = Changes(2);
    /// Owner or group.
    pub const OWNER: Changes = Changes(4);
    /// Inode or device number.
    pub const INODE: Changes = Changes(8);
    /// Size.
    pub const DATA: Changes = Changes(16);
    /// Executable bit.
    pub const MODE: Changes = Changes(32);
    /// File type (regular file, symbolic link, directory).
    pub const TYPE: Changes = Changes(64);

    /// Whether nothing differs.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether all bits of `other` are set.
    #[must_use]
    pub const fn contains(self, other: Changes) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any bit of `other` is set.
    #[must_use]
    pub const fn intersects(self, other: Changes) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Changes {
    type Output = Changes;
    fn bitor(self, rhs: Changes) -> Changes {
        Changes(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Changes {
    fn bitor_assign(&mut self, rhs: Changes) {
        self.0 |= rhs.0;
    }
}

/// The modification time of `meta` as the index stores it.
#[must_use]
pub fn mtime_of(meta: &Metadata) -> Time {
    stat_from_metadata(meta).mtime
}

/// The mtime of the index file at `path`, to be taken when the index is
/// read; `None` when the file does not exist.
///
/// # Errors
/// If `lstat` fails for another reason.
pub fn index_mtime(path: &Path) -> io::Result<Option<Time>> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(Some(mtime_of(&m))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The stat data git records for a file: all fields truncated to 32 bits.
#[cfg(unix)]
#[must_use]
pub fn stat_from_metadata(meta: &Metadata) -> Stat {
    use std::os::unix::fs::MetadataExt;
    let t = |secs: i64, nsecs: i64| Time {
        secs: secs as u32,
        nsecs: nsecs as u32,
    };
    Stat {
        ctime: t(meta.ctime(), meta.ctime_nsec()),
        mtime: t(meta.mtime(), meta.mtime_nsec()),
        dev: meta.dev() as u32,
        ino: meta.ino() as u32,
        uid: meta.uid(),
        gid: meta.gid(),
        size: meta.size() as u32,
    }
}

/// The stat data git records for a file (only times and size are available
/// on this platform).
#[cfg(not(unix))]
#[must_use]
pub fn stat_from_metadata(meta: &Metadata) -> Stat {
    let t = |st: io::Result<std::time::SystemTime>| {
        st.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| Time {
                secs: d.as_secs() as u32,
                nsecs: d.subsec_nanos(),
            })
            .unwrap_or_default()
    };
    Stat {
        ctime: t(meta.created()),
        mtime: t(meta.modified()),
        size: meta.len() as u32,
        ..Stat::default()
    }
}

#[cfg(unix)]
fn is_executable(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &Metadata) -> bool {
    false
}

/// The index mode for a work tree file, as `git add` records it. `old` is
/// the existing entry's mode: with `core.fileMode=false` its executable bit
/// is kept (a new file is `100644`), with `core.symlinks=false` a regular
/// file stays a symlink entry.
/// `None` for directories and other file types.
#[must_use]
pub fn mode_from_metadata(meta: &Metadata, opts: &StatOptions, old: Option<Mode>) -> Option<Mode> {
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Some(Mode::SYMLINK);
    }
    if !ft.is_file() {
        return None;
    }
    if let Some(old) = old {
        if !opts.symlinks && old.is_symlink() {
            return Some(Mode::SYMLINK);
        }
        if !opts.file_mode && old.is_file() {
            return Some(old);
        }
    }
    // Without core.fileMode a new file never gets the executable bit.
    Some(if opts.file_mode && is_executable(meta) {
        Mode::EXECUTABLE
    } else {
        Mode::FILE
    })
}

/// Compares the cached stat data of `entry` with `meta` (from `lstat`).
#[must_use]
pub fn compare(entry: &Entry, meta: &Metadata, opts: &StatOptions) -> Changes {
    let ft = meta.file_type();
    let mut changes = Changes::NONE;
    let mode = entry.mode;
    if mode.is_gitlink() {
        return if ft.is_dir() {
            Changes::NONE
        } else {
            Changes::TYPE
        };
    }
    if mode.is_symlink() {
        if !(ft.is_symlink() || (!opts.symlinks && ft.is_file())) {
            changes |= Changes::TYPE;
        }
    } else if mode.is_file() {
        if !ft.is_file() {
            changes |= Changes::TYPE;
        } else if opts.file_mode && (mode == Mode::EXECUTABLE) != is_executable(meta) {
            changes |= Changes::MODE;
        }
    } else {
        changes |= Changes::TYPE;
    }

    let cur = stat_from_metadata(meta);
    let old = &entry.stat;
    let minimal = opts.check_stat == CheckStat::Minimal;
    let nsec = opts.nsec && !minimal;
    if old.mtime.secs != cur.mtime.secs || (nsec && old.mtime.nsecs != cur.mtime.nsecs) {
        changes |= Changes::MTIME;
    }
    if opts.trust_ctime
        && (old.ctime.secs != cur.ctime.secs || (nsec && old.ctime.nsecs != cur.ctime.nsecs))
    {
        changes |= Changes::CTIME;
    }
    if !minimal {
        if old.uid != cur.uid || old.gid != cur.gid {
            changes |= Changes::OWNER;
        }
        if old.ino != cur.ino || (opts.dev && old.dev != cur.dev) {
            changes |= Changes::INODE;
        }
    }
    if old.size != cur.size {
        changes |= Changes::DATA;
    }
    changes
}

/// Whether `entry` is racily clean relative to an index file with mtime
/// `index_mtime`: its own mtime is the same as or newer than the index's.
/// Only whole seconds count unless [`StatOptions::nsec`] is set. Gitlinks
/// are never racy.
#[must_use]
pub fn is_racy(entry: &Entry, index_mtime: Time, opts: &StatOptions) -> bool {
    if entry.mode.is_gitlink() {
        return false;
    }
    let m = entry.stat.mtime;
    if opts.nsec {
        (m.secs, m.nsecs) >= (index_mtime.secs, index_mtime.nsecs)
    } else {
        m.secs >= index_mtime.secs
    }
}

/// The state of a work tree file relative to its index entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Matches the entry; nothing to do.
    Unchanged,
    /// The content matches, but the cached stat data does not: a refresh
    /// would record the new stat data.
    StatOnly,
    /// The content or the executable bit differs.
    Modified,
    /// Regular file, symbolic link or submodule turned into another type.
    TypeChanged,
    /// The file is missing (or a directory, or beyond a symbolic link).
    Deleted,
}

/// Decides whether work tree files match their index entries, reading
/// file contents only when the stat data cannot decide. Counts the files
/// it hashes ([`Checker::files_hashed`]).
#[derive(Debug)]
pub struct Checker {
    root: PathBuf,
    kind: HashKind,
    opts: StatOptions,
    index_mtime: Option<Time>,
    hashed: Cell<usize>,
    /// The last leading directory found to be a real directory.
    dir_cache: RefCell<Vec<u8>>,
}

impl Checker {
    /// A checker for the work tree at `root`. `index_mtime` is the mtime of
    /// the index file when it was read ([`index_mtime`]); `None` for an
    /// index that was not read from a file (then nothing is racy).
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        kind: HashKind,
        opts: StatOptions,
        index_mtime: Option<Time>,
    ) -> Checker {
        Checker {
            root: root.into(),
            kind,
            opts,
            index_mtime,
            hashed: Cell::new(0),
            dir_cache: RefCell::new(Vec::new()),
        }
    }

    /// The options in use.
    #[must_use]
    pub fn options(&self) -> &StatOptions {
        &self.opts
    }

    /// How many files [`Checker::content_differs`] has read and hashed.
    #[must_use]
    pub fn files_hashed(&self) -> usize {
        self.hashed.get()
    }

    /// The `lstat` data of the work tree file of `path`; `None` when it is
    /// missing or lies beyond a symbolic link.
    ///
    /// # Errors
    /// If `lstat` fails for a reason other than a missing path.
    pub fn lstat(&self, path: &[u8]) -> io::Result<Option<Metadata>> {
        if self.beyond_symlink(path) {
            return Ok(None);
        }
        match std::fs::symlink_metadata(os::join(&self.root, path)) {
            Ok(m) => Ok(Some(m)),
            Err(e) if is_missing(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether a leading directory of `path` is not a real directory.
    fn beyond_symlink(&self, path: &[u8]) -> bool {
        let Some(slash) = path.iter().rposition(|&b| b == b'/') else {
            return false;
        };
        let dir = &path[..slash];
        let mut cache = self.dir_cache.borrow_mut();
        let known = cache.as_slice() == dir
            || (cache.len() > dir.len() && cache.starts_with(dir) && cache[dir.len()] == b'/');
        if known {
            return false;
        }
        let mut start = 0;
        if cache.len() < dir.len() && dir.starts_with(&cache) && dir[cache.len()] == b'/' {
            start = cache.len() + 1;
        }
        for (i, &b) in dir.iter().enumerate().skip(start) {
            if b == b'/' && !self.is_real_dir(&dir[..i]) {
                return true;
            }
        }
        if !self.is_real_dir(dir) {
            return true;
        }
        *cache = dir.to_vec();
        false
    }

    fn is_real_dir(&self, rel: &[u8]) -> bool {
        std::fs::symlink_metadata(os::join(&self.root, rel)).is_ok_and(|m| m.is_dir())
    }

    /// The state of the work tree file of `entry`. Entries marked
    /// skip-worktree or assume-unchanged count as unchanged.
    ///
    /// # Errors
    /// If the file cannot be examined or read.
    pub fn check(&self, entry: &Entry) -> io::Result<Status> {
        if entry.flags.skip_worktree || entry.flags.assume_valid {
            return Ok(Status::Unchanged);
        }
        let Some(meta) = self.lstat(entry.path())? else {
            return Ok(Status::Deleted);
        };
        self.check_with(entry, &meta)
    }

    /// Like [`Checker::check`] with `lstat` data the caller already has,
    /// ignoring the assume-unchanged bit (`--really-refresh`).
    ///
    /// # Errors
    /// If the file cannot be read.
    pub fn check_with(&self, entry: &Entry, meta: &Metadata) -> io::Result<Status> {
        if meta.is_dir() && !entry.mode.is_gitlink() {
            return Ok(Status::Deleted);
        }
        if entry.flags.intent_to_add {
            return Ok(Status::Modified);
        }
        let changes = compare(entry, meta, &self.opts);
        if changes.contains(Changes::TYPE) {
            return Ok(Status::TypeChanged);
        }
        if changes.contains(Changes::MODE) {
            return Ok(Status::Modified);
        }
        if entry.mode.is_gitlink() {
            return Ok(Status::Unchanged);
        }
        if changes.is_empty() {
            let racy = self
                .index_mtime
                .is_some_and(|t| is_racy(entry, t, &self.opts));
            if racy && self.content_differs(entry, meta)? {
                return Ok(Status::Modified);
            }
            return Ok(Status::Unchanged);
        }
        // A size difference is final, unless the cached size was smudged to
        // 0 for a racily clean entry (or the entry never had stat data).
        if changes.contains(Changes::DATA) && entry.stat.size != 0 {
            return Ok(Status::Modified);
        }
        if self.content_differs(entry, meta)? {
            Ok(Status::Modified)
        } else {
            Ok(Status::StatOnly)
        }
    }

    /// Whether the work tree content of `entry` differs from its blob: the
    /// file (or the target of the symbolic link) is read and hashed as a
    /// blob, without content filters.
    ///
    /// # Errors
    /// If the file cannot be read.
    pub fn content_differs(&self, entry: &Entry, meta: &Metadata) -> io::Result<bool> {
        let path = os::join(&self.root, entry.path());
        let data = if meta.file_type().is_symlink() {
            os::path_bytes(&std::fs::read_link(&path)?)
        } else {
            std::fs::read(&path)?
        };
        self.hashed.set(self.hashed.get() + 1);
        Ok(blob_id(self.kind, &data)? != entry.id)
    }

    /// Records the current stat data (and, with [`mode_from_metadata`], the
    /// mode) of a file whose content was found unchanged.
    pub fn refresh(&self, entry: &mut Entry, meta: &Metadata) {
        entry.stat = stat_from_metadata(meta);
        if let Some(mode) = mode_from_metadata(meta, &self.opts, Some(entry.mode)) {
            entry.mode = mode;
        }
    }
}

fn blob_id(kind: HashKind, data: &[u8]) -> io::Result<ObjectId> {
    hash_object(kind, "blob", data).map_err(|e| io::Error::other(e.to_string()))
}

fn is_missing(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Before writing `index`, which was read when the index file had mtime
/// `reference`: sets the cached size of every racily clean entry whose
/// work tree file differs from its blob to 0, so the entry
/// stays "changed" after the new index gets a newer timestamp. Returns the
/// number of smudged entries.
///
/// # Errors
/// If a work tree file cannot be read.
pub fn smudge_racy_entries(
    index: &mut Index,
    reference: Time,
    checker: &Checker,
) -> io::Result<usize> {
    let mut smudge = Vec::new();
    for (pos, e) in index.entries().iter().enumerate() {
        if e.stage().get() != 0
            || e.flags.skip_worktree
            || e.flags.assume_valid
            || e.stat.size == 0
            || !is_racy(e, reference, checker.options())
        {
            continue;
        }
        let differs = match checker.lstat(e.path())? {
            Some(meta) if compare(e, &meta, checker.options()).is_empty() => {
                checker.content_differs(e, &meta)?
            }
            // Missing or with changed stat data: not racily clean.
            _ => false,
        };
        if differs {
            smudge.push(pos);
        }
    }
    for &pos in &smudge {
        if let Some(e) = index.entry_mut(pos) {
            e.stat.size = 0;
        }
    }
    Ok(smudge.len())
}
