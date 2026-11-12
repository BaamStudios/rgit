// SPDX-License-Identifier: MIT
//! Knobs for `core.fsync` and `core.fsyncMethod`.
//!
//! Parsing the configuration (including the `-component` and `none` syntax
//! of `core.fsync`) belongs to the config crate; this module only names the
//! components and methods and performs the flush.

use std::fs::File;
use std::io;
use std::ops::{BitOr, BitOrAssign, Sub};

/// Set of repository components that are hardened (`core.fsync`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FsyncComponents(u32);

impl FsyncComponents {
    /// No component (`none`).
    pub const NONE: Self = Self(0);
    /// `loose-object`
    pub const LOOSE_OBJECT: Self = Self(1 << 0);
    /// `pack`
    pub const PACK: Self = Self(1 << 1);
    /// `pack-metadata`
    pub const PACK_METADATA: Self = Self(1 << 2);
    /// `commit-graph`
    pub const COMMIT_GRAPH: Self = Self(1 << 3);
    /// `index`
    pub const INDEX: Self = Self(1 << 4);
    /// `reference`
    pub const REFERENCE: Self = Self(1 << 5);
    /// `objects` = `loose-object,pack`
    pub const OBJECTS: Self = Self(Self::LOOSE_OBJECT.0 | Self::PACK.0);
    /// `derived-metadata` = `pack-metadata,commit-graph`
    pub const DERIVED_METADATA: Self = Self(Self::PACK_METADATA.0 | Self::COMMIT_GRAPH.0);
    /// `committed` (currently = `objects`)
    pub const COMMITTED: Self = Self::OBJECTS;
    /// `added` = `committed,index`
    pub const ADDED: Self = Self(Self::COMMITTED.0 | Self::INDEX.0);
    /// `all`
    pub const ALL: Self =
        Self(Self::OBJECTS.0 | Self::DERIVED_METADATA.0 | Self::INDEX.0 | Self::REFERENCE.0);
    /// Documented platform default: `committed,-loose-object`.
    pub const PLATFORM_DEFAULT: Self = Self(Self::COMMITTED.0 & !Self::LOOSE_OBJECT.0);

    /// Looks up a component or aggregate name as written in `core.fsync`
    /// (without a leading `-`). `none` is returned as [`Self::NONE`].
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "none" => Self::NONE,
            "loose-object" => Self::LOOSE_OBJECT,
            "pack" => Self::PACK,
            "pack-metadata" => Self::PACK_METADATA,
            "commit-graph" => Self::COMMIT_GRAPH,
            "index" => Self::INDEX,
            "reference" => Self::REFERENCE,
            "objects" => Self::OBJECTS,
            "derived-metadata" => Self::DERIVED_METADATA,
            "committed" => Self::COMMITTED,
            "added" => Self::ADDED,
            "all" => Self::ALL,
            _ => return None,
        })
    }

    /// True if every component of `other` is in `self`.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for FsyncComponents {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for FsyncComponents {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl Sub for FsyncComponents {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 & !rhs.0)
    }
}

/// How data is hardened (`core.fsyncMethod`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FsyncMethod {
    /// `fsync`: fsync(2) or the platform's full flush.
    Fsync,
    /// `writeout-only`: start page-cache writeback without a full flush.
    WriteoutOnly,
    /// `batch`: only meaningful for loose objects, where the object writer
    /// issues writeout-only flushes and one final full flush itself. For
    /// any single file [`harden`] treats it like [`FsyncMethod::Fsync`].
    Batch,
}

impl Default for FsyncMethod {
    /// `writeout-only` on macOS, `fsync` elsewhere (as documented).
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::WriteoutOnly
        } else {
            Self::Fsync
        }
    }
}

impl FsyncMethod {
    /// Looks up a `core.fsyncMethod` value.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fsync" => Some(Self::Fsync),
            "writeout-only" => Some(Self::WriteoutOnly),
            "batch" => Some(Self::Batch),
            _ => None,
        }
    }
}

/// Flushes `file` to stable storage using `method`.
pub fn harden(file: &File, method: FsyncMethod) -> io::Result<()> {
    match method {
        FsyncMethod::Fsync | FsyncMethod::Batch => file.sync_all(),
        FsyncMethod::WriteoutOnly => writeout_only(file),
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn writeout_only(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall on a valid descriptor.
    let rc = unsafe {
        libc::sync_file_range(
            file.as_raw_fd(),
            0,
            0,
            libc::SYNC_FILE_RANGE_WAIT_BEFORE | libc::SYNC_FILE_RANGE_WRITE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        let err = io::Error::last_os_error();
        // Not supported by every filesystem; fall back to a full flush.
        file.sync_data().or(Err(err))
    }
}

#[cfg(target_os = "macos")]
fn writeout_only(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // Plain fsync(2) on macOS does not flush the drive cache.
    // SAFETY: plain syscall on a valid descriptor.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn writeout_only(file: &File) -> io::Result<()> {
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates() {
        assert_eq!(FsyncComponents::PLATFORM_DEFAULT, FsyncComponents::PACK);
        assert!(FsyncComponents::ALL.contains(FsyncComponents::ADDED));
        assert!(!FsyncComponents::COMMITTED.contains(FsyncComponents::INDEX));
        assert_eq!(
            FsyncComponents::from_name("added"),
            Some(FsyncComponents::LOOSE_OBJECT | FsyncComponents::PACK | FsyncComponents::INDEX)
        );
        assert_eq!(FsyncMethod::from_name("batch"), Some(FsyncMethod::Batch));
    }
}
