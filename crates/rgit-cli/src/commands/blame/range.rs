// SPDX-License-Identifier: MIT
//! `-L <start>,<end>` and `-L :<funcname>`
//! (Documentation/line-range-format.adoc): line ranges of the final file.
//!
//! Behaviour not spelled out by the documentation was taken from the
//! `git` binary: an `<end>` regex searches from the line after `<start>`,
//! `<end>` before `<start>` swaps them, an `<end>` past the end of the file
//! is cut to the last line, and a `<start>` past it is an error.

use regex::bytes::{Regex, RegexBuilder};

use crate::messages::blame as msg;

/// Why a range was rejected.
#[derive(Debug)]
pub enum RangeError {
    /// Malformed: git prints the first usage line (exit 129).
    Usage,
    /// `fatal: <msg>`.
    Fatal(String),
}

/// A parsed range: 0-based first line and the line after the last.
pub type Range = (usize, usize);

fn compile(pattern: &str) -> Result<Regex, RangeError> {
    let text = rgit_revwalk::walk::grep::translate(pattern, false)
        .map_err(|r| RangeError::Fatal(msg::invalid_regex(pattern, r)))?;
    RegexBuilder::new(&text)
        .unicode(false)
        .build()
        .map_err(|e| {
            let reason = rgit_revwalk::walk::grep::reason(&e.to_string());
            RangeError::Fatal(msg::invalid_regex(pattern, reason))
        })
}

/// The text of a line without its newline.
fn text(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// A `/regex/` term at the start of `s` (after the opening slash): the
/// pattern (with `\/` turned into `/`) and the rest after the closing
/// slash.
fn regex_term(s: &str) -> (String, &str) {
    let mut pat = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some((_, '/')) => pat.push('/'),
                Some((_, d)) => {
                    pat.push('\\');
                    pat.push(d);
                }
                None => pat.push('\\'),
            },
            '/' => return (pat, &s[i + 1..]),
            c => pat.push(c),
        }
    }
    (pat, "")
}

