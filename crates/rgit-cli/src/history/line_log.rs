// SPDX-License-Identifier: MIT
//! `log -L <start>,<end>:<file>` and `-L :<funcname>:<file>`
//! (Documentation/git-log.adoc, line-range-options.adoc): the history of
//! line ranges.
//!
//! Observed behaviour of the `git` binary, which this follows:
//!
//! - the ranges are parsed against the files of the one commit the walk
//!   starts from; every commit passes its ranges on to its parents through
//!   the diff against each parent (Myers with the indent heuristic, unless
//!   other diff options are given). A range line inside a changed block
//!   maps to the whole block on the parent's side; ranges that end up
//!   empty are dropped;
//! - a commit touches a range when a changed block overlaps it, or a pure
//!   deletion lies strictly inside it. Commits that touch no range are
//!   TREESAME (history simplification as with paths): a merge that leaves
//!   all ranges alone on one side follows that parent only, other merges
//!   are shown without a diff and pass the ranges to all parents;
//! - a tracked file missing in a parent is looked for under the name it
//!   was renamed (or copied) from; without a source, all its lines are new;
//! - the patch of a commit has one hunk per range: every line of the range
//!   in the commit, with the deletions of changed blocks that start in it
//!   or right after it.
//!   The old start is the number of old lines before the first line shown
//!   plus one (also for empty old sides), `-0,0` for new files; counts are
//!   always written.

use std::collections::HashMap;

use rgit_diff::{Change, DiffOptions};
use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_revwalk::revision::ObjectLookup;

use super::Repo;
use super::follow::{self, Entry};
use crate::Die;
use crate::commands::blame::range::{self, RangeError};
use crate::messages::rev_list as msg;

/// Ranges per path: sorted, disjoint, 0-based half-open.
type Ranges = Vec<(usize, usize)>;
type FileRanges = Vec<(Vec<u8>, Ranges)>;

/// One line of a range-limited patch.
pub struct Line {
    pub kind: u8,
    pub text: Vec<u8>,
}

/// One hunk: the 1-based starts as git prints them and the lines.
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub func: Option<Vec<u8>>,
    pub lines: Vec<Line>,
}

/// The patch of one tracked file in a shown commit.
pub struct FilePatch {
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    /// `None`: the file is new.
    pub old: Option<Entry>,
    pub new: Entry,
    pub hunks: Vec<Hunk>,
}

struct Node {
    /// Per walked parent: whether a range changed against it (roots: one
    /// entry, whether there is a range).
    touched: Vec<bool>,
    parents: Vec<ObjectId>,
    patches: Vec<FilePatch>,
}

/// The computed line log.
pub struct LineLog {
    nodes: HashMap<ObjectId, Node>,
}

/// The function-name patterns of a path.
pub type FuncNameOf = dyn Fn(&[u8]) -> Option<rgit_diff::FuncName>;

/// The diff settings of the range mapping.
pub struct Settings {
    pub diff: DiffOptions,
    /// The diff that is shown, when whitespace options make it differ.
    pub display: Option<DiffOptions>,
    pub first_parent: bool,
    /// Function-name patterns per path (`-L :<funcname>`, hunk headers).
    pub funcname: Box<FuncNameOf>,
}

/// Why `-L` was rejected.
fn bad_arg(arg: &str) -> Die {
    Die::Fatal(msg::line_log_bad_arg(arg))
}

/// Whether a `/regex/` term starting at `s` (after the slash) is closed.
fn closed_regex(s: &str) -> Option<&str> {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '/' => return Some(&s[i + 1..]),
            _ => {}
        }
    }
    None
}

