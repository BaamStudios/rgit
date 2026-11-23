// SPDX-License-Identifier: MIT
//! The `resolve` strategy and the trivial in-index merge of `git merge`
//! (Documentation/merge-strategies.adoc, git-read-tree.adoc "3-Way Merge",
//! technical/trivial-merge.adoc), implemented natively.
//!
//! Every path is first merged with the table of trivial-merge.adoc: the
//! trivial merge (`read-tree -m --trivial`) accepts only results that need
//! no file-level merging; `resolve` also takes the cases of `--aggressive`
//! (a path removed on one side and unchanged on the other, or removed on
//! both) and merges the rest file by file: regular files changed on both
//! sides get a content merge, everything else stays in conflict. The
//! messages were taken from the `git` binary; the conflict marker labels
//! are temporary file names (`.merge_file_XXXXXX`) as with git.

use std::collections::BTreeMap;

use rgit_diff::merge::{self as text, ConflictStyle, Favor, MergeOptions};
use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Index, Mode, Stage};
use rgit_merge::{ConflictedPath, MergeResult, Version};
use rgit_object::ObjectKind;

use crate::commands::read_tree::unpack_failed;
use crate::messages::merge as msg;
use crate::prelude::*;
use crate::repository::Repo;

const MODE_LINK: u32 = 0o120000;
const TYPE_MASK: u32 = 0o170000;

type V = Option<Version>;

/// The result of [`resolve`]: the merge, its output and the paths whose
/// index entries change.
pub(crate) type Resolved = (MergeResult, Vec<Line>, Vec<Vec<u8>>);

/// A line of output, on standard error when `err`.
#[derive(Debug, Clone)]
pub(crate) struct Line {
    pub err: bool,
    pub text: String,
}

impl Line {
    fn out(text: String) -> Line {
        Line { err: false, text }
    }
    fn err(text: String) -> Line {
        Line { err: true, text }
    }
}

/// Prints `lines` to their streams.
pub(crate) fn print(lines: &[Line]) {
    use std::io::Write as _;
    for l in lines {
        if l.err {
            let _ = std::io::stdout().flush();
            eprintln!("{}", l.text);
        } else {
            println!("{}", l.text);
        }
    }
    let _ = std::io::stdout().flush();
}

/// The entries of three or more trees by path.
struct Paths {
    bases: Vec<BTreeMap<Vec<u8>, Version>>,
    head: BTreeMap<Vec<u8>, Version>,
    theirs: BTreeMap<Vec<u8>, Version>,
}

fn flat(repo: &Repo, tree: &ObjectId) -> Result<BTreeMap<Vec<u8>, Version>, Die> {
    Ok(rgit_unpack::tree_entries(repo.odb(), tree)
        .map_err(unpack_failed)?
        .into_iter()
        .map(|e| {
            let v = Version {
                mode: e.mode.0,
                oid: e.id,
            };
            (e.path().to_vec(), v)
        })
        .collect())
}

impl Paths {
    fn read(repo: &Repo, bases: &[ObjectId], head: &ObjectId, theirs: &ObjectId) -> Result<Paths, Die> {
        Ok(Paths {
            bases: bases.iter().map(|b| flat(repo, b)).collect::<Result<_, _>>()?,
            head: flat(repo, head)?,
            theirs: flat(repo, theirs)?,
        })
    }

    /// Every path, in index order.
    fn all(&self) -> Vec<Vec<u8>> {
        let mut all: std::collections::BTreeSet<&Vec<u8>> = self.head.keys().collect();
        all.extend(self.theirs.keys());
        for b in &self.bases {
            all.extend(b.keys());
        }
        all.into_iter().cloned().collect()
    }
}

/// The outcome of the trivial-merge table for one path.
#[derive(Debug, Clone, Copy)]
enum Case {
    Take(V),
    NoMerge,
}

