// SPDX-License-Identifier: MIT
//! `git merge-tree <base-tree> <branch1> <branch2>`, the deprecated
//! trivial-merge mode (Documentation/git-merge-tree.adoc, "DEPRECATED
//! DESCRIPTION"): every path that is not simply our version is printed
//! with a heading ("added in remote", "removed in local", "merged",
//! "changed in both", ...), its versions and the unified diff from our
//! version to the result (a content merge with `.our`/`.their` labels).
//! Paths come in tree order with the entries of a directory before a file
//! of the same name. The format follows t4300.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use rgit_diff::merge::{self as text, MergeOptions};
use rgit_hash::ObjectId;

use crate::messages::merge_tree as msg;
use crate::prelude::*;
use crate::repository::Repo;

#[derive(Clone, Copy, PartialEq, Eq)]
struct V {
    mode: u32,
    oid: ObjectId,
}

fn flat(repo: &Repo, tree: &ObjectId) -> Result<std::collections::HashMap<Vec<u8>, V>, Die> {
    Ok(rgit_unpack::tree_entries(repo.odb(), tree)
        .map_err(crate::commands::read_tree::unpack_failed)?
        .into_iter()
        .map(|e| {
            (
                e.path().to_vec(),
                V {
                    mode: e.mode.0,
                    oid: e.id,
                },
            )
        })
        .collect())
}

/// Tree order with a directory's entries before a file of its name.
fn order(a: &[u8], b: &[u8]) -> Ordering {
    let mut x = a.split(|&c| c == b'/');
    let mut y = b.split(|&c| c == b'/');
    loop {
        match (x.next(), y.next()) {
            (Some(p), Some(q)) => match p.cmp(q) {
                Ordering::Equal => {}
                o => return o,
            },
            (None, None) => return Ordering::Equal,
            // The shorter path is the file: after the directory.
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
        }
    }
}

fn blob(repo: &Repo, v: Option<V>) -> Vec<u8> {
    v.and_then(|v| repo.odb().read(&v.oid).ok().flatten())
        .map(|o| o.data)
        .unwrap_or_default()
}

fn line(out: &mut Vec<u8>, label: &str, v: V, path: &[u8]) {
    out.extend_from_slice(format!("  {label:<6} {:06o} {} ", v.mode, v.oid.to_hex()).as_bytes());
    out.extend_from_slice(path);
    out.push(b'\n');
}

/// The hunks from `a` to `b`, without function names in the hunk headers.
fn diff(out: &mut Vec<u8>, a: &[u8], b: &[u8]) {
    let opts = rgit_diff::DiffOptions::default();
    let hunks = rgit_diff::diff_text(a, b, &opts).hunks;
    for line in hunks.split_inclusive(|&c| c == b'\n') {
        if line.starts_with(b"@@ ") {
            if let Some(end) = line[3..].windows(2).position(|w| w == b"@@") {
                out.extend_from_slice(&line[..end + 5]);
                out.push(b'\n');
                continue;
            }
        }
        out.extend_from_slice(line);
    }
}

pub(super) fn run(repo: &Repo, base: &ObjectId, ours: &ObjectId, theirs: &ObjectId) -> Result<Vec<u8>, Die> {
    let (fb, fo, ft) = (flat(repo, base)?, flat(repo, ours)?, flat(repo, theirs)?);
    let mut paths: Vec<&Vec<u8>> = fb
        .keys()
        .chain(fo.keys())
        .chain(ft.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    paths.sort_by(|a, b| order(a, b));
    let mut out = Vec::new();
    for p in paths {
        let (b, o, t) = (fb.get(p).copied(), fo.get(p).copied(), ft.get(p).copied());
        if o == t || b == t {
            continue;
        }
        if b == o {
            match (b, t) {
                (_, None) => {
                    out.extend_from_slice(msg::TRIVIAL_REMOVED_IN_REMOTE.as_bytes());
                    out.push(b'\n');
                    line(&mut out, "base", b.expect("removed"), p);
                    line(&mut out, "our", o.expect("removed"), p);
                    diff(&mut out, &blob(repo, o), b"");
                }
                (None, Some(t)) => {
                    out.extend_from_slice(msg::TRIVIAL_ADDED_IN_REMOTE.as_bytes());
                    out.push(b'\n');
                    line(&mut out, "their", t, p);
                    diff(&mut out, b"", &blob(repo, Some(t)));
                }
                (Some(_), Some(t)) => {
                    out.extend_from_slice(msg::TRIVIAL_MERGED.as_bytes());
                    out.push(b'\n');
                    line(&mut out, "result", t, p);
                    line(&mut out, "our", o.expect("present"), p);
                    diff(&mut out, &blob(repo, o), &blob(repo, Some(t)));
                }
            }
            continue;
        }
        match (o, t) {
            (None, Some(t)) => {
                out.extend_from_slice(msg::TRIVIAL_REMOVED_IN_LOCAL.as_bytes());
                out.push(b'\n');
                if let Some(b) = b {
                    line(&mut out, "base", b, p);
                }
                line(&mut out, "their", t, p);
            }
            (Some(o), None) => {
                out.extend_from_slice(msg::TRIVIAL_REMOVED_IN_REMOTE.as_bytes());
                out.push(b'\n');
                if let Some(b) = b {
                    line(&mut out, "base", b, p);
                }
                line(&mut out, "our", o, p);
                diff(&mut out, &blob(repo, Some(o)), b"");
            }
            (Some(o), Some(t)) => {
                let heading = if b.is_some() {
                    msg::TRIVIAL_CHANGED_IN_BOTH
                } else {
                    msg::TRIVIAL_ADDED_IN_BOTH
                };
                out.extend_from_slice(heading.as_bytes());
                out.push(b'\n');
                if let Some(b) = b {
                    line(&mut out, "base", b, p);
                }
                line(&mut out, "our", o, p);
                line(&mut out, "their", t, p);
                let ours = blob(repo, Some(o));
                let mopts = MergeOptions {
                    style: text::ConflictStyle::Merge,
                    favor: text::Favor::None,
                    marker_size: 0,
                    ours_label: b".our",
                    base_label: b"",
                    theirs_label: b".their",
                    diff: text::merge_diff_options(),
                };
                let merged = text::merge(&blob(repo, b), &ours, &blob(repo, Some(t)), &mopts)
                    .map(|m| m.content)
                    .unwrap_or_else(|_| ours.clone());
                diff(&mut out, &ours, &merged);
            }
            (None, None) => {}
        }
    }
    Ok(out)
}
