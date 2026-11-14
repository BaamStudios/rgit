// SPDX-License-Identifier: MIT

//! Unified diff output: hunks, hunk headers and the function-name context.

use crate::messages;
use crate::{Change, DiffOptions};

/// Maximum length in bytes of the function-name text in a hunk header.
const FUNCNAME_MAX: usize = 80;

/// Returns `true` if `line` qualifies as hunk-header context with the default
/// rule: it begins with an ASCII letter, an underscore or a dollar sign.
pub fn default_funcname(line: &[u8]) -> bool {
    matches!(line.first(), Some(c) if c.is_ascii_alphabetic() || *c == b'_' || *c == b'$')
}

/// Text shown after the hunk header for a function-name line.
fn funcname_text(line: &[u8]) -> &[u8] {
    let mut l = line;
    let cut = l.len() > FUNCNAME_MAX;
    if cut {
        l = &l[..FUNCNAME_MAX];
    }
    while let Some((&c, rest)) = l.split_last() {
        if c.is_ascii_whitespace() {
            l = rest;
        } else {
            break;
        }
    }
    if cut {
        // A UTF-8 character cut in two by the limit is left out (observed
        // with git; whitespace before it stays).
        l = &l[..l.len() - incomplete_utf8_tail(l)];
    }
    l
}

/// Length of an incomplete UTF-8 sequence at the end of `s` (0 if none).
fn incomplete_utf8_tail(s: &[u8]) -> usize {
    for back in 1..=s.len().min(3) {
        let c = s[s.len() - back];
        if c & 0xC0 == 0x80 {
            continue;
        }
        let need = match c {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return 0,
        };
        return if back < need { back } else { 0 };
    }
    0
}

fn push_range(out: &mut Vec<u8>, start: usize, len: usize) {
    use std::io::Write;
    match len {
        0 => write!(out, "{start},0").unwrap(),
        1 => write!(out, "{}", start + 1).unwrap(),
        _ => write!(out, "{},{len}", start + 1).unwrap(),
    }
}

fn push_line(out: &mut Vec<u8>, prefix: u8, line: &[u8]) {
    out.push(prefix);
    out.extend_from_slice(line);
    if line.last() != Some(&b'\n') {
        out.push(b'\n');
        out.extend_from_slice(messages::NO_NEWLINE_AT_EOF.as_bytes());
        out.push(b'\n');
    }
}

/// Writes the hunks for `changes` to `out`.
pub(crate) fn write_hunks(
    a: &[&[u8]],
    b: &[&[u8]],
    changes: &[Change],
    opts: &DiffOptions,
    out: &mut Vec<u8>,
) {
    let ctx = opts.context;
    let mut i = 0;
    // Function-name search: lines before `scanned` were already searched;
    // `funcname` is the last match found there.
    let mut scanned = 0;
    let mut funcname: Option<usize> = None;
    while i < changes.len() {
        // Collect the changes that belong to one hunk.
        let mut j = i;
        while j + 1 < changes.len() {
            let gap = changes[j + 1].a_start - (changes[j].a_start + changes[j].a_len);
            if gap <= 2 * ctx + opts.interhunk_context {
                j += 1;
            } else {
                break;
            }
        }
        let first = &changes[i];
        let last = &changes[j];
        let pre = first.a_start.min(ctx);
        let a_start = first.a_start - pre;
        let b_start = first.b_start - pre;
        let a_end = (last.a_start + last.a_len + ctx).min(a.len());
        let post = a_end - (last.a_start + last.a_len);
        let b_end = (last.b_start + last.b_len + post).min(b.len());
        let a_end = last.a_start + last.a_len + (b_end - (last.b_start + last.b_len));

        out.extend_from_slice(b"@@ -");
        push_range(out, a_start, a_end - a_start);
        out.extend_from_slice(b" +");
        push_range(out, b_start, b_end - b_start);
        out.extend_from_slice(b" @@");
        // Hunks only move forward, so each line is scanned at most once.
        if let Some(k) = (scanned..a_start).rev().find(|&k| default_funcname(a[k])) {
            funcname = Some(k);
        }
        scanned = scanned.max(a_start);
        if let Some(k) = funcname {
            out.push(b' ');
            out.extend_from_slice(funcname_text(a[k]));
        }
        out.push(b'\n');

        let mut pa = a_start;
        for c in &changes[i..=j] {
            while pa < c.a_start {
                push_line(out, b' ', a[pa]);
                pa += 1;
            }
            for line in &a[c.a_start..c.a_start + c.a_len] {
                push_line(out, b'-', line);
            }
            for line in &b[c.b_start..c.b_start + c.b_len] {
                push_line(out, b'+', line);
            }
            pa = c.a_start + c.a_len;
        }
        while pa < a_end {
            push_line(out, b' ', a[pa]);
            pa += 1;
        }
        i = j + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funcname_cut_inside_utf8() {
        // 79 ASCII bytes, then a two-byte character cut by the limit.
        let mut line = vec![b'f'; 78];
        line.push(b' ');
        line.extend_from_slice("\u{fc} tail\n".as_bytes());
        let mut want = vec![b'f'; 78];
        want.push(b' ');
        assert_eq!(funcname_text(&line), &want[..]);
        // Not cut: the text stays as it is.
        assert_eq!(
            funcname_text("fn \u{fc}\n".as_bytes()),
            "fn \u{fc}".as_bytes()
        );
        assert_eq!(incomplete_utf8_tail(&[b'a', 0xE2, 0x82]), 2);
        assert_eq!(incomplete_utf8_tail(&[b'a', 0xE2, 0x82, 0xAC]), 0);
    }
}
