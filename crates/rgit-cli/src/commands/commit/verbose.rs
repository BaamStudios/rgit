// SPDX-License-Identifier: MIT
//! `commit -v`: the diff below the scissors line of the message template
//! (Documentation/git-commit.adoc, `--verbose`). With `-v -v` the changes
//! left out of the commit follow, and the sides are named `c/`, `i/` and
//! `w/` (layout checked against the `git` binary). `--dry-run -v` prints
//! the staged diff after the status.

use rgit_hash::ObjectId;
use rgit_index::Index;
use rgit_pathspec::Pathspec;

use crate::Die;
use crate::diff::collect::{self, WorkTree};
use crate::diff::options::{DiffOpts, Formats};
use crate::diff::output::Output;
use crate::messages::commit as msg;
use crate::repository::Repo;

/// The diff of `-v`: a patch, through textconv filters (t4030).
fn options() -> DiffOpts {
    DiffOpts {
        patch: true,
        textconv: Some(true),
        ..Default::default()
    }
}

fn output<'a>(repo: &'a Repo, opts: &DiffOpts) -> Result<Output<'a>, Die> {
    let formats = Formats {
        patch: true,
        ..Default::default()
    };
    Output::new(Some(repo), repo.hash, opts, formats, true)
}

fn prefixes(out: &mut Output<'_>, src: &str, dst: &str) {
    out.patch.src_prefix = src.as_bytes().to_vec();
    out.patch.dst_prefix = dst.as_bytes().to_vec();
}

/// The diff from `base` (the parent's tree) to the index being committed;
/// `mnemonic` names the sides `c/` and `i/`.
pub(crate) fn staged_diff(
    repo: &Repo,
    base: &ObjectId,
    index: &Index,
    mnemonic: bool,
) -> Result<Vec<u8>, Die> {
    let opts = options();
    let pairs = collect::index(repo, base, index, &Pathspec::default(), None, false)?;
    let mut out = output(repo, &opts)?;
    if mnemonic {
        prefixes(&mut out, "c/", "i/");
    } else {
        out.mnemonic(&opts, "c", "i");
    }
    Ok(out.render(pairs)?.0)
}

/// What follows the scissors line for `-v` (`verbose` times).
pub(super) fn template_diff(
    repo: &Repo,
    base: &ObjectId,
    index: &Index,
    verbose: i32,
    comment: &str,
) -> Result<Vec<u8>, Die> {
    if verbose < 2 {
        return staged_diff(repo, base, index, false);
    }
    let heading = |out: &mut Vec<u8>, text: &str| {
        out.extend_from_slice(comment.as_bytes());
        out.push(b' ');
        out.extend_from_slice(text.as_bytes());
        out.push(b'\n');
    };
    let mut out = Vec::new();
    out.extend_from_slice(comment.as_bytes());
    out.push(b'\n');
    heading(&mut out, msg::DIFF_STAGED);
    out.extend_from_slice(&staged_diff(repo, base, index, true)?);

    let text = unstaged_diff(repo, index)?;
    if !text.is_empty() {
        heading(&mut out, msg::DIFF_SEPARATOR);
        heading(&mut out, msg::DIFF_UNSTAGED);
        out.extend_from_slice(&text);
    }
    Ok(out)
}

/// `status -v`: the staged diff; with `-v -v` (`verbose` times) under a
/// heading with the sides named `c/` and `i/`, followed by the changes
/// not staged (`i/`, `w/`), each part only when it is not empty.
pub(crate) fn status_diff(
    repo: &Repo,
    base: &ObjectId,
    index: &Index,
    verbose: i32,
) -> Result<Vec<u8>, Die> {
    if verbose < 2 {
        return staged_diff(repo, base, index, false);
    }
    let line = |out: &mut Vec<u8>, text: &str| {
        out.extend_from_slice(text.as_bytes());
        out.push(b'\n');
    };
    let mut out = Vec::new();
    let staged = staged_diff(repo, base, index, true)?;
    if !staged.is_empty() {
        line(&mut out, msg::DIFF_STAGED);
        out.extend_from_slice(&staged);
    }
    let unstaged = unstaged_diff(repo, index)?;
    if !unstaged.is_empty() {
        line(&mut out, msg::DIFF_SEPARATOR);
        line(&mut out, msg::DIFF_UNSTAGED);
        out.extend_from_slice(&unstaged);
    }
    Ok(out)
}

/// The changes of the work tree against `index`, sides named `i/`, `w/`.
fn unstaged_diff(repo: &Repo, index: &Index) -> Result<Vec<u8>, Die> {
    let opts = options();
    let root = repo.work_tree()?;
    let checker = repo.checker(None)?;
    let submodules = crate::submodules::IgnoreRules::for_index(repo, index, None, true)?;
    let wt = WorkTree {
        root: &root,
        checker: &checker,
        hash: repo.hash,
        submodules: submodules.as_ref(),
        dirty: Default::default(),
    };
    let pairs = collect::files(index, &Pathspec::default(), &wt, false)?;
    let mut unstaged = output(repo, &opts)?;
    prefixes(&mut unstaged, "i/", "w/");
    Ok(unstaged.render(pairs)?.0)
}
