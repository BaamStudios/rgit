// SPDX-License-Identifier: MIT
//! Editing configuration files in place: set, add, replace-all, unset,
//! unset-all, rename-section and remove-section.
//!
//! The functions here work on the bytes of one file and return the new
//! bytes; [`crate::write`] wraps them with `config.lock`. Everything that is
//! not edited is copied unchanged, so comments, blank lines, indentation,
//! CR LF line ends and the order of other lines survive.
//!
//! The placement and formatting rules were inferred from the `git` binary
//! (2.56) with differential tests and from the expectations in git's test
//! suite (t1300):
//!
//! - A new or replaced variable is written as `<TAB>name = value` on its own
//!   line, with the name spelled as the user gave it. A replaced variable
//!   takes over the whole line of the old one, including its indentation and
//!   comment; if the old one shared a line with its section header, the
//!   header stays and the new line follows it.
//! - A new variable goes after the last variable of the last section with
//!   the right name, or directly after that section's header if it is
//!   empty. Without such a section, a new section is appended at the end.
//! - `--replace-all` removes every match and writes the new value where the
//!   last match was.
//! - Unsetting the last variable of a section also removes the section
//!   header, unless the section contains a comment (including comments
//!   directly above the header). Neighbouring empty headers of the same
//!   section and the blank lines around them go too.
//! - Values are quoted when they start or end with a space or contain `;`,
//!   `#` or CR; `\`, `"`, LF and TAB are escaped.
//! - Renaming and removing sections work line by line on lines that start
//!   with a section header, as git does.

use bstr::{BString, ByteSlice};

use crate::key::{Key, KeyError};
use crate::messages;
use crate::parse::{File, Item, SectionHeader, canonical_name};

/// Lines this long (without the line feed) make rename/remove-section fail.
pub const MAX_SECTION_LINE: usize = 512 * 1024 - 1;

/// Why an edit failed. `Display` gives git's message without the prefix.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The variable name is invalid (exit code 1, or 2 for a missing
    /// section or variable name, see [`EditError::exit_code`]).
    #[error(transparent)]
    Key(#[from] KeyError),
    /// The file does not parse. git reports `bad config line N in file X`.
    #[error("bad config line {line}")]
    Syntax {
        /// 1-based line.
        line: usize,
    },
    /// More than one value matched where only one may be changed.
    #[error("{}", messages::multiple_values(.key.as_ref()))]
    MultipleValues {
        /// Canonical variable name.
        key: BString,
    },
    /// Nothing matched the variable to unset.
    #[error("no such variable")]
    NothingToUnset,
    /// The value pattern is not a valid regular expression.
    #[error("{}", messages::invalid_pattern(.0.as_ref()))]
    InvalidPattern(BString),
    /// `--comment` with a line feed.
    #[error("{}", messages::multiline_comment(.0.as_ref()))]
    MultilineComment(BString),
    /// The new section name of rename-section is invalid.
    #[error("{}", messages::invalid_section_name(.0.as_ref()))]
    InvalidSectionName(BString),
    /// rename-section or remove-section found no such section.
    #[error("{}", messages::no_such_section(.0.as_ref()))]
    NoSuchSection(BString),
    /// rename-section or remove-section met a line that is too long.
    #[error("overly long line on line {line}")]
    OverlongLine {
        /// 1-based line.
        line: usize,
    },
}

impl EditError {
    /// The exit code `git config` uses for this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            EditError::Key(KeyError::NoSection(_) | KeyError::NoVariable(_)) => 2,
            EditError::Key(_) => 1,
            EditError::Syntax { .. } => 3,
            EditError::MultipleValues { .. } | EditError::NothingToUnset => 5,
            EditError::InvalidPattern(_) => 6,
            EditError::MultilineComment(_) | EditError::NoSuchSection(_) => 128,
            EditError::InvalidSectionName(_) | EditError::OverlongLine { .. } => 255,
        }
    }
}

/// Which existing values an edit applies to (`--value`, `--fixed-value`,
/// `--append`).
#[derive(Debug, Clone, Default)]
pub enum ValuePattern {
    /// Every value.
    #[default]
    Any,
    /// No value (`--add`/`--append`: always add a new line).
    Never,
    /// Values matching an extended regular expression; `negate` for a
    /// pattern given with a leading `!`.
    Regex {
        /// The compiled expression.
        regex: regex::bytes::Regex,
        /// Match values that do *not* match.
        negate: bool,
    },
    /// Values equal to this string (`--fixed-value`).
    Exact(BString),
}