/// Split `-L` arguments into range and path. The path is after the
/// last colon that does not belong to the range.
pub fn split_arg(arg: &str) -> Result<(String, String), Die> {
    let (range, path) = if let Some(rest) = arg.strip_prefix(':').or_else(|| arg.strip_prefix("^:"))
    {
        // `:<funcname>:<file>`: the function regex runs to the last colon.
        let (f, p) = rest.rsplit_once(':').ok_or_else(|| bad_arg(arg))?;
        let lead = if arg.starts_with('^') { "^:" } else { ":" };
        (format!("{lead}{f}"), p.to_owned())
    } else {
        // `<start>,<end>`: regexes may contain colons.
        let mut rest = arg;
        let mut consumed = 0;
        for _ in 0..2 {
            let s = rest.strip_prefix('^').unwrap_or(rest);
            if let Some(r) = s.strip_prefix('/') {
                let after = closed_regex(r).ok_or_else(|| bad_arg(arg))?;
                consumed += rest.len() - after.len();
                rest = after;
            } else {
                let n = rest
                    .bytes()
                    .take_while(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-'))
                    .count();
                consumed += n;
                rest = &rest[n..];
            }
            match rest.strip_prefix(',') {
                Some(r) => {
                    consumed += 1;
                    rest = r;
                }
                None => break,
            }
        }
        let path = rest.strip_prefix(':').ok_or_else(|| bad_arg(arg))?;
        (arg[..consumed].to_owned(), path.to_owned())
    };
    if path.is_empty() || range.is_empty() || range == ":" {
        return Err(bad_arg(arg));
    }
    Ok((range, path))
}

/// Tree entries looked up so far, per tree and name: the trees of
/// directories that a commit leaves alone are read only once.
#[derive(Default)]
struct Entries(HashMap<(ObjectId, Vec<u8>), Option<Entry>>);

impl Entries {
    fn get(&mut self, repo: &Repo<'_>, tree: &ObjectId, path: &[u8]) -> Result<Option<Entry>, Die> {
        let bad = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
        let mut tree = *tree;
        let mut parts = path.split(|&b| b == b'/').peekable();
        while let Some(name) = parts.next() {
            let key = (tree, name.to_vec());
            let found = match self.0.get(&key) {
                Some(e) => *e,
                None => {
                    let e = follow::entry_at(repo, &tree, name).map_err(bad)?;
                    self.0.insert(key, e);
                    e
                }
            };
            let Some(found) = found else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(found));
            }
            if found.mode & 0o170000 != 0o040000 {
                return Ok(None);
            }
            tree = found.id;
        }
        Ok(None)
    }
}

/// The file of tree `old` that `target` (a file of tree `new` missing
/// in `old`) was renamed from: a file gone in `new` with the same
/// content, else the most similar one with at least 50 % similarity.
fn find_rename(
    repo: &Repo<'_>,
    new: &ObjectId,
    old: &ObjectId,
    target: &Entry,
) -> Result<Option<Vec<u8>>, Die> {
    use rgit_diff::diffcore::{DEFAULT_RENAME_SCORE, similarity::Pieces, similarity::similarity};
    let bad = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    if target.mode & 0o170000 == 0o160000 {
        return Ok(None);
    }
    let mut old_files = HashMap::new();
    follow::files(repo, Some(old), b"", &mut old_files).map_err(bad)?;
    let mut new_files = HashMap::new();
    follow::files(repo, Some(new), b"", &mut new_files).map_err(bad)?;
    let mut gone: Vec<(&Vec<u8>, &Entry)> = old_files
        .iter()
        .filter(|(p, e)| e.mode & 0o170000 != 0o160000 && !new_files.contains_key(*p))
        .collect();
    gone.sort_by(|a, b| a.0.cmp(b.0));
    if let Some((p, _)) = gone.iter().find(|(_, e)| e.id == target.id) {
        return Ok(Some((*p).clone()));
    }
    let data = blob(repo, &target.id)?;
    let dst = Pieces::new(&data);
    let mut best: Option<(u32, &Vec<u8>)> = None;
    for (p, e) in gone {
        let src = blob(repo, &e.id)?;
        let score = similarity(&Pieces::new(&src), &dst, DEFAULT_RENAME_SCORE);
        if score >= DEFAULT_RENAME_SCORE && best.is_none_or(|(b, _)| score > b) {
            best = Some((score, p));
        }
    }
    Ok(best.map(|(_, p)| p.clone()))
}

