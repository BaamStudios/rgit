// SPDX-License-Identifier: MIT
//! `--word-diff[=<mode>]`, `--word-diff-regex`, `--color-words`
//! (Documentation/diff-options.adoc).
//!
//! The line diff stays as it is; each run of removed and added lines of
//! a hunk is diffed again word by word. Details observed with the `git`
//! binary:
//!
//! * a word is a run of non-whitespace characters, or a match of the
//!   word regex (cut at a line feed); everything else is "whitespace";
//! * the text between changed words is taken from the new side; removed
//!   words go where they were, before the added ones; a removed or added
//!   run spans from its first to its last word, whitespace included;
//! * when nothing was added, the whole removed text is shown as removed;
//! * removed and added runs that cross lines are marked line by line;
//! * context lines are written without the leading space; "\ No
//!   newline" lines are left out (every line counts as ending in a line
//!   feed);
//! * `porcelain` writes each piece on a line of its own with ` `, `-` or
//!   `+` in front and a line feed of the text as `~`.

use super::color::DiffColors;
use crate::color::RESET;

/// The output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Plain,
    Color,
    Porcelain,
}

impl Mode {
    /// Parses the value of `--word-diff=`; `Ok(None)` for `none`.
    pub fn parse(value: &str) -> Result<Option<Mode>, ()> {
        match value {
            "plain" => Ok(Some(Mode::Plain)),
            "color" => Ok(Some(Mode::Color)),
            "porcelain" => Ok(Some(Mode::Porcelain)),
            "none" => Ok(None),
            _ => Err(()),
        }
    }
}

/// How words are found.
pub struct Words<'a> {
    pub mode: Mode,
    pub regex: Option<&'a regex::bytes::Regex>,
    pub colors: Option<&'a DiffColors>,
    pub diff: &'a rgit_diff::DiffOptions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Common,
    Removed,
    Added,
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')
}

