// SPDX-License-Identifier: MIT
//! Output of `git blame`: the default format, the `git annotate` format
//! (`-c`) and the porcelain formats (Documentation/git-blame.adoc).
//!
//! Column widths were taken from the `git` binary: the author, line number
//! and original line number columns are as wide as their widest shown
//! value; object names are one digit longer when boundary commits are
//! marked with `^`, so that the columns line up.

use std::collections::HashMap;
use std::io::Write;

use rgit_date::{DateContext, DateMode, DateStyle};
use rgit_hash::ObjectId;
use unicode_width::UnicodeWidthStr;

use super::score::{Blob, Entry, Scoreboard};
use crate::history::mailmap::Mailmap;

/// Colouring of the annotations in the default format.
pub enum Coloring {
    None,
    /// `--color-lines`: lines from the same commit as the line before.
    Repeated(String),
    /// `--color-by-age`: the first colour, then (time, colour) for lines
    /// newer than the time.
    Age(String, Vec<(i64, String)>),
}

/// How to print.
pub struct Format {
    pub coloring: Coloring,
    pub porcelain: bool,
    pub line_porcelain: bool,
    /// `-c` / `git annotate`.
    pub annotate: bool,
    /// `-f`: `None` shows names only when some line comes from another path.
    pub show_name: Option<bool>,
    pub show_number: bool,
    /// `-s`.
    pub no_author: bool,
    pub show_email: bool,
    /// `-t`.
    pub raw_time: bool,
    pub blank_boundary: bool,
    /// Minimum length of object names (full length for `-l`).
    pub abbrev: usize,
    pub date: DateMode,
    pub date_ctx: DateContext,
}

/// Commit metadata as shown.
pub struct Info {
    pub author: Vec<u8>,
    pub author_mail: Vec<u8>,
    pub author_time: i64,
    pub author_tz: Vec<u8>,
    pub committer: Vec<u8>,
    pub committer_mail: Vec<u8>,
    pub committer_time: i64,
    pub committer_tz: Vec<u8>,
    pub summary: Vec<u8>,
}

/// Name, `<email>`, time and zone of an identity header value.
fn ident(raw: Option<&[u8]>, mailmap: &Mailmap) -> (Vec<u8>, Vec<u8>, i64, Vec<u8>) {
    let Some(sig) = raw.and_then(rgit_object::SignatureRef::parse) else {
        return (Vec::new(), b"<>".to_vec(), 0, b"+0000".to_vec());
    };
    let (name, email) = mailmap.map(sig.name, sig.email);
    let mut mail = Vec::with_capacity(email.len() + 2);
    mail.push(b'<');
    mail.extend_from_slice(email);
    mail.push(b'>');
    let mut fields = sig
        .time_raw
        .split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty());
    let time = fields
        .next()
        .and_then(|t| std::str::from_utf8(t).ok()?.parse::<i64>().ok())
        .unwrap_or(0);
    let tz = fields.next().unwrap_or(b"+0000").to_vec();
    (name.to_vec(), mail, time, tz)
}

/// The first non-blank line of a commit message.
fn summary(message: &[u8]) -> Vec<u8> {
    message
        .split(|&b| b == b'\n')
        .find(|l| l.iter().any(|b| !b.is_ascii_whitespace()))
        .unwrap_or_default()
        .to_vec()
}

impl Info {
    pub fn of_commit(commit: &rgit_object::Commit, mailmap: &Mailmap) -> Info {
        let (author, author_mail, author_time, author_tz) = ident(commit.author_raw(), mailmap);
        let (committer, committer_mail, committer_time, committer_tz) =
            ident(commit.committer_raw(), mailmap);
        Info {
            author,
            author_mail,
            author_time,
            author_tz,
            committer,
            committer_mail,
            committer_time,
            committer_tz,
            summary: summary(commit.message()),
        }
    }

