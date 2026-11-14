// SPDX-License-Identifier: MIT

//! Parsing patches: git's extended diff headers and traditional unified
//! diffs (`Documentation/git-apply.adoc`, `Documentation/diff-generate-patch.adoc`).
//!
//! Spec gaps, decided by running the reference `git apply`:
//!
//! - Lines before the first header (mail headers, commit messages) are
//!   skipped. A `@@ -` line outside a patch is an error.
//! - A `diff --git` line followed by no extended header line is not a
//!   patch.
//! - Traditional patches: a name ends at a tab, else before a trailing
//!   timestamp. When `-p` is not given, it is guessed from the first
//!   traditional patch (0 when its new name has no `/`, else 1). Of two
//!   different names the shorter wins, the new one on a tie. A timestamp
//!   at the Unix epoch (in any time zone) marks a created or deleted file.
//! - Without `-p`, a name with fewer components than the default is used
//!   as it is; with an explicit `-p` it is an error.

use crate::messages as msg;
use crate::{FilePatch, Hunk, HunkLine, LineKind};

/// Settings that influence how names are read.
#[derive(Debug, Clone)]
pub struct ParseOptions {
    /// `-p<n>`: leading components removed from traditional names.
    pub p_value: usize,
    /// Whether `-p` was given (otherwise it is guessed once).
    pub p_explicit: bool,
    /// `--directory`: prepended to every name (normalized, ends in `/`
    /// unless empty).
    pub root: Vec<u8>,
    /// `--recount`: ignore the line counts of hunk headers.
    pub recount: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            p_value: 1,
            p_explicit: false,
            root: Vec::new(),
            recount: false,
        }
    }
}