fn blob(repo: &Repo<'_>, id: &ObjectId) -> Result<Vec<u8>, Die> {
    match repo.objects.read(id).ok().flatten() {
        Some((ObjectKind::Blob, data)) => Ok(data),
        _ => Err(Die::Fatal(msg::bad_object(&id.to_hex()))),
    }
}

fn lines(data: &[u8]) -> Vec<&[u8]> {
    rgit_diff::split_lines(data)
}

/// Sort and merge overlapping or adjacent ranges, dropping empty ones.
fn normalize(r: Ranges) -> Ranges {
    range::normalize(r.into_iter().filter(|(a, b)| b > a).collect())
}

fn add_ranges(map: &mut FileRanges, path: &[u8], r: Ranges) {
    if r.is_empty() {
        return;
    }
    match map.iter_mut().find(|(p, _)| p == path) {
        Some((_, old)) => {
            old.extend(r);
            *old = normalize(std::mem::take(old));
        }
        None => map.push((path.to_vec(), normalize(r))),
    }
}

/// Whether change `c` touches range `[s, e)` (of the new side).
fn touches(c: &Change, s: usize, e: usize) -> bool {
    if c.b_len == 0 {
        s < c.b_start && c.b_start < e
    } else {
        c.b_start < e && c.b_start + c.b_len > s
    }
}

/// The parent's side of range `[s, e)`.
fn map_range(changes: &[Change], s: usize, e: usize) -> (usize, usize) {
    let map = |x: usize, end: bool| -> usize {
        let mut offset: isize = 0;
        for c in changes {
            if c.b_len > 0 && c.b_start <= x && x < c.b_start + c.b_len {
                return if end { c.a_start + c.a_len } else { c.a_start };
            }
            if c.b_start + c.b_len <= x {
                offset = (c.a_start + c.a_len) as isize - (c.b_start + c.b_len) as isize;
            } else {
                break;
            }
        }
        let y = (x as isize + offset) as usize;
        if end { y + 1 } else { y }
    };
    (map(s, false), map(e - 1, true))
}

/// The hunk of range `[s, e)`.
fn hunk(a: &[&[u8]], b: &[&[u8]], changes: &[Change], s: usize, e: usize, new_file: bool) -> Hunk {
    let mut out = Vec::new();
    let mut first_old = None;
    let (mut pa, mut pb) = (0usize, 0usize);
    let mut ci = 0;
    let mut push = |kind: u8, text: &[u8], pa: usize, out: &mut Vec<Line>| {
        if first_old.is_none() {
            first_old = Some(pa);
        }
        out.push(Line {
            kind,
            text: text.to_vec(),
        });
    };
    loop {
        let at_change =
            ci < changes.len() && changes[ci].b_start == pb && changes[ci].a_start == pa;
        if pb >= e && !(at_change && pb == e) {
            break;
        }
        if at_change {
            let c = changes[ci];
            ci += 1;
            // Deleted lines show from changes that start in the range or
            // right after it.
            let show_del = c.b_start >= s && c.b_start <= e;
            for k in 0..c.a_len {
                if show_del {
                    push(b'-', a[c.a_start + k], pa, &mut out);
                }
                pa += 1;
            }
            for k in 0..c.b_len {
                let y = c.b_start + k;
                if y >= s && y < e {
                    push(b'+', b[y], pa, &mut out);
                }
                pb += 1;
            }
            continue;
        }
        if pb >= s {
            push(b' ', b[pb], pa, &mut out);
        }
        pa += 1;
        pb += 1;
    }
    let old_count = out.iter().filter(|l| l.kind != b'+').count();
    let new_count = out.iter().filter(|l| l.kind != b'-').count();
    let first = first_old.unwrap_or(pa);
    let old_start = if new_file { 0 } else { first + 1 };
    Hunk {
        old_start,
        old_count,
        new_start: s + 1,
        new_count,
        func: None,
        lines: out,
    }
}

