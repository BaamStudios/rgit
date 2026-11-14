// SPDX-License-Identifier: MIT

//! Patches for `git apply` and the commands built on it: parsing git and
//! traditional unified diffs ([`parse`]) and applying their hunks to a
//! file's content ([`apply_hunks`]).
//!
//! Written from `Documentation/git-apply.adoc`,
//! `Documentation/diff-generate-patch.adoc` and the behaviour of the
//! reference `git apply`; see the module documentation of [`parse`] and
//! [`apply_hunks`] for the spec gaps that were decided by experiment.

pub mod messages;
mod parse;

pub use parse::{ParseError, ParseOptions, Parsed, parse, unquote_prefix};

/// The kind of a hunk line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineKind {
    #[default]
    Context,
    Remove,
    Add,
}

/// One line of a hunk; `text` keeps its line feed unless the line was
/// marked `\ No newline at end of file`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HunkLine {
    pub kind: LineKind,
    pub text: Vec<u8>,
}

/// One `@@` section of a patch.
#[derive(Debug, Clone, Default)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<HunkLine>,
    /// Context lines before the first change.
    pub leading: usize,
    /// Context lines after the last change.
    pub trailing: usize,
    /// The hunk as it appeared in the patch (for `.rej` files).
    pub raw: Vec<u8>,
}

impl Hunk {
    /// Computes `leading` and `trailing`.
    pub fn finish(&mut self) {
        let is_ctx = |l: &HunkLine| l.kind == LineKind::Context;
        self.leading = self.lines.iter().take_while(|l| is_ctx(l)).count();
        self.trailing = if self.leading == self.lines.len() {
            0
        } else {
            self.lines.iter().rev().take_while(|l| is_ctx(l)).count()
        };
    }

    fn reverse(&mut self) {
        std::mem::swap(&mut self.old_start, &mut self.new_start);
        std::mem::swap(&mut self.old_count, &mut self.new_count);
        for l in &mut self.lines {
            l.kind = match l.kind {
                LineKind::Add => LineKind::Remove,
                LineKind::Remove => LineKind::Add,
                LineKind::Context => LineKind::Context,
            };
        }
    }
}

/// The patch for one file.
#[derive(Debug, Clone, Default)]
pub struct FilePatch {
    /// Name before the change (`None` for a created file).
    pub old_name: Option<Vec<u8>>,
    /// Name after the change (`None` for a deleted file).
    pub new_name: Option<Vec<u8>>,
    /// Whether the file is created; `None` when a traditional patch does
    /// not say (decided when the patch is applied).
    pub is_new: Option<bool>,
    /// Whether the file is deleted; `None` when unknown.
    pub is_delete: Option<bool>,
    pub is_rename: bool,
    pub is_copy: bool,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    /// Similarity (or, for a rewrite, dissimilarity) in percent.
    pub score: Option<u32>,
    pub is_rewrite: bool,
    /// The (possibly abbreviated) object names of the `index` line.
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub hunks: Vec<Hunk>,
    /// A binary patch (not applied yet).
    pub binary: bool,
    /// Whether the patch came with a `diff --git` header.
    pub is_git: bool,
    /// Lines added and deleted.
    pub added: usize,
    pub deleted: usize,
    /// A header followed by garbage instead of hunks: the error to report
    /// when the patch is checked or applied (statistics ignore it).
    pub garbage: Option<String>,
}

impl FilePatch {
    /// Whether the patch changes more than content: creation, deletion,
    /// rename, copy or mode.
    pub fn has_metadata(&self) -> bool {
        self.is_new == Some(true)
            || self.is_delete == Some(true)
            || self.is_rename
            || self.is_copy
            || matches!((self.old_mode, self.new_mode), (Some(a), Some(b)) if a != b)
    }

    /// Turns the patch around (`-R`).
    pub fn reverse(&mut self) {
        std::mem::swap(&mut self.old_name, &mut self.new_name);
        std::mem::swap(&mut self.is_new, &mut self.is_delete);
        std::mem::swap(&mut self.old_mode, &mut self.new_mode);
        std::mem::swap(&mut self.old_oid, &mut self.new_oid);
        std::mem::swap(&mut self.added, &mut self.deleted);
        for h in &mut self.hunks {
            h.reverse();
        }
    }

