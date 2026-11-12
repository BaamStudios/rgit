// SPDX-License-Identifier: MIT
//! Lossless parser for git's configuration file syntax.
//!
//! The parser splits the input into [`Item`]s whose byte spans cover the input
//! exactly once and in order, so concatenating the spans of all items yields
//! the original bytes. A writer (issue #99) can therefore edit single items and
//! keep comments, whitespace and the user's formatting everywhere else.
//!
//! Behaviour notes (verified against git 2.56 with differential tests):
//! - CR LF is read as LF, a lone CR is an ordinary character.
//! - The end of the input behaves like a final newline.
//! - A UTF-8 byte order mark at the very start is skipped.
//! - Whitespace between tokens is SP, HT, LF and CR; inside values only
//!   leading and trailing SP/HT are removed, internal whitespace is kept.
//! - A value-less key (`[core] bare`) is the boolean `true` and is represented
//!   as `None`; `key =` is the empty string.

use std::ops::Range;

use bstr::{BStr, BString, ByteSlice};

/// Error for a syntactically broken configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("bad config line {line}")]
pub struct SyntaxError {
    /// 1-based line number as git reports it.
    pub line: usize,
}

/// How a section header was spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderSyntax {
    /// `[section]`
    Plain,
    /// `[section "subsection"]`
    Quoted,
    /// Deprecated `[section.subsection]`: the whole name is lower-cased.
    Dotted,
}

/// A parsed `[section]` or `[section "subsection"]` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionHeader {
    /// Byte range from `[` to `]` inclusive.
    pub span: Range<usize>,
    /// 1-based line of the `[`.
    pub line: usize,
    /// Lower-cased name text before the optional quoted subsection. For the
    /// dotted syntax this still contains the dots (`[A.b]` gives `a.b`).
    pub name: BString,
    /// The unescaped quoted subsection, case preserved.
    pub subsection: Option<BString>,
    /// Spelling of the header.
    pub syntax: HeaderSyntax,
}

impl SectionHeader {
    /// The prefix that, followed by `.` and the variable name, forms the
    /// canonical variable name (`branch.main` for `[branch "main"]`).
    pub fn canonical_prefix(&self) -> BString {
        let mut out = self.name.clone();
        if let Some(sub) = &self.subsection {
            out.push(b'.');
            out.extend_from_slice(sub);
        }
        out
    }

    /// The section part (up to the first dot of the name).
    pub fn section(&self) -> &BStr {
        let name = self.name.as_bstr();
        match name.find_byte(b'.') {
            Some(i) => name[..i].as_bstr(),
            None => name,
        }
    }

    /// The effective subsection: the quoted subsection, or for the dotted
    /// syntax everything after the first dot.
    pub fn effective_subsection(&self) -> Option<BString> {
        let name = self.name.as_bstr();
        let dotted = name.find_byte(b'.').map(|i| name[i + 1..].to_owned());
        match (dotted, &self.subsection) {
            (None, None) => None,
            (Some(d), None) => Some(d),
            (None, Some(s)) => Some(s.clone()),
            (Some(d), Some(s)) => {
                let mut out = d;
                out.push(b'.');
                out.extend_from_slice(s);
                Some(out)
            }
        }
    }
}

/// A `name = value` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// Byte range from the first character of the name through the newline
    /// that ends the entry (comments and continuation lines included).
    pub span: Range<usize>,
    /// 1-based line of the variable name.
    pub line: usize,
    /// Index into [`File::items`] of the governing section header, if any.
    pub header: Option<usize>,
    /// The variable name as written (case preserved).
    pub key: BString,
    /// The unquoted, unescaped value; `None` for a value-less key.
    pub value: Option<BString>,
}

/// One element of a configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Whitespace, blank lines, comments and the byte order mark.
    Trivia(Range<usize>),
    /// A section header.
    Section(SectionHeader),
    /// A variable.
    Entry(RawEntry),
}

impl Item {
    /// Byte range of this item in the source.
    pub fn span(&self) -> Range<usize> {
        match self {
            Item::Trivia(r) => r.clone(),
            Item::Section(h) => h.span.clone(),
            Item::Entry(e) => e.span.clone(),
        }
    }
}

/// A parsed configuration file that remembers its source bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    data: BString,
    items: Vec<Item>,
}

