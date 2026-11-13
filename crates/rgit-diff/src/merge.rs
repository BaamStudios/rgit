// SPDX-License-Identifier: MIT

//! Three-way merge of one text (`git merge-file`, the content merge of
//! every merge command).
//!
//! Both sides are diffed against the base. Changes of one side that keep at
//! least one unchanged line away from every change of the other side are
//! taken over; changes that overlap or touch form a conflict. What happens
//! to a conflict depends on the style:
//!
//! - every style resolves a conflict in which both sides made the same
//!   change;
//! - `merge` diffs the two sides of a conflict against each other and keeps
//!   only the differing parts in conflict, then joins conflicts that are at
//!   most three lines apart, or apart only by lines without letters and
//!   digits;
//! - `zdiff3` moves the lines that both sides share at the start and at the
//!   end of a conflict out of it and keeps the base complete;
//! - `diff3` keeps conflicts as they are.
//!
//! The behaviour was derived from `Documentation/git-merge-file.adoc`,
//! `Documentation/git-merge.adoc` ("HOW CONFLICTS ARE PRESENTED"), the
//! conformance tests and differential tests against the `git` binary.

use crate::{Change, DiffOptions, diff_lines, is_binary, split_lines};

/// Default length of the conflict markers.
pub const DEFAULT_MARKER_SIZE: usize = 7;

/// In the `merge` style, conflicts at most this many lines apart are joined
/// (farther apart only if the lines between have no letters or digits).
const JOIN_DISTANCE: usize = 3;

/// How conflicts are written (`merge.conflictStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictStyle {
    /// Ours and theirs, minimised (`merge`, the default).
    #[default]
    Merge,
    /// Ours, base and theirs, unchanged (`diff3`).
    Diff3,
    /// Like `diff3`, with common lines at the start and end moved out
    /// (`zdiff3`).
    ZealousDiff3,
}

impl ConflictStyle {
    /// Parses a `merge.conflictStyle` value.
    pub fn from_name(name: &str) -> Option<ConflictStyle> {
        match name {
            "merge" => Some(ConflictStyle::Merge),
            "diff3" => Some(ConflictStyle::Diff3),
            "zdiff3" => Some(ConflictStyle::ZealousDiff3),
            _ => None,
        }
    }
}

/// How conflicts are resolved (`--ours`, `--theirs`, `--union`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Favor {
    /// Leave conflicts with markers in the result.
    #[default]
    None,
    /// Take our side of each conflict.
    Ours,
    /// Take their side of each conflict.
    Theirs,
    /// Take our side followed by their side.
    Union,
}

/// Options of [`merge`].
#[derive(Debug, Clone, Default)]
pub struct MergeOptions<'a> {
    pub style: ConflictStyle,
    pub favor: Favor,
    /// Length of the conflict markers; 0 means [`DEFAULT_MARKER_SIZE`].
    pub marker_size: usize,
    /// Labels after the markers: ours (`<<<<<<<`), base (`|||||||`),
    /// theirs (`>>>>>>>`). An empty label leaves the marker alone.
    pub ours_label: &'a [u8],
    pub base_label: &'a [u8],
    pub theirs_label: &'a [u8],
    /// Options of the diffs against the base.
    pub diff: DiffOptions,
}

/// Result of [`merge`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergeResult {
    /// The merged text, with conflict markers unless a [`Favor`] resolved
    /// them.
    pub content: Vec<u8>,
    /// Number of conflicts left in `content`.
    pub conflicts: usize,
}

impl MergeResult {
    /// Whether the merge was clean.
    pub fn is_clean(&self) -> bool {
        self.conflicts == 0
    }
}

/// Error of [`merge`]: one of the inputs is binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryContent;

impl std::fmt::Display for BinaryContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cannot merge binary content")
    }
}

impl std::error::Error for BinaryContent {}

/// The diff options git uses for content merges: like `git diff`, but
/// without the indent heuristic.
pub fn merge_diff_options() -> DiffOptions {
    DiffOptions {
        indent_heuristic: false,
        ..DiffOptions::default()
    }
}

