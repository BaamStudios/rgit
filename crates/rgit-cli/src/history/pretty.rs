// SPDX-License-Identifier: MIT
//! The built-in pretty formats of `git log` and `git rev-list`
//! (Documentation/pretty-formats.adoc): `oneline`, `short`, `medium`,
//! `full`, `fuller` and `raw`.
//!
//! Details not in the documentation were taken from the output of the
//! reference `git`: messages lose leading and trailing empty lines and the
//! trailing whitespace of every line, lines are indented by four spaces
//! with tabs expanded (except in `raw`), the `oneline` subject joins the
//! lines of the first paragraph with single spaces, and `raw` shows the
//! stored header lines without `encoding`.

use rgit_date::{DateContext, DateMode};
use rgit_object::{Commit, SignatureRef};

use super::mailmap::Mailmap;

/// A built-in format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pretty {
    Oneline,
    Short,
    Medium,
    Full,
    Fuller,
    Raw,
}

impl Pretty {
    /// The format named `name` (`--pretty=<name>`), as git accepts it.
    pub fn from_name(name: &str) -> Option<Pretty> {
        Some(match name {
            "oneline" => Pretty::Oneline,
            "short" => Pretty::Short,
            "medium" => Pretty::Medium,
            "full" => Pretty::Full,
            "fuller" => Pretty::Fuller,
            "raw" => Pretty::Raw,
            _ => return None,
        })
    }
}

/// Lines of a commit message: without leading and trailing empty lines,
/// trailing whitespace removed from every line.
pub fn message_lines(msg: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = msg.split(|&b| b == b'\n').map(trim_end).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let start = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    lines.drain(..start);
    lines
}

fn trim_end(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    &line[..end]
}

