// SPDX-License-Identifier: MIT
//! The diffcore transformations (gitdiffcore(7)): break detection
//! (`-B`), rename and copy detection (`-M`, `-C`), merging broken pairs
//! back, pickaxe (`-S`, `-G`, `--find-object`) and `--diff-filter`.
//!
//! The input is the list of file pairs a diff command found (tree against
//! tree, tree against index, index against work tree), in output order.
//! Contents are read through [`Contents`] only when a transformation needs
//! them. The details that the documentation leaves open (how candidates
//! are ranked, which use of a source becomes the rename, the size below
//! which files are never broken, ...) were determined by differential
//! tests against the `git` binary; they are described at the items that
//! implement them.

pub mod filter;
pub mod pickaxe;
pub mod similarity;

use std::collections::HashMap;

use rgit_hash::ObjectId;

use similarity::{Pieces, similarity};

/// Scores are fractions of this value (`R086` is 86% of it).
pub const MAX_SCORE: u32 = 60000;
/// Default minimum similarity of a rename or copy (50%).
pub const DEFAULT_RENAME_SCORE: u32 = 30000;
/// Default `-B` break score (50%).
pub const DEFAULT_BREAK_SCORE: u32 = 30000;
/// Default `-B` merge score (60%).
pub const DEFAULT_MERGE_SCORE: u32 = 36000;
/// Pairs whose larger side is smaller than this are never broken
/// (observed: 399 bytes are not broken, 400 bytes are).
const MINIMUM_BREAK_SIZE: u64 = 400;
/// Rename detection keeps this many best sources per destination
/// (observed: a destination whose four best sources went elsewhere stays
/// an addition even when a fifth one would match).
const CANDIDATES_PER_DST: usize = 4;
/// Default `diff.renameLimit`.
pub const DEFAULT_RENAME_LIMIT: usize = 1000;

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

/// Whether a mode is a blob (regular file or symlink).
pub fn is_blob_mode(mode: u32) -> bool {
    matches!(mode & S_IFMT, S_IFLNK | S_IFREG)
}

/// One side of a file pair; `extra` is carried along for the caller
/// (for example the work tree file holding the contents).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side<T> {
    pub path: Vec<u8>,
    pub mode: u32,
    /// The blob; the null id when the contents have not been hashed.
    pub oid: ObjectId,
    pub extra: T,
}

/// The status of a file pair (the letters of the raw format).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
    Unmerged,
    /// Only with `--find-copies-harder`: a copy source that did not change.
    /// Such pairs never reach the output.
    Unmodified,
}

impl Status {
    /// The status letter.
    pub fn letter(self) -> u8 {
        match self {
            Status::Added => b'A',
            Status::Copied => b'C',
            Status::Deleted => b'D',
            Status::Modified => b'M',
            Status::Renamed => b'R',
            Status::TypeChanged => b'T',
            Status::Unmerged => b'U',
            Status::Unmodified => b'X',
        }
    }
}

/// A file pair. `one` is the old side, `two` the new side.
#[derive(Debug, Clone)]
pub struct FilePair<T> {
    pub one: Option<Side<T>>,
    pub two: Option<Side<T>>,
    pub status: Status,
    /// Similarity of a rename or copy, or dissimilarity of a complete
    /// rewrite (a modification that was broken and merged back); in units
    /// of [`MAX_SCORE`], 0 when there is none.
    pub score: u32,
}

impl<T> FilePair<T> {
    /// A pair with the status derived from its sides.
    pub fn new(one: Option<Side<T>>, two: Option<Side<T>>) -> FilePair<T> {
        let status = match (&one, &two) {
            (None, _) => Status::Added,
            (_, None) => Status::Deleted,
            (Some(a), Some(b)) if a.mode & S_IFMT != b.mode & S_IFMT => Status::TypeChanged,
            (Some(a), Some(b)) if a.oid == b.oid && a.mode == b.mode && !a.oid.is_null() => {
                Status::Unmodified
            }
            _ => Status::Modified,
        };
        FilePair {
            one,
            two,
            status,
            score: 0,
        }
    }