/// A parse failure; the message lacks the `error: ` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// The result of parsing one input: the patches (or the first error) and
/// the warnings printed on the way (without `warning: `), which come
/// before the error.
#[derive(Debug)]
pub struct Parsed {
    pub patches: Result<Vec<FilePatch>, ParseError>,
    pub warnings: Vec<String>,
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn chomp(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// Splits `data` into lines, each with its line feed (the last one may
/// lack it).
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            out.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// Unquotes a C-style quoted name at the start of `s`; returns it and the
/// rest after the closing quote.
pub fn unquote_prefix(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let body = s.strip_prefix(b"\"")?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let c = body[i];
        i += 1;
        match c {
            b'"' => return Some((out, &body[i..])),
            b'\\' => {
                let e = *body.get(i)?;
                i += 1;
                let b = match e {
                    b'a' => 7,
                    b'b' => 8,
                    b'f' => 12,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 11,
                    b'\\' | b'"' => e,
                    b'0'..=b'3' => {
                        let d2 = *body.get(i)?;
                        let d3 = *body.get(i + 1)?;
                        if !(b'0'..=b'7').contains(&d2) || !(b'0'..=b'7').contains(&d3) {
                            return None;
                        }
                        i += 2;
                        ((e - b'0') << 6) | ((d2 - b'0') << 3) | (d3 - b'0')
                    }
                    _ => return None,
                };
                out.push(b);
            }
            b'\n' => return None,
            _ => out.push(c),
        }
    }
    None
}

/// Removes `p` leading components (each up to and including a `/`).
fn strip_components(name: &[u8], p: usize) -> Option<&[u8]> {
    let mut s = name;
    for _ in 0..p {
        let i = s.iter().position(|&b| b == b'/')?;
        s = &s[i + 1..];
    }
    (!s.is_empty()).then_some(s)
}

fn digits(s: &[u8], n: usize) -> Option<i64> {
    if s.len() < n || !s[..n].iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(
        s[..n]
            .iter()
            .fold(0i64, |a, &d| a * 10 + i64::from(d - b'0')),
    )
}

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// A timestamp as GNU diff writes it after a name:
/// `YYYY-MM-DD HH:MM:SS[.frac][ ][+-]HH[:]MM`. Returns whether it is the
/// Unix epoch.
fn parse_timestamp(s: &[u8]) -> Option<bool> {
    let s = s.trim_ascii();
    let year = digits(s, 4)?;
    if s.get(4) != Some(&b'-') || s.get(7) != Some(&b'-') {
        return None;
    }
    let month = digits(&s[5..], 2)?;
    let day = digits(&s[8..], 2)?;
    if !matches!(s.get(10), Some(b' ' | b'T')) {
        return None;
    }
    let t = &s[11..];
    let hour = digits(t, 2)?;
    if t.get(2) != Some(&b':') || t.get(5) != Some(&b':') {
        return None;
    }
    let min = digits(&t[3..], 2)?;
    let sec = digits(&t[6..], 2)?;
    let mut r = &t[8..];
    let mut frac_zero = true;
    if let Some(rest) = r.strip_prefix(b".") {
        let n = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 {
            return None;
        }
        frac_zero = rest[..n].iter().all(|&b| b == b'0');
        r = &rest[n..];
    }
    let r = r.trim_ascii_start();
    let mut offset = 0;
    if !r.is_empty() {
        let sign = match r[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let r = &r[1..];
        let hh = digits(r, 2)?;
        let rest = r[2..].strip_prefix(b":").unwrap_or(&r[2..]);
        let mm = digits(rest, 2)?;
        if rest.len() != 2 {
            return None;
        }
        offset = sign * (hh * 3600 + mm * 60);
    }
    let local = days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec;
    Some(frac_zero && local - offset == 0)
}

/// A name from a `---`/`+++` line or the `diff` line of a traditional
/// patch, before `-p` is applied.
#[derive(Debug, Clone)]
struct RawName {
    name: Vec<u8>,
    /// For a name that starts with a quoted part: the unquoted text after
    /// the closing quote (`"a/"sub/f` is `a/` plus `sub/f`).
    rest: Option<Vec<u8>>,
    /// `/dev/null`, or a timestamp at the epoch.
    null: bool,
}

impl RawName {
    /// Removes `p` leading components. A quoted part counts first; when
    /// it has enough slashes, the rest of the name is ignored (the
    /// reference git does the same, even when that leaves nothing).
    fn strip(&self, p: usize) -> Option<Vec<u8>> {
        let Some(rest) = &self.rest else {
            return strip_components(&self.name, p).map(<[u8]>::to_vec);
        };
        let slashes = self.name.iter().filter(|&&b| b == b'/').count();
        if p <= slashes {
            let mut s = self.name.as_slice();
            for _ in 0..p {
                let i = s.iter().position(|&b| b == b'/')?;
                s = &s[i + 1..];
            }
            return Some(s.to_vec());
        }
        strip_components(rest, p - slashes).map(<[u8]>::to_vec)
    }
}

/// Reads the name at the start of `s` (the text after `--- `).
fn raw_name(s: &[u8]) -> Option<RawName> {
    let s = chomp(s);
    let s = s.strip_suffix(b"\r").unwrap_or(s);
    if s.first() == Some(&b'"') {
        let (name, after) = unquote_prefix(s)?;
        let end = after
            .iter()
            .position(|&b| b == b'\t' || b == b' ')
            .unwrap_or(after.len());
        let epoch = parse_timestamp(&after[end..]).unwrap_or(false);
        let null = name == b"/dev/null" || epoch;
        return Some(RawName {
            name,
            rest: Some(after[..end].to_vec()),
            null,
        });
    }
    // A name ends at a tab followed by a timestamp (or nothing); names
    // may contain tabs themselves.
    let tab_end = s
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| **b == b'\t')
        .map(|(i, _)| i)
        .find(|&i| s[i + 1..].trim_ascii().is_empty() || parse_timestamp(&s[i + 1..]).is_some())
        .or_else(|| s.iter().position(|&b| b == b'\t'));
    let (name, ts): (&[u8], &[u8]) = match tab_end {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => split_trailing_timestamp(s),
    };
    let name = name.trim_ascii_end();
    if name.is_empty() {
        return None;
    }
    let epoch = parse_timestamp(ts).unwrap_or(false);
    Some(RawName {
        null: name == b"/dev/null" || epoch,
        name: name.to_vec(),
        rest: None,
    })
}

/// A name followed by a space-separated timestamp (a patch whose tabs
/// were turned into spaces): the name and the timestamp.
fn split_trailing_timestamp(s: &[u8]) -> (&[u8], &[u8]) {
    for (i, &b) in s.iter().enumerate() {
        if b == b' ' && parse_timestamp(&s[i + 1..]).is_some() && !s[..i].trim_ascii().is_empty() {
            return (&s[..i], &s[i + 1..]);
        }
    }
    (s, b"")
}

/// The parser state for one input.
struct Parser<'a, 'o> {
    lines: Vec<&'a [u8]>,
    source: &'a str,
    opts: &'o mut ParseOptions,
    warnings: Vec<String>,
}

fn starts_with_word(line: &[u8], word: &[u8]) -> bool {
    line.starts_with(word)
}

/// `@@ -a[,b] +c[,d] @@`: the four numbers, or `None` when the line is
/// not a valid hunk header (overflow included).
fn parse_hunk_header(line: &[u8]) -> Option<(usize, usize, usize, usize)> {
    fn number(s: &[u8]) -> Option<(usize, &[u8])> {
        let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 {
            return None;
        }
        let mut v: usize = 0;
        for &d in &s[..n] {
            v = v.checked_mul(10)?.checked_add(usize::from(d - b'0'))?;
        }
        Some((v, &s[n..]))
    }
    fn range(s: &[u8]) -> Option<(usize, usize, &[u8])> {
        let (start, rest) = number(s)?;
        if let Some(rest) = rest.strip_prefix(b",") {
            let (count, rest) = number(rest)?;
            Some((start, count, rest))
        } else {
            Some((start, 1, rest))
        }
    }
    let s = line.strip_prefix(b"@@ -")?;
    let (os, oc, rest) = range(s)?;
    let rest = rest.strip_prefix(b" +")?;
    let (ns, nc, rest) = range(rest)?;
    rest.starts_with(b" @@").then_some((os, oc, ns, nc))
}

impl Parser<'_, '_> {
    fn err(&self, m: String) -> ParseError {
        ParseError(m)
    }