/// The trivial-merge table; `aggressive` adds the `--aggressive` cases.
fn classify(bases: &[V], h: V, r: V, aggressive: bool) -> Case {
    let empty_bases = bases.is_empty();
    if h == r {
        // 5ALT, and (aggressive) removed on both sides.
        if h.is_some() || aggressive || empty_bases || bases.iter().all(Option::is_none) {
            return Case::Take(h);
        }
        return Case::NoMerge;
    }
    let base_is = |v: V| {
        if empty_bases {
            v.is_none()
        } else {
            bases.contains(&v)
        }
    };
    let h_base = base_is(h);
    let r_base = base_is(r);
    if h_base && r_base {
        // 16: both sides went back to different ancestors.
        return Case::NoMerge;
    }
    if h_base && (r.is_some() || aggressive) {
        return Case::Take(r);
    }
    if r_base && (h.is_some() || aggressive) {
        return Case::Take(h);
    }
    Case::NoMerge
}

fn bases_of(paths: &Paths, p: &[u8]) -> Vec<V> {
    paths.bases.iter().map(|b| b.get(p).copied()).collect()
}

/// The tree of the trivial in-index merge, `None` when a path needs a
/// file-level merge; `changed` gets the paths whose index entry would
/// change.
pub(super) fn trivial(
    repo: &Repo,
    base: &ObjectId,
    head: &ObjectId,
    theirs: &ObjectId,
    changed: &mut Vec<Vec<u8>>,
) -> Result<Option<ObjectId>, Die> {
    let paths = Paths::read(repo, std::slice::from_ref(base), head, theirs)?;
    let mut out = Vec::new();
    let mut ok = true;
    for p in paths.all() {
        let h = paths.head.get(&p).copied();
        let r = paths.theirs.get(&p).copied();
        match classify(&bases_of(&paths, &p), h, r, false) {
            Case::Take(v) => {
                if v != h {
                    changed.push(p.clone());
                }
                if let Some(v) = v {
                    out.push((p, v));
                }
            }
            Case::NoMerge => {
                changed.push(p);
                ok = false;
            }
        }
    }
    if !ok {
        return Ok(None);
    }
    write_tree(repo, &out).map(Some)
}

/// A random `.merge_file_XXXXXX` name.
fn temp_label(seed: &mut u64) -> Vec<u8> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = b".merge_file_".to_vec();
    for _ in 0..6 {
        // xorshift
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        out.push(CHARS[(*seed % CHARS.len() as u64) as usize]);
    }
    out
}

fn seed() -> u64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    t ^ (u64::from(std::process::id()) << 32) | 1
}

fn hex(v: V) -> String {
    v.map(|v| v.oid.to_hex()).unwrap_or_default()
}

fn mode_str(v: V) -> String {
    v.map(|v| format!("{:o}", v.mode)).unwrap_or_default()
}

fn is_regular(v: Version) -> bool {
    v.mode & TYPE_MASK == 0o100000
}