    /// The score as a percentage (rounded down).
    pub fn percent(&self) -> u32 {
        self.score * 100 / MAX_SCORE
    }

    /// A complete rewrite: a modification shown with its dissimilarity.
    pub fn is_rewrite(&self) -> bool {
        matches!(self.status, Status::Modified | Status::TypeChanged) && self.score > 0
    }
}

/// Reads contents for the transformations that need them.
pub trait Contents<T> {
    /// The contents of `side` in blob form (after conversion for work tree
    /// files). Unreadable contents count as empty.
    fn read(&self, side: &Side<T>) -> Vec<u8>;
}

impl<T, F: Fn(&Side<T>) -> Vec<u8>> Contents<T> for F {
    fn read(&self, side: &Side<T>) -> Vec<u8> {
        self(side)
    }
}

/// Rename and copy detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Detect {
    #[default]
    Off,
    Renames,
    Copies,
}

/// What to do (diff-options(1)).
#[derive(Debug, Clone)]
pub struct Options {
    pub detect: Detect,
    /// `--find-copies-harder`: unmodified files are copy sources too (the
    /// caller must then include [`Status::Unmodified`] pairs).
    pub find_copies_harder: bool,
    /// Minimum similarity of a rename or copy (`-M<n>`).
    pub rename_score: u32,
    /// `-B<n>/<m>`: break score and merge score.
    pub break_rewrites: Option<(u32, u32)>,
    /// `-l<n>` / `diff.renameLimit`; 0 is unlimited.
    pub rename_limit: usize,
    /// `--[no-]rename-empty`: empty blobs may be rename sources.
    pub rename_empty: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            detect: Detect::Off,
            find_copies_harder: false,
            rename_score: DEFAULT_RENAME_SCORE,
            break_rewrites: None,
            rename_limit: DEFAULT_RENAME_LIMIT,
            rename_empty: true,
        }
    }
}

impl Options {
    /// Whether any transformation of [`run`] is active.
    pub fn active(&self) -> bool {
        self.detect != Detect::Off || self.break_rewrites.is_some()
    }
}

/// What happened besides the result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The exhaustive rename check was skipped; the limit that would have
    /// been needed.
    pub rename_limit_needed: Option<usize>,
    /// With `--find-copies-harder`, the exhaustive check used only the
    /// changed files as sources because of the limit.
    pub copies_from_modified_only: bool,
}

/// Parse a score as `-M`, `-C` and `-B` take it: digits read as a decimal
/// fraction (`5` is 50%, `05` is 5%), or a percentage with `%`. Returns
/// the score and the rest of the text.
pub fn parse_score(s: &str) -> (Option<u32>, &str) {
    // Digits with at most one decimal point: `5` and `.5` and `0.5` are
    // all one half; `1.5` is more than 100% (which matches nothing).
    let mut num: u128 = 0;
    let mut digits = 0u32;
    let mut frac: Option<u32> = None;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        match c {
            '0'..='9' => {
                num = num
                    .saturating_mul(10)
                    .saturating_add(u128::from(c as u8 - b'0'));
                digits += 1;
                if let Some(f) = frac.as_mut() {
                    *f += 1;
                }
            }
            '.' if frac.is_none() => frac = Some(0),
            _ => break,
        }
        end = i + c.len_utf8();
    }
    if digits == 0 {
        // A lone `%` or nothing: no number.
        return (None, s[end..].strip_prefix('%').unwrap_or(&s[end..]));
    }
    let mut rest = &s[end..];
    let pow = |n: u32| 10u128.checked_pow(n).unwrap_or(u128::MAX);
    let scale = if let Some(r) = rest.strip_prefix('%') {
        rest = r;
        pow(frac.unwrap_or(0)).saturating_mul(100)
    } else {
        pow(frac.unwrap_or(digits))
    };
    let score = num.saturating_mul(u128::from(MAX_SCORE)) / scale;
    (Some(u32::try_from(score).unwrap_or(u32::MAX)), rest)
}

