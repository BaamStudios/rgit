// SPDX-License-Identifier: MIT
//! The diff of a patch mode as files and hunks: parsing `diff -p` output
//! (plain, and the coloured version shown to the user line by line),
//! splitting a hunk at the context between its changes, reading an edited
//! hunk back, and putting the selected hunks of a file together into a
//! patch for `apply`.
//!
//! Hunk numbers follow the unified diff format: a split part starts at
//! the old line its first line has in the file, and at the new line that
//! all earlier parts of the file being applied would give it. When the
//! selected hunks are put together, the new start lines are recomputed
//! from the hunks actually selected, and selected parts of a split hunk
//! that share context lines are joined again.

/// One line of the diff: as `apply` sees it and as it is shown.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub(crate) plain: Vec<u8>,
    pub(crate) shown: Vec<u8>,
}

impl Line {
    fn same(text: Vec<u8>) -> Line {
        Line {
            shown: text.clone(),
            plain: text,
        }
    }

    /// The first byte of the plain line (`' '`, `-`, `+`, `\`).
    pub(crate) fn kind(&self) -> u8 {
        self.plain.first().copied().unwrap_or(b' ')
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Decision {
    Undecided,
    Use,
    Skip,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HunkKind {
    /// The `old mode`/`new mode` lines of the header.
    Mode,
    Text,
}

#[derive(Clone, Debug)]
pub(crate) struct Hunk {
    pub(crate) kind: HunkKind,
    pub(crate) old_start: usize,
    pub(crate) old_count: usize,
    pub(crate) new_start: usize,
    pub(crate) new_count: usize,
    /// The header line from the diff; `None` when it is generated (split
    /// or edited hunks).
    pub(crate) header: Option<Line>,
    pub(crate) lines: Vec<Line>,
    pub(crate) decision: Decision,
    /// Edited by the user: shown coloured by line type.
    pub(crate) edited: bool,
}

impl Hunk {
    /// `@@ -a,b +c,d @@` (a count of one is left out).
    pub(crate) fn range_text(&self) -> String {
        let part = |start: usize, count: usize| {
            if count == 1 {
                format!("{start}")
            } else {
                format!("{start},{count}")
            }
        };
        format!(
            "-{} +{}",
            part(self.old_start, self.old_count),
            part(self.new_start, self.new_count)
        )
    }

    pub(crate) fn generated_header(&self) -> String {
        format!("@@ {} @@\n", self.range_text())
    }

    /// The header as `apply` gets it.
    fn plain_header(&self) -> Vec<u8> {
        match &self.header {
            Some(h) if !self.edited => h.plain.clone(),
            _ => self.generated_header().into_bytes(),
        }
    }

    /// Whether the hunk has changes separated by context lines.
    pub(crate) fn can_split(&self) -> bool {
        self.kind == HunkKind::Text
            && segments(&self.lines).iter().filter(|s| !s.context).count() > 1
    }
}

#[derive(Debug)]
pub(crate) struct FileDiff {
    pub(crate) header: Vec<Line>,
    /// The `old mode`/`new mode` lines (also part of `hunks` as the first
    /// hunk when present).
    pub(crate) mode_lines: Vec<Line>,
    pub(crate) hunks: Vec<Hunk>,
    pub(crate) deletion: bool,
    pub(crate) addition: bool,
    pub(crate) binary: bool,
}

/// Parses the diff into files; `shown` has one line per line of `plain`.
pub(crate) fn parse(plain: &[u8], shown: Option<&[u8]>) -> Vec<FileDiff> {
    let plain_lines = split_lines(plain);
    let shown_lines = shown.map(split_lines);
    let line_at = |i: usize| -> Line {
        let p = plain_lines[i].to_vec();
        let s = shown_lines
            .as_ref()
            .and_then(|s| s.get(i))
            .map(|s| s.to_vec())
            .unwrap_or_else(|| p.clone());
        Line { plain: p, shown: s }
    };
    let mut files: Vec<FileDiff> = Vec::new();
    // Remaining old and new lines of the hunk being read.
    let mut remaining: Option<(usize, usize)> = None;
    for i in 0..plain_lines.len() {
        let text = plain_lines[i];
        if let Some((old, new)) = remaining {
            let Some(file) = files.last_mut() else { break };
            let hunk = file.hunks.last_mut().expect("hunk");
            let kind = text.first().copied().unwrap_or(b'\n');
            let mut line = line_at(i);
            let (old, new) = match kind {
                b'\\' => (old, new),
                b'-' => (old.saturating_sub(1), new),
                b'+' => (old, new.saturating_sub(1)),
                b' ' => (old.saturating_sub(1), new.saturating_sub(1)),
                b'\n' | b'\r' => {
                    // An empty context line (diff.suppressBlankEmpty).
                    line.plain = {
                        let mut v = vec![b' '];
                        v.extend_from_slice(text);
                        v
                    };
                    (old.saturating_sub(1), new.saturating_sub(1))
                }
                _ => {
                    remaining = None;
                    continue_outside(&mut files, text, &line_at(i), &mut remaining);
                    continue;
                }
            };
            hunk.lines.push(line);
            remaining = if old == 0 && new == 0 {
                // A "\ No newline" line may still follow.
                match plain_lines.get(i + 1) {
                    Some(next) if next.first() == Some(&b'\\') => Some((0, 0)),
                    _ => None,
                }
            } else {
                Some((old, new))
            };
            continue;
        }
        continue_outside(&mut files, text, &line_at(i), &mut remaining);
    }
    for f in &mut files {
        if f.deletion || f.addition {
            if !f.hunks.iter().any(|h| h.kind == HunkKind::Text) {
                // An empty file: one hunk without lines.
                f.hunks.push(Hunk {
                    kind: HunkKind::Text,
                    old_start: 0,
                    old_count: 0,
                    new_start: 0,
                    new_count: 0,
                    header: None,
                    lines: Vec::new(),
                    decision: Decision::Undecided,
                    edited: false,
                });
            }
        }
    }
    files
}

/// A line outside of a hunk: a new file, a header line or a hunk header.
fn continue_outside(
    files: &mut Vec<FileDiff>,
    text: &[u8],
    line: &Line,
    remaining: &mut Option<(usize, usize)>,
) {
    if text.starts_with(b"diff ") {
        files.push(FileDiff {
            header: vec![line.clone()],
            mode_lines: Vec::new(),
            hunks: Vec::new(),
            deletion: false,
            addition: false,
            binary: false,
        });
        return;
    }
    let Some(file) = files.last_mut() else {
        return;
    };
    if text.starts_with(b"@@ ") {
        if let Some((os, oc, ns, nc)) = parse_header(text) {
            file.hunks.push(Hunk {
                kind: HunkKind::Text,
                old_start: os,
                old_count: oc,
                new_start: ns,
                new_count: nc,
                header: Some(line.clone()),
                lines: Vec::new(),
                decision: Decision::Undecided,
                edited: false,
            });
            *remaining = if oc == 0 && nc == 0 {
                None
            } else {
                Some((oc, nc))
            };
        }
        return;
    }
    if !file.hunks.iter().any(|h| h.kind == HunkKind::Text) {
        if text.starts_with(b"old mode ") || text.starts_with(b"new mode ") {
            file.mode_lines.push(line.clone());
            if file.hunks.is_empty() {
                file.hunks.push(Hunk {
                    kind: HunkKind::Mode,
                    old_start: 0,
                    old_count: 0,
                    new_start: 0,
                    new_count: 0,
                    header: None,
                    lines: Vec::new(),
                    decision: Decision::Undecided,
                    edited: false,
                });
            }
            file.hunks[0].lines.push(line.clone());
            return;
        }
        if text.starts_with(b"deleted file mode ") {
            file.deletion = true;
        } else if text.starts_with(b"new file mode ") {
            file.addition = true;
        } else if text.starts_with(b"Binary files ") || text.starts_with(b"GIT binary patch") {
            file.binary = true;
        }
        file.header.push(line.clone());
    }
}

/// `@@ -a[,b] +c[,d] @@...` → (a, b, c, d).
pub(crate) fn parse_header(text: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let rest = text.strip_prefix(b"@@ -")?;
    let end = rest.windows(3).position(|w| w == b" @@")?;
    let ranges = std::str::from_utf8(&rest[..end]).ok()?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |r: &str| -> Option<(usize, usize)> {
        match r.split_once(',') {
            Some((s, c)) => Some((s.parse().ok()?, c.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (os, oc) = range(old)?;
    let (ns, nc) = range(new)?;
    Some((os, oc, ns, nc))
}

/// The lines of `data`, each with its line feed.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in data.iter().enumerate() {
        if *b == b'\n' {
            out.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// A run of context lines or of changed lines (a `\` line belongs to the
/// run of the line before it).
struct Segment {
    context: bool,
    begin: usize,
    end: usize,
}

fn segments(lines: &[Line]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let k = l.kind();
        if k == b'\\' {
            if let Some(last) = out.last_mut() {
                last.end = i + 1;
                continue;
            }
        }
        let context = k == b' ';
        match out.last_mut() {
            Some(last) if last.context == context => last.end = i + 1,
            _ => out.push(Segment {
                context,
                begin: i,
                end: i + 1,
            }),
        }
    }
    out
}

fn counts(lines: &[Line]) -> (usize, usize) {
    let mut old = 0;
    let mut new = 0;
    for l in lines {
        match l.kind() {
            b' ' => {
                old += 1;
                new += 1;
            }
            b'-' => old += 1,
            b'+' => new += 1,
            _ => {}
        }
    }
    (old, new)
}

/// Splits `h` at the context lines between its changes; each part keeps
/// the context before and after its changes (shared with its neighbours).
pub(crate) fn split(h: &Hunk) -> Vec<Hunk> {
    let segs = segments(&h.lines);
    let mut parts = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        if s.context {
            continue;
        }
        let begin = match i.checked_sub(1).map(|p| &segs[p]) {
            Some(prev) if prev.context => prev.begin,
            _ => s.begin,
        };
        let end = match segs.get(i + 1) {
            Some(next) if next.context => next.end,
            _ => s.end,
        };
        let (old_before, new_before) = counts(&h.lines[..begin]);
        let lines = h.lines[begin..end].to_vec();
        let (oc, nc) = counts(&lines);
        parts.push(Hunk {
            kind: HunkKind::Text,
            old_start: h.old_start + old_before,
            old_count: oc,
            new_start: h.new_start + new_before,
            new_count: nc,
            header: None,
            lines,
            decision: Decision::Undecided,
            edited: h.edited,
        });
    }
    parts
}

/// The result of reading back an edited hunk.
pub(crate) enum Edited {
    /// Nothing left: the edit is aborted.
    Empty,
    /// Not a usable hunk: what is left of the text, for `apply` to
    /// complain about.
    Garbage(Hunk),
    Hunk(Hunk),
}

/// Reads an edited hunk: comment lines are dropped, a missing header
/// keeps the start lines of `orig`, the counts are recomputed, and an
/// empty line is an empty context line. The hunk ends at the first line
/// that is not part of a hunk.
pub(crate) fn read_edited(text: &[u8], comment: &[u8], orig: &Hunk) -> Edited {
    let lines: Vec<&[u8]> = split_lines(text)
        .into_iter()
        .filter(|l| !l.starts_with(comment))
        .collect();
    if lines.iter().all(|l| l.iter().all(u8::is_ascii_whitespace)) {
        return Edited::Empty;
    }
    let mut it = lines.into_iter().peekable();
    let (mut old_start, mut new_start) = (orig.old_start, orig.new_start);
    if let Some(first) = it.peek() {
        if first.starts_with(b"@@ ") {
            if let Some((os, _, ns, _)) = parse_header(first) {
                old_start = os;
                new_start = ns;
            }
            it.next();
        }
    }
    let mut body = Vec::new();
    for l in it {
        let mut l = l.to_vec();
        if !l.ends_with(b"\n") {
            l.push(b'\n');
        }
        match l[0] {
            b' ' | b'-' | b'+' | b'\\' => {}
            b'\n' => l.insert(0, b' '),
            _ => break,
        }
        body.push(Line::same(l));
    }
    if body.is_empty() {
        // Keep the text under the old header; `apply` rejects it.
        let rest: Vec<Line> = split_lines(text)
            .into_iter()
            .filter(|l| !l.starts_with(comment) && !l.starts_with(b"@@ "))
            .map(|l| Line::same(l.to_vec()))
            .collect();
        return Edited::Garbage(Hunk {
            kind: HunkKind::Text,
            lines: rest,
            edited: true,
            decision: Decision::Use,
            ..orig.clone()
        });
    }
    let (oc, nc) = counts(&body);
    Edited::Hunk(Hunk {
        kind: HunkKind::Text,
        old_start,
        old_count: oc,
        new_start,
        new_count: nc,
        header: None,
        lines: body,
        decision: Decision::Use,
        edited: true,
    })
}

/// The header of `file` for `apply`: the mode lines only when `with_mode`.
fn file_header(file: &FileDiff, with_mode: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, l) in file.header.iter().enumerate() {
        out.extend_from_slice(&l.plain);
        if i == 0 && with_mode {
            for m in &file.mode_lines {
                out.extend_from_slice(&m.plain);
            }
        }
    }
    out
}

/// Joins `b` into `a` when they share context lines.
fn join(a: &mut Hunk, b: &Hunk) -> bool {
    let a_end = a.old_start + a.old_count;
    if b.old_start >= a_end || b.old_start < a.old_start {
        return false;
    }
    let overlap = a_end - b.old_start;
    // The shared lines are context at the end of `a` and the start of `b`.
    let tail_ok = a.lines.len() >= overlap
        && a.lines[a.lines.len() - overlap..]
            .iter()
            .all(|l| l.kind() == b' ');
    let head_ok = b.lines.len() >= overlap && b.lines[..overlap].iter().all(|l| l.kind() == b' ');
    if !tail_ok || !head_ok {
        return false;
    }
    a.lines.extend(b.lines[overlap..].iter().cloned());
    let (oc, nc) = counts(&a.lines);
    a.old_count = oc;
    a.new_count = nc;
    a.edited = true;
    true
}

/// The patch applying the hunks of `file` that `pick` selects; `None`
/// when nothing is selected. With `reverse` the patch is applied in
/// reverse: then the new side is where the hunks are, and the old start
/// lines are recomputed instead of the new ones.
pub(crate) fn file_patch(
    file: &FileDiff,
    pick: &dyn Fn(&Hunk) -> bool,
    reverse: bool,
) -> Option<Vec<u8>> {
    let with_mode = file
        .hunks
        .iter()
        .any(|h| h.kind == HunkKind::Mode && pick(h));
    let mut any = with_mode;
    let mut joined: Vec<Hunk> = Vec::new();
    for h in file.hunks.iter().filter(|h| h.kind == HunkKind::Text) {
        if !pick(h) {
            continue;
        }
        any = true;
        if h.lines.is_empty() {
            continue;
        }
        if let Some(last) = joined.last_mut() {
            if join(last, h) {
                continue;
            }
        }
        joined.push(h.clone());
    }
    if !any {
        return None;
    }
    let mut out = file_header(file, with_mode);
    let mut delta: isize = 0;
    for h in &joined {
        let mut h = h.clone();
        // An insertion names the line it follows, a removal of
        // everything the line before it.
        let (fixed, fixed_count, other_count) = if reverse {
            (h.new_start, h.new_count, h.old_count)
        } else {
            (h.old_start, h.old_count, h.new_count)
        };
        let d = if reverse { -delta } else { delta };
        let mut start = fixed as isize + d;
        if fixed_count == 0 {
            start += 1;
        }
        if other_count == 0 {
            start -= 1;
        }
        let start = start.max(0) as usize;
        let slot = if reverse {
            &mut h.old_start
        } else {
            &mut h.new_start
        };
        if *slot != start {
            *slot = start;
            h.edited = true;
        }
        out.extend_from_slice(&h.plain_header());
        for l in &h.lines {
            out.extend_from_slice(&l.plain);
        }
        delta += h.new_count as isize - h.old_count as isize;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(text: &str) -> Hunk {
        let files = parse(text.as_bytes(), None);
        files[0].hunks[0].clone()
    }

    #[test]
    fn split_shares_context() {
        let h = hunk(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,5 @@\n-A\n+Z\n B\n+Y\n C\n-D\n+X\n",
        );
        assert!(h.can_split());
        let parts = split(&h);
        let ranges: Vec<String> = parts.iter().map(Hunk::range_text).collect();
        assert_eq!(ranges, ["-1,2 +1,2", "-2,2 +2,3", "-3,2 +4,2"]);
    }

    #[test]
    fn split_parts_join_again() {
        let text = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,4 @@\n+first\n a\n b\n+last\n";
        let mut files = parse(text.as_bytes(), None);
        let parts = split(&files[0].hunks[0]);
        assert_eq!(parts[1].range_text(), "-1,2 +2,3");
        files[0].hunks = parts;
        let patch = file_patch(&files[0], &|_| true, false).unwrap();
        let patch = String::from_utf8(patch).unwrap();
        assert!(
            patch.ends_with("@@ -1,2 +1,4 @@\n+first\n a\n b\n+last\n"),
            "{patch}"
        );
        let only_second = file_patch(&files[0], &|h| h.new_start == 2, false).unwrap();
        let only_second = String::from_utf8(only_second).unwrap();
        assert!(
            only_second.ends_with("@@ -1,2 +1,3 @@\n a\n b\n+last\n"),
            "{only_second}"
        );
    }

    #[test]
    fn mode_change_is_a_hunk() {
        let text = "diff --git a/m b/m\nold mode 100644\nnew mode 100755\nindex 1..2\n--- a/m\n+++ b/m\n@@ -1 +1,2 @@\n a\n+b\n";
        let files = parse(text.as_bytes(), None);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].kind, HunkKind::Mode);
        let patch = file_patch(&files[0], &|h| h.kind == HunkKind::Text, false).unwrap();
        assert!(!String::from_utf8(patch).unwrap().contains("mode"));
    }

    #[test]
    fn edited_hunk_is_recounted() {
        let orig = hunk("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -3,2 +3,3 @@\n a\n+b\n c\n");
        let Edited::Hunk(h) = read_edited(b"# x\n+b\n a\n\nzzz\n+q\n", b"#", &orig) else {
            panic!("not a hunk");
        };
        assert_eq!(h.range_text(), "-3,2 +3,3");
        assert!(matches!(
            read_edited(b"# only\n", b"#", &orig),
            Edited::Empty
        ));
        assert!(matches!(
            read_edited(b"garbage\n", b"#", &orig),
            Edited::Garbage(_)
        ));
    }
}