impl LineLog {
    /// Parse the `-L` arguments against `tip` and run the line log over
    /// the commits reachable from `tip` but not from `excluded`.
    pub fn compute(
        repo: &Repo<'_>,
        args: &[(String, String)],
        tip: ObjectId,
        excluded: &[ObjectId],
        settings: &Settings,
    ) -> Result<LineLog, Die> {
        let bad = |id: &ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
        let tip_tree = repo.tree_of(&tip).ok_or_else(|| bad(&tip))?;
        let mut start: FileRanges = Vec::new();
        let mut anchors: HashMap<Vec<u8>, usize> = HashMap::new();
        for (spec, path) in args {
            let p = path.as_bytes().to_vec();
            let entry = follow::entry_at(repo, &tip_tree, &p)
                .map_err(|id| bad(&id))?
                .filter(|e| e.mode & 0o170000 != 0o040000)
                .ok_or_else(|| Die::Fatal(msg::line_log_no_path(path)))?;
            let data = blob(repo, &entry.id)?;
            let ls = lines(&data);
            let anchor = anchors.get(&p).copied().unwrap_or(0);
            let func = (settings.funcname)(&p);
            let r = match range::parse(spec, &ls, anchor, path, func.as_ref()) {
                Ok(r) => r,
                Err(RangeError::Fatal(m)) => return Err(Die::Fatal(m)),
                Err(RangeError::Usage) => return Err(bad_arg(&format!("{spec}:{path}"))),
            };
            anchors.insert(p.clone(), r.1);
            add_ranges(&mut start, &p, vec![r]);
        }

        // The commits to process, children before parents.
        let mut hidden = std::collections::HashSet::new();
        {
            let mut stack: Vec<ObjectId> = excluded.to_vec();
            while let Some(id) = stack.pop() {
                if !hidden.insert(id) {
                    continue;
                }
                let (c, _) = repo.read_commit(&id).ok_or_else(|| bad(&id))?;
                stack.extend(repo.parents(&id, &c));
            }
        }
        let mut parents_of: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        let mut pending: HashMap<ObjectId, usize> = HashMap::new();
        let mut stack = vec![tip];
        while let Some(id) = stack.pop() {
            if hidden.contains(&id) || parents_of.contains_key(&id) {
                continue;
            }
            let (c, _) = repo.read_commit(&id).ok_or_else(|| bad(&id))?;
            let mut ps: Vec<ObjectId> = repo.parents(&id, &c);
            if settings.first_parent {
                ps.truncate(1);
            }
            ps.retain(|p| !hidden.contains(p));
            let mut seen = Vec::new();
            for p in &ps {
                if !seen.contains(p) {
                    seen.push(*p);
                    *pending.entry(*p).or_insert(0) += 1;
                    stack.push(*p);
                }
            }
            parents_of.insert(id, ps);
        }

        let mut ranges: HashMap<ObjectId, FileRanges> = HashMap::new();
        ranges.insert(tip, start);
        let mut entries = Entries::default();
        let mut nodes = HashMap::new();
        let mut ready = vec![tip];
        while let Some(id) = ready.pop() {
            let ps = parents_of.remove(&id).unwrap_or_default();
            let mine = ranges.remove(&id).unwrap_or_default();
            let mut uniq = Vec::new();
            for p in &ps {
                if !uniq.contains(p) {
                    uniq.push(*p);
                }
            }
            if !mine.is_empty() {
                let node =
                    Self::process(repo, &id, &ps, mine, settings, &mut ranges, &mut entries)?;
                nodes.insert(id, node);
            }
            for p in uniq {
                let n = pending.get_mut(&p).expect("counted");
                *n -= 1;
                if *n == 0 {
                    ready.push(p);
                }
            }
        }
        Ok(LineLog { nodes })
    }