    fn line_text(&self, i: usize) -> String {
        lossy(chomp(self.lines[i]))
    }

    /// `-p` stripping plus `--directory`.
    fn finish_name(&self, name: &[u8], p: usize) -> Option<Vec<u8>> {
        let stripped = strip_components(name, p)?;
        Some(self.rooted(stripped))
    }

    fn rooted(&self, name: &[u8]) -> Vec<u8> {
        let mut out = self.opts.root.clone();
        out.extend_from_slice(name);
        out
    }

    /// Name from a `---`/`+++` line with the current `-p`.
    fn header_line_name(&self, rest: &[u8]) -> Option<(Option<Vec<u8>>, bool)> {
        let raw = raw_name(rest)?;
        if raw.name == b"/dev/null" {
            return Some((None, true));
        }
        let name = raw.strip(self.opts.p_value).map(|n| self.rooted(&n));
        Some((name, raw.null))
    }

    /// Name from `rename from`/`copy to` lines (no `a/` prefix: one
    /// component less is removed).
    fn extended_name(&self, rest: &[u8]) -> Option<Vec<u8>> {
        let rest = chomp(rest);
        let name = if rest.first() == Some(&b'"') {
            let (n, tail) = unquote_prefix(rest)?;
            if !tail.trim_ascii().is_empty() {
                return None;
            }
            n
        } else {
            rest.to_vec()
        };
        self.finish_name(&name, self.opts.p_value.saturating_sub(1))
    }

    /// The name both sides of `diff --git a/X b/X` agree on, if any.
    fn git_header_name(&self, rest: &[u8]) -> Option<Vec<u8>> {
        let rest = chomp(rest);
        let p = self.opts.p_value;
        if rest.first() == Some(&b'"') {
            let (first, tail) = unquote_prefix(rest)?;
            let second = tail.strip_prefix(b" ")?;
            let second = if second.first() == Some(&b'"') {
                let (s, t) = unquote_prefix(second)?;
                if !t.is_empty() {
                    return None;
                }
                s
            } else {
                second.to_vec()
            };
            let a = strip_components(&first, p)?;
            let b = strip_components(&second, p)?;
            return (a == b).then(|| self.finish_name(a, 0)).flatten();
        }
        // Unquoted first name; the second may be quoted.
        if let Some(q) = rest.windows(2).position(|w| w == b" \"") {
            let (second, tail) = unquote_prefix(&rest[q + 1..])?;
            if tail.is_empty() {
                let a = strip_components(&rest[..q], p)?;
                let b = strip_components(&second, p)?;
                if a == b {
                    return self.finish_name(a, 0);
                }
            }
        }
        // Both unquoted: find the space that splits the line into two
        // names that agree after stripping.
        for (i, &c) in rest.iter().enumerate() {
            if c != b' ' {
                continue;
            }
            let (a, b) = (&rest[..i], &rest[i + 1..]);
            if let (Some(x), Some(y)) = (strip_components(a, p), strip_components(b, p)) {
                if x == y {
                    return self.finish_name(x, 0);
                }
            }
        }
        None
    }