    /// The name shown in messages: `old => new` for renames and copies,
    /// otherwise the existing name. Quoting is up to `quote`.
    pub fn display_name(&self, quote: impl Fn(&[u8]) -> String) -> String {
        match (&self.old_name, &self.new_name) {
            (Some(o), Some(n)) if o != n => format!("{} => {}", quote(o), quote(n)),
            (_, Some(n)) => quote(n),
            (Some(o), None) => quote(o),
            (None, None) => String::new(),
        }
    }
}

/// Matching settings for [`apply_hunks`].
#[derive(Debug, Clone, Copy)]
pub struct ApplyOptions {
    /// `-C<n>`: context lines that must match; `usize::MAX` keeps all.
    pub min_context: usize,
    /// `--unidiff-zero`.
    pub unidiff_zero: bool,
    /// `--allow-overlap`.
    pub allow_overlap: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            min_context: usize::MAX,
            unidiff_zero: false,
            allow_overlap: false,
        }
    }
}

/// What happened to one hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkResult {
    /// Applied at line `line` (1-based) of the result; `offset` is the
    /// distance from where the patch expected it, `reduced` the context
    /// that was kept when context had to be dropped.
    Applied {
        line: usize,
        offset: i64,
        reduced: Option<(usize, usize)>,
    },
    /// Not applicable; `preimage` holds the lines that were searched for.
    Failed { preimage: Vec<u8> },
}

/// The outcome of [`apply_hunks`]: the new content (failed hunks
/// skipped) and one result per hunk.
#[derive(Debug, Clone)]
pub struct Applied {
    pub content: Vec<u8>,
    pub hunks: Vec<HunkResult>,
}

impl Applied {
    /// Whether every hunk applied.
    pub fn is_clean(&self) -> bool {
        self.hunks
            .iter()
            .all(|h| matches!(h, HunkResult::Applied { .. }))
    }
}

struct Line {
    text: Vec<u8>,
    patched: bool,
}

fn split_image(data: &[u8]) -> Vec<Line> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            out.push(Line {
                text: data[start..=i].to_vec(),
                patched: false,
            });
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(Line {
            text: data[start..].to_vec(),
            patched: false,
        });
    }
    out
}

fn matches_at(img: &[Line], pre: &[&[u8]], at: usize, allow_overlap: bool) -> bool {
    if at + pre.len() > img.len() {
        return false;
    }
    pre.iter()
        .zip(&img[at..at + pre.len()])
        .all(|(p, l)| *p == l.text.as_slice() && (allow_overlap || !l.patched))
}

/// Finds where `pre` matches: at the start only, at the end only, or the
/// position closest to `pos` (looking forward first on ties).
fn find_pos(
    img: &[Line],
    pre: &[&[u8]],
    pos: usize,
    begin: bool,
    end: bool,
    allow_overlap: bool,
) -> Option<usize> {
    if pre.len() > img.len() {
        return None;
    }
    let limit = img.len() - pre.len();
    if begin || end {
        let at = if begin { 0 } else { limit };
        if begin && end && limit != 0 {
            return None;
        }
        return matches_at(img, pre, at, allow_overlap).then_some(at);
    }
    let start = pos.min(limit);
    if matches_at(img, pre, start, allow_overlap) {
        return Some(start);
    }
    let mut d = 1;
    loop {
        let fwd = start + d;
        let back = start.checked_sub(d);
        if fwd > limit && back.is_none() {
            return None;
        }
        if fwd <= limit && matches_at(img, pre, fwd, allow_overlap) {
            return Some(fwd);
        }
        if let Some(b) = back {
            if matches_at(img, pre, b, allow_overlap) {
                return Some(b);
            }
        }
        d += 1;
    }
}

