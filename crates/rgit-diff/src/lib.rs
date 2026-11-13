// SPDX-License-Identifier: MIT

//! Line diff with git's default algorithm (Myers), unified patch output and
//! the three-way merge of one text ([`merge`]).
//!
//! The output is meant to be byte-identical to `git diff` with default
//! options. Where the documentation does not specify behaviour (which of
//! several minimal edit scripts is shown, where hunks are placed, how the
//! indent heuristic scores positions), the behaviour was derived from the
//! Myers paper and from differential tests against the `git` binary.

mod compact;
mod indent;
mod lines;
pub mod merge;
pub mod messages;
mod myers;
mod patch;
mod unified;

pub use lines::{BINARY_CHECK_BYTES, is_binary, split_lines};
pub use patch::{
    MODE_SYMLINK, PatchFile, PatchOptions, quote_path, write_file_patch, write_file_patch_with,
};
pub use unified::default_funcname;

/// Options for the line diff and its unified output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOptions {
    /// Number of context lines (`-U<n>`, default 3).
    pub context: usize,
    /// Extra lines between hunks that still join them (`--inter-hunk-context`).
    pub interhunk_context: usize,
    /// Shift hunk boundaries with the indent heuristic (default on).
    pub indent_heuristic: bool,
    /// Always produce a minimal diff (`--minimal`), even when slow.
    pub minimal: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions {
            context: 3,
            interhunk_context: 0,
            indent_heuristic: true,
            minimal: false,
        }
    }
}

/// One change of an edit script: `a_len` lines of the old side starting at
/// `a_start` are replaced by `b_len` lines of the new side starting at
/// `b_start` (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub a_start: usize,
    pub a_len: usize,
    pub b_start: usize,
    pub b_len: usize,
}

/// Number of inserted and deleted lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub insertions: usize,
    pub deletions: usize,
}

/// Computes the edit script between two lists of lines.
pub fn diff_lines(a: &[&[u8]], b: &[&[u8]], opts: &DiffOptions) -> Vec<Change> {
    let (ia, ib, classes) = lines::intern(a, b);
    let (mut ca, mut cb) = myers::diff(&ia, &ib, classes, opts.minimal);
    compact::compact(&ia, a, &mut ca, &ib, b, &mut cb, opts.indent_heuristic);
    changes_from_flags(&ca, &cb)
}

fn changes_from_flags(ca: &[bool], cb: &[bool]) -> Vec<Change> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < ca.len() || j < cb.len() {
        let da = i < ca.len() && ca[i];
        let db = j < cb.len() && cb[j];
        if da || db {
            let (si, sj) = (i, j);
            while i < ca.len() && ca[i] {
                i += 1;
            }
            while j < cb.len() && cb[j] {
                j += 1;
            }
            out.push(Change {
                a_start: si,
                a_len: i - si,
                b_start: sj,
                b_len: j - sj,
            });
        } else {
            i += 1;
            j += 1;
        }
    }
    out
}

/// Size of the blocks in which a common tail is cut off before diffing.
const TAIL_BLOCK: usize = 1024;

/// Without context lines git leaves out a long common tail of both texts
/// before diffing (observed: whole 1024-byte blocks, then up to the end of
/// the line). This changes where ambiguous changes are placed, so it is
/// reproduced here.
fn trim_common_tail<'x>(a: &'x [u8], b: &'x [u8]) -> (&'x [u8], &'x [u8]) {
    let common = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let cut = common / TAIL_BLOCK * TAIL_BLOCK;
    if cut == 0 {
        return (a, b);
    }
    let keep = |x: &'x [u8]| -> &'x [u8] {
        let mut k = x.len() - cut;
        if k > 0 && x[k - 1] != b'\n' {
            k = match x[k..].iter().position(|&c| c == b'\n') {
                Some(p) => k + p + 1,
                None => x.len(),
            };
        }
        &x[..k]
    };
    (keep(a), keep(b))
}

/// Result of [`diff_text`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextDiff {
    /// The hunks in unified format (starting with `@@`), empty if equal.
    pub hunks: Vec<u8>,
    pub stats: Stats,
}

/// Diffs two texts and renders the hunks in unified format.
pub fn diff_text(a: &[u8], b: &[u8], opts: &DiffOptions) -> TextDiff {
    let (a, b) = if opts.context == 0 {
        trim_common_tail(a, b)
    } else {
        (a, b)
    };
    let la = split_lines(a);
    let lb = split_lines(b);
    let changes = diff_lines(&la, &lb, opts);
    let mut stats = Stats::default();
    for c in &changes {
        stats.deletions += c.a_len;
        stats.insertions += c.b_len;
    }
    let mut hunks = Vec::new();
    unified::write_hunks(&la, &lb, &changes, opts, &mut hunks);
    TextDiff { hunks, stats }
}