    fn parse_mode(&self, rest: &[u8], i: usize) -> Result<u32, ParseError> {
        let text = chomp(rest);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        let ok =
            !text.is_empty() && text.iter().all(|b| (b'0'..=b'7').contains(b)) && text.len() <= 7;
        if !ok {
            return Err(self.err(msg::invalid_mode(self.source, i + 1, &lossy(rest))));
        }
        Ok(text.iter().fold(0u32, |a, &d| a * 8 + u32::from(d - b'0')))
    }

    fn parse_percent(rest: &[u8]) -> Option<u32> {
        let t = chomp(rest);
        let n = t.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 || n > 6 {
            return None;
        }
        Some(
            t[..n]
                .iter()
                .fold(0u32, |a, &d| a * 10 + u32::from(d - b'0')),
        )
    }

    /// Parses the git header at line `i`. Returns the patch (without
    /// hunks) and the index of the first line after the header; `None`
    /// when the `diff --git` line has no extended header lines.
    fn git_header(&mut self, i: usize) -> Result<Option<(FilePatch, usize, bool)>, ParseError> {
        let def = self.git_header_name(&self.lines[i][b"diff --git ".len()..]);
        let mut p = FilePatch {
            is_git: true,
            ..FilePatch::default()
        };
        let mut is_new = false;
        let mut is_delete = false;
        let mut saw_old_line = false;
        let mut saw_new_line = false;
        let mut any = false;
        // Lines that say what kind of change this is (creation, deletion,
        // rename, copy): only one kind may appear.
        let mut kind_lines: Vec<usize> = Vec::new();
        let mut j = i + 1;
        while j < self.lines.len() {
            let line = self.lines[j];
            if let Some(rest) = line.strip_prefix(b"--- ") {
                saw_old_line = true;
                let (name, null) = self.header_line_name(rest).unwrap_or((None, false));
                if null && name.is_none() {
                    if is_delete {
                        return Err(self.err(msg::inconsistent_old(self.source, j + 1)));
                    }
                    is_new = true;
                } else {
                    if is_new {
                        return Err(self.err(msg::expected_dev_null(self.source, j + 1)));
                    }
                    if is_delete {
                        if let (Some(d), Some(n)) = (&def, &name) {
                            if d != n {
                                return Err(self.err(msg::inconsistent_old(self.source, j + 1)));
                            }
                        }
                    }
                    // The name of the `diff --git` line wins when known.
                    p.old_name = def.clone().or(name);
                }
            } else if let Some(rest) = line.strip_prefix(b"+++ ") {
                saw_new_line = true;
                let (name, null) = self.header_line_name(rest).unwrap_or((None, false));
                if null && name.is_none() && is_delete {
                    // expected
                } else if is_delete {
                    return Err(self.err(msg::expected_dev_null(self.source, j + 1)));
                } else if null && name.is_none() {
                    if is_new {
                        return Err(self.err(msg::inconsistent_new(self.source, j + 1)));
                    }
                    is_delete = true;
                } else {
                    if is_new {
                        if let (Some(d), Some(n)) = (&def, &name) {
                            if d != n {
                                return Err(self.err(msg::inconsistent_new(self.source, j + 1)));
                            }
                        }
                    }
                    p.new_name = def.clone().or(name);
                }
            } else if let Some(rest) = line.strip_prefix(b"old mode ") {
                p.old_mode = Some(self.parse_mode(rest, j)?);
            } else if let Some(rest) = line.strip_prefix(b"new mode ") {
                p.new_mode = Some(self.parse_mode(rest, j)?);
            } else if let Some(rest) = line.strip_prefix(b"deleted file mode ") {
                p.old_mode = Some(self.parse_mode(rest, j)?);
                is_delete = true;
                kind_lines.push(j + 1);
            } else if let Some(rest) = line.strip_prefix(b"new file mode ") {
                p.new_mode = Some(self.parse_mode(rest, j)?);
                is_new = true;
                kind_lines.push(j + 1);
            } else if let Some(rest) = line
                .strip_prefix(b"rename from ")
                .or_else(|| line.strip_prefix(b"rename old "))
            {
                p.is_rename = true;
                kind_lines.push(j + 1);
                p.old_name = self.extended_name(rest);
            } else if let Some(rest) = line
                .strip_prefix(b"rename to ")
                .or_else(|| line.strip_prefix(b"rename new "))
            {
                p.is_rename = true;
                p.new_name = self.extended_name(rest);
            } else if let Some(rest) = line.strip_prefix(b"copy from ") {
                p.is_copy = true;
                kind_lines.push(j + 1);
                p.old_name = self.extended_name(rest);
            } else if let Some(rest) = line.strip_prefix(b"copy to ") {
                p.is_copy = true;
                p.new_name = self.extended_name(rest);
            } else if let Some(rest) = line.strip_prefix(b"similarity index ") {
                p.score = Self::parse_percent(rest);
            } else if let Some(rest) = line.strip_prefix(b"dissimilarity index ") {
                p.score = Self::parse_percent(rest);
                p.is_rewrite = true;
            } else if let Some(rest) = line.strip_prefix(b"index ") {
                self.index_line(&mut p, rest);
            } else {
                break;
            }
            any = true;
            j += 1;
        }
        if !any {
            return Ok(None);
        }
        let kinds = usize::from(is_new)
            + usize::from(is_delete)
            + usize::from(p.is_rename)
            + usize::from(p.is_copy);
        if kinds > 1 {
            return Err(self.err(msg::inconsistent_lines(kind_lines[0], kind_lines[1])));
        }
        p.is_new = Some(is_new);
        p.is_delete = Some(is_delete);
        let named_by_lines =
            saw_old_line || saw_new_line || p.old_name.is_some() || p.new_name.is_some();
        if !named_by_lines {
            let Some(def) = def.clone() else {
                return Err(self.err(msg::lacks_filename_removing(
                    self.opts.p_value,
                    self.source,
                    j + 1,
                )));
            };
            if !is_new {
                p.old_name = Some(def.clone());
            }
            if !is_delete {
                p.new_name = Some(def);
            }
        } else if p.old_name.is_none() && p.new_name.is_none() && def.is_none() {
            return Err(self.err(msg::lacks_filename_removing(
                self.opts.p_value,
                self.source,
                j + 1,
            )));
        }
        if (!is_new && p.old_name.is_none()) || (!is_delete && p.new_name.is_none()) {
            return Err(self.err(msg::lacks_filename(self.source, j + 1)));
        }
        Ok(Some((p, j, saw_old_line || saw_new_line)))
    }