    /// The pseudo commit of the final contents.
    pub fn fake(name: &str, mail: &str, time: i64, tz: &str, summary: String) -> Info {
        Info {
            author: name.as_bytes().to_vec(),
            author_mail: mail.as_bytes().to_vec(),
            author_time: time,
            author_tz: tz.as_bytes().to_vec(),
            committer: name.as_bytes().to_vec(),
            committer_mail: mail.as_bytes().to_vec(),
            committer_time: time,
            committer_tz: tz.as_bytes().to_vec(),
            summary: summary.into_bytes(),
        }
    }
}

fn tz_int(tz: &[u8]) -> i32 {
    std::str::from_utf8(tz)
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0)
}

/// Sort entries by final line and merge neighbours that continue each other
/// in the same suspect.
pub fn coalesce(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by_key(|e| e.lno);
    let mut out: Vec<Entry> = Vec::with_capacity(entries.len());
    for e in entries {
        // Where the previous entry would continue, in both files.
        let next = out.last().map(|p| (p.suspect, p.lno + p.num, p.s_lno + p.num));
        match out.last_mut() {
            Some(p) if next == Some((e.suspect, e.lno, e.s_lno)) => p.num += e.num,
            _ => out.push(e),
        }
    }
    out
}

fn push_line(out: &mut Vec<u8>, line: &[u8]) {
    out.extend_from_slice(line);
    if !line.ends_with(b"\n") {
        out.push(b'\n');
    }
}

fn digits(n: usize) -> usize {
    n.to_string().len()
}

/// Quote a path for the porcelain `filename` and `previous` lines.
fn quoted(path: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    crate::quote::write_quoted(&mut out, path, true);
    out
}

pub struct Printer<'a, 'r> {
    pub sb: &'a Scoreboard<'r>,
    pub infos: &'a HashMap<usize, Info>,
    pub fmt: &'a Format,
    pub final_lines: &'a Blob,
    pub final_path: &'a [u8],
    /// Unique abbreviation of an object name to at least `len` digits.
    pub abbrev: &'a dyn Fn(&ObjectId, usize) -> String,
}

impl Printer<'_, '_> {
    fn commit_of(&self, e: &Entry) -> usize {
        self.sb.origins[e.suspect].commit
    }

    pub fn print(&self, entries: &[Entry], out: &mut impl Write) -> std::io::Result<()> {
        let mut buf = Vec::new();
        if self.fmt.porcelain {
            self.porcelain(entries, &mut buf);
        } else {
            self.default(entries, &mut buf);
        }
        out.write_all(&buf)
    }

    /// The commit information (`commit`: also author, committer and
    /// summary) and the file name of an entry.
    fn meta(&self, e: &Entry, commit: bool, out: &mut Vec<u8>) {
        let n = self.commit_of(e);
        let info = &self.infos[&n];
        let origin = &self.sb.origins[e.suspect];
        fn field(out: &mut Vec<u8>, k: &str, v: &[u8]) {
            out.extend_from_slice(k.as_bytes());
            out.push(b' ');
            out.extend_from_slice(v);
            out.push(b'\n');
        }
        if commit {
            field(out, "author", &info.author);
            field(out, "author-mail", &info.author_mail);
            field(out, "author-time", info.author_time.to_string().as_bytes());
            field(out, "author-tz", &info.author_tz);
            field(out, "committer", &info.committer);
            field(out, "committer-mail", &info.committer_mail);
            field(out, "committer-time", info.committer_time.to_string().as_bytes());
            field(out, "committer-tz", &info.committer_tz);
            field(out, "summary", &info.summary);
            if self.sb.nodes[n].boundary {
                out.extend_from_slice(b"boundary\n");
            }
        }
        if let Some(p) = origin.previous {
            let prev = &self.sb.origins[p];
            let mut v = self.sb.nodes[prev.commit].id.to_hex().into_bytes();
            v.push(b' ');
            v.extend_from_slice(&quoted(&prev.path));
            field(out, "previous", &v);
        }
        field(out, "filename", &quoted(&origin.path));
    }

