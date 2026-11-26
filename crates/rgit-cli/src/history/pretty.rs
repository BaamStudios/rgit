// SPDX-License-Identifier: MIT
//! The built-in pretty formats of `git log` and `git rev-list`
//! (Documentation/pretty-formats.adoc): `oneline`, `short`, `medium`,
//! `full`, `fuller`, `raw`, plus the names of `reference` and the user
//! formats (`format:`, `tformat:`; expanded by `userformat`) and
//! `pretty.<name>` aliases.
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

/// A format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pretty {
    Oneline,
    Short,
    Medium,
    Full,
    Fuller,
    Raw,
    /// `reference`: `%C(auto)%h (%s, %ad)` with short dates by default.
    Reference,
    /// `email`: the headers of a patch mail and the message.
    Email,
    /// `mboxrd`: `email` with `From ` lines of the body quoted.
    Mboxrd,
    /// `format:<s>` (`terminator` false) or `tformat:<s>`.
    User {
        format: Vec<u8>,
        terminator: bool,
    },
}

/// The built-in names in the order abbreviations are tried.
const BUILTIN: [&str; 9] = [
    "oneline",
    "short",
    "medium",
    "full",
    "fuller",
    "email",
    "raw",
    "mboxrd",
    "reference",
];

/// Why a `--pretty` value is not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrettyError {
    /// Neither a format nor an alias.
    Invalid,
    /// An alias that leads back to itself.
    Loop,
    /// A known format rgit does not have yet.
    Unsupported(&'static str),
}

impl Pretty {
    /// The built-in format `name` names: the whole name or an abbreviation,
    /// in any case.
    fn builtin(name: &str) -> Option<Result<Pretty, PrettyError>> {
        if name.is_empty() {
            return None;
        }
        let lower = name.to_ascii_lowercase();
        let full = BUILTIN.iter().find(|b| b.starts_with(&lower))?;
        Some(Ok(match *full {
            "oneline" => Pretty::Oneline,
            "short" => Pretty::Short,
            "medium" => Pretty::Medium,
            "full" => Pretty::Full,
            "fuller" => Pretty::Fuller,
            "raw" => Pretty::Raw,
            "reference" => Pretty::Reference,
            "email" => Pretty::Email,
            "mboxrd" => Pretty::Mboxrd,
            other => return Some(Err(PrettyError::Unsupported(other))),
        }))
    }