    fn index_line(&self, p: &mut FilePatch, rest: &[u8]) {
        let rest = chomp(rest);
        let (ids, mode) = match rest.iter().position(|&b| b == b' ') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let Some(dots) = ids.windows(2).position(|w| w == b"..") else {
            return;
        };
        p.old_oid = Some(lossy(&ids[..dots]));
        p.new_oid = Some(lossy(&ids[dots + 2..]));
        if let Some(m) = mode {
            if !m.is_empty() && m.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let v = m.iter().fold(0u32, |a, &d| a * 8 + u32::from(d - b'0'));
                if p.old_mode.is_none() {
                    p.old_mode = Some(v);
                }
                if p.new_mode.is_none() {
                    p.new_mode = Some(v);
                }
            }
        }
    }

    /// Parses the traditional header at `i` (`---` and `+++` lines).
    fn traditional_header(&mut self, i: usize) -> Result<FilePatch, ParseError> {
        let first = raw_name(&self.lines[i][4..]);
        let second = raw_name(&self.lines[i + 1][4..]);
        if !self.opts.p_explicit {
            // Guess -p from the first traditional patch.
            let probe = match (&second, &first) {
                (Some(s), _) if s.name != b"/dev/null" => Some(&s.name),
                (_, Some(f)) if f.name != b"/dev/null" => Some(&f.name),
                _ => None,
            };
            if let Some(n) = probe {
                self.opts.p_value = usize::from(n.contains(&b'/'));
                self.opts.p_explicit = true;
            }
        }
        let p_value = self.opts.p_value;
        let name_of = |r: &Option<RawName>| -> Option<Vec<u8>> {
            let r = r.as_ref()?;
            if r.name == b"/dev/null" {
                return None;
            }
            r.strip(p_value).map(|n| self.rooted(&n))
        };
        let old = name_of(&first);
        let new = name_of(&second);
        let mut p = FilePatch::default();
        let old_null = first.as_ref().is_some_and(|r| r.null);
        let new_null = second.as_ref().is_some_and(|r| r.null);
        if old_null && !new_null {
            p.is_new = Some(true);
            p.new_name = new;
            if p.new_name.is_none() {
                return Err(self.err(msg::no_filename(self.source, i + 1)));
            }
        } else if new_null && !old_null {
            p.is_delete = Some(true);
            p.old_name = old;
            if p.old_name.is_none() {
                return Err(self.err(msg::no_filename(self.source, i + 1)));
            }
        } else {
            let name = match (old, new) {
                (None, None) => return Err(self.err(msg::no_filename(self.source, i + 1))),
                (Some(o), None) => o,
                (None, Some(n)) => n,
                (Some(o), Some(n)) => {
                    if o.len() < n.len() {
                        o
                    } else {
                        n
                    }
                }
            };
            p.old_name = Some(name.clone());
            p.new_name = Some(name);
        }
        Ok(p)
    }

