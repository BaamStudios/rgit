// SPDX-License-Identifier: MIT

//! Whitespace-insensitive line comparison (`-w`, `-b`,
//! `--ignore-space-at-eol`, `--ignore-cr-at-eol`) and ignorable changes
//! (`--ignore-blank-lines`, `-I<regex>`), as described in
//! Documentation/diff-options.adoc.
//!
//! Details that the documentation leaves open were taken from the
//! behaviour of the `git` binary:
//!
//! * whitespace is space, tab, carriage return and line feed (vertical
//!   tab and form feed count as text);
//! * with any of the whitespace options a missing newline at the end of
//!   the file is not a difference; `--ignore-cr-at-eol` only drops a
//!   carriage return directly before the line feed;
//! * a line is blank when it is empty, or, with a whitespace option, when
//!   it consists of whitespace only;
//! * `-I` matches each line with its line feed, where `^` and `$` also
//!   match around line feeds (so `^$` matches every line); a change is ignorable
//!   when every removed and added line matches one of the expressions (or
//!   every line is blank with `--ignore-blank-lines`);
//! * an ignorable change is shown when fewer than `context` unchanged
//!   lines separate it from a shown change (this repeats, so a run of
//!   close ignorable changes is shown), and when it lies inside a hunk
//!   that joins shown changes.

use crate::{Change, DiffOptions};

/// Which whitespace differences are ignored when comparing lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Whitespace {
    /// `-w`: all whitespace.
    pub all: bool,
    /// `-b`: changes in the amount of whitespace.
    pub change: bool,
    /// `--ignore-space-at-eol`.
    pub at_eol: bool,
    /// `--ignore-cr-at-eol`.
    pub cr_at_eol: bool,
}

impl Whitespace {
    /// Whether any option is set.
    pub fn any(&self) -> bool {
        self.all || self.change || self.at_eol || self.cr_at_eol
    }
}

/// The expressions of `-I<regex>`.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRegexes(pub Vec<regex::bytes::Regex>);

impl PartialEq for IgnoreRegexes {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(&other.0)
                .all(|(a, b)| a.as_str() == b.as_str())
    }
}

impl Eq for IgnoreRegexes {}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

/// The key a line is compared by, or `None` when the line is compared
/// as it is.
pub(crate) fn key(line: &[u8], ws: &Whitespace) -> Option<Vec<u8>> {
    if !ws.any() {
        return None;
    }
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    if ws.all {
        return Some(body.iter().copied().filter(|&c| !is_space(c)).collect());
    }
    if ws.change {
        let mut out = Vec::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            if is_space(body[i]) {
                while i < body.len() && is_space(body[i]) {
                    i += 1;
                }
                if i < body.len() {
                    out.push(b' ');
                }
            } else {
                out.push(body[i]);
                i += 1;
            }
        }
        return Some(out);
    }
    if ws.at_eol {
        let end = body.len() - body.iter().rev().take_while(|&&c| is_space(c)).count();
        return Some(body[..end].to_vec());
    }
    // `--ignore-cr-at-eol`: only a carriage return before the line feed.
    let body = if line.ends_with(b"\r\n") {
        &line[..line.len() - 2]
    } else {
        body
    };
    Some(body.to_vec())
}

/// Whether `line` is blank.
fn is_blank(line: &[u8], ws: &Whitespace) -> bool {
    if ws.any() {
        line.iter().all(|&c| is_space(c))
    } else {
        line.is_empty() || line == b"\n"
    }
}

/// Whether every line of a change is ignorable.
fn change_ignorable(a: &[&[u8]], b: &[&[u8]], c: &Change, opts: &DiffOptions) -> bool {
    let lines = a[c.a_start..c.a_start + c.a_len]
        .iter()
        .chain(&b[c.b_start..c.b_start + c.b_len]);
    if opts.ignore_blank_lines && lines.clone().all(|l| is_blank(l, &opts.whitespace)) {
        return true;
    }
    let res = &opts.ignore_regex.0;
    !res.is_empty() && lines.clone().all(|l| res.iter().any(|r| r.is_match(l)))
}

/// Which changes are shown before grouping into hunks (see the module
/// documentation). All of them unless changes can be ignored.
pub(crate) fn shown_changes(
    a: &[&[u8]],
    b: &[&[u8]],
    changes: &[Change],
    opts: &DiffOptions,
) -> Vec<bool> {
    if !opts.ignore_blank_lines && opts.ignore_regex.0.is_empty() {
        return vec![true; changes.len()];
    }
    let mut shown: Vec<bool> = changes
        .iter()
        .map(|c| !change_ignorable(a, b, c, opts))
        .collect();
    let gap = |i: usize| changes[i + 1].a_start - (changes[i].a_start + changes[i].a_len);
    loop {
        let mut grew = false;
        for i in 0..changes.len().saturating_sub(1) {
            if shown[i] != shown[i + 1] && gap(i) < opts.context {
                shown[i] = true;
                shown[i + 1] = true;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(line: &str, ws: Whitespace) -> String {
        String::from_utf8(key(line.as_bytes(), &ws).unwrap()).unwrap()
    }

    #[test]
    fn keys() {
        let all = Whitespace {
            all: true,
            ..Default::default()
        };
        let change = Whitespace {
            change: true,
            ..Default::default()
        };
        let eol = Whitespace {
            at_eol: true,
            ..Default::default()
        };
        let cr = Whitespace {
            cr_at_eol: true,
            ..Default::default()
        };
        assert_eq!(k(" a \tb\r\n", all), "ab");
        assert_eq!(k("a\x0bb\n", all), "a\x0bb");
        assert_eq!(k(" a \t b \n", change), " a b");
        assert_eq!(k(" a  b \r\n", eol), " a  b");
        assert_eq!(k("b \r\n", cr), "b ");
        assert_eq!(k("b\r", cr), "b\r");
        assert_eq!(k("b", cr), "b");
        assert!(key(b"x", &Whitespace::default()).is_none());
    }

    #[test]
    fn ignorable_changes_near_shown_ones() {
        let opts = DiffOptions {
            ignore_blank_lines: true,
            ..DiffOptions::default()
        };
        let a: Vec<&[u8]> = vec![b"1\n", b"2\n", b"3\n", b"4\n", b"5\n", b"6\n", b"7\n"];
        let b: Vec<&[u8]> = vec![
            b"1\n", b"\n", b"2\n", b"X\n", b"4\n", b"5\n", b"6\n", b"\n", b"7\n",
        ];
        let changes = [
            Change {
                a_start: 1,
                a_len: 0,
                b_start: 1,
                b_len: 1,
            },
            Change {
                a_start: 2,
                a_len: 1,
                b_start: 3,
                b_len: 1,
            },
            Change {
                a_start: 6,
                a_len: 0,
                b_start: 7,
                b_len: 1,
            },
        ];
        // One line before the change, three after it.
        assert_eq!(
            shown_changes(&a, &b, &changes, &opts),
            vec![true, true, false]
        );
    }
}
