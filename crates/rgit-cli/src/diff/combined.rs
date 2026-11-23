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

use std::collections::HashMap;

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
    /// The path in each parent (another one after rename detection).
    pub parent_paths: Vec<Vec<u8>>,
    /// The status against each parent (`R` for a rename).
    pub statuses: Vec<u8>,
    /// The result's contents when they are not in the object database
    /// (a work tree file).
    pub result_data: Option<Vec<u8>>,
}

fn status(parent: Option<Side>, result: Option<Side>) -> char {
    match (parent, result) {
        (None, _) => 'A',
        (_, None) => 'D',
        (Some(p), Some(r)) if p.mode & 0o170000 != r.mode & 0o170000 => 'T',
        _ => 'M',
    }
}

/// The paths of `tree` that differ from every one of `parents` (trees),
/// in path order, from the pairs of each parent's diff after the
/// diffcore transformations of `out` (rename detection): a renamed path
/// is compared with its source in that parent. A path whose content and
/// mode a parent already has (also after an exact rename) is not shown.
/// The comparison is recursive; `show_trees` (`-t`) adds the
/// directories that differ.
pub fn collect(
    repo: &Repo,
    out: &super::output::Output<'_>,
    pathspec: &rgit_pathspec::Pathspec,
    parents: &[ObjectId],
    tree: &ObjectId,
    show_trees: bool,
) -> Result<Vec<File>, crate::Die> {
    type Entry = (Option<Side>, Option<Side>, Vec<u8>, u8);
    let mut maps: Vec<HashMap<Vec<u8>, Entry>> = Vec::new();
    // A pickaxe keeps the paths it matches against any parent (the
    // paths themselves come from the diffs without it, t4064).
    let pickaxe = out.has_pickaxe();
    let mut picked: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    for p in parents {
        let mut w = super::collect::TreeDiff {
            repo,
            pathspec,
            recursive: true,
            show_trees,
            max_depth: None,
            pairs: Vec::new(),
        };
        w.walk(Some(p), Some(tree), b"")?;
        let all = std::mem::take(&mut w.pairs);
        if pickaxe {
            for pr in out.diffcore(all.clone())? {
                picked.insert(if pr.new.is_some() {
                    pr.path.clone()
                } else {
                    pr.old_path.clone()
                });
            }
        }
        let pairs = out.diffcore_with(all, false)?;
        let mut m = HashMap::new();
        for pr in pairs {
            let side = |s: &Option<super::collect::Side>| {
                s.as_ref().map(|s| Side {
                    mode: s.mode,
                    oid: s.oid,
                })
            };
            let (old, new) = (side(&pr.old), side(&pr.new));
            if old == new {
                continue;
            }
            let key = if new.is_some() {
                pr.path.clone()
            } else {
                pr.old_path.clone()
            };
            let status = match pr.renamed {
                Some(c) => c,
                None => status(old, new) as u8,
            };
            m.insert(key, (old, new, pr.old_path.clone(), status));
        }
        maps.push(m);
    }
    let Some((first, rest)) = maps.split_first() else {
        return Ok(Vec::new());
    };
    let mut keys: Vec<&Vec<u8>> = first
        .keys()
        .filter(|k| rest.iter().all(|m| m.contains_key(*k)))
        .filter(|k| !pickaxe || picked.contains(*k))
        .collect();
    keys.sort();
    // `-O<orderfile>` / `diff.orderFile`.
    if !out.order.is_empty() {
        keys.sort_by_cached_key(|k| super::order::rank(&out.order, k));
    }
    Ok(keys
        .into_iter()
        .map(|k| {
            let entries: Vec<&Entry> = maps.iter().map(|m| &m[k]).collect();
            File {
                path: k.clone(),
                parents: entries.iter().map(|e| e.0).collect(),
                result: entries[0].1,
                parent_paths: entries.iter().map(|e| e.2.clone()).collect(),
                statuses: entries.iter().map(|e| e.3).collect(),
                result_data: None,
            }
        })
        .collect())
}

/// How a work tree diff shows unmerged paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmergedAs {
    /// "Unmerged" and the diff against stage 2 (ours).
    Default,
    /// "Unmerged" and the diff against stage 1, 2 or 3 (`-1`, `-2`,
    /// `-3`), or nothing more (`-0`).
    Stage(usize),
    /// The combined diff of stages 2 and 3 with the work tree file
    /// (`-c`, `--cc`, the patch of `git diff`), where it differs from
    /// both.
    Combined,
}