    /// Parses the hunks starting at line `j`; returns the line after them.
    fn hunks(&mut self, p: &mut FilePatch, mut j: usize) -> Result<usize, ParseError> {
        let n = self.lines.len();
        while j < n && self.lines[j].starts_with(b"@@ -") {
            let header = self.lines[j];
            let Some((os, mut oc, ns, mut nc)) = parse_hunk_header(header) else {
                return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
            };
            let start = j;
            j += 1;
            if self.opts.recount {
                if let Some((o, nn)) = self.recount(j) {
                    oc = o;
                    nc = nn;
                }
            }
            let mut hunk = Hunk {
                old_start: os,
                old_count: oc,
                new_start: ns,
                new_count: nc,
                ..Hunk::default()
            };
            let (mut old_left, mut new_left) = (oc, nc);
            while old_left > 0 || new_left > 0 {
                if j >= n {
                    return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
                }
                let line = self.lines[j];
                let first = line.first().copied();
                match first {
                    Some(b' ') | Some(b'\n') => {
                        if old_left == 0 || new_left == 0 {
                            return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
                        }
                        old_left -= 1;
                        new_left -= 1;
                        let text = if first == Some(b'\n') {
                            line
                        } else {
                            &line[1..]
                        };
                        hunk.lines.push(HunkLine {
                            kind: LineKind::Context,
                            text: text.to_vec(),
                        });
                    }
                    Some(b'-') => {
                        if old_left == 0 {
                            return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
                        }
                        old_left -= 1;
                        hunk.lines.push(HunkLine {
                            kind: LineKind::Remove,
                            text: line[1..].to_vec(),
                        });
                    }
                    Some(b'+') => {
                        if new_left == 0 {
                            return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
                        }
                        new_left -= 1;
                        hunk.lines.push(HunkLine {
                            kind: LineKind::Add,
                            text: line[1..].to_vec(),
                        });
                    }
                    Some(b'\\') => {
                        let Some(last) = hunk.lines.last_mut() else {
                            return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
                        };
                        if last.text.ends_with(b"\n") {
                            last.text.pop();
                        }
                    }
                    _ => return Err(self.err(msg::corrupt_patch(self.source, j + 1))),
                }
                j += 1;
            }
            if j < n && self.lines[j].starts_with(b"\\") {
                if let Some(last) = hunk.lines.last_mut() {
                    if last.text.ends_with(b"\n") {
                        last.text.pop();
                    }
                }
                j += 1;
            }
            hunk.raw = self.lines[start..j].concat();
            hunk.finish();
            // A hunk that changes nothing is not a hunk (unless the counts
            // were recomputed).
            if !self.opts.recount && hunk.lines.iter().all(|l| l.kind == LineKind::Context) {
                return Err(self.err(msg::corrupt_patch(self.source, j + 1)));
            }
            p.added += hunk
                .lines
                .iter()
                .filter(|l| l.kind == LineKind::Add)
                .count();
            p.deleted += hunk
                .lines
                .iter()
                .filter(|l| l.kind == LineKind::Remove)
                .count();
            p.hunks.push(hunk);
        }
        Ok(j)
    }

