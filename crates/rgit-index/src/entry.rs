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
#[derive(Clone, PartialEq, Eq, Hash)]
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
        }
    }

    /// Path relative to the top of the worktree, `/` separated.
    pub fn path(&self) -> &[u8] {
        &self.path
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

/// Checks a path for the index: not empty, no leading or trailing `/`, no
/// empty, `.`, `..` or `.git` (any case) component.
pub fn verify_path(path: &[u8]) -> bool {
    !path.is_empty()
        && path
            .split(|&b| b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b".." && !c.eq_ignore_ascii_case(b".git"))
        && !path.contains(&0)
}