    /// `--incremental`: the entries in the order they were found, without
    /// the lines.
    pub fn incremental(&self, batches: &[Vec<Entry>], out: &mut impl Write) -> std::io::Result<()> {
        let mut shown = std::collections::HashSet::new();
        for batch in batches {
            let mut buf = Vec::new();
            for e in batch {
                let n = self.commit_of(e);
                let hex = self.sb.nodes[n].id.to_hex();
                let head = format!("{hex} {} {} {}\n", e.s_lno + 1, e.lno + 1, e.num);
                buf.extend_from_slice(head.as_bytes());
                let first = shown.insert(n);
                self.meta(e, first, &mut buf);
            }
            out.write_all(&buf)?;
        }
        Ok(())
    }

    fn porcelain(&self, entries: &[Entry], out: &mut Vec<u8>) {
        let mut shown = std::collections::HashSet::new();
        for e in entries {
            let hex = self.sb.nodes[self.commit_of(e)].id.to_hex();
            for i in 0..e.num {
                out.extend_from_slice(hex.as_bytes());
                let head = if i == 0 {
                    format!(" {} {} {}\n", e.s_lno + i + 1, e.lno + i + 1, e.num)
                } else {
                    format!(" {} {}\n", e.s_lno + i + 1, e.lno + i + 1)
                };
                out.extend_from_slice(head.as_bytes());
                if self.fmt.line_porcelain || (i == 0 && shown.insert(self.commit_of(e))) {
                    self.meta(e, true, out);
                }
                out.push(b'\t');
                push_line(out, self.final_lines.line(e.lno + i));
            }
        }
    }

    fn date(&self, info: &Info) -> String {
        if self.fmt.raw_time {
            return format!(
                "{} {}",
                info.author_time,
                String::from_utf8_lossy(&info.author_tz)
            );
        }
        let time = u64::try_from(info.author_time).unwrap_or(0);
        rgit_date::show_date(
            time,
            tz_int(&info.author_tz),
            &self.fmt.date,
            &self.fmt.date_ctx,
        )
        .unwrap_or_default()
    }

    /// The width of the date column: dates are padded to the length of a
    /// date with two-digit day and hour (observed behaviour of git).
    fn date_width(&self) -> usize {
        if matches!(self.fmt.date.style, DateStyle::Relative) {
            return RELATIVE_WIDTH;
        }
        rgit_date::show_date(SAMPLE_TIME, 0, &self.fmt.date, &self.fmt.date_ctx)
            .map_or(0, |s| s.len())
    }

    fn who(&self, info: &Info) -> String {
        if self.fmt.show_email {
            String::from_utf8_lossy(&info.author_mail).into_owned()
        } else {
            String::from_utf8_lossy(&info.author).into_owned()
        }
    }