/// The subject: the first paragraph joined with spaces.
pub fn subject(lines: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().take_while(|l| !l.is_empty()).enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

/// Append `line` with tabs expanded to every eighth column.
fn push_expanded(out: &mut Vec<u8>, line: &[u8]) {
    let mut col = 0usize;
    for &b in line {
        if b == b'\t' {
            let n = 8 - col % 8;
            out.extend(std::iter::repeat_n(b' ', n));
            col += n;
        } else {
            out.push(b);
            // Continuation bytes of UTF-8 sequences take no column.
            if b & 0xc0 != 0x80 {
                col += 1;
            }
        }
    }
}

/// `±hhmm` as an integer (`+0130` is 130), the form rgit-date takes.
fn tz_hhmm(offset_minutes: i32) -> i32 {
    let abs = offset_minutes.abs();
    let v = abs / 60 * 100 + abs % 60;
    if offset_minutes < 0 { -v } else { v }
}

fn push_ident(out: &mut Vec<u8>, sig: Option<&SignatureRef<'_>>, mailmap: Option<&Mailmap>) {
    if let Some(sig) = sig {
        let (name, email) = match mailmap {
            Some(mm) => mm.map(sig.name, sig.email),
            None => (sig.name, sig.email),
        };
        out.extend_from_slice(name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(email);
        out.push(b'>');
    }
}

fn date(sig: Option<&SignatureRef<'_>>, mode: &DateMode, ctx: &DateContext) -> String {
    let Some(t) = sig.and_then(|s| s.time()) else {
        return String::new();
    };
    let secs = u64::try_from(t.seconds).unwrap_or(0);
    rgit_date::show_date(secs, tz_hhmm(t.offset_minutes), mode, ctx).unwrap_or_default()
}

/// What to print for one commit.
pub struct Entry<'a> {
    /// The commit name as shown (full or abbreviated).
    pub name: &'a str,
    /// Parent names for `--parents`, already formatted.
    pub shown_parents: &'a [String],
    /// Abbreviated parents for the `Merge:` line (only for merges).
    pub merge_parents: &'a [String],
    /// The parsed commit.
    pub commit: &'a Commit,
    /// The raw object bytes (for `raw`).
    pub raw: &'a [u8],
}

/// Settings shared by all entries.
pub struct Style {
    pub pretty: Pretty,
    /// Identities are shown through this mailmap.
    pub mailmap: Option<Mailmap>,
    pub date_mode: DateMode,
    pub date_ctx: DateContext,
    /// Colour of the commit name (`color.diff.commit`) when colouring.
    pub commit_color: Option<String>,
}

/// Append one entry. `first` is false for every entry after the first,
/// which then gets the separator line of the multi-line formats.
pub fn write_entry(out: &mut Vec<u8>, style: &Style, e: &Entry<'_>, first: bool) {
    let lines = message_lines(e.commit.message());
    let color = style.commit_color.as_deref();
    if style.pretty == Pretty::Oneline {
        out.extend_from_slice(color.unwrap_or("").as_bytes());
        out.extend_from_slice(e.name.as_bytes());
        for p in e.shown_parents {
            out.push(b' ');
            out.extend_from_slice(p.as_bytes());
        }
        if color.is_some() {
            out.extend_from_slice(crate::color::RESET.as_bytes());
        }
        out.push(b' ');
        out.extend_from_slice(&subject(&lines));
        out.push(b'\n');
        return;
    }
    if !first {
        out.push(b'\n');
    }
    out.extend_from_slice(color.unwrap_or("").as_bytes());
    out.extend_from_slice(b"commit ");
    out.extend_from_slice(e.name.as_bytes());
    for p in e.shown_parents {
        out.push(b' ');
        out.extend_from_slice(p.as_bytes());
    }
    if color.is_some() {
        out.extend_from_slice(crate::color::RESET.as_bytes());
    }
    out.push(b'\n');
    let author = e.commit.author();
    let committer = e.commit.committer();
    match style.pretty {
        Pretty::Raw => {
            let end = e
                .raw
                .windows(2)
                .position(|w| w == b"\n\n")
                .map_or(e.raw.len(), |i| i + 1);
            for line in e.raw[..end].split_inclusive(|&b| b == b'\n') {
                if !line.starts_with(b"encoding ") {
                    out.extend_from_slice(line);
                }
            }
            if !e.raw[..end].ends_with(b"\n") {
                out.push(b'\n');
            }
        }
        _ => {
            if e.merge_parents.len() > 1 {
                out.extend_from_slice(b"Merge:");
                for p in e.merge_parents {
                    out.push(b' ');
                    out.extend_from_slice(p.as_bytes());
                }
                out.push(b'\n');
            }
            let fuller = style.pretty == Pretty::Fuller;
            out.extend_from_slice(if fuller { b"Author:     " } else { b"Author: " });
            push_ident(out, author.as_ref(), style.mailmap.as_ref());
            out.push(b'\n');
            match style.pretty {
                Pretty::Medium => {
                    out.extend_from_slice(b"Date:   ");
                    out.extend_from_slice(
                        date(author.as_ref(), &style.date_mode, &style.date_ctx).as_bytes(),
                    );
                    out.push(b'\n');
                }
                Pretty::Full => {
                    out.extend_from_slice(b"Commit: ");
                    push_ident(out, committer.as_ref(), style.mailmap.as_ref());
                    out.push(b'\n');
                }
                Pretty::Fuller => {
                    out.extend_from_slice(b"AuthorDate: ");
                    out.extend_from_slice(
                        date(author.as_ref(), &style.date_mode, &style.date_ctx).as_bytes(),
                    );
                    out.extend_from_slice(b"\nCommit:     ");
                    push_ident(out, committer.as_ref(), style.mailmap.as_ref());
                    out.extend_from_slice(b"\nCommitDate: ");
                    out.extend_from_slice(
                        date(committer.as_ref(), &style.date_mode, &style.date_ctx).as_bytes(),
                    );
                    out.push(b'\n');
                }
                _ => {}
            }
        }
    }
    let shown: &[&[u8]] = if style.pretty == Pretty::Short {
        let n = lines
            .iter()
            .position(|l| l.is_empty())
            .unwrap_or(lines.len());
        &lines[..n]
    } else {
        &lines
    };
    if shown.is_empty() {
        return;
    }
    out.push(b'\n');
    for line in shown {
        out.extend_from_slice(b"    ");
        if style.pretty == Pretty::Raw {
            out.extend_from_slice(line);
        } else {
            push_expanded(out, line);
        }
        out.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_subject() {
        let msg = b"\n\n  first  \n   second\t \n\n\nbody\ttab x\n \n\t\n\nend   \n\n\n";
        let lines = message_lines(msg);
        assert_eq!(lines.first(), Some(&&b"  first"[..]));
        assert_eq!(lines.last(), Some(&&b"end"[..]));
        assert_eq!(subject(&lines), b"  first    second");
        let mut out = Vec::new();
        push_expanded(&mut out, b"body\ttab x");
        assert_eq!(out, b"body    tab x");
        assert!(message_lines(b"").is_empty());
    }

    #[test]
    fn tz() {
        assert_eq!(tz_hhmm(90), 130);
        assert_eq!(tz_hhmm(-300), -500);
    }
}
