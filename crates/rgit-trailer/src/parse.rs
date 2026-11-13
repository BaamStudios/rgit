// SPDX-License-Identifier: MIT
//! Finding the trailer block of a message (Documentation/git-interpret-trailers.adoc,
//! "DESCRIPTION" and "OTHER RULES"; details checked against the `git`
//! binary).
//!
//! The input is cut into three parts:
//!
//! 1. the message: everything before a `---` divider line (unless
//!    disabled), without the ignorable tail;
//! 2. the ignorable tail: a scissors line and everything after it, and the
//!    run of empty lines, comment lines and an old-style `Conflicts:` block
//!    that ends the message (never starting at the very beginning);
//! 3. the divider and what follows it (a patch).
//!
//! The trailer block is the last paragraph of the message after its first
//! paragraph (the title). It counts when all its lines are trailers, or when
//! it has a git-generated or configured trailer and at least a quarter of
//! its lines are trailers. Continuation lines (starting with a space or a
//! tab) belong to the line above; comment lines are skipped.

use bstr::BString;

use crate::config::{Config, prefix_ci};
use crate::messages::{GIT_GENERATED_PREFIXES, SCISSORS};

/// How the input is read.
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions<'a> {
    /// The comment string (`core.commentChar` / `core.commentString`).
    pub comment: &'a [u8],
    /// Treat a `---` line as the end of the message (`--no-divider` turns
    /// it off).
    pub divider: bool,
}

impl Default for ParseOptions<'_> {
    fn default() -> Self {
        ParseOptions {
            comment: b"#",
            divider: true,
        }
    }
}

/// A trailer: key and value, both without surrounding whitespace. A value
/// that continues over several lines keeps its newlines and indentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    /// The key as written, or the configured key when it refers to a
    /// `trailer.<alias>.key`.
    pub token: BString,
    pub value: BString,
    /// The configured item the key refers to.
    pub item: Option<usize>,
}

/// A line (with its continuation lines) of the trailer block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Trailer(Trailer),
    /// Anything else, exactly as in the input (without the final newline).
    Other(BString),
}

/// The result of [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Whether the message has a trailer block.
    pub found: bool,
    /// The byte range of the input the trailer block takes (including
    /// whitespace-only lines after it). Without a block both are the end
    /// of the message, where a new block goes.
    pub start: usize,
    pub end: usize,
    /// Whether the block is preceded by a blank line; without a block,
    /// whether the message ends with one (an empty message does not), so
    /// that none has to be written before a new block.
    pub blank_line_before: bool,
    /// The lines of the block (comment lines left out).
    pub lines: Vec<Line>,
}

impl Info {
    /// The trailers of the block.
    pub fn trailers(&self) -> impl Iterator<Item = &Trailer> {
        self.lines.iter().filter_map(|l| match l {
            Line::Trailer(t) => Some(t),
            Line::Other(_) => None,
        })
    }
}

pub(crate) fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

pub(crate) fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&b| !is_space(b)).unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|&b| !is_space(b))
        .map_or(start, |p| p + 1);
    &s[start..end]
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(|&b| is_space(b))
}

fn is_comment(line: &[u8], comment: &[u8]) -> bool {
    !comment.is_empty() && line.starts_with(comment)
}

/// Lines of `text` as `(start, content end, next start)`; a final line
/// without newline counts.
fn lines(text: &[u8], from: usize, to: usize) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    let mut pos = from;
    while pos < to {
        let end = text[pos..to]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(to, |p| pos + p);
        let next = if end < to { end + 1 } else { to };
        out.push((pos, end, next));
        pos = next;
    }
    out
}

/// The position of the separator of a trailer line or `--trailer`
/// argument: the key (letters, digits, `-`) may be followed by spaces or
/// tabs, then one of `separators` must come. `Some(0)` means an empty key.
pub fn find_separator(line: &[u8], separators: &[u8]) -> Option<usize> {
    let mut whitespace = false;
    for (i, &c) in line.iter().enumerate() {
        if separators.contains(&c) {
            return Some(i);
        }
        if !whitespace && (c.is_ascii_alphanumeric() || c == b'-') {
            continue;
        }
        if i != 0 && (c == b' ' || c == b'\t') {
            whitespace = true;
            continue;
        }
        return None;
    }
    None
}

/// The separator of a trailer: like [`find_separator`], but a `:` right
/// after the key that starts `//` (a URL) does not count, nor does an
/// empty key.
pub(crate) fn trailer_separator(line: &[u8], separators: &[u8]) -> Option<usize> {
    let sep = find_separator(line, separators)?;
    if sep == 0 {
        return None;
    }
    if line[sep] == b':' && !is_space(line[sep - 1]) && line[sep + 1..].starts_with(b"//") {
        return None;
    }
    Some(sep)
}

/// The start of the divider line (`---` followed by whitespace or the end
/// of the line), or the length of the input.
pub fn divider_start(input: &[u8]) -> usize {
    for (start, end, _) in lines(input, 0, input.len()) {
        let line = &input[start..end];
        if line.starts_with(b"---") && line.get(3).is_none_or(|&b| is_space(b)) {
            return start;
        }
    }
    input.len()
}