fn basename(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&c| c == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Lazily loaded contents of the sides of the input pairs.
struct Files<'a, T> {
    pairs: &'a [FilePair<T>],
    contents: &'a dyn Contents<T>,
    /// (pair, new side?) -> data, pieces, id.
    cache: HashMap<(usize, bool), Loaded>,
}

struct Loaded {
    size: u64,
    oid: ObjectId,
    /// The contents until they are cut into pieces.
    data: Vec<u8>,
    pieces: Option<Pieces>,
}

impl<T> Files<'_, T> {
    fn side(&self, key: (usize, bool)) -> &Side<T> {
        let p = &self.pairs[key.0];
        if key.1 {
            p.two.as_ref()
        } else {
            p.one.as_ref()
        }
        .expect("side present")
    }

    fn load(&mut self, key: (usize, bool)) -> &mut Loaded {
        if !self.cache.contains_key(&key) {
            let side = self.side(key);
            let data = self.contents.read(side);
            let oid = if side.oid.is_null() {
                rgit_hash::hash_object(side.oid.kind(), "blob", &data).unwrap_or(side.oid)
            } else {
                side.oid
            };
            let loaded = Loaded {
                size: data.len() as u64,
                oid,
                data,
                pieces: None,
            };
            self.cache.insert(key, loaded);
        }
        self.cache.get_mut(&key).expect("inserted")
    }

    /// The object id, hashing work tree contents when needed.
    fn oid(&mut self, key: (usize, bool)) -> ObjectId {
        let side = self.side(key);
        if !side.oid.is_null() {
            return side.oid;
        }
        self.load(key).oid
    }

    fn size(&mut self, key: (usize, bool)) -> u64 {
        self.load(key).size
    }

    /// Cut a file into pieces (once; the contents are dropped then).
    fn cut(&mut self, key: (usize, bool)) {
        let l = self.load(key);
        if l.pieces.is_none() {
            l.pieces = Some(Pieces::new(&l.data));
            l.data = Vec::new();
        }
    }

    fn pieces(&self, key: (usize, bool)) -> &Pieces {
        self.cache[&key].pieces.as_ref().expect("cut")
    }

    fn similarity(&mut self, a: (usize, bool), b: (usize, bool), minimum: u32) -> u32 {
        // The sizes alone may rule the pair out.
        let (sa, sb) = (self.size(a), self.size(b));
        let (max, min) = (sa.max(sb), sa.min(sb));
        if max > 0 && min * u64::from(MAX_SCORE) < u64::from(minimum) * max {
            return 0;
        }
        self.cut(a);
        self.cut(b);
        similarity(self.pieces(a), self.pieces(b), minimum)
    }
}

/// Break: `Some(dissimilarity)` when the pair is broken. The pair is
/// broken when the deleted plus the inserted material reaches the break
/// score relative to the larger side; the dissimilarity is the deleted
/// material relative to the old size. Type changes between blobs are
/// always broken.
fn break_pair<T>(files: &mut Files<'_, T>, i: usize, break_score: u32) -> Option<u32> {
    let p = &files.pairs[i];
    let (one, two) = (p.one.as_ref()?, p.two.as_ref()?);
    if !is_blob_mode(one.mode) || !is_blob_mode(two.mode) {
        return None;
    }
    if p.status == Status::TypeChanged {
        return Some(MAX_SCORE);
    }
    if p.status != Status::Modified || (!one.oid.is_null() && one.oid == two.oid) {
        return None;
    }
    let (a, b) = ((i, false), (i, true));
    let (sa, sb) = (files.size(a), files.size(b));
    let max = sa.max(sb);
    if max < MINIMUM_BREAK_SIZE {
        return None;
    }
    files.cut(a);
    files.cut(b);
    let copied = files.pieces(a).copied(files.pieces(b));
    let deleted = sa.saturating_sub(copied);
    let inserted = sb.saturating_sub(copied);
    let m = u64::from(MAX_SCORE);
    let merge = (deleted * m).checked_div(sa).unwrap_or(0);
    if (deleted + inserted) * m / max < u64::from(break_score) {
        return None;
    }
    Some(u32::try_from(merge.min(m)).unwrap_or(MAX_SCORE))
}