    fn default(&self, entries: &[Entry], out: &mut Vec<u8>) {
        let fmt = self.fmt;
        let hex_len = self.sb.nodes.first().map_or(40, |n| n.id.kind().hex_len());
        // Column widths.
        let mut name_w = 0;
        let mut author_w = 0;
        let mut date_w = 0;
        let mut max_lno = 0;
        let mut max_slno = 0;
        let mut abbrev = fmt.abbrev.min(hex_len);
        let mut boundary = false;
        let mut other_path = false;
        let mut seen = std::collections::HashSet::new();
        for e in entries {
            let n = self.commit_of(e);
            let origin = &self.sb.origins[e.suspect];
            other_path |= origin.path != self.final_path;
            name_w = name_w.max(UnicodeWidthStr::width(
                String::from_utf8_lossy(&origin.path).as_ref(),
            ));
            max_lno = max_lno.max(e.lno + e.num);
            max_slno = max_slno.max(e.s_lno + e.num);
            boundary |= self.sb.nodes[n].boundary;
            if seen.insert(n) {
                let info = &self.infos[&n];
                author_w = author_w.max(UnicodeWidthStr::width(self.who(info).as_str()));
                date_w = date_w.max(self.date(info).len());
                let id = &self.sb.nodes[n].id;
                if !id.is_null() && abbrev < hex_len {
                    abbrev = abbrev.max((self.abbrev)(id, fmt.abbrev).len());
                }
            }
        }
        if !fmt.raw_time {
            date_w = date_w.max(self.date_width());
        }
        if boundary && !fmt.blank_boundary && !fmt.annotate {
            abbrev += 1;
        }
        let abbrev = abbrev.min(hex_len);
        let show_name = fmt.show_name.unwrap_or(other_path);
        let lno_w = digits(max_lno);
        let slno_w = digits(max_slno);

        let mut prev: Option<usize> = None;
        for e in entries {
            let n = self.commit_of(e);
            let node = &self.sb.nodes[n];
            let hex = node.id.to_hex();
            let info = &self.infos[&n];
            let origin = &self.sb.origins[e.suspect];
            for i in 0..e.num {
                let lno = e.lno + i + 1;
                let line = self.final_lines.line(e.lno + i);
                if fmt.annotate {
                    // Padded to ten bytes, not columns.
                    let who = self.who(info);
                    let pad = " ".repeat(10usize.saturating_sub(who.len()));
                    let s = format!(
                        "{}\t({pad}{who}\t{}\t{})",
                        &hex[..abbrev],
                        self.date(info),
                        lno
                    );
                    out.extend_from_slice(s.as_bytes());
                    push_line(out, line);
                    continue;
                }
                let mut s = String::new();
                let color = match &fmt.coloring {
                    Coloring::None => None,
                    Coloring::Repeated(c) => (prev == Some(n)).then_some(c.as_str()),
                    Coloring::Age(first, steps) => Some(
                        steps
                            .iter()
                            .filter(|(t, _)| info.author_time > *t)
                            .map(|(_, c)| c.as_str())
                            .next_back()
                            .unwrap_or(first),
                    ),
                };
                prev = Some(n);
                if let Some(c) = color {
                    s.push_str(c);
                }
                if node.boundary && fmt.blank_boundary {
                    s.push_str(&" ".repeat(abbrev));
                } else if node.boundary {
                    s.push('^');
                    s.push_str(&hex[..abbrev - 1]);
                } else {
                    s.push_str(&hex[..abbrev]);
                }
                if show_name {
                    let p = String::from_utf8_lossy(&origin.path);
                    let pad = name_w.saturating_sub(UnicodeWidthStr::width(p.as_ref()));
                    s.push(' ');
                    s.push_str(&p);
                    s.push_str(&" ".repeat(pad));
                }
                if fmt.show_number {
                    s.push_str(&format!(" {:>slno_w$}", e.s_lno + i + 1));
                }
                if fmt.no_author {
                    s.push_str(&format!(" {lno:>lno_w$}) "));
                } else {
                    let who = self.who(info);
                    let pad = author_w.saturating_sub(UnicodeWidthStr::width(who.as_str()));
                    let date = self.date(info);
                    s.push_str(&format!(
                        " ({who}{} {date:<date_w$} {lno:>lno_w$}) ",
                        " ".repeat(pad)
                    ));
                }
                if color.is_some() {
                    s.push_str(RESET);
                }
                out.extend_from_slice(s.as_bytes());
                push_line(out, line);
            }
        }
    }
}

const RESET: &str = "\x1b[m";

/// Width of relative dates (the longest form, `4 years, 11 months ago`).
const RELATIVE_WIDTH: usize = 22;

/// 2001-09-20 15:50:00 UTC: a time with two-digit day and hour.
const SAMPLE_TIME: u64 = 1_001_001_000;