/// Merges the changes from `base` to `theirs` into `ours`.
///
/// Fails if one of the three texts is binary (see [`is_binary`]); the
/// caller decides what a binary merge means.
pub fn merge(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    opts: &MergeOptions<'_>,
) -> Result<MergeResult, BinaryContent> {
    if is_binary(base) || is_binary(ours) || is_binary(theirs) {
        return Err(BinaryContent);
    }
    let b = split_lines(base);
    let o = split_lines(ours);
    let t = split_lines(theirs);
    let ours_changes = diff_lines(&b, &o, &opts.diff);
    let theirs_changes = diff_lines(&b, &t, &opts.diff);
    let regions = collect_regions(&ours_changes, &theirs_changes, &o, &t);
    let regions = match opts.style {
        ConflictStyle::Merge => {
            let refined = refine(regions, &o, &t, &opts.diff);
            join_close_conflicts(refined, &o)
        }
        ConflictStyle::ZealousDiff3 => trim_common_ends(regions, &o, &t),
        ConflictStyle::Diff3 => regions,
    };
    let mut w = Writer {
        b: &b,
        o: &o,
        t: &t,
        opts,
        out: Vec::with_capacity(ours.len().max(theirs.len())),
        conflicts: 0,
    };
    w.write(&regions);
    Ok(MergeResult {
        content: w.out,
        conflicts: w.conflicts,
    })
}

/// A range of lines of one text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Range {
    start: usize,
    len: usize,
}

impl Range {
    fn end(self) -> usize {
        self.start + self.len
    }