/// The length of `msg` without its ignorable tail (scissors line and what
/// follows, trailing empty and comment lines, an old-style conflicts list).
pub fn message_end(msg: &[u8], comment: &[u8]) -> usize {
    let mut scissors = comment.to_vec();
    scissors.extend_from_slice(SCISSORS.as_bytes());
    let all = lines(msg, 0, msg.len());
    let end = all
        .iter()
        .find(|&&(s, e, _)| msg[s..e] == scissors[..])
        .map_or(msg.len(), |&(s, _, _)| s);
    let mut boc = 0;
    let mut in_conflicts = false;
    for (s, e, _) in lines(msg, 0, end) {
        let line = &msg[s..e];
        if line.is_empty() || is_comment(line, comment) {
            if boc == 0 {
                boc = s;
            }
        } else if line == b"Conflicts:" {
            in_conflicts = true;
            if boc == 0 {
                boc = s;
            }
        } else if in_conflicts && line.first() == Some(&b'\t') {
            continue;
        } else {
            boc = 0;
            in_conflicts = false;
        }
    }
    if boc == 0 { end } else { boc }
}

/// Where the title (the first paragraph) ends: at the first blank line.
fn title_end(msg: &[u8], end: usize) -> usize {
    lines(msg, 0, end)
        .into_iter()
        .find(|&(s, e, _)| is_blank(&msg[s..e]))
        .map_or(end, |(s, _, _)| s)
}

/// Whether `line` makes the block count with fewer than all trailers: a
/// git-generated prefix, or a key that refers to a configured trailer.
fn recognized(line: &[u8], conf: &Config) -> bool {
    if GIT_GENERATED_PREFIXES
        .iter()
        .any(|p| line.starts_with(p.as_bytes()))
    {
        return true;
    }
    match trailer_separator(line, &conf.separators) {
        Some(sep) => {
            let raw = &line[..sep];
            conf.items
                .iter()
                .any(|i| prefix_ci(raw, i.key_or_name()) || prefix_ci(raw, &i.name))
        }
        None => false,
    }
}

/// Find the trailer block of `input`.
pub fn parse(input: &[u8], conf: &Config, opts: &ParseOptions<'_>) -> Info {
    let limit = if opts.divider {
        divider_start(input)
    } else {
        input.len()
    };
    let end = message_end(&input[..limit], opts.comment);
    let title = title_end(input, end);
    let body = lines(input, title, end);

    // Walk back over the last paragraph.
    let mut i = body.len();
    while i > 0 && is_blank(&input[body[i - 1].0..body[i - 1].1]) {
        i -= 1;
    }
    let mut trailers = 0usize;
    let mut others = 0usize;
    let mut continuation = 0usize;
    let mut known = false;
    let mut start = None;
    let mut any = false;
    while i > 0 {
        i -= 1;
        let (s, e, next) = body[i];
        let line = &input[s..e];
        if is_comment(line, opts.comment) {
            others += continuation;
            continuation = 0;
            continue;
        }
        if is_blank(line) {
            start = Some(next);
            break;
        }
        any = true;
        if line[0] == b' ' || line[0] == b'\t' {
            continuation += 1;
            continue;
        }
        if recognized(line, conf) {
            trailers += 1 + continuation;
            known = true;
        } else if trailer_separator(line, &conf.separators).is_some() {
            trailers += 1 + continuation;
        } else {
            others += 1 + continuation;
        }
        continuation = 0;
    }
    others += continuation;
    let start = start.unwrap_or(title);
    let found = any && trailers > 0 && (others == 0 || (known && trailers * 3 >= others));
    if !found {
        let blank_line_before = lines(input, 0, end)
            .last()
            .is_some_and(|&(s, e, _)| is_blank(&input[s..e]));
        return Info {
            found: false,
            start: end,
            end,
            blank_line_before,
            lines: Vec::new(),
        };
    }
    Info {
        found: true,
        start,
        end,
        blank_line_before: true,
        lines: block_lines(input, start, end, conf, opts.comment),
    }
}

/// The lines of the block `input[start..end]`.
fn block_lines(input: &[u8], start: usize, end: usize, conf: &Config, comment: &[u8]) -> Vec<Line> {
    // Raw text of each entry (with continuation lines) and whether it is a
    // trailer.
    let mut raw: Vec<(Vec<u8>, bool)> = Vec::new();
    let mut can_continue = false;
    for (s, e, _) in lines(input, start, end) {
        let line = &input[s..e];
        if is_comment(line, comment) {
            can_continue = false;
            continue;
        }
        if is_blank(line) {
            continue;
        }
        if (line[0] == b' ' || line[0] == b'\t') && can_continue {
            let last = raw.last_mut().expect("continued entry");
            last.0.push(b'\n');
            last.0.extend_from_slice(line);
            continue;
        }
        let is_trailer = trailer_separator(line, &conf.separators).is_some();
        raw.push((line.to_vec(), is_trailer));
        can_continue = true;
    }
    raw.into_iter()
        .map(|(text, is_trailer)| {
            if !is_trailer {
                return Line::Other(text.into());
            }
            let sep = trailer_separator(&text, &conf.separators).expect("trailer");
            Line::Trailer(make_trailer(&text[..sep], &text[sep + 1..], conf))
        })
        .collect()
}

/// A trailer from a raw key and value: trimmed, and with the configured
/// key when the key refers to an item that has one.
pub(crate) fn make_trailer(token: &[u8], value: &[u8], conf: &Config) -> Trailer {
    let token = trim(token);
    let item = conf.find_item(token);
    let token = match item.and_then(|i| conf.items[i].key.as_ref()) {
        Some(key) => key.clone(),
        None => token.into(),
    };
    Trailer {
        token,
        value: trim(value).into(),
        item,
    }
}
