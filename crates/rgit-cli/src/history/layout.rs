// SPDX-License-Identifier: MIT
//! Text layout for the user formats of `log` and `rev-list`
//! (Documentation/pretty-formats.adoc): display width of UTF-8 text,
//! `%w(...)` line wrapping and the truncation of `%<(<n>,trunc)` and
//! friends.
//!
//! The documentation only says that `%w` works "like the `-w` option of
//! git-shortlog(1)"; the rules below were worked out with differential
//! tests against the reference `git`:
//!
//! - Text is laid out word by word; a line gets the first indent, every
//!   further line the second one. A word that would end past the width
//!   moves to a new line, which starts after the last whitespace character
//!   before it (earlier whitespace stays at the end of the old line).
//! - A tab advances to the next multiple of eight columns, other
//!   whitespace takes one column.
//! - A newline followed by an ASCII letter or digit joins the lines with a
//!   space. Followed by another newline it keeps an empty line; followed by
//!   anything else it ends the line (and one following whitespace character
//!   is dropped).
//! - A width of 0 only indents: every line keeps its text, also empty
//!   ones, except an empty last line.
//! - Truncation counts display columns; characters without width (tabs,
//!   control characters, combining marks) are never removed.

use unicode_width::UnicodeWidthChar;

/// The columns `c` takes on a terminal.
fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Characters of `s` with their byte ranges and widths; invalid UTF-8
/// bytes count as one column each, a colour escape sequence (`ESC [ ...
/// m`) is one item without width.
fn chars(s: &[u8]) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos >= s.len() {
            return None;
        }
        let start = pos;
        if s[pos] == 0x1b
            && s.get(pos + 1) == Some(&b'[')
            && let Some(m) = s[pos + 2..]
                .iter()
                .position(|&b| !(b.is_ascii_digit() || b == b';'))
            && s[pos + 2 + m] == b'm'
        {
            pos += 2 + m + 1;
            return Some((start, pos, 0));
        }
        let (_, len, c) = bstr::ByteSlice::char_indices(&s[pos..]).next()?;
        pos += len;
        let w = if c == '\u{FFFD}' && !s[start..pos].starts_with("\u{FFFD}".as_bytes()) {
            1
        } else {
            char_width(c)
        };
        Some((start, pos, w))
    })
}

/// The display width of `s`.
pub fn width(s: &[u8]) -> usize {
    chars(s).map(|(_, _, w)| w).sum()
}

/// The column the end of `out` is at (the width since the last newline).
pub fn column(out: &[u8]) -> usize {
    let start = out.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    width(&out[start..])
}

/// Cut columns out of `s` so that `left` columns remain at the start and
/// `right` columns at the end, with `..` in the gap.
pub fn cut(s: &[u8], left: usize, right: usize) -> Vec<u8> {
    let total = width(s);
    let gap_end = total.saturating_sub(right);
    let mut out = Vec::with_capacity(s.len());
    let mut col = 0;
    let mut dots = false;
    for (start, end, w) in chars(s) {
        if w > 0 && col + w > left && col < gap_end {
            if !dots {
                out.extend_from_slice(b"..");
                dots = true;
            }
        } else {
            out.extend_from_slice(&s[start..end]);
        }
        col += w;
    }
    if !dots {
        out.extend_from_slice(b"..");
    }
    out
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Lay out `text` as `%w(<width>,<indent1>,<indent2>)` does.
pub fn wrap(text: &[u8], width: usize, indent1: usize, indent2: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 16);
    if width == 0 {
        let mut indent = indent1;
        let mut rest = text;
        loop {
            let (line, next) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            };
            if next.is_none() && line.is_empty() {
                break;
            }
            out.extend(std::iter::repeat_n(b' ', indent));
            out.extend_from_slice(line);
            match next {
                Some(n) => {
                    out.push(b'\n');
                    rest = n;
                }
                None => break,
            }
            indent = indent2;
        }
        return out;
    }
    Wrapper {
        text,
        out: &mut out,
        width,
        indent: indent1,
        indent2,
    }
    .run();
    out
}

struct Wrapper<'a> {
    text: &'a [u8],
    out: &'a mut Vec<u8>,
    width: usize,
    /// The indent of the current line.
    indent: usize,
    indent2: usize,
}

impl Wrapper<'_> {
    fn run(&mut self) {
        let text = self.text;
        // Where the current line's text starts.
        let mut line = 0;
        // The whitespace (or position after a joined newline) the line may
        // be broken at, once a word was placed on the line.
        let mut brk: Option<usize> = None;
        let mut col = self.indent;
        let mut i = 0;
        loop {
            let at_end = i >= text.len();
            if !at_end && !is_space(text[i]) {
                let (_, end, w) = chars(&text[i..]).next().unwrap_or((0, 1, 1));
                col += w;
                i += end;
                continue;
            }
            if col > self.width
                && let Some(b) = brk
            {
                // The last word does not fit: break before it.
                self.out.push(b'\n');
                line = b + usize::from(text.get(b).is_some_and(|&c| is_space(c)));
                brk = None;
                self.indent = self.indent2;
                col = self.indent;
                i = line;
                continue;
            }
            if at_end && i == line {
                return;
            }
            let from = match brk {
                Some(b) => b,
                None => {
                    self.out.extend(std::iter::repeat_n(b' ', self.indent));
                    line
                }
            };
            self.out.extend_from_slice(&text[from..i.min(text.len())]);
            if at_end {
                return;
            }
            let c = text[i];
            brk = Some(i);
            if c == b'\n' {
                let next = text.get(i + 1).copied();
                if next == Some(b'\n') {
                    self.out.extend_from_slice(b"\n\n");
                    line = i + 2;
                } else if next.is_some_and(|n| n.is_ascii_alphanumeric()) {
                    self.out.push(b' ');
                    brk = Some(i + 1);
                    col += 1;
                    i += 1;
                    continue;
                } else {
                    self.out.push(b'\n');
                    line = i + 1 + usize::from(next.is_some_and(is_space));
                }
                brk = None;
                self.indent = self.indent2;
                col = self.indent;
                i = line;
                continue;
            }
            col = if c == b'\t' {
                (col / 8 + 1) * 8
            } else {
                col + 1
            };
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths() {
        assert_eq!(width(b"abc"), 3);
        assert_eq!(width("日本".as_bytes()), 4);
        assert_eq!(width(b"a\tb"), 2);
        assert_eq!(column(b"xx\nabc"), 3);
    }

    #[test]
    fn cutting() {
        assert_eq!(cut(b"second", 2, 0), b"se..");
        assert_eq!(cut(b"second", 0, 2), b"..nd");
        assert_eq!(cut(b"second", 1, 2), b"s..nd");
        assert_eq!(cut(b"ab\tcdef", 1, 0), b"a..\t");
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap(b"ab cd ef", 5, 0, 2), b"ab cd\n  ef");
        assert_eq!(wrap(b"line  in", 6, 0, 0), b"line \nin");
        assert_eq!(wrap(b"a\nb", 10, 1, 2), b" a b");
        assert_eq!(wrap(b"a\n\nb\n", 10, 1, 2), b" a\n\n  b\n");
        assert_eq!(wrap(b"a\n\tb", 10, 0, 2), b"a\n  b");
        assert_eq!(wrap(b"\n\nab\n\n", 0, 1, 2), b" \n  \n  ab\n  \n");
    }
}