impl ValuePattern {
    /// Compile a value pattern; a leading `!` inverts it.
    pub fn regex(pattern: &[u8]) -> Result<Self, EditError> {
        let (negate, body) = match pattern.strip_prefix(b"!") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let regex = compile_ere(body).ok_or_else(|| EditError::InvalidPattern(pattern.into()))?;
        Ok(ValuePattern::Regex { regex, negate })
    }

    /// Exact comparison (`--fixed-value`); no `!` negation.
    pub fn exact(value: &[u8]) -> Self {
        ValuePattern::Exact(value.into())
    }

    /// Whether an existing value (`None` for a value-less key) matches.
    pub fn matches(&self, value: Option<&[u8]>) -> bool {
        match self {
            ValuePattern::Any => true,
            ValuePattern::Never => false,
            ValuePattern::Regex { regex, negate } => match value {
                Some(v) => regex.is_match(v) != *negate,
                None => *negate,
            },
            // git treats a value-less key as matching any fixed value, but
            // as matching only negated regular expressions.
            ValuePattern::Exact(p) => value.is_none_or(|v| v == p.as_slice()),
        }
    }
}

/// Compile a POSIX extended regular expression as git uses it for values
/// (no `REG_NEWLINE`: `.` matches a line feed, `^`/`$` anchor the whole
/// value). Returns `None` for an invalid expression.
pub fn compile_ere(pattern: &[u8]) -> Option<regex::bytes::Regex> {
    let text = std::str::from_utf8(pattern).ok()?;
    regex::bytes::RegexBuilder::new(text)
        .unicode(false)
        .dot_matches_new_line(true)
        .build()
        .ok()
}

/// One call of git's "set multivar": set, add, replace-all, unset or
/// unset-all depending on the fields.
#[derive(Debug, Clone)]
pub struct SetRequest<'a> {
    /// The variable name as the user wrote it; its spelling is used for new
    /// lines and section headers.
    pub key: &'a [u8],
    /// The new value; `None` unsets.
    pub value: Option<&'a [u8]>,
    /// Which existing values are affected.
    pub pattern: &'a ValuePattern,
    /// Change all matches (`--replace-all`, `--unset-all`, `--all`) instead
    /// of failing when more than one matches.
    pub all: bool,
    /// `--comment` message, unformatted.
    pub comment: Option<&'a [u8]>,
}

/// A byte range of the old file replaced by new text.
#[derive(Debug)]
struct Splice {
    start: usize,
    end: usize,
    text: Option<BString>,
}

/// Apply a [`SetRequest`] to the bytes of a configuration file.
pub fn set(data: &[u8], req: &SetRequest<'_>) -> Result<Vec<u8>, EditError> {
    let key = Key::parse(req.key)?;
    let comment = req.comment.map(format_comment).transpose()?;
    let file = File::parse(data).map_err(|e| EditError::Syntax { line: e.line })?;
    let doc = Doc::new(&file);
    let canonical = key.canonical();

    let matched: Vec<usize> = doc
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            Item::Entry(e)
                if canonical_name(doc.header_of(i), &e.key) == canonical
                    && req
                        .pattern
                        .matches(e.value.as_deref().map(|v| v.as_bytes())) =>
            {
                Some(i)
            }
            _ => None,
        })
        .collect();
    if matched.len() > 1 && !req.all {
        return Err(EditError::MultipleValues { key: canonical });
    }

    let mut splices = Vec::new();
    match req.value {
        None => {
            if matched.is_empty() {
                return Err(EditError::NothingToUnset);
            }
            let mut runs: Vec<(usize, usize)> = Vec::new();
            for &i in &matched {
                if let Some(h) = doc.header_index(i) {
                    if let Some(run) = doc.empty_run(h, &matched) {
                        if !runs.contains(&run) {
                            runs.push(run);
                        }
                    }
                }
            }
            for (start, end) in &runs {
                splices.push(Splice {
                    start: *start,
                    end: *end,
                    text: None,
                });
            }
            for &i in &matched {
                let (start, end) = doc.entry_range(i);
                if !runs.iter().any(|(s, e)| *s <= start && end <= *e) {
                    splices.push(Splice {
                        start,
                        end,
                        text: None,
                    });
                }
            }
        }
        Some(value) => {
            let line = entry_line(
                name_part(req.key),
                value,
                comment.as_ref().map(|c| c.as_slice()),
            );
            if let Some((&last, rest)) = matched.split_last() {
                for &i in rest {
                    let (start, end) = doc.entry_range(i);
                    splices.push(Splice {
                        start,
                        end,
                        text: None,
                    });
                }
                let (start, end) = doc.entry_range(last);
                splices.push(Splice {
                    start,
                    end,
                    text: Some(line),
                });
            } else if let Some(pos) = doc.insert_position(&key) {
                splices.push(Splice {
                    start: pos,
                    end: pos,
                    text: Some(line),
                });
            } else {
                let mut text = section_header(section_part(req.key), subsection_part(req.key));
                text.push(b'\n');
                text.extend_from_slice(&line);
                splices.push(Splice {
                    start: data.len(),
                    end: data.len(),
                    text: Some(text),
                });
            }
        }
    }
    Ok(apply(data, splices))
}