/// Work tree diffs (`diff`, `diff-files`) with unmerged paths: replaces
/// the pairs `collect::files` made for them according to `how`, and
/// returns the paths shown as combined diffs.
pub fn worktree(
    repo: &Repo,
    out: &super::output::Output<'_>,
    unmerged: &[super::collect::Unmerged],
    pairs: Vec<super::collect::Pair>,
    how: UnmergedAs,
) -> Result<(Vec<File>, Vec<super::collect::Pair>), crate::Die> {
    use super::collect::Pair;
    if how == UnmergedAs::Default || unmerged.is_empty() {
        return Ok((Vec::new(), pairs));
    }
    let side = |s: &super::collect::Side| Side {
        mode: s.mode,
        oid: s.oid,
    };
    let mut files = Vec::new();
    let mut combined: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
    if how == UnmergedAs::Combined {
        for u in unmerged {
            let parents = [
                u.stages[1].as_ref().map(side),
                u.stages[2].as_ref().map(side),
            ];
            let (result, data) = match &u.file {
                Some(f) => {
                    let data = out.contents(f, &u.path)?;
                    let oid = rgit_hash::hash_object(repo.hash, "blob", &data)
                        .map_err(|e| crate::Die::Fatal(e.to_string()))?;
                    (Some(Side { mode: f.mode, oid }), Some(data))
                }
                None => (None, None),
            };
            if parents.contains(&result) {
                continue;
            }
            // The work tree file is not hashed: its id shows as zeros.
            let result = result.map(|r| Side {
                mode: r.mode,
                oid: ObjectId::null(repo.hash),
            });
            combined.insert(&u.path);
            files.push(File {
                path: u.path.clone(),
                parent_paths: vec![u.path.clone(); 2],
                statuses: parents.iter().map(|p| status(*p, result) as u8).collect(),
                parents: parents.to_vec(),
                result,
                result_data: data,
            });
        }
    }
    let mut rest = Vec::with_capacity(pairs.len());
    let mut last_unmerged: Option<Vec<u8>> = None;
    for p in pairs {
        if combined.contains(&p.path[..]) {
            continue;
        }
        if p.unmerged {
            last_unmerged = Some(p.path.clone());
            let path = p.path.clone();
            rest.push(p);
            if let UnmergedAs::Stage(n @ 1..=3) = how {
                let u = unmerged.iter().find(|u| u.path == path);
                if let Some((Some(s), Some(f))) = u.map(|u| (&u.stages[n - 1], &u.file)) {
                    rest.push(Pair::new(path, Some(s.clone()), Some(f.clone())));
                }
            }
            continue;
        }
        // The pair against "ours" that follows the "Unmerged" one.
        if matches!(how, UnmergedAs::Stage(_)) && last_unmerged.as_deref() == Some(&p.path[..]) {
            continue;
        }
        rest.push(p);
    }
    Ok((files, rest))
}

/// Writes the combined diffs of a work tree diff in the first of the
/// formats that has one (raw, patch, names); statistics have none.
pub fn worktree_output(
    repo: &Repo,
    out: &super::output::Output<'_>,
    files: &[File],
    dense: bool,
    out_bytes: &mut Vec<u8>,
) {
    let mut bytes = Vec::new();
    let bytes = &mut bytes;
    let f = out.formats;
    let style = PathStyle {
        quote: out.quote_high,
        nul: out.nul,
        all_paths: false,
    };
    if f.raw {
        let abbrev = (out.raw_abbrev != 0).then_some(out.raw_abbrev);
        raw(repo, files, abbrev, style, bytes);
    } else if f.name_only || f.name_status {
        names(files, f.name_status, style, bytes);
    } else if f.patch {
        let opts = PatchOptions {
            dense,
            all_paths: false,
            abbrev: match out.index_abbrev {
                0 => ObjectId::null(repo.hash).to_hex().len(),
                n => n,
            },
            quote: style.quote,
            diff: &out.patch.diff,
            output: Some(out),
        };
        patch(repo, files, &opts, bytes);
    }
    // `--line-prefix`.
    for line in bytes.split_inclusive(|&c| c == b'\n') {
        out_bytes.extend_from_slice(out.line_prefix.as_deref().unwrap_or_default());
        out_bytes.extend_from_slice(line);
    }
}

