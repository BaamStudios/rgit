// SPDX-License-Identifier: MIT

//! Index entries.

use std::cmp::Ordering;
use std::fmt;

use rgit_hash::ObjectId;

/// A `stat(2)` timestamp as stored in the index (both parts truncated to
/// 32 bits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Time {
    /// Seconds since the epoch.
    pub secs: u32,
    /// Nanosecond fraction.
    pub nsecs: u32,
}

/// The `stat(2)` data cached for an entry to detect worktree changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stat {
    /// Last metadata change.
    pub ctime: Time,
    /// Last data change.
    pub mtime: Time,
    /// Device number.
    pub dev: u32,
    /// Inode number.
    pub ino: u32,
    /// Owner.
    pub uid: u32,
    /// Group.
    pub gid: u32,
    /// File size, truncated to 32 bits.
    pub size: u32,
}

/// The file mode of an entry (the 32-bit on-disk value).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mode(pub u32);

impl Mode {
    /// Regular, non-executable file (`100644`).
    pub const FILE: Mode = Mode(0o100644);
    /// Executable file (`100755`).
    pub const EXECUTABLE: Mode = Mode(0o100755);
    /// Symbolic link (`120000`).
    pub const SYMLINK: Mode = Mode(0o120000);
    /// Submodule commit (`160000`).
    pub const GITLINK: Mode = Mode(0o160000);
    /// Sparse directory entry (`040000`, sparse index only).
    pub const DIR: Mode = Mode(0o040000);

    /// The object type bits (`mode & 0o170000`).
    pub fn kind(self) -> u32 {
        self.0 & 0o170000
    }

    /// True for regular files (executable or not).
    pub fn is_file(self) -> bool {
        self.kind() == 0o100000
    }

    /// True for symbolic links.
    pub fn is_symlink(self) -> bool {
        self.kind() == 0o120000
    }

    /// True for gitlinks (submodules).
    pub fn is_gitlink(self) -> bool {
        self.kind() == 0o160000
    }

    /// True for sparse directory entries.
    pub fn is_dir(self) -> bool {
        self.kind() == 0o040000
    }
}

impl fmt::Debug for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mode({:06o})", self.0)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:06o}", self.0)
    }
}

/// The merge stage of an entry: 0 for a merged entry, 1 to 3 for the base,
/// "ours" and "theirs" versions of a conflict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stage(u8);

impl Stage {
    /// Merged (normal) entry.
    pub const MERGED: Stage = Stage(0);
    /// Common ancestor of a conflict.
    pub const BASE: Stage = Stage(1);
    /// Our side of a conflict.
    pub const OURS: Stage = Stage(2);
    /// Their side of a conflict.
    pub const THEIRS: Stage = Stage(3);

    /// Stage from a number; `None` unless `0..=3`.
    pub const fn new(n: u8) -> Option<Stage> {
        if n <= 3 { Some(Stage(n)) } else { None }
    }

    /// The stage number.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// True for stages 1 to 3.
    pub const fn is_conflict(self) -> bool {
        self.0 != 0
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Per-entry flags besides the stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags {
    /// "Assume valid" (`git update-index --assume-unchanged`).
    pub assume_valid: bool,
    /// Skip the worktree file (sparse checkout); needs an extended entry.
    pub skip_worktree: bool,
    /// Intent to add (`git add -N`); needs an extended entry.
    pub intent_to_add: bool,
}

/// On-disk flag bits.
pub(crate) mod bits {
    pub const ASSUME_VALID: u16 = 0x8000;
    pub const EXTENDED: u16 = 0x4000;
    pub const STAGE_MASK: u16 = 0x3000;
    pub const STAGE_SHIFT: u16 = 12;
    pub const NAME_MASK: u16 = 0x0fff;
    pub const SKIP_WORKTREE: u16 = 0x4000;
    pub const INTENT_TO_ADD: u16 = 0x2000;
    /// Every bit of the extended flags rgit understands.
    pub const EXTENDED_KNOWN: u16 = SKIP_WORKTREE | INTENT_TO_ADD;
}

impl Flags {
    /// True when the entry must be written with extended flags (and the
    /// index therefore as version 3 or later).
    pub fn needs_extended(self) -> bool {
        self.skip_worktree || self.intent_to_add
    }

    pub(crate) fn extended_bits(self) -> u16 {
        let mut v = 0;
        if self.skip_worktree {
            v |= bits::SKIP_WORKTREE;
        }
        if self.intent_to_add {
            v |= bits::INTENT_TO_ADD;
        }
        v
    }
}

/// One index entry: a path at a stage with its object id, mode and cached
/// stat data.
///
/// The path and stage determine the entry's position in the index, so they
/// can only be set when the entry is created.
#[derive(Clone)]
pub struct Entry {
    /// Cached `stat(2)` data.
    pub stat: Stat,
    /// File mode.
    pub mode: Mode,
    /// Object id of the blob, gitlink commit or (sparse index) tree.
    pub id: ObjectId,
    /// Flags besides the stage.
    pub flags: Flags,
    pub(crate) stage: Stage,
    pub(crate) path: Vec<u8>,
    /// Split index: 1 + the position of the shared index entry this one
    /// came from, 0 for an entry the shared index does not have. Not part
    /// of the entry's value (ignored by `==`).
    pub(crate) base: u32,
    /// `CE_FSMONITOR_VALID`: the file system monitor reported no change
    /// since the entry was last checked. Not part of the entry's value.
    pub(crate) fsm_valid: bool,
    /// The stat data the entry was read with (`None` for an entry made in
    /// memory): an entry whose stat data was filled anew counts as checked.
    pub(crate) read_stat: Option<Stat>,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Entry) -> bool {
        self.stat == other.stat
            && self.mode == other.mode
            && self.id == other.id
            && self.flags == other.flags
            && self.stage == other.stage
            && self.path == other.path
    }
}

impl Eq for Entry {}

impl std::hash::Hash for Entry {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.stat.hash(h);
        self.mode.hash(h);
        self.id.hash(h);
        self.flags.hash(h);
        self.stage.hash(h);
        self.path.hash(h);
    }
}