    /// One commit: its patches and touched flags; passes the ranges on.
    fn process(
        repo: &Repo<'_>,
        id: &ObjectId,
        parents: &[ObjectId],
        mine: FileRanges,
        settings: &Settings,
        ranges: &mut HashMap<ObjectId, FileRanges>,
        entries: &mut Entries,
    ) -> Result<Node, Die> {
        let bad = |id: &ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
        let tree = repo.tree_of(id).ok_or_else(|| bad(id))?;
        let mut files = Vec::new();
        for (path, r) in &mine {
            let Some(entry) = entries.get(repo, &tree, path)? else {
                continue;
            };
            // Read only when the file changed.
            files.push((path.clone(), entry, std::cell::OnceCell::new(), r.clone()));
        }

        // Per parent: the parent's ranges, whether something changed, and
        // the patches.
        let mut per_parent = Vec::new();
        for p in parents {
            let ptree = repo.tree_of(p).ok_or_else(|| bad(p))?;
            let mut out: FileRanges = Vec::new();
            let mut touched = false;
            let mut patches = Vec::new();
            for (path, entry, cell, r) in &files {
                let mut old_path = path.clone();
                let mut old = entries.get(repo, &ptree, path)?;
                if old.is_none() {
                    if let Some(src) = find_rename(repo, &tree, &ptree, entry)? {
                        old = entries.get(repo, &ptree, &src)?;
                        old_path = src;
                    }
                }
                if cell.get().is_none() && old.is_none_or(|o| o.id != entry.id) {
                    let _ = cell.set(blob(repo, &entry.id)?);
                }
                let data = cell.get().map_or(&[][..], Vec::as_slice);
                let Some(old) = old else {
                    touched = true;
                    patches.push(Self::patch(
                        path,
                        path,
                        None,
                        *entry,
                        &[],
                        data,
                        &[],
                        r,
                        settings,
                    ));
                    continue;
                };
                if old.id == entry.id {
                    add_ranges(&mut out, &old_path, r.clone());
                    continue;
                }
                let odata = blob(repo, &old.id)?;
                let (a, b) = (lines(&odata), lines(data));
                let changes = rgit_diff::diff_lines(&a, &b, &settings.diff);
                let file_touched = r
                    .iter()
                    .any(|&(s, e)| changes.iter().any(|c| touches(c, s, e)));
                let mapped: Ranges = r.iter().map(|&(s, e)| map_range(&changes, s, e)).collect();
                add_ranges(&mut out, &old_path, mapped);
                if file_touched {
                    touched = true;
                    // Whitespace options change only what is shown.
                    let shown = match &settings.display {
                        Some(opts) => rgit_diff::diff_lines(&a, &b, opts),
                        None => changes,
                    };
                    let p = Self::patch(
                        &old_path,
                        path,
                        Some(old),
                        *entry,
                        &odata,
                        data,
                        &shown,
                        r,
                        settings,
                    );
                    if !p.hunks.is_empty() {
                        patches.push(p);
                    }
                }
            }
            per_parent.push((out, touched, patches));
        }

        if parents.is_empty() {
            let mut patches = Vec::new();
            for (path, entry, cell, r) in &files {
                if cell.get().is_none() {
                    let _ = cell.set(blob(repo, &entry.id)?);
                }
                let data = cell.get().map_or(&[][..], Vec::as_slice);
                patches.push(Self::patch(
                    path,
                    path,
                    None,
                    *entry,
                    &[],
                    data,
                    &[],
                    r,
                    settings,
                ));
            }
            return Ok(Node {
                touched: vec![!patches.is_empty()],
                parents: Vec::new(),
                patches,
            });
        }
        let touched: Vec<bool> = per_parent.iter().map(|(_, t, _)| *t).collect();
        let same = touched.iter().position(|t| !t);
        let mut patches = Vec::new();
        for (i, (out, _, pp)) in per_parent.into_iter().enumerate() {
            if same.is_some_and(|s| s != i) {
                continue;
            }
            if parents.len() == 1 {
                patches = pp;
            }
            let entry = ranges.entry(parents[i]).or_default();
            for (path, r) in out {
                add_ranges(entry, &path, r);
            }
        }
        Ok(Node {
            touched,
            parents: parents.to_vec(),
            patches,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn patch(
        old_path: &[u8],
        new_path: &[u8],
        old: Option<Entry>,
        new: Entry,
        odata: &[u8],
        data: &[u8],
        changes: &[Change],
        r: &Ranges,
        settings: &Settings,
    ) -> FilePatch {
        let (a, b) = (lines(odata), lines(data));
        let all;
        let changes = if old.is_none() {
            all = [Change {
                a_start: 0,
                a_len: 0,
                b_start: 0,
                b_len: b.len(),
            }];
            &all[..]
        } else {
            changes
        };
        let func = (settings.funcname)(new_path);
        let mut hunks: Vec<Hunk> = r
            .iter()
            .map(|&(s, e)| hunk(&a, &b, changes, s, e, old.is_none()))
            .collect();
        if old.is_some() {
            // Function names (observed): those of a whole-file diff whose
            // context is as long as the longest range (at least three
            // lines); each range takes the name of the diff hunk of its
            // first change.
            let ctx = r.iter().map(|(s, e)| e - s).max().unwrap_or(0).max(3);
            let mut groups: Vec<(usize, Option<Vec<u8>>)> = Vec::new();
            let mut group_of = vec![0usize; changes.len()];
            let mut prev_end: Option<usize> = None;
            let mut scanned = 0usize;
            let mut last: Option<Vec<u8>> = None;
            for (i, c) in changes.iter().enumerate() {
                let joined = prev_end.is_some_and(|p| c.a_start <= p + 2 * ctx);
                if !joined {
                    let start = c.a_start.saturating_sub(ctx);
                    if let Some(t) = (scanned..start.min(a.len()))
                        .rev()
                        .find_map(|k| rgit_diff::funcname::find_func(func.as_ref(), a[k]))
                    {
                        last = Some(rgit_diff::funcname_text(t).to_vec()).filter(|t| !t.is_empty());
                    }
                    scanned = scanned.max(start);
                    groups.push((start, last.clone()));
                }
                group_of[i] = groups.len() - 1;
                prev_end = Some(c.a_start + c.a_len);
            }
            for (h, &(s, e)) in hunks.iter_mut().zip(r) {
                if let Some(i) = changes.iter().position(|c| touches(c, s, e)) {
                    h.func.clone_from(&groups[group_of[i]].1);
                }
            }
        }
        hunks.retain(|h| old.is_none() || h.lines.iter().any(|l| l.kind != b' '));
        FilePatch {
            old_path: old_path.to_vec(),
            new_path: new_path.to_vec(),
            old,
            new,
            hunks,
        }
    }

    /// History simplification: whether `id` leaves the ranges alone
    /// against `parent` (roots: whether it has none).
    pub fn treesame(&self, id: &ObjectId, parent: Option<&ObjectId>) -> bool {
        let Some(node) = self.nodes.get(id) else {
            return true;
        };
        match parent {
            None => !node.touched.first().copied().unwrap_or(false),
            Some(p) => node
                .parents
                .iter()
                .position(|q| q == p)
                .is_none_or(|i| !node.touched[i]),
        }
    }

    /// The patches of commit `id`.
    pub fn patches(&self, id: &ObjectId) -> &[FilePatch] {
        self.nodes.get(id).map_or(&[], |n| &n.patches)
    }

    /// The paths of commit `id`'s patches (both sides).
    pub fn paths(&self, id: &ObjectId) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for p in self.patches(id) {
            for x in [&p.old_path, &p.new_path] {
                if !out.contains(x) {
                    out.push(x.clone());
                }
            }
        }
        out
    }
}

/// How patches are written.
pub struct Render<'x> {
    pub src_prefix: &'x [u8],
    pub dst_prefix: &'x [u8],
    pub abbrev: &'x dyn Fn(&ObjectId) -> String,
    pub null: String,
    /// `-R`: the patches turn the new contents into the old.
    pub reverse: bool,
}

fn quoted(out: &mut Vec<u8>, prefix: &[u8], path: &[u8]) {
    let mut full = prefix.to_vec();
    full.extend_from_slice(path);
    rgit_diff::quote_path(out, &full, true);
}

/// The mode a diff shows: regular files are 100644 or 100755.
fn canonical(mode: u32) -> u32 {
    match mode & 0o170000 {
        0o100000 if mode & 0o111 != 0 => 0o100755,
        0o100000 => 0o100644,
        _ => mode,
    }
}

/// The lines of a hunk with `-` and `+` swapped; within each run of
/// changed lines the (new) deletions come first.
fn reversed_lines(lines: &[Line]) -> Vec<&Line> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == b' ' {
            out.push(&lines[i]);
            i += 1;
            continue;
        }
        let end = lines[i..]
            .iter()
            .position(|l| l.kind == b' ')
            .map_or(lines.len(), |p| i + p);
        out.extend(lines[i..end].iter().filter(|l| l.kind == b'+'));
        out.extend(lines[i..end].iter().filter(|l| l.kind == b'-'));
        i = end;
    }
    out
}