/// Copy `data` with the splices applied. Before inserted text, and where a
/// removal leaves a line unterminated, a line feed is added.
fn apply(data: &[u8], mut splices: Vec<Splice>) -> Vec<u8> {
    splices.sort_by_key(|s| (s.start, s.end));
    let mut out = Vec::with_capacity(data.len() + 64);
    let mut copied = 0;
    for s in splices {
        if s.start < copied {
            continue;
        }
        out.extend_from_slice(&data[copied..s.start]);
        if s.start > copied && data[s.start - 1] != b'\n' {
            out.push(b'\n');
        }
        if let Some(text) = &s.text {
            out.extend_from_slice(text);
        }
        copied = s.end;
    }
    out.extend_from_slice(&data[copied..]);
    out
}

/// The parsed file with the position helpers the edits need.
struct Doc<'f> {
    data: &'f [u8],
    items: &'f [Item],
}

impl<'f> Doc<'f> {
    fn new(file: &'f File) -> Self {
        Doc {
            data: file.data().as_bytes(),
            items: file.items(),
        }
    }

    fn header_index(&self, i: usize) -> Option<usize> {
        match &self.items[i] {
            Item::Entry(e) => e.header,
            _ => None,
        }
    }

    fn header_of(&self, i: usize) -> Option<&'f SectionHeader> {
        self.header_index(i).and_then(|h| self.header(h))
    }

    fn header(&self, i: usize) -> Option<&'f SectionHeader> {
        match &self.items[i] {
            Item::Section(h) => Some(h),
            _ => None,
        }
    }

    /// Length of a leading byte order mark.
    fn bom_len(&self) -> usize {
        if self.data.starts_with(b"\xef\xbb\xbf") {
            3
        } else {
            0
        }
    }

    /// End of a section header: after `]`, plus a directly following line
    /// end.
    fn header_end(&self, h: &SectionHeader) -> usize {
        let end = h.span.end;
        let rest = &self.data[end..];
        if rest.starts_with(b"\n") {
            end + 1
        } else if rest.starts_with(b"\r\n") {
            end + 2
        } else {
            end
        }
    }

    /// Where item `i` ends for the purpose of the next edit.
    fn item_end(&self, i: usize) -> usize {
        match &self.items[i] {
            Item::Section(h) => self.header_end(h),
            item => item.span().end,
        }
    }

    /// End of the last non-trivia item before `i` (or the start of the
    /// content).
    fn previous_end(&self, i: usize) -> usize {
        (0..i)
            .rev()
            .find(|&j| !matches!(self.items[j], Item::Trivia(_)))
            .map(|j| self.item_end(j))
            .unwrap_or_else(|| self.bom_len())
    }

    /// The bytes an entry occupies: from the start of its line (or the end
    /// of a header on the same line) through its line end.
    fn entry_range(&self, i: usize) -> (usize, usize) {
        let span = self.items[i].span();
        let line_start = self.data[..span.start]
            .rfind_byte(b'\n')
            .map_or(0, |p| p + 1);
        (line_start.max(self.previous_end(i)), span.end)
    }

    /// Where a new variable for `key` goes: after the last item of the last
    /// section with the same name.
    fn insert_position(&self, key: &Key) -> Option<usize> {
        let mut found = None;
        for (i, item) in self.items.iter().enumerate() {
            let header = match item {
                Item::Section(h) => Some(h),
                Item::Entry(_) => self.header_of(i),
                Item::Trivia(_) => None,
            };
            if header.is_some_and(|h| section_matches(h, key)) {
                found = Some(i);
            }
        }
        found.map(|i| self.item_end(i))
    }

    /// The byte range of the section around header `h` if, with the entries
    /// in `removed` gone, it holds nothing but whitespace: from the end of
    /// the item before it through the start of the next other section.
    /// Neighbouring headers of the same section belong to it.
    fn empty_run(&self, h: usize, removed: &[usize]) -> Option<(usize, usize)> {
        let prefix = self.header(h)?.canonical_prefix();
        let same = |j: usize| {
            self.header(j)
                .is_some_and(|other| other.canonical_prefix() == prefix)
        };
        let has_comment = |j: usize| match &self.items[j] {
            Item::Trivia(r) => self.data[r.clone()]
                .iter()
                .any(|c| *c == b'#' || *c == b';'),
            _ => false,
        };
        let mut start = self.bom_len();
        for (j, item) in self.items[..h].iter().enumerate().rev() {
            match item {
                Item::Trivia(_) if has_comment(j) => return None,
                Item::Trivia(_) => {}
                Item::Section(_) if same(j) => {}
                Item::Entry(_) if removed.contains(&j) => {}
                _ => {
                    start = self.item_end(j);
                    break;
                }
            }
        }
        let mut end = self.data.len();
        for (j, item) in self.items.iter().enumerate().skip(h + 1) {
            match item {
                Item::Trivia(_) if has_comment(j) => return None,
                Item::Trivia(_) => {}
                Item::Section(_) if same(j) => {}
                Item::Section(other) => {
                    end = other.span.start;
                    break;
                }
                Item::Entry(_) if removed.contains(&j) => {}
                Item::Entry(_) => return None,
            }
        }
        Some((start, end))
    }
}