/// A rename or copy source.
struct Src {
    pair: usize,
    /// A deleted file, or the old half of a broken pair: its last use can
    /// be a rename.
    deleted: bool,
    /// An unchanged file (`--find-copies-harder`).
    unmodified: bool,
    /// Destinations (pair indices) using this source, with the score.
    uses: Vec<(usize, u32)>,
}

/// Run break, rename/copy detection and merge-broken on `pairs`.
pub fn run<T: Clone>(
    pairs: Vec<FilePair<T>>,
    opts: &Options,
    contents: &dyn Contents<T>,
) -> (Vec<FilePair<T>>, Report) {
    let mut report = Report::default();
    if !opts.active() {
        let pairs = pairs
            .into_iter()
            .filter(|p| p.status != Status::Unmodified)
            .collect();
        return (pairs, report);
    }
    let n = pairs.len();
    let mut files = Files {
        pairs: &pairs,
        contents,
        cache: HashMap::new(),
    };

    // diffcore-break
    let mut broken: Vec<Option<u32>> = vec![None; n];
    let merge_score = opts.break_rewrites.map_or(DEFAULT_MERGE_SCORE, |b| b.1);
    if let Some((break_score, _)) = opts.break_rewrites {
        for (i, b) in broken.iter_mut().enumerate() {
            *b = break_pair(&mut files, i, break_score);
        }
    }

    // diffcore-rename
    // Destination of each pair (pair index -> (source index, score)).
    let mut matched: Vec<Option<(usize, u32)>> = vec![None; n];
    let mut srcs: Vec<Src> = Vec::new();
    if opts.detect != Detect::Off {
        let copies = opts.detect == Detect::Copies;
        // Submodules take part too (only exact renames can find them).
        let file = |s: &Option<Side<T>>| s.as_ref().is_some_and(|s| s.mode & S_IFMT != S_IFDIR);
        let blob_one = |p: &FilePair<T>| file(&p.one);
        let blob_two = |p: &FilePair<T>| file(&p.two);
        let mut dsts: Vec<usize> = Vec::new();
        for (i, p) in pairs.iter().enumerate() {
            let source = match (p.status, broken[i]) {
                (_, Some(d)) if blob_one(p) => {
                    if d >= merge_score {
                        Some(true)
                    } else {
                        copies.then_some(false)
                    }
                }
                (Status::Deleted, _) if blob_one(p) => Some(true),
                (Status::Modified | Status::TypeChanged, _) if copies && blob_one(p) => Some(false),
                (Status::Unmodified, _) if copies && opts.find_copies_harder && blob_one(p) => {
                    Some(false)
                }
                _ => None,
            };
            if let Some(deleted) = source {
                let one = p.one.as_ref().expect("source side");
                let empty =
                    !opts.rename_empty && is_blob_mode(one.mode) && files.size((i, false)) == 0;
                if !empty {
                    srcs.push(Src {
                        pair: i,
                        deleted,
                        unmodified: p.status == Status::Unmodified,
                        uses: Vec::new(),
                    });
                }
            }
            let dest = (p.status == Status::Added || broken[i].is_some()) && blob_two(p);
            if dest {
                dsts.push(i);
            }
        }
        if !srcs.is_empty() && !dsts.is_empty() {
            detect_renames(
                &mut files,
                opts,
                &mut srcs,
                &dsts,
                &mut matched,
                &mut report,
            );
        }
    }

    // Rename or copy: the last use of a source that is gone becomes the
    // rename. The old half of a broken pair is gone only when its new half
    // was taken by another source.
    let mut status_of: Vec<Option<Status>> = vec![None; n];
    let mut renamed_away = vec![false; n];
    for s in &srcs {
        let gone =
            s.deleted && (pairs[s.pair].status == Status::Deleted || matched[s.pair].is_some());
        for (k, &(dst, _)) in s.uses.iter().enumerate() {
            let rename = gone && k + 1 == s.uses.len();
            status_of[dst] = Some(if rename {
                Status::Renamed
            } else {
                Status::Copied
            });
            if rename {
                renamed_away[s.pair] = true;
            }
        }
    }

    // Output in the original order; diffcore-merge-broken.
    let mut out = Vec::with_capacity(n);
    for (i, p) in pairs.iter().enumerate() {
        if let Some((si, score)) = matched[i] {
            let src = &pairs[srcs[si].pair];
            let (one, two) = (src.one.as_ref(), p.two.as_ref());
            if one.map(|s| &s.path) == two.map(|s| &s.path) {
                // A "rename" to the same path (a tree with duplicate
                // entries): nothing when the contents are the same.
                let q = FilePair::new(src.one.clone(), p.two.clone());
                if q.status != Status::Unmodified {
                    out.push(q);
                }
                continue;
            }
            out.push(FilePair {
                one: src.one.clone(),
                two: p.two.clone(),
                status: status_of[i].unwrap_or(Status::Copied),
                score,
            });
            continue;
        }
        match p.status {
            Status::Unmodified => continue,
            Status::Deleted if renamed_away[i] => continue,
            _ => {}
        }
        let mut q = p.clone();
        if let Some(d) = broken[i] {
            q.score = if d >= merge_score { d } else { 0 };
        }
        out.push(q);
    }
    (out, report)
}

