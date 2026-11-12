// SPDX-License-Identifier: MIT

//! The work tree side of rgit.
//!
//! - [`ignore`]: gitignore(5) patterns and their sources (`.gitignore` per
//!   directory, `$GIT_COMMON_DIR/info/exclude`, `core.excludesFile`,
//!   command-line excludes) with git's precedence, and the leading-directory
//!   walk of `git check-ignore`.
//! - [`stat`]: deciding cheaply whether a work tree file still matches its
//!   index entry (cached `lstat` data, `core.trustCtime`, `core.checkStat`,
//!   `core.fileMode`, `core.symlinks`), the racy-git rule from
//!   `Documentation/technical/racy-git.adoc`, smudging racily clean entries
//!   before the index is written, and a content check that counts the files
//!   it hashes.
//!
//! Behaviour follows git's documentation (gitignore(5),
//! git-check-ignore(1), git-config(1) `core.*`, the racy-git technical
//! note) and was checked against the `git` binary; see the differential
//! tests in `tests/`.

pub mod ignore;
pub mod messages;
pub mod stat;

mod os;
