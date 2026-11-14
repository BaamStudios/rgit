// SPDX-License-Identifier: MIT
//! diffcore-pickaxe: keep the file pairs that add or remove a string
//! (`-S`), whose added or removed lines match a regular expression
//! (`-G`), or that add or remove an object (`--find-object`).
//!
//! Matching is supplied by the caller, so this crate does not depend on a
//! regular expression engine.

use rgit_hash::ObjectId;

use super::{Contents, FilePair, Side, Status};
use crate::{DiffOptions, diff_lines, is_binary, split_lines};

/// What to look for.
pub enum Criterion<'a> {
    /// `-S`: the number of occurrences in a text differs between the
    /// sides (`count` returns the number of non-overlapping matches).
    Count(&'a dyn Fn(&[u8]) -> usize),
    /// `-G`: an added or removed line matches (`matches` gets the line
    /// without its newline). Binary files are skipped unless `text`.
    Grep {
        matches: &'a dyn Fn(&[u8]) -> bool,
        text: bool,
    },
    /// `--find-object`: a side is any of these objects (and the pair is
    /// a change: a rename that keeps the object counts).
    Objects(&'a [ObjectId]),
}

fn data<T>(contents: &dyn Contents<T>, side: Option<&Side<T>>) -> Option<Vec<u8>> {
    side.map(|s| contents.read(s))
}

/// Whether a pair matches.
fn matches<T>(p: &FilePair<T>, how: &Criterion<'_>, contents: &dyn Contents<T>) -> bool {
    if p.status == Status::Unmerged {
        return false;
    }
    fn blob<T>(s: &Option<Side<T>>) -> Option<&Side<T>> {
        s.as_ref().filter(|s| super::is_blob_mode(s.mode))
    }
    match how {
        Criterion::Objects(ids) => {
            let one = p.one.as_ref().map(|s| s.oid);
            let two = p.two.as_ref().map(|s| s.oid);
            ids.iter()
                .any(|id| one.as_ref() == Some(id) || two.as_ref() == Some(id))
        }
        Criterion::Count(count) => {
            let a = data(contents, blob(&p.one)).map_or(0, |d| count(&d));
            let b = data(contents, blob(&p.two)).map_or(0, |d| count(&d));
            a != b
        }
        Criterion::Grep { matches, text } => {
            let a = data(contents, blob(&p.one)).unwrap_or_default();
            let b = data(contents, blob(&p.two)).unwrap_or_default();
            if !text && (is_binary(&a) || is_binary(&b)) {
                return false;
            }
            if a == b {
                return false;
            }
            let la = split_lines(&a);
            let lb = split_lines(&b);
            let strip = |l: &[u8]| -> Vec<u8> {
                let l = l.strip_suffix(b"\n").unwrap_or(l);
                l.to_vec()
            };
            for c in diff_lines(&la, &lb, &DiffOptions::default()) {
                let removed = la[c.a_start..c.a_start + c.a_len].iter();
                let added = lb[c.b_start..c.b_start + c.b_len].iter();
                if removed.chain(added).any(|l| matches(&strip(l))) {
                    return true;
                }
            }
            false
        }
    }
}

/// Keep the matching pairs; with `all` (`--pickaxe-all`), keep every pair
/// when any pair matches.
pub fn pickaxe<T>(
    pairs: Vec<FilePair<T>>,
    how: &Criterion<'_>,
    all: bool,
    contents: &dyn Contents<T>,
) -> Vec<FilePair<T>> {
    if all {
        if pairs.iter().any(|p| matches(p, how, contents)) {
            pairs
        } else {
            Vec::new()
        }
    } else {
        pairs
            .into_iter()
            .filter(|p| matches(p, how, contents))
            .collect()
    }
}

/// Count the non-overlapping occurrences of `needle` in `hay` (`-S`
/// without `--pickaxe-regex`).
pub fn count_literal(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            n += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_counts() {
        assert_eq!(count_literal(b"aaaa", b"aa"), 2);
        assert_eq!(count_literal(b"aaaaa", b"aa"), 2);
        assert_eq!(count_literal(b"aaa", b"aa"), 1);
        assert_eq!(count_literal(b"x", b""), 0);
    }
}