/// How [`raw`] and [`names`] write paths.
#[derive(Debug, Clone, Copy, Default)]
pub struct PathStyle {
    /// Quote unusual paths (`core.quotePath` for bytes >= 0x80).
    pub quote: bool,
    /// `-z`: NUL after fields and paths, no quoting.
    pub nul: bool,
    /// `--combined-all-paths`: the path in every parent before the
    /// result's.
    pub all_paths: bool,
}

impl PathStyle {
    fn write_paths(&self, f: &File, out: &mut Vec<u8>) {
        let write = |path: &[u8], out: &mut Vec<u8>| {
            if self.nul {
                out.extend_from_slice(path);
                out.push(0);
            } else {
                rgit_diff::quote_path(out, path, self.quote);
            }
        };
        if self.all_paths {
            for p in &f.parent_paths {
                write(p, out);
                if !self.nul {
                    out.push(b'\t');
                }
            }
        }
        write(&f.path, out);
        if !self.nul {
            out.push(b'\n');
        }
    }
}

/// `--name-only` and `--name-status` of combined diffs.
pub fn names(files: &[File], status: bool, style: PathStyle, out: &mut Vec<u8>) {
    for f in files {
        if status {
            out.extend_from_slice(&f.statuses);
            out.push(if style.nul { 0 } else { b'\t' });
        }
        style.write_paths(f, out);
    }
}

/// The raw lines: `::<modes> <ids> <statuses>\t<path>`.
pub fn raw(
    repo: &Repo,
    files: &[File],
    abbrev: Option<usize>,
    style: PathStyle,
    out: &mut Vec<u8>,
) {
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
        out.extend_from_slice(&f.statuses);
        out.push(if style.nul { 0 } else { b'\t' });
        style.write_paths(f, out);
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
/// parents' lines come first (as git shows them). Lines are compared
/// with the whitespace options `ws` (the earlier parent's text is kept).
fn merge_lost<'a>(
    rows: &mut Vec<(&'a [u8], u64)>,
    new: &[&'a [u8]],
    bit: u64,
    ws: &rgit_diff::Whitespace,
) {
    let same = |a: &[u8], b: &[u8]| match rgit_diff::whitespace::key(a, ws) {
        Some(k) => rgit_diff::whitespace::key(b, ws) == Some(k),
        None => a == b,
    };
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
            lcs[i][j] = if same(old[i].0, new[j]) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && same(old[i].0, new[j]) {
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
    /// `--combined-all-paths`: a `---` line for every parent.
    pub all_paths: bool,
    pub abbrev: usize,
    pub quote: bool,
    pub diff: &'a DiffOptions,
    /// The diff attributes (binary, textconv) of the paths, if any.
    pub output: Option<&'a super::output::Output<'a>>,
}

/// Appends the combined patch of `files`.
pub fn patch(repo: &Repo, files: &[File], opts: &PatchOptions<'_>, out: &mut Vec<u8>) {
    for f in files {
        file_patch(repo, f, opts, out);
    }
}

fn file_patch(repo: &Repo, f: &File, opts: &PatchOptions<'_>, out: &mut Vec<u8>) {
    let mut result = match &f.result_data {
        Some(d) => d.clone(),
        None => read(repo, f.result),
    };
    let mut parents: Vec<Vec<u8>> = f.parents.iter().map(|s| read(repo, *s)).collect();
    let mut binary = is_binary(&result) || parents.iter().any(|p| is_binary(p));
    if let Some(o) = opts.output {
        // The path's diff attribute decides; textconv output is text.
        let mut forced = None;
        for data in std::iter::once(&mut result).chain(parents.iter_mut()) {
            if let Ok((d, b)) = o.combined_view(&f.path, std::mem::take(data)) {
                *data = d;
                forced = forced.or(b);
            }
        }
        if let Some(b) = forced {
            binary = b;
        }
    }
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
                        &opts.diff.whitespace,
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
    if opts.all_paths {
        for (s, p) in f.parents.iter().zip(&f.parent_paths) {
            if s.is_none() {
                out.extend_from_slice(b"--- /dev/null\n");
            } else {
                out.extend_from_slice(b"--- ");
                let mut full = b"a/".to_vec();
                full.extend_from_slice(p);
                rgit_diff::quote_path(out, &full, opts.quote);
                out.push(b'\n');
            }
        }
    } else if added_file {
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