impl Entry {
    /// A new entry with empty stat data and no flags.
    pub fn new(path: impl Into<Vec<u8>>, stage: Stage, mode: Mode, id: ObjectId) -> Entry {
        Entry {
            stat: Stat::default(),
            mode,
            id,
            flags: Flags::default(),
            stage,
            path: path.into(),
            base: 0,
            fsm_valid: false,
            read_stat: None,
        }
    }

    /// Whether the file system monitor vouches for the entry
    /// (`CE_FSMONITOR_VALID`): marked valid, or given stat data from the
    /// work tree since it was read or created.
    pub fn fsmonitor_valid(&self) -> bool {
        self.fsm_valid
            || match self.read_stat {
                Some(s) => s != self.stat,
                None => self.stat != Stat::default(),
            }
    }

    /// Marks the entry valid or invalid for the file system monitor.
    pub fn set_fsmonitor_valid(&mut self, valid: bool) {
        self.fsm_valid = valid;
        self.read_stat = Some(self.stat);
    }

    /// Path relative to the top of the worktree, `/` separated.
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// The same entry under another path (`core.ignoreCase`: a new entry
    /// takes the spelling the index already uses for its directory).
    #[must_use]
    pub fn with_path(mut self, path: impl Into<Vec<u8>>) -> Entry {
        self.path = path.into();
        self
    }

    /// Merge stage.
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The flags as `git ls-files --debug` prints them (in hex): the
    /// on-disk 16-bit flags without the name length, plus the extended
    /// flags in the upper 16 bits.
    pub fn debug_flags(&self) -> u32 {
        let mut v = u32::from(self.stage.0) << bits::STAGE_SHIFT;
        if self.flags.assume_valid {
            v |= u32::from(bits::ASSUME_VALID);
        }
        if self.flags.needs_extended() {
            v |= u32::from(bits::EXTENDED);
        }
        v | (u32::from(self.flags.extended_bits()) << 16)
    }

    /// Index order: by path bytes, then by stage.
    pub fn cmp_key(&self, path: &[u8], stage: Stage) -> Ordering {
        self.path.as_slice().cmp(path).then(self.stage.cmp(&stage))
    }
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("path", &String::from_utf8_lossy(&self.path))
            .field("stage", &self.stage.0)
            .field("mode", &self.mode)
            .field("id", &self.id)
            .field("flags", &self.flags)
            .field("stat", &self.stat)
            .finish()
    }
}

/// The longest path [`verify_path`] accepts unless set otherwise.
pub const DEFAULT_MAX_PATH_LEN: usize = 1_073_740_799;

static MAX_PATH_LEN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(DEFAULT_MAX_PATH_LEN);

/// Sets the longest path [`verify_path`] accepts (`index.maxPathLen`) for
/// the whole process.
pub fn set_max_path_len(len: usize) {
    MAX_PATH_LEN.store(len, std::sync::atomic::Ordering::Relaxed);
}

static PROTECT_NTFS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static PROTECT_HFS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(cfg!(target_os = "macos"));

/// Sets `core.protectNTFS` and `core.protectHFS` for the path checks of the
/// whole process ([`verify_path`], [`verify_path_mode`], [`Index::add`]).
/// Defaults, as in git: NTFS protection on every platform, HFS+ protection
/// on macOS.
///
/// [`Index::add`]: crate::Index::add
pub fn set_path_protection(ntfs: bool, hfs: bool) {
    PROTECT_NTFS.store(ntfs, std::sync::atomic::Ordering::Relaxed);
    PROTECT_HFS.store(hfs, std::sync::atomic::Ordering::Relaxed);
}

