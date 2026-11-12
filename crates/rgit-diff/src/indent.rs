// SPDX-License-Identifier: MIT

//! Indent heuristic: choosing where an ambiguous group of changed lines goes.
//!
//! git documents this only as "shift diff hunk boundaries to make patches
//! easier to read" (`--indent-heuristic`, on by default). Everything below was
//! derived from black-box experiments against the `git` binary: about 100,000
//! generated slider cases (a block of lines that can be placed at several
//! positions), with the candidate positions and git's choice recorded, plus
//! targeted probes. The structure (what is measured around a split, how two
//! positions are compared, the search window) was found by testing hypotheses
//! against that data; the weights were obtained by integer programming as one
//! point of the feasible region and reproduce every recorded case.
//!
//! Model:
//! * A candidate position of a group `[start, end)` is scored at its two
//!   splits (before `start` and before `end`). For each split we measure the
//!   line at the split, the blank lines before it and the indentation of the
//!   nearest non-blank line above, and the blank lines after the line at the
//!   split plus the indentation of the next non-blank line below.
//! * Each split gets a penalty from those measurements, and an "effective
//!   indent" (the indent of the line at the split, or of the next non-blank
//!   line if it is blank).
//! * Two candidates are compared by `WEIGHT * sign(indent difference) +
//!   penalty difference`. Candidates are visited from the top; a candidate
//!   replaces the best so far if it compares lower or equal.
//! * Only positions at most `size + 1` lines (and at most 100 lines) above
//!   the lowest position are considered.

/// Largest distance above the lowest position that is considered.
const MAX_SLIDE: usize = 100;

/// Blank lines scanned at most in either direction from a split.
const MAX_BLANKS: usize = 20;

const WEIGHT_INDENT: i64 = 82;
const START_OF_FILE: i64 = 4;
const END_OF_FILE: i64 = -5;
const TOTAL_BLANK: i64 = -41;
const POST_BLANK: i64 = 8;
const INDENTED: i64 = -6;
const INDENTED_BLANK: i64 = 13;
const OUTDENTED: i64 = 32;
const OUTDENTED_BLANK: i64 = 22;
const DEDENTED: i64 = 31;
const DEDENTED_BLANK: i64 = 22;

fn is_blank(line: &[u8]) -> bool {
    line.iter()
        .all(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
}

/// Indentation width (tabs advance to the next multiple of 8), or `None` for
/// a blank line.
fn indent_of(line: &[u8]) -> Option<i64> {
    let mut col = 0i64;
    for &c in line {
        match c {
            b' ' => col += 1,
            b'\t' => col += 8 - col % 8,
            _ => break,
        }
    }
    if is_blank(line) { None } else { Some(col) }
}

#[derive(Debug, Clone, Copy)]
struct Split {
    at_eof: bool,
    /// Indent of the line at the split, -1 if blank or at the end.
    line_indent: i64,
    line_blank: bool,
    pre_blank: i64,
    pre_indent: i64,
    post_blank: i64,
    post_indent: i64,
}

fn measure(lines: &[&[u8]], s: usize) -> Split {
    let n = lines.len();
    let at_eof = s >= n;
    let (line_indent, line_blank) = if at_eof {
        (-1, false)
    } else {
        match indent_of(lines[s]) {
            Some(i) => (i, false),
            None => (-1, true),
        }
    };
    let mut pre_blank = 0usize;
    let mut pre_indent = -1;
    let mut i = s;
    while i > 0 {
        if pre_blank >= MAX_BLANKS {
            pre_indent = PRE_CAP_INDENT;
            break;
        }
        match indent_of(lines[i - 1]) {
            None => {
                pre_blank += 1;
                i -= 1;
            }
            Some(ind) => {
                pre_indent = ind;
                break;
            }
        }
    }
    let mut post_blank = 0usize;
    let mut post_indent = -1;
    let mut j = s + 1;
    while j < n {
        if post_blank >= MAX_BLANKS {
            post_indent = 0;
            break;
        }
        match indent_of(lines[j]) {
            None => {
                post_blank += 1;
                j += 1;
            }
            Some(ind) => {
                post_indent = ind;
                break;
            }
        }
    }
    Split {
        at_eof,
        line_indent,
        line_blank,
        pre_blank: pre_blank as i64,
        pre_indent,
        post_blank: post_blank as i64,
        post_indent,
    }
}

/// Indent assumed for the line above a split when only blank lines were
/// found within [`MAX_BLANKS`].
const PRE_CAP_INDENT: i64 = -1;

/// Returns `(penalty, effective indent)` of a split.
fn score_split(m: &Split) -> (i64, i64) {
    let mut p = 0;
    if m.pre_indent == -1 && m.pre_blank == 0 {
        p += START_OF_FILE;
    }
    if m.at_eof || (m.line_blank && m.post_indent == -1) {
        p += END_OF_FILE;
    }
    let post_blank = if m.line_blank { 1 + m.post_blank } else { 0 };
    let total_blank = m.pre_blank + post_blank;
    p += TOTAL_BLANK * total_blank + POST_BLANK * post_blank;
    let indent = if m.line_indent != -1 {
        m.line_indent
    } else {
        m.post_indent
    };
    let blanks = total_blank != 0;
    if indent != -1 && m.pre_indent != -1 && indent != m.pre_indent {
        p += if indent > m.pre_indent {
            if blanks { INDENTED_BLANK } else { INDENTED }
        } else if m.post_indent != -1 && m.post_indent > indent {
            if blanks { OUTDENTED_BLANK } else { OUTDENTED }
        } else if blanks {
            DEDENTED_BLANK
        } else {
            DEDENTED
        };
    }
    (p, indent)
}

fn score(lines: &[&[u8]], start: usize, end: usize) -> (i64, i64) {
    let (p1, i1) = score_split(&measure(lines, start));
    let (p2, i2) = score_split(&measure(lines, end));
    (p1 + p2, i1 + i2)
}

/// Compares two candidates: negative or zero if `a` is at least as good.
fn compare(a: (i64, i64), b: (i64, i64)) -> i64 {
    WEIGHT_INDENT * (a.1 - b.1).signum() + (a.0 - b.0)
}

/// Chooses the end of the group `[start, end)` (currently at its lowest
/// position; `earliest_end` is the end at its highest position).
pub(crate) fn best_end(lines: &[&[u8]], start: usize, end: usize, earliest_end: usize) -> usize {
    let size = end - start;
    let first = earliest_end.max(end.saturating_sub((size + 1).min(MAX_SLIDE)));
    let mut best: Option<(usize, (i64, i64))> = None;
    for e in first..=end {
        let s = score(lines, e - size, e);
        match best {
            Some((_, b)) if compare(s, b) > 0 => {}
            _ => best = Some((e, s)),
        }
    }
    best.map(|b| b.0).unwrap_or(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation() {
        assert_eq!(indent_of(b"  x\n"), Some(2));
        assert_eq!(indent_of(b"\tx\n"), Some(8));
        assert_eq!(indent_of(b"  \tx\n"), Some(8));
        assert_eq!(indent_of(b" \t\n"), None);
        assert_eq!(indent_of(b"\n"), None);
    }
}