/// Whether a new variable for `key` may go into section `h`. The name as
/// written in the header compares case-insensitively (for the deprecated
/// `[section.sub]` syntax that includes the subsection), a quoted
/// subsection exactly.
fn section_matches(h: &SectionHeader, key: &Key) -> bool {
    let theirs = h.canonical_prefix();
    let mut ours = key.section.clone();
    if let Some(sub) = &key.subsection {
        ours.push(b'.');
        ours.extend_from_slice(sub);
    }
    let n = h.name.len();
    theirs.len() == ours.len()
        && theirs[..n].eq_ignore_ascii_case(&ours[..n])
        && theirs[n..] == ours[n..]
}

/// The section part of a user-given key (before the first dot).
fn section_part(key: &[u8]) -> &[u8] {
    let dot = key.find_byte(b'.').unwrap_or(key.len());
    &key[..dot]
}

/// The subsection part of a user-given key, if any.
fn subsection_part(key: &[u8]) -> Option<&[u8]> {
    let first = key.find_byte(b'.')?;
    let last = key.rfind_byte(b'.')?;
    (first < last).then(|| &key[first + 1..last])
}

/// The variable part of a user-given key (after the last dot).
fn name_part(key: &[u8]) -> &[u8] {
    let dot = key.rfind_byte(b'.').map_or(0, |d| d + 1);
    &key[dot..]
}

/// `[section]` or `[section "sub"]` without a line end.
fn section_header(section: &[u8], subsection: Option<&[u8]>) -> BString {
    let mut out = BString::from("[");
    out.extend_from_slice(section);
    if let Some(sub) = subsection {
        out.extend_from_slice(b" \"");
        for &c in sub {
            if c == b'"' || c == b'\\' {
                out.push(b'\\');
            }
            out.push(c);
        }
        out.push(b'"');
    }
    out.push(b']');
    out
}

/// `<TAB>name = value[comment]<LF>`.
fn entry_line(name: &[u8], value: &[u8], comment: Option<&[u8]>) -> BString {
    let mut out = BString::from("\t");
    out.extend_from_slice(name);
    out.extend_from_slice(b" = ");
    out.extend_from_slice(&quote_value(value));
    if let Some(c) = comment {
        out.extend_from_slice(c);
    }
    out.push(b'\n');
    out
}

