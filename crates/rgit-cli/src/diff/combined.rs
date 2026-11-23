// SPDX-License-Identifier: MIT
//! Combined diffs of a merge (Documentation/diff-generate-patch.adoc
//! "Combined diff format", diff-format.adoc "diff format for merges"):
//! the result compared with every parent at once, `-c` (`--combined`) and
//! `--cc` (dense), as patches and in the raw format.
//!
//! Only paths that differ from every parent are shown. A patch has one
//! column per parent: `+` for a result line that parent does not have,
//! `-` for a parent line the result lost (equal lost lines of several
//! parents share a row). Rows that differ from some parent form changes;
//! `--cc` keeps only the changes that are not simply one parent's version
//! taken over, and the hunks show three lines of context around the
//! changes kept (lines of other changes do not count as context). These
//! rules, the hunk header and the funcname quirk (the line found loses
//! its last character) were derived from the documentation and the `git`
//! binary.

use std::collections::{BTreeSet, HashMap};

use rgit_diff::{DiffOptions, diff_lines, is_binary, split_lines};
use rgit_hash::ObjectId;

use crate::repository::Repo;

const CONTEXT: usize = 3;
const FUNCNAME_MAX: usize = 40;

/// A tree entry: mode and object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    pub mode: u32,
    pub oid: ObjectId,
}

/// A path that differs from every parent.
#[derive(Debug, Clone)]
pub struct File {
    pub path: Vec<u8>,
    pub parents: Vec<Option<Side>>,
    pub result: Option<Side>,
}

fn flat(repo: &Repo, tree: Option<&ObjectId>) -> HashMap<Vec<u8>, Side> {
    let Some(tree) = tree else {
        return HashMap::new();
    };
    rgit_unpack::tree_entries(repo.odb(), tree)
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            (
                e.path().to_vec(),
                Side {
                    mode: e.mode.0,
                    oid: e.id,
                },
            )
        })
        .collect()
}

/// The paths of `tree` that differ from every one of `parents` (trees),
/// in path order. With `renames`, a path added or deleted relative to a
/// parent counts as unchanged there when the parent has the same blob at
/// a path the result lacks (or the other way round): an exact rename, as
/// the patch formats of git's `log` and `show` see it.
pub fn collect(repo: &Repo, parents: &[ObjectId], tree: &ObjectId, renames: bool) -> Vec<File> {
    let result = flat(repo, Some(tree));
    let ps: Vec<HashMap<Vec<u8>, Side>> = parents.iter().map(|p| flat(repo, Some(p))).collect();
    let mut paths: BTreeSet<&Vec<u8>> = result.keys().collect();
    for p in &ps {
        paths.extend(p.keys());
    }
    let mut out = Vec::new();
    for path in paths {
        let r = result.get(path).copied();
        let sides: Vec<Option<Side>> = ps.iter().map(|p| p.get(path).copied()).collect();
        let renamed = |p: usize| {
            renames
                && match (sides[p], r) {
                    // Added here: the parent has the blob at a path the
                    // result lacks.
                    (None, Some(rv)) => ps[p]
                        .iter()
                        .any(|(q, v)| v.oid == rv.oid && !result.contains_key(q)),
                    // Deleted here: the result has the blob at a path the
                    // parent lacks.
                    (Some(pv), None) => result
                        .iter()
                        .any(|(q, v)| v.oid == pv.oid && !ps[p].contains_key(q)),
                    _ => false,
                }
        };
        if sides.iter().all(|s| *s != r) && !(0..sides.len()).any(renamed) {
            out.push(File {
                path: path.clone(),
                parents: sides,
                result: r,
            });
        }
    }
    out
}

fn status(parent: Option<Side>, result: Option<Side>) -> char {
    match (parent, result) {
        (None, _) => 'A',
        (_, None) => 'D',
        (Some(p), Some(r)) if p.mode & 0o170000 != r.mode & 0o170000 => 'T',
        _ => 'M',
    }
}