impl File {
    /// Parse a complete file. On a syntax error nothing is returned; use
    /// [`Parser`] to see the items before the error.
    pub fn parse(data: impl Into<Vec<u8>>) -> Result<File, SyntaxError> {
        let data: Vec<u8> = data.into();
        let items = Parser::new(&data).collect::<Result<Vec<_>, _>>()?;
        Ok(File {
            data: data.into(),
            items,
        })
    }

    /// The source bytes.
    pub fn data(&self) -> &BStr {
        self.data.as_bstr()
    }

    /// All items in source order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The source text of an item.
    pub fn text(&self, item: &Item) -> &BStr {
        self.data[item.span()].as_bstr()
    }

    /// Iterate over the variables with their canonical names
    /// (`section[.subsection].key`, section and key lower-cased).
    pub fn entries(&self) -> impl Iterator<Item = (BString, &RawEntry)> {
        self.items.iter().filter_map(move |item| match item {
            Item::Entry(e) => {
                let header = e.header.and_then(|i| match &self.items[i] {
                    Item::Section(h) => Some(h),
                    _ => None,
                });
                Some((canonical_name(header, &e.key), e))
            }
            _ => None,
        })
    }
}

/// Build the canonical variable name for a key below `header`.
pub fn canonical_name(header: Option<&SectionHeader>, key: &[u8]) -> BString {
    let mut name = match header {
        Some(h) => {
            let mut p = h.canonical_prefix();
            p.push(b'.');
            p
        }
        None => BString::default(),
    };
    name.extend(key.iter().map(|b| b.to_ascii_lowercase()));
    name
}

/// git's `isspace` (SP, HT, LF, CR; not VT or FF).
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Streaming parser yielding [`Item`]s in order. After an error it yields
/// nothing more.
pub struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    line: usize,
    eof: bool,
    failed: bool,
    item_index: usize,
    current_header: Option<usize>,
    pending: Option<Item>,
    started: bool,
}

const BOM: &[u8] = b"\xef\xbb\xbf";