/// Quote and escape a value for writing.
pub fn quote_value(value: &[u8]) -> BString {
    let quote = value.first() == Some(&b' ')
        || value.last() == Some(&b' ')
        || value.iter().any(|c| matches!(c, b';' | b'#' | b'\r'));
    let mut out = BString::default();
    if quote {
        out.push(b'"');
    }
    for &c in value {
        match c {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ => out.push(c),
        }
    }
    if quote {
        out.push(b'"');
    }
    out
}

/// Turn a `--comment` message into the text placed after the value: used
/// as is if it starts with whitespace followed by `#`, `#...` gets a space
/// in front, anything else `" # "`.
pub fn format_comment(message: &[u8]) -> Result<BString, EditError> {
    if message.contains(&b'\n') {
        return Err(EditError::MultilineComment(message.into()));
    }
    let ws = message
        .iter()
        .take_while(|c| matches!(c, b' ' | b'\t'))
        .count();
    let mut out = BString::default();
    if ws > 0 && message.get(ws) == Some(&b'#') {
        // as is
    } else if message.first() == Some(&b'#') {
        out.push(b' ');
    } else {
        out.extend_from_slice(b" # ");
    }
    out.extend_from_slice(message);
    Ok(out)
}

/// Check a new section name for rename-section: not empty, and the part
/// before the first dot only letters, digits and `-`.
fn valid_section_name(name: &[u8]) -> bool {
    !name.is_empty()
        && section_part(name)
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

/// git's `isspace`.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Parse a section header at the start of `line` (after leading
/// whitespace). Returns the name as written (`section`, `section.sub`, or
/// `section.sub` for `[section "sub"]` with the subsection unescaped) and
/// the offset after `]`.
fn line_header(line: &[u8]) -> Option<(BString, usize)> {
    let mut i = line.iter().take_while(|c| is_space(**c)).count();
    if line.get(i) != Some(&b'[') {
        return None;
    }
    i += 1;
    let mut name = BString::default();
    loop {
        let c = *line.get(i)?;
        i += 1;
        if c == b']' {
            return Some((name, i));
        }
        if is_space(c) {
            break;
        }
        if !(c.is_ascii_alphanumeric() || c == b'.' || c == b'-') {
            return None;
        }
        name.push(c);
    }
    while line.get(i).is_some_and(|c| is_space(*c)) {
        i += 1;
    }
    if line.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    name.push(b'.');
    loop {
        let mut c = *line.get(i)?;
        i += 1;
        if c == b'"' {
            break;
        }
        if c == b'\n' {
            return None;
        }
        if c == b'\\' {
            c = *line.get(i)?;
            i += 1;
        }
        name.push(c);
    }
    (line.get(i) == Some(&b']')).then_some((name, i + 1))
}

/// Rename (`new = Some`) or remove (`new = None`) every section whose
/// header, as written, equals `old` (`section` or `section.subsection`,
/// compared exactly).
///
/// Like git this works line by line: a header is recognised only at the
/// start of a line, and removal drops every line up to the next header.
pub fn rename_section(data: &[u8], old: &[u8], new: Option<&[u8]>) -> Result<Vec<u8>, EditError> {
    let new_header = match new {
        Some(n) if !valid_section_name(n) => return Err(EditError::InvalidSectionName(n.into())),
        Some(n) => Some(section_header(section_part(n), subsection_part_any(n))),
        None => None,
    };
    let mut out = Vec::with_capacity(data.len());
    let mut removing = false;
    let mut found = false;
    for (index, line) in data.lines_with_terminator().enumerate() {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        if content.len() >= MAX_SECTION_LINE {
            return Err(EditError::OverlongLine { line: index + 1 });
        }
        if let Some((name, after)) = line_header(line) {
            if name == old {
                found = true;
                match &new_header {
                    None => removing = true,
                    Some(header) => {
                        removing = false;
                        out.extend_from_slice(header);
                        out.push(b'\n');
                        let rest = &line[after..];
                        let ws = rest.iter().take_while(|c| is_space(**c)).count();
                        let rest = &rest[ws..];
                        if !rest.is_empty() {
                            out.push(b'\t');
                            out.extend_from_slice(rest);
                        }
                    }
                }
                continue;
            }
            removing = false;
        }
        if !removing {
            out.extend_from_slice(line);
        }
    }
    if !found {
        return Err(EditError::NoSuchSection(old.into()));
    }
    Ok(out)
}

/// Copy every section whose header, as written, equals `old` to a new
/// section `new`, placed right after the original (`git branch -c`).
pub fn copy_section(data: &[u8], old: &[u8], new: &[u8]) -> Result<Vec<u8>, EditError> {
    if !valid_section_name(new) {
        return Err(EditError::InvalidSectionName(new.into()));
    }
    let header = section_header(section_part(new), subsection_part_any(new));
    let mut out = Vec::with_capacity(data.len() * 2);
    // Body lines of the section being copied.
    let mut copying: Option<Vec<u8>> = None;
    let mut found = false;
    let flush = |out: &mut Vec<u8>, body: Vec<u8>| {
        if !out.is_empty() && !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.extend_from_slice(&header);
        out.push(b'\n');
        out.extend_from_slice(&body);
    };
    for (index, line) in data.lines_with_terminator().enumerate() {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        if content.len() >= MAX_SECTION_LINE {
            return Err(EditError::OverlongLine { line: index + 1 });
        }
        if let Some((name, after)) = line_header(line) {
            if let Some(body) = copying.take() {
                flush(&mut out, body);
            }
            if name == old {
                found = true;
                let rest = &line[after..];
                let ws = rest.iter().take_while(|c| is_space(**c)).count();
                let rest = &rest[ws..];
                let mut body = Vec::new();
                if !rest.is_empty() {
                    body.push(b'\t');
                    body.extend_from_slice(rest);
                }
                copying = Some(body);
            }
        } else if let Some(body) = copying.as_mut() {
            body.extend_from_slice(line);
        }
        out.extend_from_slice(line);
    }
    if let Some(body) = copying.take() {
        flush(&mut out, body);
    }
    if !found {
        return Err(EditError::NoSuchSection(old.into()));
    }
    Ok(out)
}

/// Like [`subsection_part`], but everything after the first dot (rename
/// targets may contain more dots, `a.` gives an empty subsection).
fn subsection_part_any(name: &[u8]) -> Option<&[u8]> {
    name.find_byte(b'.').map(|d| &name[d + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(data: &str, key: &str, value: Option<&str>, all: bool) -> Result<String, EditError> {
        let pattern = ValuePattern::Any;
        let req = SetRequest {
            key: key.as_bytes(),
            value: value.map(str::as_bytes),
            pattern: &pattern,
            all,
            comment: None,
        };
        set(data.as_bytes(), &req).map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn copy_section_after_original() {
        let data = "[core]\n\tbare = false\n[branch \"a\"]\n\tremote = .\n[x]\n\ty = 1\n";
        let out = copy_section(data.as_bytes(), b"branch.a", b"branch.b").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "[core]\n\tbare = false\n[branch \"a\"]\n\tremote = .\n[branch \"b\"]\n\tremote = .\n[x]\n\ty = 1\n"
        );
        let out = copy_section(b"[branch \"a\"]\n\tk = v", b"branch.a", b"branch.b").unwrap();
        assert_eq!(out, b"[branch \"a\"]\n\tk = v\n[branch \"b\"]\n\tk = v");
        assert!(matches!(
            copy_section(b"[x]\n", b"branch.a", b"branch.b"),
            Err(EditError::NoSuchSection(_))
        ));
    }

    #[test]
    fn placement() {
        assert_eq!(
            run("[s]\nk = v\n", "s.k", Some("w"), false).unwrap(),
            "[s]\n\tk = w\n"
        );
        assert_eq!(
            run("[s] k = v ; c\n[t]\n", "s.k", Some("w"), false).unwrap(),
            "[s]\n\tk = w\n[t]\n"
        );
        assert_eq!(
            run("[s] # hc\n[t]\n", "s.b", Some("2"), false).unwrap(),
            "[s]\n\tb = 2\n # hc\n[t]\n"
        );
        assert_eq!(
            run("[s]\n# c\n\n[t]\nx=1\n", "s.b", Some("2"), false).unwrap(),
            "[s]\n\tb = 2\n# c\n\n[t]\nx=1\n"
        );
        assert_eq!(
            run("[s]\na=1", "t.k", Some("v"), false).unwrap(),
            "[s]\na=1\n[t]\n\tk = v\n"
        );
        assert_eq!(
            run("", "S.Sub.KeY", Some(" lead"), false).unwrap(),
            "[S \"Sub\"]\n\tKeY = \" lead\"\n"
        );
        assert_eq!(
            run("[s.Sub]\nk=1\n", "s.Sub.k", Some("2"), false).unwrap(),
            "[s.Sub]\nk=1\n\tk = 2\n"
        );
    }

    #[test]
    fn unset_removes_empty_sections() {
        assert_eq!(
            run("[s] k = v\n[t]\n", "s.k", None, false).unwrap(),
            "[t]\n"
        );
        assert_eq!(
            run("# top\n[s]\nk=1\n", "s.k", None, false).unwrap(),
            "# top\n[s]\n"
        );
        assert_eq!(
            run("[t]   \n[s]\nk=1\n", "s.k", None, false).unwrap(),
            "[t]\n"
        );
        assert_eq!(
            run("[s]\nj=1\n[s]\nk=1\n", "s.k", None, false).unwrap(),
            "[s]\nj=1\n"
        );
        assert_eq!(
            run("[s]\nk=1\n[s]\nj=1\n", "s.k", None, false).unwrap(),
            "[s]\n[s]\nj=1\n"
        );
        assert_eq!(
            run("\u{feff}[s]\nk=1\n", "s.k", None, false).unwrap(),
            "\u{feff}\n"
        );
        assert_eq!(
            run("[t]\nx=1\n  [s]  \n  k=1  \n  [u]\n", "s.k", None, false).unwrap(),
            "[t]\nx=1\n[u]\n"
        );
        assert!(matches!(
            run("[s]\nk=1\nk=2\n", "s.k", None, false),
            Err(EditError::MultipleValues { .. })
        ));
        assert_eq!(run("[s]\nk=1\nk=2\n", "s.k", None, true).unwrap(), "");
    }

    #[test]
    fn quoting() {
        for (v, q) in [
            (" a", "\" a\""),
            ("a\r", "\"a\r\""),
            ("\ta", "\\ta"),
            ("a\"b\\c", "a\\\"b\\\\c"),
            ("x;y", "\"x;y\""),
            ("", ""),
            ("a\nb\x08", "a\\nb\x08"),
        ] {
            assert_eq!(quote_value(v.as_bytes()), q, "{v:?}");
        }
    }

    #[test]
    fn comments() {
        assert_eq!(format_comment(b"x").unwrap(), " # x");
        assert_eq!(format_comment(b"#x").unwrap(), " #x");
        assert_eq!(format_comment(b"\t# x").unwrap(), "\t# x");
        assert_eq!(format_comment(b" x").unwrap(), " #  x");
        assert!(format_comment(b"a\nb").is_err());
    }

    #[test]
    fn rename() {
        let r = |d: &str, o: &str, n: Option<&str>| {
            rename_section(d.as_bytes(), o.as_bytes(), n.map(str::as_bytes))
                .map(|v| String::from_utf8(v).unwrap())
        };
        let ok: &[(&str, &str, Option<&str>, &str)] = &[
            (
                "[s] ; hc\n\ta=1\n",
                "s",
                Some("new.Sub"),
                "[new \"Sub\"]\n\t; hc\n\ta=1\n",
            ),
            ("[s]   \n", "s", Some("t"), "[t]\n"),
            ("[s] # c", "s", Some("y"), "[y]\n\t# c"),
            ("[S]\n[s]\n", "s", Some("t"), "[S]\n[t]\n"),
            ("[t]\nx=1 \\\n[s]\n", "s", None, "[t]\nx=1 \\\n"),
            ("[s]\na=1\n\n   \n  [u]\n", "s", None, "  [u]\n"),
            ("[s]\n", "s", Some("a."), "[a \"\"]\n"),
        ];
        for (input, old, new, want) in ok {
            assert_eq!(
                r(input, old, *new).ok().as_deref(),
                Some(*want),
                "{input:?}"
            );
        }
        let missing = r("[s]\n", "q", None);
        assert!(matches!(missing, Err(EditError::NoSuchSection(_))));
        let invalid = r("[s]\n", "s", Some("a b"));
        assert!(matches!(invalid, Err(EditError::InvalidSectionName(_))));
    }
}