/// The raw lines: `::<modes> <ids> <statuses>\t<path>`.
pub fn raw(repo: &Repo, files: &[File], abbrev: Option<usize>, quote: bool, out: &mut Vec<u8>) {
    let null = ObjectId::null(repo.hash);
    let id = |s: Option<Side>| {
        let oid = s.map_or(null, |s| s.oid);
        match abbrev {
            Some(n) => super::output::raw_ellipsis(repo.abbrev(&oid, n), repo.hash),
            None => oid.to_hex(),
        }
    };
    for f in files {
        for _ in &f.parents {
            out.push(b':');
        }
        let mut modes: Vec<String> = f
            .parents
            .iter()
            .map(|s| format!("{:06o}", s.map_or(0, |s| s.mode)))
            .collect();
        modes.push(format!("{:06o}", f.result.map_or(0, |s| s.mode)));
        out.extend_from_slice(modes.join(" ").as_bytes());
        for s in &f.parents {
            out.push(b' ');
            out.extend_from_slice(id(*s).as_bytes());
        }
        out.push(b' ');
        out.extend_from_slice(id(f.result).as_bytes());
        out.push(b' ');
        for s in &f.parents {
            out.push(status(*s, f.result) as u8);
        }
        out.push(b'\t');
        rgit_diff::quote_path(out, &f.path, quote);
        out.push(b'\n');
    }
}

/// A row of the combined patch.
#[derive(Debug, Clone)]
struct Row<'a> {
    line: &'a [u8],
    /// The parents the row is new to (a result line) or was lost from.
    mask: u64,
    /// A line lost from the parents in `mask`; otherwise a result line.
    lost: bool,
    /// Index of the result line (for lost rows: the next one).
    at: usize,
}

/// Merges `new` (lost lines of parent `bit`) into `rows` by their longest
/// common subsequence: equal lines share a row. Matches are taken as late
/// as possible, and where both lists have lines of their own the earlier
/// parents' lines come first (as git shows them).
fn merge_lost<'a>(rows: &mut Vec<(&'a [u8], u64)>, new: &[&'a [u8]], bit: u64) {
    if rows.is_empty() {
        rows.extend(new.iter().map(|l| (*l, bit)));
        return;
    }
    // Work on the reversed lists so that greedy matches are the latest.
    let old: Vec<(&[u8], u64)> = rows.iter().rev().copied().collect();
    let new: Vec<&[u8]> = new.iter().rev().copied().collect();
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i].0 == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && old[i].0 == new[j] {
            out.push((old[i].0, old[i].1 | bit));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push((new[j], bit));
            j += 1;
        } else {
            out.push(old[i]);
            i += 1;
        }
    }
    out.reverse();
    *rows = out;
}

fn read(repo: &Repo, s: Option<Side>) -> Vec<u8> {
    s.and_then(|s| repo.odb().read(&s.oid).ok().flatten())
        .map(|o| o.data)
        .unwrap_or_default()
}

/// What the combined patch shows.
pub struct PatchOptions<'a> {
    pub dense: bool,
    pub abbrev: usize,
    pub quote: bool,
    pub diff: &'a DiffOptions,
}

/// Appends the combined patch of `files`.
pub fn patch(repo: &Repo, files: &[File], opts: &PatchOptions<'_>, out: &mut Vec<u8>) {
    for f in files {
        file_patch(repo, f, opts, out);
    }
}