/// Applies `hunks` to `image` in order.
///
/// Matching rules (spec gaps of git-apply.adoc, determined against the
/// reference git):
///
/// - The search starts at the hunk's new start line and takes the
///   nearest match, forward first on a tie.
/// - A hunk starting at old line 0, or at line 1 without
///   `--unidiff-zero`, must match at the start of the file; a hunk
///   without trailing context must match at the end (not with
///   `--unidiff-zero`).
/// - With `-C<n>`, a hunk that does not match drops one line of context
///   from the side that has more (both sides when equal) until both sides
///   are at most `n` lines; dropping leading context lifts the start
///   anchor and moves the expected position up by one line, dropping
///   trailing context lifts the end anchor.
/// - Lines changed by an earlier hunk do not match again unless
///   `allow_overlap` is set.
pub fn apply_hunks(image: &[u8], hunks: &[Hunk], opts: &ApplyOptions) -> Applied {
    let mut img = split_image(image);
    let mut results = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        let pre_all: Vec<&[u8]> = hunk
            .lines
            .iter()
            .filter(|l| l.kind != LineKind::Add)
            .map(|l| l.text.as_slice())
            .collect();
        let post_all: Vec<&[u8]> = hunk
            .lines
            .iter()
            .filter(|l| l.kind != LineKind::Remove)
            .map(|l| l.text.as_slice())
            .collect();
        let mut leading = hunk.leading;
        let mut trailing = hunk.trailing;
        let mut begin = hunk.old_start == 0 || (hunk.old_start == 1 && !opts.unidiff_zero);
        let mut end = !opts.unidiff_zero && hunk.trailing == 0;
        let expected = hunk.new_start.saturating_sub(1) as i64;
        let mut pos = expected;
        let mut reduced = false;
        let result = loop {
            let drop_lead = hunk.leading - leading;
            let drop_trail = hunk.trailing - trailing;
            let pre = &pre_all[drop_lead..pre_all.len() - drop_trail];
            let post = &post_all[drop_lead..post_all.len() - drop_trail];
            let start = usize::try_from(pos.max(0)).unwrap_or(0);
            if let Some(at) = find_pos(&img, pre, start, begin, end, opts.allow_overlap) {
                let new_lines = post.iter().map(|t| Line {
                    text: t.to_vec(),
                    patched: true,
                });
                img.splice(at..at + pre.len(), new_lines);
                break HunkResult::Applied {
                    line: at + 1,
                    offset: at as i64 - pos,
                    reduced: reduced.then_some((leading, trailing)),
                };
            }
            if leading <= opts.min_context && trailing <= opts.min_context {
                break HunkResult::Failed {
                    preimage: pre_all.concat(),
                };
            }
            reduced = true;
            let (cut_lead, cut_trail) = match leading.cmp(&trailing) {
                std::cmp::Ordering::Greater => (true, false),
                std::cmp::Ordering::Less => (false, true),
                std::cmp::Ordering::Equal => (true, true),
            };
            if cut_lead {
                leading -= 1;
                begin = false;
                pos -= 1;
            }
            if cut_trail {
                trailing -= 1;
                end = false;
            }
        };
        results.push(result);
    }
    let content = img.into_iter().flat_map(|l| l.text).collect();
    Applied {
        content,
        hunks: results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(text: &str) -> FilePatch {
        let mut o = ParseOptions::default();
        parse(text.as_bytes(), "t", &mut o)
            .patches
            .unwrap()
            .remove(0)
    }

    #[test]
    fn simple_offset() {
        let p = patch("--- a/f\n+++ b/f\n@@ -3,3 +3,3 @@\n 1\n-2\n+X\n 3\n");
        let r = apply_hunks(b"1\n2\n3\n4\n", &p.hunks, &ApplyOptions::default());
        assert_eq!(r.content, b"1\nX\n3\n4\n");
        assert_eq!(
            r.hunks[0],
            HunkResult::Applied {
                line: 1,
                offset: -2,
                reduced: None
            }
        );
    }

    #[test]
    fn anchors() {
        // No trailing context: must match at the end.
        let p = patch("--- a/f\n+++ b/f\n@@ -1,2 +1,3 @@\n a\n b\n+c\n");
        let r = apply_hunks(b"a\nb\nc\n", &p.hunks, &ApplyOptions::default());
        assert!(!r.is_clean());
        let r = apply_hunks(b"a\nb\n", &p.hunks, &ApplyOptions::default());
        assert_eq!(r.content, b"a\nb\nc\n");
    }

    #[test]
    fn fuzz() {
        let p = patch("--- a/f\n+++ b/f\n@@ -7,7 +7,7 @@\n 7\n Q\n 9\n-10\n+X\n 11\n 12\n 13\n");
        let img: Vec<u8> = (1..=20)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let opts = ApplyOptions {
            min_context: 1,
            ..Default::default()
        };
        let r = apply_hunks(&img, &p.hunks, &opts);
        assert_eq!(
            r.hunks[0],
            HunkResult::Applied {
                line: 9,
                offset: 4,
                reduced: Some((1, 1))
            }
        );
        let opts = ApplyOptions {
            min_context: 2,
            ..Default::default()
        };
        assert!(!apply_hunks(&img, &p.hunks, &opts).is_clean());
    }
}