fn detect_renames<T>(
    files: &mut Files<'_, T>,
    opts: &Options,
    srcs: &mut [Src],
    dsts: &[usize],
    matched: &mut [Option<(usize, u32)>],
    report: &mut Report,
) {
    let copies = opts.detect == Detect::Copies;
    let mut used = vec![false; srcs.len()];
    let pairs = files.pairs;
    let src_side = |s: &Src| pairs[s.pair].one.as_ref().expect("source side");
    let dst_side = |d: usize| pairs[d].two.as_ref().expect("destination side");
    let compatible = |s: &Src, d: usize| src_side(s).mode & S_IFMT == dst_side(d).mode & S_IFMT;
    // Only blobs can be similar without being equal.
    let similar = |s: &Src, d: usize| compatible(s, d) && is_blob_mode(dst_side(d).mode);

    // Exact renames: the same blob. Among several sources, one with the
    // same file name wins, else the first.
    let mut by_oid: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    for (k, s) in srcs.iter().enumerate() {
        by_oid
            .entry(files.oid((s.pair, false)))
            .or_default()
            .push(k);
    }
    for &d in dsts {
        let oid = files.oid((d, true));
        let Some(cands) = by_oid.get(&oid) else {
            continue;
        };
        let ok: Vec<usize> = cands
            .iter()
            .copied()
            .filter(|&k| (copies || !used[k]) && compatible(&srcs[k], d) && srcs[k].pair != d)
            .collect();
        let name = basename(&dst_side(d).path);
        let pick = ok
            .iter()
            .copied()
            .find(|&k| basename(&src_side(&srcs[k]).path) == name)
            .or_else(|| ok.first().copied());
        if let Some(k) = pick {
            used[k] = true;
            matched[d] = Some((k, MAX_SCORE));
        }
    }

    let remaining_srcs =
        |used: &[bool]| -> Vec<usize> { (0..srcs.len()).filter(|&k| copies || !used[k]).collect() };
    let remaining_dsts = |matched: &[Option<(usize, u32)>]| -> Vec<usize> {
        dsts.iter()
            .copied()
            .filter(|&d| matched[d].is_none())
            .collect()
    };

    // Files moved to another directory under the same name, when both
    // names are unique among the remaining files (renames only, without
    // break detection). The threshold is halfway between the minimum
    // score and 100%.
    if opts.detect == Detect::Renames && opts.break_rewrites.is_none() {
        let rs = remaining_srcs(&used);
        let rd = remaining_dsts(matched);
        let mut src_names: HashMap<&[u8], (usize, usize)> = HashMap::new();
        for &k in &rs {
            let e = src_names
                .entry(basename(&src_side(&srcs[k]).path))
                .or_insert((k, 0));
            e.1 += 1;
        }
        let mut dst_names: HashMap<&[u8], usize> = HashMap::new();
        for &d in &rd {
            *dst_names.entry(basename(&dst_side(d).path)).or_insert(0) += 1;
        }
        let threshold = opts.rename_score + (MAX_SCORE - opts.rename_score.min(MAX_SCORE)) / 2;
        for &d in &rd {
            let name = basename(&dst_side(d).path);
            if dst_names.get(name) != Some(&1) {
                continue;
            }
            let Some(&(k, 1)) = src_names.get(name) else {
                continue;
            };
            if used[k] || !similar(&srcs[k], d) || srcs[k].pair == d {
                continue;
            }
            let score = files.similarity((srcs[k].pair, false), (d, true), threshold);
            if score >= threshold {
                used[k] = true;
                matched[d] = Some((k, score));
            }
        }
    }

    // The exhaustive comparison of the remaining files.
    let mut rs = remaining_srcs(&used);
    let rd = remaining_dsts(matched);
    if !rs.is_empty() && !rd.is_empty() {
        let limit = opts.rename_limit as u128;
        let too_many = |n: usize| limit > 0 && (n as u128) * (rd.len() as u128) > limit * limit;
        let mut skip = false;
        if too_many(rs.len()) {
            report.rename_limit_needed = Some(rs.len().max(rd.len()));
            // With --find-copies-harder, try the changed files alone.
            let changed: Vec<usize> = rs
                .iter()
                .copied()
                .filter(|&k| !srcs[k].unmodified)
                .collect();
            if copies && opts.find_copies_harder && !too_many(changed.len()) {
                rs = changed;
                report.copies_from_modified_only = true;
            } else {
                skip = true;
            }
        }
        if !skip {
            // (score, same name, destination order, source order)
            let mut all: Vec<(u32, bool, usize, usize)> = Vec::new();
            for (di, &d) in rd.iter().enumerate() {
                let name = basename(&dst_side(d).path).to_vec();
                let mut best: Vec<(u32, bool, usize, usize)> = Vec::new();
                for &k in &rs {
                    if !similar(&srcs[k], d) || srcs[k].pair == d {
                        continue;
                    }
                    let score =
                        files.similarity((srcs[k].pair, false), (d, true), opts.rename_score);
                    if score < opts.rename_score || score == 0 {
                        continue;
                    }
                    let same = basename(&src_side(&srcs[k]).path) == name.as_slice();
                    let pos = best
                        .iter()
                        .position(|b| (score, same) > (b.0, b.1))
                        .unwrap_or(best.len());
                    if pos < CANDIDATES_PER_DST {
                        best.insert(pos, (score, same, di, k));
                        best.truncate(CANDIDATES_PER_DST);
                    }
                }
                all.extend(best);
            }
            all.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(b.1.cmp(&a.1))
                    .then(a.2.cmp(&b.2))
                    .then(a.3.cmp(&b.3))
            });
            for (score, _, di, k) in all {
                let d = rd[di];
                if matched[d].is_some() || (!copies && used[k]) {
                    continue;
                }
                used[k] = true;
                matched[d] = Some((k, score));
            }
        }
    }

    for &d in dsts {
        if let Some((k, score)) = matched[d] {
            srcs[k].uses.push((d, score));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores() {
        assert_eq!(parse_score("5"), (Some(30000), ""));
        assert_eq!(parse_score("50%"), (Some(30000), ""));
        assert_eq!(parse_score("05"), (Some(3000), ""));
        assert_eq!(parse_score("499"), (Some(29940), ""));
        assert_eq!(parse_score("100%"), (Some(MAX_SCORE), ""));
        assert_eq!(parse_score("20%/80%"), (Some(12000), "/80%"));
        assert_eq!(parse_score(""), (None, ""));
        assert_eq!(parse_score(".5"), (Some(30000), ""));
        assert_eq!(parse_score("0.5"), (Some(30000), ""));
        assert_eq!(parse_score("1.5"), (Some(90000), ""));
        assert_eq!(parse_score("101%"), (Some(60600), ""));
    }
}