    /// The format `--pretty=<value>` selects; `alias` looks up
    /// `pretty.<name>`.
    pub fn parse(
        value: &str,
        alias: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Pretty, PrettyError> {
        let mut seen: Vec<String> = Vec::new();
        let mut current = value.to_string();
        loop {
            if let Some(f) = current.strip_prefix("format:") {
                return Ok(Pretty::User {
                    format: f.as_bytes().to_vec(),
                    terminator: false,
                });
            }
            if let Some(f) = current.strip_prefix("tformat:") {
                return Ok(Pretty::User {
                    format: f.as_bytes().to_vec(),
                    terminator: true,
                });
            }
            if let Some(b) = Pretty::builtin(&current) {
                return b;
            }
            let key = current.to_ascii_lowercase();
            if let Some(next) = alias(&key) {
                if seen.contains(&key) {
                    return Err(PrettyError::Loop);
                }
                seen.push(key);
                current = next;
                continue;
            }
            if current.contains('%') || current.is_empty() {
                return Ok(Pretty::User {
                    format: current.into_bytes(),
                    terminator: true,
                });
            }
            return Err(PrettyError::Invalid);
        }
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

/// Append `line` with tabs expanded to every `width`th column (0: kept).
fn push_expanded(out: &mut Vec<u8>, line: &[u8], width: usize) {
    if width == 0 {
        out.extend_from_slice(line);
        return;
    }
    let mut col = 0usize;
    for &b in line {
        if b == b'\t' {
            let n = width - col % width;
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

/// `Name <email>`, through the mailmap.
pub fn ident(sig: &SignatureRef<'_>, mailmap: Option<&Mailmap>) -> Vec<u8> {
    let mut out = Vec::new();
    push_ident(&mut out, Some(sig), mailmap);
    out
}

/// The date of `sig` in `mode`.
pub fn date_of(sig: &SignatureRef<'_>, mode: &DateMode, ctx: &DateContext) -> String {
    date(Some(sig), mode, ctx)
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
    /// `--decorate`: the decoration after the name (may be empty).
    pub decoration: &'a [u8],
    /// `--show-signature`: lines shown after the name (may be empty).
    pub signature: &'a [u8],
    /// `log -g`: the reflog entry the commit is shown for.
    pub reflog: Option<&'a super::userformat::ReflogInfo>,
    /// `email`/`mboxrd` as `log` writes them (a mail: `From <id>` line,
    /// encoded headers, `[PATCH]`), not as `rev-list` does.
    pub as_mail: bool,
    /// The full commit id (the `From <id>` line of a mail).
    pub full_id: String,
    /// The notes shown after the message (each block starts with an
    /// empty line), may be empty.
    pub notes: &'a [u8],
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
    /// `-z`: entries end (oneline) or are separated by NUL.
    pub zero: bool,
    /// `--[no-]expand-tabs[=<n>]`: the tab width of indented messages
    /// (`None`: 8, 0: tabs are kept).
    pub tab_width: Option<usize>,
}

/// Append one entry. `first` is false for every entry after the first,
/// which then gets the separator line of the multi-line formats.
pub fn write_entry(out: &mut Vec<u8>, style: &Style, e: &Entry<'_>, first: bool) {
    if matches!(style.pretty, Pretty::Email | Pretty::Mboxrd) {
        if !first {
            out.push(if style.zero { 0 } else { b'\n' });
        }
        write_email(out, style, e);
        return;
    }
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
        out.extend_from_slice(e.decoration);
        out.push(b' ');
        out.extend_from_slice(e.signature);
        match e.reflog {
            // Under `-g` the reflog entry takes the place of the subject.
            Some(r) => {
                out.extend_from_slice(&rgit_bstr::to_bytes(&r.selector));
                out.extend_from_slice(b": ");
                out.extend_from_slice(&r.message);
            }
            None => out.extend_from_slice(&subject(&lines)),
        }
        out.extend_from_slice(e.notes);
        out.push(if style.zero { 0 } else { b'\n' });
        return;
    }
    if !first {
        out.push(if style.zero { 0 } else { b'\n' });
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
    out.extend_from_slice(e.decoration);
    out.push(b'\n');
    out.extend_from_slice(e.signature);
    if let Some(r) = e.reflog {
        out.extend_from_slice(b"Reflog: ");
        out.extend_from_slice(&rgit_bstr::to_bytes(&r.selector));
        out.extend_from_slice(b" (");
        out.extend_from_slice(&r.name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(&r.email);
        out.extend_from_slice(b">)\nReflog message: ");
        out.extend_from_slice(&r.message);
        out.push(b'\n');
    }
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
    if !shown.is_empty() {
        out.push(b'\n');
        for line in shown {
            out.extend_from_slice(b"    ");
            if style.pretty == Pretty::Raw {
                out.extend_from_slice(line);
            } else {
                push_expanded(out, line, style.tab_width.unwrap_or(8));
            }
            out.push(b'\n');
        }
    }
    out.extend_from_slice(e.notes);
}

/// The `email` and `mboxrd` formats: for `log` a mail as `format-patch`
/// starts it (`From <id> Mon Sep 17 00:00:00 2001`, `From:` and
/// `Subject: [PATCH]` encoded as needed, an RFC 2822 `Date:`), for
/// `rev-list` the plain headers after the `commit` line; then an empty
/// line and the body.
fn write_email(out: &mut Vec<u8>, style: &Style, e: &Entry<'_>) {
    let author = e.commit.author();
    let (subject, body) = crate::commands::format_patch::mail::split_message(e.commit.message());
    if e.as_mail {
        out.extend_from_slice(b"From ");
        out.extend_from_slice(e.full_id.as_bytes());
        out.extend_from_slice(b" Mon Sep 17 00:00:00 2001\n");
    } else {
        out.extend_from_slice(b"commit ");
        out.extend_from_slice(e.name.as_bytes());
        out.push(b'\n');
    }
    if let Some(a) = author.as_ref() {
        let (name, email) = match &style.mailmap {
            Some(mm) => mm.map(a.name, a.email),
            None => (a.name, a.email),
        };
        out.extend_from_slice(b"From: ");
        if e.as_mail {
            rgit_mail::write_address(out, name, email, "UTF-8", true);
        } else {
            out.extend_from_slice(name);
            out.extend_from_slice(b" <");
            out.extend_from_slice(email);
            out.push(b'>');
        }
        out.push(b'\n');
        let rfc = rgit_date::parse_date_mode("rfc2822", false).unwrap_or_default();
        out.extend_from_slice(b"Date: ");
        out.extend_from_slice(date(Some(a), &rfc, &style.date_ctx).as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(b"Subject: ");
    if e.as_mail {
        out.extend_from_slice(b"[PATCH] ");
        rgit_mail::write_text(out, &subject, "UTF-8", true);
    } else {
        rgit_mail::write_folded(out, &subject);
    }
    out.push(b'\n');
    // A message that is not plain ASCII gets MIME headers.
    if !e.commit.message().is_ascii() {
        out.extend_from_slice(
            b"MIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\n\
              Content-Transfer-Encoding: 8bit\n",
        );
    }
    out.push(b'\n');
    if style.pretty == Pretty::Mboxrd {
        out.extend_from_slice(&crate::commands::format_patch::mail::mboxrd_escape(&body));
    } else {
        out.extend_from_slice(&body);
    }
    // The notes come with their separator (see `Printer::commit`).
    out.extend_from_slice(e.notes);
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
        push_expanded(&mut out, b"body\ttab x", 8);
        assert_eq!(out, b"body    tab x");
        assert!(message_lines(b"").is_empty());
    }

    #[test]
    fn tz() {
        assert_eq!(tz_hhmm(90), 130);
        assert_eq!(tz_hhmm(-300), -500);
    }
}