    /// `--recount`: the old and new line counts of the hunk body starting
    /// at `j`, or `None` (with a warning) when an unexpected line shows up.
    fn recount(&mut self, mut j: usize) -> Option<(usize, usize)> {
        let (mut old, mut new) = (0, 0);
        while j < self.lines.len() {
            let line = self.lines[j];
            match line.first() {
                Some(b' ') | Some(b'\n') => {
                    old += 1;
                    new += 1;
                }
                Some(b'-') => old += 1,
                Some(b'+') => new += 1,
                Some(b'\\') => {}
                _ if line.starts_with(b"@@ ") || line.starts_with(b"diff ") => break,
                _ => {
                    self.warnings.push(msg::recount_unexpected(&lossy(line)));
                    return None;
                }
            }
            j += 1;
        }
        Some((old, new))
    }

    /// `GIT binary patch` or `Binary files ... differ` at `j`: marks the
    /// patch binary and skips its data.
    fn binary(&mut self, p: &mut FilePatch, mut j: usize) -> usize {
        let n = self.lines.len();
        if j >= n {
            return j;
        }
        let line = chomp(self.lines[j]);
        if line == b"GIT binary patch" {
            p.binary = true;
            j += 1;
            // Up to two blocks (forward and reverse), each `literal N` or
            // `delta N` followed by data lines and an empty line.
            for _ in 0..2 {
                if j >= n {
                    break;
                }
                let l = chomp(self.lines[j]);
                if !(l.starts_with(b"literal ") || l.starts_with(b"delta ")) {
                    break;
                }
                j += 1;
                while j < n && !chomp(self.lines[j]).is_empty() {
                    j += 1;
                }
                if j < n {
                    j += 1;
                }
            }
        } else if line.starts_with(b"Binary files ") && line.ends_with(b" differ") {
            p.binary = true;
            j += 1;
        }
        j
    }

    fn run(&mut self) -> Result<Vec<FilePatch>, ParseError> {
        let mut out = Vec::new();
        let n = self.lines.len();
        let mut i = 0;
        while i < n {
            let line = self.lines[i];
            if line.starts_with(b"@@ -") && parse_hunk_header(line).is_some() {
                return Err(self.err(msg::fragment_without_header(
                    self.source,
                    i + 1,
                    &self.line_text(i),
                )));
            }
            if starts_with_word(line, b"diff --git ") {
                let Some((mut p, j, had_lines)) = self.git_header(i)? else {
                    i += 1;
                    continue;
                };
                let mut k = self.hunks(&mut p, j)?;
                if p.hunks.is_empty() {
                    k = self.binary(&mut p, k);
                }
                if p.hunks.is_empty()
                    && !p.binary
                    && had_lines
                    && !p.has_metadata()
                    && k < n
                    && !self.lines[k].starts_with(b"diff ")
                {
                    // Reported only when the patch is checked or applied.
                    p.garbage = Some(msg::only_garbage(self.source, k + 1));
                }
                self.check_contents(&p)?;
                out.push(p);
                i = k;
                continue;
            }
            if line.starts_with(b"--- ")
                && i + 2 < n
                && self.lines[i + 1].starts_with(b"+++ ")
                && self.lines[i + 2].starts_with(b"@@ -")
            {
                let mut p = self.traditional_header(i)?;
                let k = self.hunks(&mut p, i + 2)?;
                if p.is_new.is_none() && (p.deleted > 0 || p.hunks.len() > 1) {
                    p.is_new = Some(false);
                }
                if p.is_delete.is_none() && (p.added > 0 || p.hunks.len() > 1) {
                    p.is_delete = Some(false);
                }
                self.check_contents(&p)?;
                out.push(p);
                i = k;
                continue;
            }
            i += 1;
        }
        Ok(out)
    }