/// A decimal number at the start of `s` (with an optional sign), and the
/// rest.
fn number(s: &str) -> Option<(i64, &str)> {
    let digits_from = usize::from(s.starts_with(['+', '-']));
    let len = s[digits_from..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if len == 0 {
        return None;
    }
    let end = digits_from + len;
    let n = s[..end].parse::<i64>().ok()?;
    Some((n, &s[end..]))
}

/// Parse one `-L` argument against the lines of the final file. `anchor`
/// is the (0-based) line where relative regex searches start: the line
/// after the previous range. `path` is used in messages.
pub fn parse(
    spec: &str,
    lines: &[&[u8]],
    anchor: usize,
    path: &str,
    func: Option<&rgit_diff::FuncName>,
) -> Result<Range, RangeError> {
    let nlines = lines.len();
    if let Some(rest) = spec.strip_prefix("^:") {
        return funcname(rest, lines, 0, func);
    }
    if let Some(rest) = spec.strip_prefix(':') {
        return funcname(rest, lines, anchor, func);
    }

    // <start>
    let (begin, rest) = if let Some(r) = spec.strip_prefix("^/") {
        let (pat, rest) = regex_term(r);
        (search(&pat, lines, 0)? + 1, rest)
    } else if let Some(r) = spec.strip_prefix('/') {
        let (pat, rest) = regex_term(r);
        (search(&pat, lines, anchor)? + 1, rest)
    } else if spec.starts_with(',') {
        (1, spec)
    } else {
        let (n, rest) = number(spec).ok_or(RangeError::Usage)?;
        if n < 1 {
            return Err(RangeError::Fatal(msg::invalid_line_number(&n.to_string())));
        }
        (usize::try_from(n).unwrap_or(usize::MAX), rest)
    };

    // <end>
    let (mut first, mut last) = match rest.strip_prefix(',') {
        None if rest.is_empty() => (begin, usize::MAX),
        None => return Err(RangeError::Usage),
        Some("") => (begin, usize::MAX),
        Some(end) => {
            if let Some(r) = end.strip_prefix('/') {
                let (pat, tail) = regex_term(r);
                if !tail.is_empty() {
                    return Err(RangeError::Usage);
                }
                (begin, search(&pat, lines, begin)? + 1)
            } else {
                let (n, tail) = number(end).ok_or(RangeError::Usage)?;
                if !tail.is_empty() {
                    return Err(RangeError::Usage);
                }
                if end.starts_with('+') {
                    if n == 0 {
                        return Err(RangeError::Fatal(msg::INVALID_EMPTY_RANGE.into()));
                    }
                    let n = usize::try_from(n).unwrap_or(usize::MAX);
                    (begin, begin.saturating_add(n - 1))
                } else if end.starts_with('-') {
                    if n == 0 {
                        return Err(RangeError::Fatal(msg::INVALID_EMPTY_RANGE.into()));
                    }
                    let n = usize::try_from(-n).unwrap_or(usize::MAX);
                    (begin.saturating_sub(n - 1).max(1), begin)
                } else {
                    if n < 1 {
                        return Err(RangeError::Fatal(msg::invalid_line_number(&n.to_string())));
                    }
                    (begin, usize::try_from(n).unwrap_or(usize::MAX))
                }
            }
        }
    };
    if last < first {
        std::mem::swap(&mut first, &mut last);
    }
    if first > nlines {
        return Err(RangeError::Fatal(msg::only_lines(path, nlines)));
    }
    Ok((first - 1, last.min(nlines)))
}

/// The first line at or after `from` matching `pattern`.
fn search(pattern: &str, lines: &[&[u8]], from: usize) -> Result<usize, RangeError> {
    let re = compile(pattern)?;
    (from..lines.len())
        .find(|&i| re.is_match(text(lines[i])))
        .ok_or_else(|| RangeError::Fatal(msg::no_match(pattern, from + 1)))
}

/// `:<funcname>`: from the first function line at or after `from` that
/// matches the regex up to (not including) the next function line
/// (`func`: the diff driver's patterns, else the default rule).
fn funcname(
    pattern: &str,
    lines: &[&[u8]],
    from: usize,
    func: Option<&rgit_diff::FuncName>,
) -> Result<Range, RangeError> {
    let re = compile(pattern)?;
    let is_func = |l: &[u8]| rgit_diff::funcname::find_func(func, l).is_some();
    let start = (from..lines.len())
        .find(|&i| is_func(lines[i]) && re.is_match(text(lines[i])))
        .ok_or_else(|| RangeError::Fatal(msg::no_funcname_match(pattern, from + 1)))?;
    let end = (start + 1..lines.len())
        .find(|&i| is_func(lines[i]))
        .unwrap_or(lines.len());
    Ok((start, end))
}

/// Sort the ranges and merge overlapping and adjacent ones.
pub fn normalize(mut ranges: Vec<Range>) -> Vec<Range> {
    ranges.sort_unstable();
    let mut out: Vec<Range> = Vec::with_capacity(ranges.len());
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<&'static [u8]> {
        vec![b"a\n", b"b x\n", b"c\n", b"d x\n", b"e\n"]
    }

    fn ok(spec: &str, anchor: usize) -> Range {
        parse(spec, &lines(), anchor, "f", None).unwrap()
    }

    #[test]
    fn numbers_and_offsets() {
        assert_eq!(ok("2,3", 0), (1, 3));
        assert_eq!(ok("3,2", 0), (1, 3));
        assert_eq!(ok("2", 0), (1, 5));
        assert_eq!(ok(",2", 0), (0, 2));
        assert_eq!(ok("2,+2", 0), (1, 3));
        assert_eq!(ok("4,-2", 0), (2, 4));
        assert_eq!(ok("2,-9", 0), (0, 2));
        assert_eq!(ok("1,99", 0), (0, 5));
    }

    #[test]
    fn regexes() {
        assert_eq!(ok("/x/", 0), (1, 5));
        assert_eq!(ok("/x/", 2), (3, 5));
        assert_eq!(ok("^/x/", 2), (1, 5));
        assert_eq!(ok("/x/,/x/", 0), (1, 4));
    }

    #[test]
    fn errors() {
        assert!(matches!(parse("X", &lines(), 0, "f", None), Err(RangeError::Usage)));
        assert!(matches!(parse("1,+", &lines(), 0, "f", None), Err(RangeError::Usage)));
        assert!(matches!(parse("0", &lines(), 0, "f", None), Err(RangeError::Fatal(_))));
        assert!(matches!(parse("6", &lines(), 0, "f", None), Err(RangeError::Fatal(_))));
        assert!(matches!(parse("1,+0", &lines(), 0, "f", None), Err(RangeError::Fatal(_))));
    }

    #[test]
    fn merge() {
        assert_eq!(normalize(vec![(3, 5), (0, 2), (1, 3)]), vec![(0, 5)]);
        assert_eq!(normalize(vec![(2, 3), (0, 1)]), vec![(0, 1), (2, 3)]);
    }
}