/// The settings of [`set_path_protection`]: (`core.protectNTFS`,
/// `core.protectHFS`).
pub fn path_protection() -> (bool, bool) {
    (
        PROTECT_NTFS.load(std::sync::atomic::Ordering::Relaxed),
        PROTECT_HFS.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Checks a path for the index: not empty, no leading or trailing `/`, no
/// empty, `.`, `..` or `.git` (any case) component, not longer than the
/// limit of [`set_max_path_len`], and no component that NTFS or HFS+ would
/// take for `.git` while [`set_path_protection`] asks for that check.
pub fn verify_path(path: &[u8]) -> bool {
    let (ntfs, hfs) = path_protection();
    verify_path_with(path, None, ntfs, hfs)
}

/// [`verify_path`] for an entry of `mode`: a symbolic link may not be named
/// `.gitmodules` (or what NTFS or HFS+ take for it) in any directory.
pub fn verify_path_mode(path: &[u8], mode: Mode) -> bool {
    let (ntfs, hfs) = path_protection();
    verify_path_with(path, Some(mode), ntfs, hfs)
}

/// [`verify_path_mode`] with explicit `core.protectNTFS` (`ntfs`) and
/// `core.protectHFS` (`hfs`) settings.
///
/// - NTFS: `.git` followed only by spaces and dots, `.git` followed by an
///   alternate data stream (`.git::$INDEX_ALLOCATION`), the 8.3 short name
///   `git~1`, and any of these behind a backslash (NTFS separates
///   directories with backslashes too).
/// - HFS+: `.git` with code points HFS+ ignores when it compares names
///   (zero-width joiners, direction marks, ...).
pub fn verify_path_with(path: &[u8], mode: Option<Mode>, ntfs: bool, hfs: bool) -> bool {
    if path.is_empty()
        || path.len() > MAX_PATH_LEN.load(std::sync::atomic::Ordering::Relaxed)
        || path.contains(&0)
    {
        return false;
    }
    let mut comps = path.split(|&b| b == b'/').peekable();
    while let Some(c) = comps.next() {
        if c.is_empty() || c == b"." || c == b".." || c.eq_ignore_ascii_case(b".git") {
            return false;
        }
        if ntfs && c.split(|&b| b == b'\\').any(ntfs_dot_git) {
            return false;
        }
        if hfs && hfs_equivalent(c, b".git") {
            return false;
        }
        let last = comps.peek().is_none();
        if last && mode.is_some_and(Mode::is_symlink) {
            // NTFS also separates directories with backslashes.
            let file = c.rsplit(|&b| b == b'\\').next().unwrap_or(c);
            let gitmodules = c.eq_ignore_ascii_case(b".gitmodules")
                || (ntfs
                    && (file.eq_ignore_ascii_case(b".gitmodules") || ntfs_dot_gitmodules(file)))
                || (hfs && hfs_equivalent(c, b".gitmodules"));
            if gitmodules {
                return false;
            }
        }
    }
    true
}

/// NTFS ignores trailing dots and spaces, and `:` starts a stream name.
fn ntfs_tail_is_empty(tail: &[u8]) -> bool {
    for &b in tail {
        match b {
            b'.' | b' ' => {}
            b':' => return true,
            _ => return false,
        }
    }
    true
}

/// `.git` on NTFS: the name itself or its 8.3 short name `git~1`, compared
/// without case, with nothing but an ignored tail behind it.
fn ntfs_dot_git(c: &[u8]) -> bool {
    let tail = if c.len() >= 4 && c[0] == b'.' && c[1..4].eq_ignore_ascii_case(b"git") {
        &c[4..]
    } else if c.len() >= 5 && c[..5].eq_ignore_ascii_case(b"git~1") {
        &c[5..]
    } else {
        return false;
    };
    ntfs_tail_is_empty(tail)
}

/// `.gitmodules` on NTFS: the name itself or one of its 8.3 short names
/// (`gitmod~1` to `gitmod~4`, or the hashed form `gi7eba~1` to `gi7eba~9`
/// NTFS uses once four names with that prefix exist).
fn ntfs_dot_gitmodules(c: &[u8]) -> bool {
    const NAME: &[u8] = b"gitmodules";
    if c.len() > NAME.len() && c[0] == b'.' && c[1..=NAME.len()].eq_ignore_ascii_case(NAME) {
        return ntfs_tail_is_empty(&c[NAME.len() + 1..]);
    }
    if c.len() >= 8 && c[6] == b'~' {
        let (six, digit) = (&c[..6], c[7]);
        let short = six.eq_ignore_ascii_case(b"gitmod") && (b'1'..=b'4').contains(&digit);
        let hashed = six.eq_ignore_ascii_case(b"gi7eba") && (b'1'..=b'9').contains(&digit);
        if short || hashed {
            return ntfs_tail_is_empty(&c[8..]);
        }
    }
    false
}

/// Code points HFS+ ignores when comparing names.
fn hfs_ignorable(ch: char) -> bool {
    matches!(ch as u32,
        0x200c..=0x200f | 0x202a..=0x202e | 0x206a..=0x206f | 0xfeff)
}

fn hfs_equivalent(c: &[u8], name: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(c) else {
        return false;
    };
    let folded: String = s.chars().filter(|&ch| !hfs_ignorable(ch)).collect();
    folded.as_bytes().eq_ignore_ascii_case(name)
}
