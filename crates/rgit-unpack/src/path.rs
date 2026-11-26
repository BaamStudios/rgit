// SPDX-License-Identifier: MIT

//! Which tree paths may be checked out.
//!
//! The rules are those of the index ([`rgit_index::verify_path_with`]: no
//! empty, `.`, `..` or `.git` component in any case, no NUL), with the
//! repository's settings for the paths that a file system may treat as
//! `.git`:
//!
//! - NTFS (`core.protectNTFS`; the reference git refuses these on every
//!   platform unless the setting is false): `.git` followed only by spaces
//!   and dots, `.git` followed by an alternate data stream (`.git::$DATA`),
//!   and the 8.3 short name `git~1`.
//! - HFS+ (`core.protectHFS`, default only on macOS): `.git` with Unicode
//!   characters HFS+ ignores (zero-width joiners, direction marks, ...).
//! - A symbolic link named `.gitmodules` (or an NTFS/HFS+ equivalent) is
//!   refused in any directory.

use rgit_index::Mode;

/// The `core.protect*` switches and `core.ignoreCase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathOptions {
    /// `core.ignoreCase`: a work tree file is tracked if the index has its
    /// path in any case, so a file system that folds case does not make
    /// tracked files look like untracked files in the way.
    pub ignore_case: bool,
    /// `core.protectNTFS` (default true, as the reference git behaves).
    pub protect_ntfs: bool,
    /// `core.protectHFS` (default true on macOS only).
    pub protect_hfs: bool,
}

impl Default for PathOptions {
    fn default() -> Self {
        PathOptions {
            ignore_case: false,
            protect_ntfs: true,
            protect_hfs: cfg!(target_os = "macos"),
        }
    }
}

/// Whether a tree entry at `path` with `mode` may enter the index and the
/// work tree.
pub fn verify_checkout_path(path: &[u8], mode: Mode, opts: PathOptions) -> bool {
    rgit_index::verify_path_with(path, Some(mode), opts.protect_ntfs, opts.protect_hfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(p: &str, mode: Mode) -> bool {
        verify_checkout_path(
            p.as_bytes(),
            mode,
            PathOptions {
                ignore_case: false,
                protect_ntfs: true,
                protect_hfs: false,
            },
        )
    }

    #[test]
    fn refused() {
        for p in [
            ".git/config",
            ".GIT/x",
            "a/../b",
            "GIT~1",
            ".git.",
            ".git ",
            ".git::$INDEX_ALLOCATION",
            "sub/GIT~1",
        ] {
            assert!(!ok(p, Mode::FILE), "{p}");
        }
        for p in [
            ".gitmodules",
            "sub/.GITMODULES",
            "gitmod~1",
            ".gitmodules .",
        ] {
            assert!(!ok(p, Mode::SYMLINK), "{p}");
            assert!(ok(p, Mode::FILE), "{p}");
        }
        assert!(ok("a\\b", Mode::FILE));
        assert!(ok(".gitx", Mode::FILE));
        assert!(ok("x.gitmodules", Mode::SYMLINK));
        let hfs = PathOptions {
            ignore_case: false,
            protect_ntfs: false,
            protect_hfs: true,
        };
        assert!(!verify_checkout_path(
            ".g\u{200c}it".as_bytes(),
            Mode::FILE,
            hfs
        ));
        assert!(verify_checkout_path(
            ".g\u{200c}it".as_bytes(),
            Mode::FILE,
            PathOptions {
                ignore_case: false,
                protect_ntfs: true,
                protect_hfs: false,
            }
        ));
    }
}
