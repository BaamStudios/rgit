// SPDX-License-Identifier: MIT
//! Sparse checkout support shared by the commands (git-sparse-checkout(1),
//! git-read-tree(1) "SPARSE CHECKOUT", config/sparse.adoc).
//!
//! - [`load`]: the patterns of `$GIT_DIR/info/sparse-checkout` when
//!   `core.sparseCheckout` is on, read in cone mode with
//!   `core.sparseCheckoutCone`, warnings printed.
//! - [`clear_present`]: entries marked skip-worktree whose file exists
//!   after all lose the bit (unless `sparse.expectFilesOutsideOfPatterns`);
//!   done whenever a command reads the index.
//! - [`print_warnings`]: the paths an update left as they were.

use std::path::{Path, PathBuf};

use rgit_config::ConfigSet;
use rgit_index::Index;
use rgit_worktree::sparse::Sparse;

use crate::repository::Repo;

/// `core.sparseCheckout`.
pub fn enabled(config: &ConfigSet) -> bool {
    config
        .bool("core.sparsecheckout")
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// `core.sparseCheckoutCone` as the patterns are read (off unless set).
pub fn cone_config(config: &ConfigSet) -> Option<bool> {
    config.bool("core.sparsecheckoutcone").ok().flatten()
}

/// `core.ignoreCase`.
pub fn ignore_case(config: &ConfigSet) -> bool {
    config
        .bool("core.ignorecase")
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// `$GIT_DIR/info/sparse-checkout` of the current worktree.
pub fn file_path(repo: &Repo) -> PathBuf {
    repo.setup
        .cwd
        .join(&repo.setup.git_dir)
        .join("info")
        .join("sparse-checkout")
}

/// Reads the patterns in `data`, printing the warnings about the cone
/// form.
pub fn parse(data: &[u8], cone: bool, icase: bool) -> Sparse {
    let (sparse, warnings) = Sparse::parse(data, cone, icase);
    for w in warnings {
        eprintln!("warning: {w}");
    }
    sparse
}

/// The sparse checkout patterns when `core.sparseCheckout` is on and the
/// file exists.
pub fn load(repo: &Repo) -> Option<Sparse> {
    if !enabled(&repo.config) || repo.setup.work_tree.is_none() {
        return None;
    }
    let data = std::fs::read(file_path(repo)).ok()?;
    Some(parse(
        &data,
        cone_config(&repo.config).unwrap_or(false),
        ignore_case(&repo.config),
    ))
}

/// Clears the skip-worktree bit of entries whose file is present in the
/// work tree (`root`), when `core.sparseCheckout` is on and
/// `sparse.expectFilesOutsideOfPatterns` is off. Returns whether an entry
/// changed.
pub fn clear_present(config: &ConfigSet, root: &Path, index: &mut Index) -> bool {
    if !enabled(config)
        || config
            .bool("sparse.expectfilesoutsideofpatterns")
            .ok()
            .flatten()
            .unwrap_or(false)
    {
        return false;
    }
    clear_present_files(root, index)
}

/// Clears the skip-worktree bit of every entry whose file is present in
/// the work tree `root`. Returns whether an entry changed.
pub fn clear_present_files(root: &Path, index: &mut Index) -> bool {
    let mut changed = false;
    let mut missing_dir: Option<Vec<u8>> = None;
    for pos in 0..index.len() {
        let e = &index.entries()[pos];
        if !e.flags.skip_worktree || e.mode.is_dir() {
            continue;
        }
        let path = e.path();
        let dir = path
            .iter()
            .rposition(|&b| b == b'/')
            .map_or(&path[..0], |i| &path[..i]);
        // Skipped files usually sit in directories that are gone.
        if !dir.is_empty() && missing_dir.as_deref() == Some(dir) {
            continue;
        }
        if !dir.is_empty() && std::fs::symlink_metadata(os_path(root, dir)).is_err() {
            missing_dir = Some(dir.to_vec());
            continue;
        }
        if std::fs::symlink_metadata(os_path(root, path)).is_ok() {
            if let Some(e) = index.entry_mut(pos) {
                e.flags.skip_worktree = false;
                changed = true;
            }
        }
    }
    changed
}

/// Prints what a work tree update left despite the sparse patterns:
/// grouped warnings for porcelain commands, one `error: ` line per path
/// for plumbing.
pub fn print_warnings(w: &rgit_unpack::SparseWarnings, porcelain: bool) {
    if w.is_empty() {
        return;
    }
    if porcelain {
        eprint!("{}", w.porcelain_text());
    } else {
        for m in w.plumbing_messages() {
            eprintln!("error: {m}");
        }
    }
}

/// Which paths `add`, `rm` and `mv` leave alone without `--sparse`:
/// skip-worktree entries, and with `core.sparseCheckout` every path
/// outside the patterns.
pub struct Outside {
    patterns: Option<Sparse>,
    allowed: bool,
}

impl Outside {
    /// `allowed` is `--sparse` (nothing is outside then).
    pub fn new(repo: &Repo, allowed: bool) -> Outside {
        Outside {
            patterns: if allowed { None } else { load(repo) },
            allowed,
        }
    }

    /// Whether the index entry `e` is outside the sparse checkout.
    pub fn entry(&self, e: &rgit_index::Entry) -> bool {
        !self.allowed
            && ((e.flags.skip_worktree && e.stage() == rgit_index::Stage::MERGED)
                || self.path(e.path()))
    }

    /// Whether `path` lies outside the sparse patterns.
    pub fn path(&self, path: &[u8]) -> bool {
        !self.allowed && self.patterns.as_ref().is_some_and(|s| !s.includes(path))
    }
}

/// Prints the refusal of `add`, `rm` and `mv` to touch `items` (paths or
/// pathspecs) outside the sparse checkout, with the `updateSparsePath`
/// advice.
pub fn report_outside(
    advisor: &rgit_messages::Advisor,
    items: &[Vec<u8>],
) -> Result<(), crate::Die> {
    use crate::messages::sparse_checkout as msg;
    let mut text = String::from(msg::OUTSIDE_SPARSE_HEADER);
    text.push('\n');
    for i in items {
        text.push_str(&String::from_utf8_lossy(i));
        text.push('\n');
    }
    eprint!("{text}");
    advisor
        .advise(
            rgit_messages::Advice::UpdateSparsePath,
            msg::UPDATE_SPARSE_PATH_HINT,
        )
        .map_err(crate::Die::Fatal)
}

fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
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