/// The word ranges of `text`.
fn split(text: &[u8], regex: Option<&regex::bytes::Regex>) -> Vec<std::ops::Range<usize>> {
    let mut words = Vec::new();
    match regex {
        None => {
            let mut i = 0;
            while i < text.len() {
                if is_space(text[i]) {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < text.len() && !is_space(text[i]) {
                    i += 1;
                }
                words.push(start..i);
            }
        }
        Some(re) => {
            let mut at = 0;
            while at < text.len() {
                let Some(m) = re.find_at(text, at) else {
                    break;
                };
                let mut end = m.end();
                if let Some(nl) = text[m.start()..end].iter().position(|&c| c == b'\n') {
                    end = m.start() + nl;
                }
                if end > m.start() {
                    words.push(m.start()..end);
                    at = end;
                } else {
                    at = m.start() + 1;
                }
            }
        }
    }
    words
}

impl Words<'_> {
    fn paint(&self, out: &mut Vec<u8>, kind: Kind, piece: &[u8]) {
        let (open, close): (&[u8], &[u8]) = match (self.mode, kind) {
            (Mode::Plain, Kind::Removed) => (b"[-", b"-]"),
            (Mode::Plain, Kind::Added) => (b"{+", b"+}"),
            _ => (b"", b""),
        };
        let color = self.colors.and_then(|c| match kind {
            Kind::Removed => Some(&c.old),
            Kind::Added => Some(&c.new),
            Kind::Common => None,
        });
        if let Some(c) = color {
            out.extend_from_slice(c.as_bytes());
        }
        out.extend_from_slice(open);
        out.extend_from_slice(piece);
        out.extend_from_slice(close);
        if color.is_some() {
            out.extend_from_slice(RESET.as_bytes());
        }
    }

    /// Writes a piece of text of one kind, line by line.
    fn segment(&self, out: &mut Vec<u8>, kind: Kind, text: &[u8]) {
        let mut rest = text;
        while !rest.is_empty() {
            let (piece, nl) = match rest.iter().position(|&c| c == b'\n') {
                Some(p) => (&rest[..p], true),
                None => (rest, false),
            };
            if self.mode == Mode::Porcelain {
                if !piece.is_empty() {
                    out.push(match kind {
                        Kind::Common => b' ',
                        Kind::Removed => b'-',
                        Kind::Added => b'+',
                    });
                    out.extend_from_slice(piece);
                    out.push(b'\n');
                }
                if nl {
                    out.extend_from_slice(b"~\n");
                }
            } else {
                if !piece.is_empty() {
                    if kind == Kind::Common {
                        out.extend_from_slice(piece);
                    } else {
                        self.paint(out, kind, piece);
                    }
                }
                if nl {
                    out.push(b'\n');
                }
            }
            rest = &rest[piece.len() + usize::from(nl)..];
        }
    }

    /// One run of removed and added lines.
    fn block(&self, out: &mut Vec<u8>, minus: &[u8], plus: &[u8]) {
        if plus.is_empty() {
            self.segment(out, Kind::Removed, minus);
            return;
        }
        let mw = split(minus, self.regex);
        let pw = split(plus, self.regex);
        let ml: Vec<&[u8]> = mw.iter().map(|r| &minus[r.clone()]).collect();
        let pl: Vec<&[u8]> = pw.iter().map(|r| &plus[r.clone()]).collect();
        let changes = rgit_diff::diff_lines(&ml, &pl, self.diff);
        let mut pos = 0;
        for c in &changes {
            let at = if c.b_len > 0 {
                pw[c.b_start].start
            } else if c.b_start > 0 {
                pw[c.b_start - 1].end
            } else {
                0
            };
            let at = at.max(pos);
            self.segment(out, Kind::Common, &plus[pos..at]);
            pos = at;
            if c.a_len > 0 {
                let r = mw[c.a_start].start..mw[c.a_start + c.a_len - 1].end;
                self.segment(out, Kind::Removed, &minus[r]);
            }
            if c.b_len > 0 {
                let end = pw[c.b_start + c.b_len - 1].end;
                self.segment(out, Kind::Added, &plus[pos..end]);
                pos = end;
            }
        }
        self.segment(out, Kind::Common, &plus[pos..]);
    }

    /// Rewrites the hunks of one file's patch (`plain`) into `out`; the
    /// header lines are copied (coloured as `meta` when colouring).
    pub fn patch(&self, out: &mut Vec<u8>, plain: &[u8]) {
        let mut in_header = true;
        let mut minus = Vec::new();
        let mut plus = Vec::new();
        let paint = |out: &mut Vec<u8>, color: Option<&String>, text: &[u8]| match color {
            Some(c) => {
                out.extend_from_slice(c.as_bytes());
                out.extend_from_slice(text);
                out.extend_from_slice(RESET.as_bytes());
            }
            None => out.extend_from_slice(text),
        };
        for raw in plain.split_inclusive(|&b| b == b'\n') {
            let line = raw.strip_suffix(b"\n").unwrap_or(raw);
            if in_header && !line.starts_with(b"@@") {
                let meta = if line.starts_with(b"Binary files ") {
                    None
                } else {
                    self.colors.map(|c| &c.meta)
                };
                paint(out, meta, line);
                out.push(b'\n');
                continue;
            }
            in_header = false;
            let sign = line.first().copied().unwrap_or(b' ');
            if sign != b'-'
                && sign != b'+'
                && sign != b'\\'
                && (!minus.is_empty() || !plus.is_empty())
            {
                self.block(out, &minus, &plus);
                minus.clear();
                plus.clear();
            }
            match sign {
                b'-' => {
                    minus.extend_from_slice(&line[1..]);
                    minus.push(b'\n');
                }
                b'+' => {
                    plus.extend_from_slice(&line[1..]);
                    plus.push(b'\n');
                }
                b'\\' => {}
                b'@' => {
                    let end = line[2..]
                        .windows(2)
                        .position(|w| w == b"@@")
                        .map_or(line.len(), |p| p + 4);
                    paint(out, self.colors.map(|c| &c.frag), &line[..end]);
                    let rest = &line[end..];
                    if let (Some(c), Some(func)) = (self.colors, rest.strip_prefix(b" ")) {
                        paint(out, Some(&c.context), b" ");
                        paint(out, Some(&c.func), func);
                    } else {
                        out.extend_from_slice(rest);
                    }
                    out.push(b'\n');
                }
                _ => {
                    let text = line.get(1..).unwrap_or_default();
                    match self.mode {
                        Mode::Porcelain => {
                            out.push(b' ');
                            out.extend_from_slice(text);
                            out.extend_from_slice(b"\n~\n");
                        }
                        _ => {
                            if !text.is_empty() {
                                paint(out, self.colors.map(|c| &c.context), text);
                            }
                            out.push(b'\n');
                        }
                    }
                }
            }
        }
        if !minus.is_empty() || !plus.is_empty() {
            self.block(out, &minus, &plus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mode: Mode, plain: &str) -> String {
        let diff = rgit_diff::DiffOptions::default();
        let w = Words {
            mode,
            regex: None,
            colors: None,
            diff: &diff,
        };
        let mut out = Vec::new();
        w.patch(&mut out, plain.as_bytes());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn plain_and_porcelain() {
        let plain = "@@ -1,2 +1,2 @@\n ctx\n-foo bar  baz\n-qux\n+foo  BAR baz quux\n";
        assert_eq!(
            run(Mode::Plain, plain),
            "@@ -1,2 +1,2 @@\nctx\nfoo  [-bar-]{+BAR+} baz [-qux-]{+quux+}\n"
        );
        assert_eq!(
            run(Mode::Porcelain, "@@ -1 +1 @@\n-p q r\n-s\n+p q\n"),
            "@@ -1 +1 @@\n p q\n-r\n~\n-s\n~\n"
        );
        assert_eq!(
            run(Mode::Plain, "@@ -1 +0,0 @@\n-x  y\n"),
            "@@ -1 +0,0 @@\n[-x  y-]\n"
        );
    }
}