/// The `resolve` strategy: the result (with conflict stages and the
/// work-tree versions of conflicted paths in its tree), its output and the
/// paths whose index entries change.
pub(crate) fn resolve(
    repo: &Repo,
    bases: &[ObjectId],
    head: &ObjectId,
    theirs: &ObjectId,
    style: ConflictStyle,
) -> Result<Resolved, Die> {
    let paths = Paths::read(repo, bases, head, theirs)?;
    let mut lines = vec![Line::out(msg::TRYING_SIMPLE.to_owned())];
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut changed = Vec::new();
    for p in paths.all() {
        let h = paths.head.get(&p).copied();
        let r = paths.theirs.get(&p).copied();
        let bs = bases_of(&paths, &p);
        match classify(&bs, h, r, true) {
            Case::Take(v) => {
                if v != h {
                    changed.push(p.clone());
                }
                if let Some(v) = v {
                    out.push((p, v));
                }
            }
            Case::NoMerge => {
                changed.push(p.clone());
                pending.push((p, bs.into_iter().flatten().next(), h, r));
            }
        }
    }
    let mut conflicts = Vec::new();
    if !pending.is_empty() {
        lines.push(Line::out(msg::SIMPLE_FAILED.to_owned()));
        let mut seed = seed();
        for (p, b, h, r) in pending {
            let name = String::from_utf8_lossy(&p).into_owned();
            let mut conflict = true;
            let mut keep = h;
            match (h, r) {
                (Some(hv), Some(rv)) if is_regular(hv) && is_regular(rv) => {
                    if b.is_none() {
                        lines.push(Line::out(msg::added_differently(&name)));
                    } else {
                        lines.push(Line::out(msg::resolve_auto_merging(&name)));
                    }
                    let base = match b {
                        Some(b) if is_regular(b) => read_blob(repo, &b.oid)?,
                        _ => Vec::new(),
                    };
                    let ours = read_blob(repo, &hv.oid)?;
                    let theirs_data = read_blob(repo, &rv.oid)?;
                    let (ol, bl, tl) = (temp_label(&mut seed), temp_label(&mut seed), temp_label(&mut seed));
                    let mopts = MergeOptions {
                        style,
                        favor: Favor::None,
                        marker_size: 0,
                        ours_label: &ol,
                        base_label: &bl,
                        theirs_label: &tl,
                        diff: text::merge_diff_options(),
                    };
                    let empty = repo.hash.empty_blob();
                    let no_base = b.is_none() && !matches!(repo.odb().read(&empty), Ok(Some(_)));
                    let outcome = if no_base {
                        // git merges against the empty blob, which fails
                        // when the repository does not have it: our
                        // version stays, in conflict.
                        lines.push(Line::err(format!(
                            "fatal: {}",
                            msg::resolve_unreadable_blob(&empty.to_hex())
                        )));
                        lines.push(Line::err(format!("error: {}", msg::RESOLVE_COULD_NOT_STAT)));
                        Err(text::BinaryContent)
                    } else {
                        text::merge(&base, &ours, &theirs_data, &mopts)
                    };
                    let (data, clean) = match outcome {
                        Ok(m) => {
                            let clean = m.is_clean();
                            (m.content, clean)
                        }
                        Err(text::BinaryContent) => (ours, false),
                    };
                    if !clean {
                        lines.push(Line::err(msg::resolve_content_conflict(&name)));
                    }
                    let mode_clean = hv.mode == rv.mode;
                    if !mode_clean {
                        lines.push(Line::err(msg::resolve_permissions(
                            &name,
                            &mode_str(b),
                            &mode_str(h),
                            &mode_str(r),
                        )));
                    }
                    let oid = repo
                        .odb()
                        .write(ObjectKind::Blob, &data)
                        .map_err(|e| Die::Fatal(e.to_string()))?;
                    keep = Some(Version { mode: hv.mode, oid });
                    conflict = !(clean && mode_clean);
                }
                (Some(hv), Some(rv)) if hv.mode == MODE_LINK || rv.mode == MODE_LINK => {
                    lines.push(Line::err(msg::resolve_symlink(&name)));
                }
                _ => {
                    lines.push(Line::err(msg::resolve_not_handling(
                        &name,
                        &hex(b),
                        &hex(h),
                        &hex(r),
                    )));
                }
            }
            if let Some(v) = keep {
                out.push((p.clone(), v));
            }
            if conflict {
                conflicts.push(ConflictedPath {
                    path: p,
                    stages: [b, h, r],
                });
            }
        }
        if !conflicts.is_empty() {
            lines.push(Line::err(format!("fatal: {}", msg::MERGE_PROGRAM_FAILED)));
        }
    }
    let tree = write_tree(repo, &out)?;
    Ok((
        MergeResult {
            tree,
            clean: conflicts.is_empty(),
            conflicts,
            messages: Vec::new(),
            rename_limit_needed: None,
        },
        lines,
        changed,
    ))
}

fn read_blob(repo: &Repo, oid: &ObjectId) -> Result<Vec<u8>, Die> {
    match repo.odb().read(oid) {
        Ok(Some(o)) => Ok(o.data),
        _ => Err(Die::Fatal(rgit_merge::messages::missing_object(&oid.to_hex()))),
    }
}

/// Writes the tree of `entries` (stage 0).
fn write_tree(repo: &Repo, entries: &[(Vec<u8>, Version)]) -> Result<ObjectId, Die> {
    let mut index = Index::new(repo.hash);
    let add = AddOptions {
        ok_to_replace: true,
        skip_df_check: false,
        skip_path_check: true,
    };
    for (p, v) in entries {
        index
            .add(Entry::new(p.clone(), Stage::MERGED, Mode(v.mode), v.oid), add)
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    rgit_commit::write_tree(repo.odb(), &index, &rgit_commit::WriteTreeOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))
}