fn file_patch(repo: &Repo, f: &File, opts: &PatchOptions<'_>, out: &mut Vec<u8>) {
    let result = read(repo, f.result);
    let parents: Vec<Vec<u8>> = f.parents.iter().map(|s| read(repo, *s)).collect();
    let binary = is_binary(&result) || parents.iter().any(|p| is_binary(p));
    let r_lines = split_lines(&result);
    let n = r_lines.len();
    let all: u64 = (1u64 << f.parents.len()) - 1;
    let mut added = vec![0u64; n];
    let mut lost: Vec<Vec<(&[u8], u64)>> = vec![Vec::new(); n + 1];
    let p_lines: Vec<Vec<&[u8]>> = parents.iter().map(|p| split_lines(p)).collect();
    if !binary {
        for (p, pl) in p_lines.iter().enumerate() {
            let bit = 1u64 << p;
            for c in diff_lines(pl, &r_lines, opts.diff) {
                for a in added.iter_mut().skip(c.b_start).take(c.b_len) {
                    *a |= bit;
                }
                if c.a_len > 0 {
                    merge_lost(
                        &mut lost[c.b_start],
                        &pl[c.a_start..c.a_start + c.a_len],
                        bit,
                    );
                }
            }
        }
    }

    // The rows in order.
    let mut rows: Vec<Row<'_>> = Vec::new();
    for j in 0..=n {
        for (line, mask) in &lost[j] {
            rows.push(Row {
                line,
                mask: *mask,
                lost: true,
                at: j,
            });
        }
        if j < n {
            rows.push(Row {
                line: r_lines[j],
                mask: added[j],
                lost: false,
                at: j,
            });
        }
    }
    let changed = |r: &Row<'_>| r.lost || r.mask != 0;

    // Changes: runs of changed rows, joined when fewer than CONTEXT
    // unchanged lines lie between them; with --cc only those that are not
    // one parent's version taken over (or several equal parents' versions).
    let mut keep = vec![false; rows.len()];
    let mut i = 0;
    while i < rows.len() {
        if !changed(&rows[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut gap = 0;
        while i < rows.len() {
            if changed(&rows[i]) {
                end = i + 1;
                gap = 0;
            } else {
                gap += 1;
                if gap >= CONTEXT {
                    break;
                }
            }
            i += 1;
        }
        let group = &rows[start..end];
        let union = group
            .iter()
            .filter(|r| changed(r))
            .fold(0, |m, r| m | r.mask);
        let simple = union != all && group.iter().filter(|r| changed(r)).all(|r| r.mask == union);
        if !(opts.dense && simple) {
            for (k, r) in keep[start..end].iter_mut().zip(group) {
                *k = changed(r);
            }
        }
        i = end;
    }
    let interesting = keep.iter().any(|k| *k);

    // Header.
    let shown_path = |out: &mut Vec<u8>, prefix: &[u8]| {
        let mut full = prefix.to_vec();
        full.extend_from_slice(&f.path);
        rgit_diff::quote_path(out, &full, opts.quote);
    };
    if !binary && !interesting && opts.dense {
        return;
    }
    out.extend_from_slice(if opts.dense {
        b"diff --cc "
    } else {
        b"diff --combined "
    });
    rgit_diff::quote_path(out, &f.path, opts.quote);
    out.push(b'\n');
    let null = ObjectId::null(repo.hash);
    let abbr = |s: Option<Side>| repo.abbrev(&s.map_or(null, |s| s.oid), opts.abbrev);
    let modes: Vec<u32> = f.parents.iter().map(|s| s.map_or(0, |s| s.mode)).collect();
    let rmode = f.result.map_or(0, |s| s.mode);
    let added_file = f.parents.iter().all(Option::is_none);
    let deleted = f.result.is_none();
    let ids: Vec<String> = f.parents.iter().map(|s| abbr(*s)).collect();
    out.extend_from_slice(format!("index {}..{}\n", ids.join(","), abbr(f.result)).as_bytes());
    if added_file {
        out.extend_from_slice(format!("new file mode {rmode:06o}\n").as_bytes());
    } else if deleted {
        let m: Vec<String> = modes.iter().map(|m| format!("{m:06o}")).collect();
        out.extend_from_slice(format!("deleted file mode {}\n", m.join(",")).as_bytes());
    }
    if !added_file && !deleted && modes.iter().any(|m| *m != rmode) {
        let m: Vec<String> = modes.iter().map(|m| format!("{m:06o}")).collect();
        out.extend_from_slice(format!("mode {}..{rmode:06o}\n", m.join(",")).as_bytes());
    }
    if binary {
        out.extend_from_slice(b"Binary files differ\n");
        return;
    }
    if added_file {
        out.extend_from_slice(b"--- /dev/null\n");
    } else {
        out.extend_from_slice(b"--- ");
        shown_path(out, b"a/");
        out.push(b'\n');
    }
    if deleted {
        out.extend_from_slice(b"+++ /dev/null\n");
    } else {
        out.extend_from_slice(b"+++ ");
        shown_path(out, b"b/");
        out.push(b'\n');
    }

    // Windows: kept rows with CONTEXT unchanged lines before and after;
    // a change that falls into a window is shown with its own context.
    let mut show = vec![false; rows.len()];
    let mut seeds: Vec<usize> = (0..rows.len()).filter(|&k| keep[k]).collect();
    while let Some(k) = seeds.pop() {
        show[k] = true;
        let mut ctx = 0;
        let mut j = k;
        while j > 0 && ctx < CONTEXT {
            j -= 1;
            if !changed(&rows[j]) {
                ctx += 1;
            } else if !keep[j] {
                keep[j] = true;
                seeds.push(j);
            }
            show[j] = true;
        }
        let mut ctx = 0;
        let mut j = k + 1;
        while j < rows.len() && ctx < CONTEXT {
            if !changed(&rows[j]) {
                ctx += 1;
            } else if !keep[j] {
                keep[j] = true;
                seeds.push(j);
            }
            show[j] = true;
            j += 1;
        }
    }
    // A row between two shown windows closes the gap.
    let np = f.parents.len();
    let mut since = 0;
    let mut i = 0;
    while i < rows.len() {
        if !show[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < rows.len() && show[i] {
            i += 1;
        }
        hunk(&rows[start..i], &rows[..start], since, np, &r_lines, out);
        since = rows[i - 1].at + usize::from(!rows[i - 1].lost);
    }
}

/// One hunk of `rows`; `before` are the rows above it, `since` the first
/// result line after the previous hunk.
fn hunk(
    rows: &[Row<'_>],
    before: &[Row<'_>],
    since: usize,
    np: usize,
    r_lines: &[&[u8]],
    out: &mut Vec<u8>,
) {
    let mut header = vec![b'@'; np + 1];
    // Per parent: lines before the hunk and in it.
    for p in 0..np {
        let bit = 1u64 << p;
        let has = |r: &Row<'_>| {
            if r.lost {
                r.mask & bit != 0
            } else {
                r.mask & bit == 0
            }
        };
        let pre = before.iter().filter(|r| has(r)).count();
        let len = rows.iter().filter(|r| has(r)).count();
        let start = if len == 0 && pre > 0 { pre } else { pre + 1 };
        header.extend_from_slice(format!(" -{start},{len}").as_bytes());
    }
    let pre = before.iter().filter(|r| !r.lost).count();
    let len = rows.iter().filter(|r| !r.lost).count();
    let start = if len == 0 && pre > 0 { pre } else { pre + 1 };
    header.extend_from_slice(format!(" +{start},{len} ").as_bytes());
    header.extend(std::iter::repeat_n(b'@', np + 1));
    // The function line: searched upwards from the line above the hunk to
    // the end of the previous hunk in the result. git shows at most its
    // first FUNCNAME_MAX characters, trailing whitespace trimmed, and
    // drops the last character.
    let first = rows.first().map_or(0, |r| r.at);
    if let Some(l) = r_lines[since.min(first)..first.min(r_lines.len())]
        .iter()
        .rev()
        .find(|l| rgit_diff::default_funcname(l))
    {
        let mut text: &[u8] = l.strip_suffix(b"\n").unwrap_or(l);
        if text.len() > FUNCNAME_MAX {
            text = &text[..FUNCNAME_MAX];
        }
        while let Some((&c, rest)) = text.split_last() {
            if c.is_ascii_whitespace() {
                text = rest;
            } else {
                break;
            }
        }
        text = &text[..text.len().saturating_sub(1)];
        if !text.is_empty() {
            header.push(b' ');
            header.extend_from_slice(text);
        }
    }
    out.extend_from_slice(&header);
    out.push(b'\n');
    for r in rows {
        for p in 0..np {
            let set = r.mask & (1u64 << p) != 0;
            out.push(match (set, r.lost) {
                (true, true) => b'-',
                (true, false) => b'+',
                _ => b' ',
            });
        }
        out.extend_from_slice(r.line);
        if !r.line.ends_with(b"\n") {
            out.push(b'\n');
            out.extend_from_slice(rgit_diff::messages::NO_NEWLINE_AT_EOF.as_bytes());
            out.push(b'\n');
        }
    }
}