impl<'a> Parser<'a> {
    /// Create a parser over `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Parser {
            data,
            pos: 0,
            line: 1,
            eof: false,
            failed: false,
            item_index: 0,
            current_header: None,
            pending: None,
            started: false,
        }
    }

    /// Read one character. CR LF becomes LF, the end of input yields LF.
    /// Every returned LF advances the line counter.
    fn next_char(&mut self) -> u8 {
        let c = if self.pos >= self.data.len() {
            self.eof = true;
            b'\n'
        } else {
            let c = self.data[self.pos];
            self.pos += 1;
            if c == b'\r' && self.data.get(self.pos) == Some(&b'\n') {
                self.pos += 1;
                b'\n'
            } else {
                c
            }
        };
        if c == b'\n' {
            self.line += 1;
        }
        c
    }

    fn error(&mut self, line: usize) -> SyntaxError {
        self.failed = true;
        SyntaxError { line }
    }

    /// Error for an unexpected end of line: reported on the line it ends.
    fn incomplete(&mut self) -> SyntaxError {
        let line = self.line - 1;
        self.error(line)
    }

    fn emit(&mut self, item: Item) -> Item {
        self.item_index += 1;
        item
    }

    /// Parse the section header after `[`; `start` is the offset of `[`.
    fn section_header(&mut self, start: usize) -> Result<SectionHeader, SyntaxError> {
        let line = self.line;
        let mut name = BString::default();
        loop {
            let c = self.next_char();
            if self.eof {
                return Err(self.error(self.line));
            }
            if is_space(c) {
                // `[ "x"]` (empty name) is accepted, like git does.
                let sub = self.extended_subsection(c)?;
                return Ok(SectionHeader {
                    span: start..self.pos,
                    line,
                    name,
                    subsection: Some(sub),
                    syntax: HeaderSyntax::Quoted,
                });
            }
            if c == b']' {
                if name.is_empty() {
                    return Err(self.error(self.line));
                }
                let syntax = if name.contains(&b'.') {
                    HeaderSyntax::Dotted
                } else {
                    HeaderSyntax::Plain
                };
                return Ok(SectionHeader {
                    span: start..self.pos,
                    line,
                    name,
                    subsection: None,
                    syntax,
                });
            }
            if !(c.is_ascii_alphanumeric() || c == b'.' || c == b'-') {
                return Err(self.error(self.line));
            }
            name.push(c.to_ascii_lowercase());
        }
    }

    /// Parse ` "subsection"]` after the section name; `c` is the first
    /// whitespace character.
    fn extended_subsection(&mut self, mut c: u8) -> Result<BString, SyntaxError> {
        loop {
            if c == b'\n' {
                return Err(self.incomplete());
            }
            c = self.next_char();
            if !is_space(c) {
                break;
            }
        }
        if c != b'"' {
            return Err(self.error(self.line));
        }
        let mut sub = BString::default();
        loop {
            let mut c = self.next_char();
            if c == b'"' {
                break;
            }
            if c == b'\n' {
                return Err(self.incomplete());
            }
            if c == b'\\' {
                c = self.next_char();
                if c == b'\n' {
                    return Err(self.incomplete());
                }
            }
            sub.push(c);
        }
        if self.next_char() != b']' {
            return Err(self.error(self.line));
        }
        Ok(sub)
    }

    /// Parse a variable starting with the alphabetic character `first`.
    fn entry(&mut self, first: u8) -> Result<(BString, Option<BString>), SyntaxError> {
        let mut key = BString::from(vec![first]);
        let mut c;
        loop {
            c = self.next_char();
            if self.eof || !(c.is_ascii_alphanumeric() || c == b'-') {
                break;
            }
            key.push(c);
        }
        while c == b' ' || c == b'\t' {
            c = self.next_char();
        }
        if c == b'\n' {
            return Ok((key, None));
        }
        if c != b'=' {
            return Err(self.error(self.line));
        }
        let value = self.value()?;
        Ok((key, Some(value)))
    }

    /// Parse a value after `=` through the end of its (last) line.
    fn value(&mut self) -> Result<BString, SyntaxError> {
        let mut value = BString::default();
        // Length of `value` without trailing unquoted whitespace.
        let mut trimmed = 0usize;
        let mut quote = false;
        let mut comment = false;
        loop {
            let c = self.next_char();
            if c == b'\n' {
                if quote {
                    return Err(self.incomplete());
                }
                value.truncate(trimmed);
                return Ok(value);
            }
            if comment {
                continue;
            }
            if is_space(c) && !quote {
                if !value.is_empty() {
                    value.push(c);
                }
                continue;
            }
            if !quote && (c == b';' || c == b'#') {
                comment = true;
                continue;
            }
            // Any other character keeps the whitespace seen so far.
            trimmed = value.len();
            match c {
                b'\\' => {
                    let e = self.next_char();
                    let out = match e {
                        b'\n' => continue,
                        b't' => b'\t',
                        b'b' => 0x08,
                        b'n' => b'\n',
                        b'\\' | b'"' => e,
                        _ => return Err(self.error(self.line)),
                    };
                    value.push(out);
                    trimmed = value.len();
                }
                b'"' => quote = !quote,
                _ => {
                    value.push(c);
                    trimmed = value.len();
                }
            }
        }
    }

    fn next_item(&mut self) -> Option<Result<Item, SyntaxError>> {
        if let Some(item) = self.pending.take() {
            return Some(Ok(self.emit(item)));
        }
        if self.failed || (self.eof && self.pos >= self.data.len()) {
            return None;
        }
        let trivia_start = self.pos;
        if !self.started {
            self.started = true;
            if self.data.starts_with(BOM) {
                self.pos = BOM.len();
            }
        }
        let mut comment = false;
        loop {
            let before = self.pos;
            let c = self.next_char();
            if c == b'\n' {
                if self.eof {
                    return self.trivia(trivia_start, self.pos);
                }
                comment = false;
                continue;
            }
            if comment || is_space(c) {
                continue;
            }
            if c == b'#' || c == b';' {
                comment = true;
                continue;
            }
            let item = if c == b'[' {
                match self.section_header(before) {
                    Ok(h) => {
                        let index = self.item_index + usize::from(trivia_start < before);
                        self.current_header = Some(index);
                        Item::Section(h)
                    }
                    Err(e) => return Some(Err(e)),
                }
            } else if c.is_ascii_alphabetic() {
                let line = self.line;
                match self.entry(c) {
                    Ok((key, value)) => Item::Entry(RawEntry {
                        span: before..self.pos,
                        line,
                        header: self.current_header,
                        key,
                        value,
                    }),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                return Some(Err(self.error(self.line)));
            };
            if trivia_start < before {
                self.pending = Some(item);
                return Some(Ok(self.emit(Item::Trivia(trivia_start..before))));
            }
            return Some(Ok(self.emit(item)));
        }
    }

    fn trivia(&mut self, start: usize, end: usize) -> Option<Result<Item, SyntaxError>> {
        if start < end {
            Some(Ok(self.emit(Item::Trivia(start..end))))
        } else {
            None
        }
    }
}

