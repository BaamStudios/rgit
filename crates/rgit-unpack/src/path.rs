// SPDX-License-Identifier: MIT

//! Which tree paths may be checked out.
//!
//! On top of the index rules ([`rgit_index::verify_path`]: no empty, `.`,
//! `..` or `.git` component in any case, no NUL), paths that a file system
//! may treat as `.git` are refused:
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

/// The `core.protect*` switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathOptions {
    /// `core.protectNTFS` (default true, as the reference git behaves).
    pub protect_ntfs: bool,
    /// `core.protectHFS` (default true on macOS only).
    pub protect_hfs: bool,
}

impl Default for PathOptions {
    fn default() -> Self {
        PathOptions {
            protect_ntfs: true,
            protect_hfs: cfg!(target_os = "macos"),
        }
    }
}

/// Whether a tree entry at `path` with `mode` may enter the index and the
/// work tree.
pub fn verify_checkout_path(path: &[u8], mode: Mode, opts: PathOptions) -> bool {
    if !rgit_index::verify_path(path) {
        return false;
    }
    let mut comps = path.split(|&b| b == b'/').peekable();
    while let Some(c) = comps.next() {
        let last = comps.peek().is_none();
        if opts.protect_ntfs && ntfs_equivalent(c, b"git", b"git~1") {
            return false;
        }
        if opts.protect_hfs && hfs_equivalent(c, b".git") {
            return false;
        }
        if last && mode.is_symlink() {
            let gitmodules = c.eq_ignore_ascii_case(b".gitmodules")
                || (opts.protect_ntfs && ntfs_equivalent(c, b"gitmodules", b"gitmod~1"))
                || (opts.protect_hfs && hfs_equivalent(c, b".gitmodules"));
            if gitmodules {
                return false;
            }
        }
    }
    true
}

/// `.<name>` with trailing spaces/dots or a stream suffix, or the short
/// name, compared without case.
fn ntfs_equivalent(c: &[u8], name: &[u8], short: &[u8]) -> bool {
    if c.eq_ignore_ascii_case(short) {
        return true;
    }
    let n = name.len() + 1;
    if c.len() < n || c[0] != b'.' || !c[1..n].eq_ignore_ascii_case(name) {
        return false;
    }
    let rest = &c[n..];
    let end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
    rest[..end].iter().all(|&b| b == b' ' || b == b'.')
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(p: &str, mode: Mode) -> bool {
        verify_checkout_path(
            p.as_bytes(),
            mode,
            PathOptions {
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
                protect_ntfs: true,
                protect_hfs: false
            }
        ));
    }
}