    /// A creation must not need old lines, a deletion must not leave any.
    fn check_contents(&self, p: &FilePatch) -> Result<(), ParseError> {
        if p.is_new == Some(true) && p.deleted > 0 {
            let name = p.new_name.as_deref().unwrap_or_default();
            return Err(self.err(msg::new_depends_on_old(&lossy(name))));
        }
        if p.is_delete == Some(true) && p.added > 0 {
            let name = p.old_name.as_deref().unwrap_or_default();
            return Err(self.err(msg::deleted_has_contents(&lossy(name))));
        }
        Ok(())
    }
}

/// Parses all patches in `input`. `source` names the input in messages
/// (`<stdin>` or the file name as given).
///
/// The first problem, as git reports it, ends parsing.
pub fn parse(input: &[u8], source: &str, opts: &mut ParseOptions) -> Parsed {
    let mut parser = Parser {
        lines: split_lines(input),
        source,
        opts,
        warnings: Vec::new(),
    };
    let patches = parser.run();
    let warnings = std::mem::take(&mut parser.warnings);
    Parsed { patches, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(input: &str) -> FilePatch {
        let mut o = ParseOptions::default();
        let mut p = parse(input.as_bytes(), "t", &mut o).patches.unwrap();
        assert_eq!(p.len(), 1);
        p.remove(0)
    }

    #[test]
    fn timestamps() {
        assert_eq!(
            parse_timestamp(b"1970-01-01 00:00:00.000000000 +0000"),
            Some(true)
        );
        assert_eq!(
            parse_timestamp(b"1969-12-31 16:00:00.000000000 -0800"),
            Some(true)
        );
        assert_eq!(parse_timestamp(b"1970-01-01 09:00:00 +0900"), Some(true));
        assert_eq!(parse_timestamp(b"1969-12-31 16:00:00 -08:00"), Some(true));
        assert_eq!(
            parse_timestamp(b"1982-09-16 07:00:00.000000000 -0800"),
            Some(false)
        );
        assert_eq!(parse_timestamp(b"nonsense"), None);
    }

    #[test]
    fn git_names() {
        let p = one("diff --git a/x y/f b/x y/f\nold mode 100644\nnew mode 100755\n");
        assert_eq!(p.old_name.as_deref(), Some(&b"x y/f"[..]));
        assert_eq!(p.new_mode, Some(0o100755));
        let p = one("diff --git \"a/q\\157\" \"b/q\\157\"\nnew file mode 100644\n");
        assert_eq!(p.new_name.as_deref(), Some(&b"qo"[..]));
        assert_eq!(p.is_new, Some(true));
    }

    #[test]
    fn traditional_names() {
        let p = one("--- a/f.orig\t2010-01-01\n+++ b/f\t2010-01-01\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(p.new_name.as_deref(), Some(&b"f"[..]));
        let p = one(
            "--- a/post image.txt 1969-12-31 16:00:00 -08:00\n+++ b/post image.txt 2005-04-12 02:14:06 -07:00\n@@ -0,0 +1 @@\n+x\n",
        );
        assert_eq!(p.new_name.as_deref(), Some(&b"post image.txt"[..]));
        assert_eq!(p.is_new, Some(true));
    }

    #[test]
    fn hunks_and_no_newline() {
        let p =
            one("--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n");
        assert_eq!(p.hunks.len(), 1);
        assert_eq!(p.hunks[0].lines[1].text, b"b");
        assert_eq!(p.hunks[0].lines[2].text, b"c\n");
        assert_eq!((p.added, p.deleted), (1, 1));
    }

    #[test]
    fn errors() {
        let mut o = ParseOptions::default();
        let e = parse(b"@@ -1 +1 @@\n-a\n+b\n", "f.patch", &mut o)
            .patches
            .unwrap_err();
        assert_eq!(
            e.0,
            "patch fragment without header at f.patch:1: @@ -1 +1 @@"
        );
        let e = parse(b"diff --git a/f b/f\nold mode 10x644\n", "m", &mut o)
            .patches
            .unwrap_err();
        assert_eq!(e.0, "invalid mode at m:2: 10x644\n");
    }
}