impl Iterator for Parser<'_> {
    type Item = Result<Item, SyntaxError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_item()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(input: &[u8]) -> Result<Vec<(String, Option<String>)>, usize> {
        let file = File::parse(input).map_err(|e| e.line)?;
        // Losslessness: spans cover the input exactly.
        let mut pos = 0;
        for item in file.items() {
            assert_eq!(item.span().start, pos, "gap before {item:?}");
            pos = item.span().end;
        }
        assert_eq!(pos, input.len());
        Ok(file
            .entries()
            .map(|(n, e)| {
                (
                    n.to_string(),
                    e.value.as_ref().map(|v| v.to_str_lossy().into_owned()),
                )
            })
            .collect())
    }

    fn one(input: &str) -> Option<String> {
        let v = list(input.as_bytes()).unwrap();
        assert_eq!(v.len(), 1, "{v:?}");
        v[0].1.clone()
    }

    #[test]
    fn whitespace_rules() {
        assert_eq!(one("[s]\nk = big \t\t blue \n").unwrap(), "big \t\t blue");
        assert_eq!(one("[s]\nk = \"big blue \"\n").unwrap(), "big blue ");
        assert_eq!(one("[s]\nk = \"\" x\n").unwrap(), "x");
        assert_eq!(one("[s]\nk = a \"\" \n").unwrap(), "a ");
        assert_eq!(one("[s]\nk = a  \\\n  b\n").unwrap(), "a    b");
        assert_eq!(one("[s]\nk = \"a\" # \"b\n").unwrap(), "a");
        assert_eq!(one("[s]\nk=1 \\\n# c\n").unwrap(), "1 ");
    }

    #[test]
    fn escapes_and_crlf() {
        assert_eq!(one("[s]\nk=\"a\\tb\\nc\\bd\"\n").unwrap(), "a\tb\nc\u{8}d");
        assert_eq!(one("[s]\r\nk=v\r\n").unwrap(), "v");
        assert_eq!(one("[s]\nk=a\rb\n").unwrap(), "a\rb");
        assert_eq!(one("[s]\nk=\"a\\\r\nb\"\n").unwrap(), "ab");
        assert_eq!(one("\u{feff}[s]\nk=v").unwrap(), "v");
    }

    #[test]
    fn names() {
        let v = list(b"[S.Sub]\nK\n[S \"a\\\"b\\\\c\\td\"] k=1\n [t] j=2\n").unwrap();
        assert_eq!(v[0], ("s.sub.k".into(), None));
        assert_eq!(v[1], ("s.a\"b\\ctd.k".into(), Some("1".into())));
        assert_eq!(v[2], ("t.j".into(), Some("2".into())));
        assert_eq!(list(b"k=v").unwrap()[0].0, "k");
        assert_eq!(list(b"[s.x \"Y\"]k").unwrap()[0].0, "s.x.Y.k");
    }

    #[test]
    fn error_lines() {
        let cases: &[(&[u8], usize)] = &[
            (b"[section]\nkey garbage\n", 2),
            (b"[section\nkey=v\n", 1),
            (b"[s]\nk=\"abc\n", 2),
            (b"[s \"x\"\n]k=1", 2),
            (b"[s \"a\nb\"]\nk=v\n", 1),
            (b"[s]\nk\n=v\n", 3),
            (b"[s]\nk=a\\  \n", 2),
            (b"[s", 2),
            (b"[s \"x", 1),
            (b"[s \"x\"", 2),
            (b"[s]\n\n[t", 4),
            (b"[s]\nk = \"a\\\"", 2),
            (b"[s]\nk=\"a\\\n\n", 3),
            (b"[s]\nk=a\\\n\\q\n", 3),
            (b"[s \"a\" ]\n", 1),
            (b"[]\n", 1),
            (b"[s_x]\n", 1),
            (b"\xef\xbb[s]\n", 1),
            (b"[s]\n1k=v\n", 2),
            (b"[s]\nk\x0b= a\n", 2),
        ];
        for (input, line) in cases {
            assert_eq!(list(input), Err(*line), "{:?}", input.as_bstr());
        }
    }
}