/// The patches of one commit, one plain patch (indicators `+`, `-` and
/// space) per file.
pub fn write(patches: &[FilePatch], r: &Render<'_>) -> Vec<Vec<u8>> {
    use std::io::Write as _;
    let mut files = Vec::new();
    for p in patches {
        let mut out = Vec::new();
        let new_mode = canonical(p.new.mode);
        let new_id = (r.abbrev)(&p.new.id);
        // The two sides as shown: (prefix, path) of the old and new side.
        let (a, b) = if r.reverse {
            ((r.dst_prefix, &p.new_path), (r.src_prefix, &p.old_path))
        } else {
            ((r.src_prefix, &p.old_path), (r.dst_prefix, &p.new_path))
        };
        out.extend_from_slice(b"diff --git ");
        quoted(&mut out, a.0, a.1);
        out.push(b' ');
        quoted(&mut out, b.0, b.1);
        out.push(b'\n');
        match &p.old {
            None if r.reverse => {
                let _ = writeln!(out, "deleted file mode {new_mode:06o}");
                let _ = writeln!(out, "index {}..{}", new_id, r.null);
                out.extend_from_slice(b"--- ");
                quoted(&mut out, a.0, a.1);
                out.extend_from_slice(b"\n+++ /dev/null\n");
            }
            None => {
                let _ = writeln!(out, "new file mode {new_mode:06o}");
                let _ = writeln!(out, "index {}..{}", r.null, new_id);
                out.extend_from_slice(b"--- /dev/null\n+++ ");
                quoted(&mut out, b.0, b.1);
                out.push(b'\n');
            }
            Some(old) => {
                let old_mode = canonical(old.mode);
                let old_id = (r.abbrev)(&old.id);
                let ((m1, i1), (m2, i2)) = if r.reverse {
                    ((new_mode, &new_id), (old_mode, &old_id))
                } else {
                    ((old_mode, &old_id), (new_mode, &new_id))
                };
                if m1 == m2 {
                    let _ = writeln!(out, "index {i1}..{i2} {m2:06o}");
                } else {
                    let _ = writeln!(out, "old mode {m1:06o}");
                    let _ = writeln!(out, "new mode {m2:06o}");
                    let _ = writeln!(out, "index {i1}..{i2}");
                }
                out.extend_from_slice(b"--- ");
                quoted(&mut out, a.0, a.1);
                out.extend_from_slice(b"\n+++ ");
                quoted(&mut out, b.0, b.1);
                out.push(b'\n');
            }
        }
        for h in &p.hunks {
            let (os, oc, ns, nc) = if r.reverse {
                (h.new_start, h.new_count, h.old_start, h.old_count)
            } else {
                (h.old_start, h.old_count, h.new_start, h.new_count)
            };
            let _ = write!(out, "@@ -{os},{oc} +{ns},{nc} @@");
            if let Some(f) = &h.func {
                out.push(b' ');
                out.extend_from_slice(f);
            }
            out.push(b'\n');
            let lines: Vec<&Line> = if r.reverse {
                reversed_lines(&h.lines)
            } else {
                h.lines.iter().collect()
            };
            for l in lines {
                let kind = match (l.kind, r.reverse) {
                    (b'+', true) => b'-',
                    (b'-', true) => b'+',
                    (k, _) => k,
                };
                out.push(kind);
                out.extend_from_slice(&l.text);
                if l.text.last() != Some(&b'\n') {
                    out.push(b'\n');
                    out.extend_from_slice(rgit_diff::messages::NO_NEWLINE_AT_EOF.as_bytes());
                    out.push(b'\n');
                }
            }
        }
        files.push(out);
    }
    files
}