    fn lines<'x>(self, l: &'x [&'x [u8]]) -> &'x [&'x [u8]] {
        &l[self.start..self.end()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Only our side changed these lines.
    Ours,
    /// Only their side changed these lines.
    Theirs,
    Conflict,
}

/// A changed part of the merge, with its lines in base, ours and theirs.
#[derive(Debug, Clone, Copy)]
struct Region {
    kind: Kind,
    b: Range,
    o: Range,
    t: Range,
}

fn end_of(c: &Change) -> usize {
    c.a_start + c.a_len
}

/// Offset of the lines after `c` on the changed side, relative to the base.
fn shift_after(c: &Change) -> isize {
    (c.b_start + c.b_len) as isize - end_of(c) as isize
}

fn shifted(pos: usize, by: isize) -> usize {
    (pos as isize + by) as usize
}

/// Walks both edit scripts in base order and classifies every change.
///
/// Two changes of the same base lines to the same new lines are no conflict
/// (both sides made the same change). Every other overlap is one, even if
/// both sides end up with the same lines.
fn collect_regions(ours: &[Change], theirs: &[Change], o: &[&[u8]], t: &[&[u8]]) -> Vec<Region> {
    let mut regions = Vec::new();
    // Offsets of ours and theirs relative to the base after the changes
    // handled so far.
    let (mut so, mut st) = (0isize, 0isize);
    let (mut i, mut j) = (0, 0);
    while i < ours.len() || j < theirs.len() {
        let ours_first =
            j == theirs.len() || (i < ours.len() && end_of(&ours[i]) < theirs[j].a_start);
        let theirs_first =
            i == ours.len() || (j < theirs.len() && end_of(&theirs[j]) < ours[i].a_start);
        if ours_first {
            let c = &ours[i];
            regions.push(Region {
                kind: Kind::Ours,
                b: Range {
                    start: c.a_start,
                    len: c.a_len,
                },
                o: Range {
                    start: c.b_start,
                    len: c.b_len,
                },
                t: Range {
                    start: shifted(c.a_start, st),
                    len: c.a_len,
                },
            });
            so = shift_after(c);
            i += 1;
            continue;
        }
        if theirs_first {
            let c = &theirs[j];
            regions.push(Region {
                kind: Kind::Theirs,
                b: Range {
                    start: c.a_start,
                    len: c.a_len,
                },
                o: Range {
                    start: shifted(c.a_start, so),
                    len: c.a_len,
                },
                t: Range {
                    start: c.b_start,
                    len: c.b_len,
                },
            });
            st = shift_after(c);
            j += 1;
            continue;
        }
        // The changes overlap or touch: a conflict that grows as long as
        // further changes of either side reach into it.
        let same_base = ours[i].a_start == theirs[j].a_start && ours[i].a_len == theirs[j].a_len;
        let start = ours[i].a_start.min(theirs[j].a_start);
        let mut end = end_of(&ours[i]).max(end_of(&theirs[j]));
        let (o_start, t_start) = (shifted(start, so), shifted(start, st));
        so = shift_after(&ours[i]);
        st = shift_after(&theirs[j]);
        i += 1;
        j += 1;
        let first = (i, j);
        loop {
            if i < ours.len() && ours[i].a_start <= end {
                end = end.max(end_of(&ours[i]));
                so = shift_after(&ours[i]);
                i += 1;
            } else if j < theirs.len() && theirs[j].a_start <= end {
                end = end.max(end_of(&theirs[j]));
                st = shift_after(&theirs[j]);
                j += 1;
            } else {
                break;
            }
        }
        let r = Region {
            kind: Kind::Conflict,
            b: Range {
                start,
                len: end - start,
            },
            o: Range {
                start: o_start,
                len: shifted(end, so) - o_start,
            },
            t: Range {
                start: t_start,
                len: shifted(end, st) - t_start,
            },
        };
        // Same change on both sides: nothing to do, ours has the lines.
        if (i, j) != first || !same_base || r.o.lines(o) != r.t.lines(t) {
            regions.push(r);
        }
    }
    regions
}

/// `merge` style: diffs the two sides of every conflict and keeps only the
/// differing parts in conflict. The base of such a part is not known; it
/// keeps the base of the whole conflict.
fn refine(regions: Vec<Region>, o: &[&[u8]], t: &[&[u8]], diff: &DiffOptions) -> Vec<Region> {
    let mut out = Vec::with_capacity(regions.len());
    for r in regions {
        if r.kind != Kind::Conflict || (r.o.len == 0) != (r.t.len == 0) {
            out.push(r);
            continue;
        }
        for c in diff_lines(r.o.lines(o), r.t.lines(t), diff) {
            out.push(Region {
                kind: Kind::Conflict,
                b: r.b,
                o: Range {
                    start: r.o.start + c.a_start,
                    len: c.a_len,
                },
                t: Range {
                    start: r.t.start + c.b_start,
                    len: c.b_len,
                },
            });
        }
    }
    out
}

fn has_alnum(lines: &[&[u8]]) -> bool {
    lines
        .iter()
        .any(|l| l.iter().any(|c| c.is_ascii_alphanumeric()))
}

/// `merge` style: joins neighbouring conflicts (with no other change between
/// them) that are close or separated only by lines without letters and
/// digits.
fn join_close_conflicts(regions: Vec<Region>, o: &[&[u8]]) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::with_capacity(regions.len());
    for r in regions {
        if let Some(prev) = out.last_mut()
            && prev.kind == Kind::Conflict
            && r.kind == Kind::Conflict
        {
            let gap = Range {
                start: prev.o.end(),
                len: r.o.start - prev.o.end(),
            };
            if gap.len <= JOIN_DISTANCE || !has_alnum(gap.lines(o)) {
                let b_start = prev.b.start.min(r.b.start);
                let b_end = prev.b.end().max(r.b.end());
                prev.b = Range {
                    start: b_start,
                    len: b_end - b_start,
                };
                prev.o.len = r.o.end() - prev.o.start;
                prev.t.len = r.t.end() - prev.t.start;
                continue;
            }
        }
        out.push(r);
    }
    out
}

/// `zdiff3` style: moves the lines both sides share at the start and the
/// end of a conflict out of it.
fn trim_common_ends(regions: Vec<Region>, o: &[&[u8]], t: &[&[u8]]) -> Vec<Region> {
    regions
        .into_iter()
        .map(|mut r| {
            if r.kind != Kind::Conflict {
                return r;
            }
            let (ol, tl) = (r.o.lines(o), r.t.lines(t));
            let head = ol.iter().zip(tl).take_while(|(a, b)| a == b).count();
            let tail = ol[head..]
                .iter()
                .rev()
                .zip(tl[head..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            r.o = Range {
                start: r.o.start + head,
                len: r.o.len - head - tail,
            };
            r.t = Range {
                start: r.t.start + head,
                len: r.t.len - head - tail,
            };
            r
        })
        .collect()
}

struct Writer<'x, 'o> {
    b: &'x [&'x [u8]],
    o: &'x [&'x [u8]],
    t: &'x [&'x [u8]],
    opts: &'x MergeOptions<'o>,
    out: Vec<u8>,
    conflicts: usize,
}

/// End-of-line style of a line: `Some(true)` for CRLF, `Some(false)` for LF,
/// `None` if the line does not exist or has no line end.
fn crlf_at(lines: &[&[u8]], i: usize) -> Option<bool> {
    let l = lines.get(i)?;
    if !l.ends_with(b"\n") {
        return None;
    }
    Some(l.ends_with(b"\r\n"))
}

impl Writer<'_, '_> {
    fn write(&mut self, regions: &[Region]) {
        // Position in ours up to which the output is written.
        let mut pos = 0;
        for r in regions {
            self.lines(
                self.o,
                Range {
                    start: pos,
                    len: r.o.start - pos,
                },
            );
            match r.kind {
                Kind::Ours => self.lines(self.o, r.o),
                Kind::Theirs => self.lines(self.t, r.t),
                Kind::Conflict => self.conflict(r),
            }
            pos = r.o.end();
        }
        self.lines(
            self.o,
            Range {
                start: pos,
                len: self.o.len() - pos,
            },
        );
    }

    fn lines(&mut self, l: &[&[u8]], r: Range) {
        for line in r.lines(l) {
            self.out.extend_from_slice(line);
        }
    }

    /// Writes the lines of a conflict side and ends the last one if it has
    /// no line end.
    fn section(&mut self, l: &[&[u8]], r: Range, crlf: bool) {
        self.lines(l, r);
        if r.len > 0 && !l[r.end() - 1].ends_with(b"\n") {
            self.eol(crlf);
        }
    }

    fn eol(&mut self, crlf: bool) {
        self.out
            .extend_from_slice(if crlf { b"\r\n" } else { b"\n" });
    }

    fn marker(&mut self, c: u8, label: &[u8], crlf: bool) {
        let size = match self.opts.marker_size {
            0 => DEFAULT_MARKER_SIZE,
            n => n,
        };
        self.out.extend(std::iter::repeat_n(c, size));
        if !label.is_empty() {
            self.out.push(b' ');
            self.out.extend_from_slice(label);
        }
        self.eol(crlf);
    }

    /// Whether markers (and added line ends) of a conflict use CRLF: the
    /// line before the conflict in ours and in theirs (the first line if
    /// the conflict starts the file) and the first line of the base must
    /// not end in LF alone, and the base's first line must end in CRLF.
    fn needs_crlf(&self, r: &Region) -> bool {
        let before = |start: usize| start.saturating_sub(1);
        [
            crlf_at(self.o, before(r.o.start)),
            crlf_at(self.t, before(r.t.start)),
            crlf_at(self.b, 0),
        ]
        .into_iter()
        .try_fold(None, |_, v| match v {
            Some(false) => Err(()),
            v => Ok(v),
        })
        .is_ok_and(|last| last == Some(true))
    }

    fn conflict(&mut self, r: &Region) {
        let crlf = self.needs_crlf(r);
        match self.opts.favor {
            Favor::Ours => return self.lines(self.o, r.o),
            Favor::Theirs => return self.lines(self.t, r.t),
            Favor::Union => {
                self.section(self.o, r.o, crlf);
                return self.lines(self.t, r.t);
            }
            Favor::None => {}
        }
        self.conflicts += 1;
        let opts = self.opts;
        self.marker(b'<', opts.ours_label, crlf);
        self.section(self.o, r.o, crlf);
        if opts.style != ConflictStyle::Merge {
            self.marker(b'|', opts.base_label, crlf);
            self.section(self.b, r.b, crlf);
        }
        self.marker(b'=', b"", crlf);
        self.section(self.t, r.t, crlf);
        self.marker(b'>', opts.theirs_label, crlf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(words: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for w in words.split_whitespace() {
            v.extend_from_slice(w.as_bytes());
            v.push(b'\n');
        }
        v
    }

    fn run(base: &str, ours: &str, theirs: &str, style: ConflictStyle) -> (String, usize) {
        let opts = MergeOptions {
            style,
            ours_label: b"o",
            base_label: b"b",
            theirs_label: b"t",
            diff: merge_diff_options(),
            ..MergeOptions::default()
        };
        let r = merge(&lines(base), &lines(ours), &lines(theirs), &opts).unwrap();
        (
            String::from_utf8(r.content).unwrap().replace('\n', " "),
            r.conflicts,
        )
    }

    #[test]
    fn clean_and_adjacent() {
        assert_eq!(
            run("a b c d", "A b c d", "a b C d", ConflictStyle::Merge).0,
            "A b C d "
        );
        assert_eq!(
            run("a b c d", "A b c d", "a B c d", ConflictStyle::Merge),
            ("<<<<<<< o A b ======= a B >>>>>>> t c d ".into(), 1)
        );
        assert_eq!(
            run("a b c", "a X c", "a X c", ConflictStyle::Diff3),
            ("a X c ".into(), 0)
        );
    }

    #[test]
    fn joins_close_conflicts() {
        assert_eq!(
            run(
                "a b c d e f",
                "A b c d E f",
                "X b c d Y f",
                ConflictStyle::Merge
            )
            .1,
            1
        );
        assert_eq!(
            run(
                "a b c d e f g",
                "A b c d e F g",
                "X b c d e Y g",
                ConflictStyle::Merge
            )
            .1,
            2
        );
        assert_eq!(
            run(
                "a - - - - f g",
                "A - - - - F g",
                "X - - - - Y g",
                ConflictStyle::Merge
            )
            .1,
            1
        );
        assert_eq!(
            run("a b c d", "A b C d", "X b Y d", ConflictStyle::Diff3).1,
            2
        );
    }

    #[test]
    fn zdiff3_trims() {
        assert_eq!(
            run("a b c d", "a X Y d", "a X Z d", ConflictStyle::ZealousDiff3).0,
            "a X <<<<<<< o Y ||||||| b b c ======= Z >>>>>>> t d "
        );
    }

    #[test]
    fn binary() {
        assert_eq!(
            merge(b"a\0", b"", b"", &MergeOptions::default()),
            Err(BinaryContent)
        );
    }
}
