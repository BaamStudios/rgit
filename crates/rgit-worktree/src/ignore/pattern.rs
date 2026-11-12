// SPDX-License-Identifier: MIT

//! One gitignore(5) pattern and a list of them read from one source.
//!
//! Line syntax (gitignore(5), "PATTERN FORMAT"), as the `git` binary
//! applies it:
//!
//! - A UTF-8 byte order mark at the start of a file and a carriage return
//!   before the line feed are dropped.
//! - Blank lines and lines starting with `#` are no patterns, but still
//!   count for line numbers.
//! - Trailing spaces are dropped unless escaped with a backslash. Tabs
//!   are kept.
//! - A leading `!` negates the pattern; `\!` and `\#` stand for a literal
//!   first character (the backslash stays in the glob, where it escapes).
//! - A trailing `/` restricts the pattern to directories and is removed.
//! - Without any other `/` the pattern is compared with the last path
//!   component at any depth below the pattern's base directory. With a
//!   `/` at the start or in the middle it is compared with the whole path
//!   relative to the base directory (a leading `/` is removed).
//! - Globs are [`rgit_pathspec::wildmatch`]; `*`, `?` and `[...]` do not
//!   match `/` in whole-path patterns, `**` spans directories.
//! - A pattern that becomes empty (`!`, `/`, `!/`) matches nothing.

use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};

/// One parsed pattern line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// The line as `check-ignore -v` shows it: after dropping the carriage
    /// return and unescaped trailing spaces, with `!` and the trailing `/`.
    text: Vec<u8>,
    /// The glob proper: without `!`, the trailing `/` and a leading `/`.
    glob: Vec<u8>,
    /// 1-based line number in its source.
    line: usize,
    negative: bool,
    dir_only: bool,
    /// No `/` in the glob: matched against the last path component.
    basename: bool,
}

impl Pattern {
    /// Parses one line (without its line feed). Returns `None` for blank
    /// lines, comments and patterns that can never match.
    #[must_use]
    pub fn parse(line: &[u8], line_no: usize) -> Option<Pattern> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b'#' {
            return None;
        }
        let text = trim_trailing_spaces(line);
        if text.is_empty() {
            return None;
        }
        let mut glob = text;
        let negative = glob[0] == b'!';
        if negative {
            glob = &glob[1..];
        }
        let dir_only = glob.last() == Some(&b'/');
        if dir_only {
            glob = &glob[..glob.len() - 1];
        }
        let basename = !glob.contains(&b'/');
        if let Some(rest) = glob.strip_prefix(b"/") {
            glob = rest;
        }
        if glob.is_empty() {
            return None;
        }
        Some(Pattern {
            text: text.to_vec(),
            glob: glob.to_vec(),
            line: line_no,
            negative,
            dir_only,
            basename,
        })
    }

    /// The pattern as written (see the module documentation).
    #[must_use]
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    /// The glob that is matched.
    #[must_use]
    pub fn glob(&self) -> &[u8] {
        &self.glob
    }

    /// 1-based line number in the source (0 for patterns that do not come
    /// from a file).
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    /// Whether the pattern starts with `!` (re-includes what it matches).
    #[must_use]
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// Whether the pattern ends with `/` (matches directories only).
    #[must_use]
    pub fn is_dir_only(&self) -> bool {
        self.dir_only
    }

    /// Whether the pattern has no `/` apart from a trailing one and is
    /// matched against the last path component.
    #[must_use]
    pub fn is_basename(&self) -> bool {
        self.basename
    }

    /// Whether the pattern matches `path`, given relative to the directory
    /// of the pattern's source (`rel`). `is_dir` says whether the path is a
    /// directory. With `icase` letters compare case-insensitively
    /// (`core.ignoreCase`).
    #[must_use]
    pub fn matches(&self, rel: &[u8], is_dir: bool, icase: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let mut flags = WildmatchFlags::NONE;
        if icase {
            flags |= WildmatchFlags::CASEFOLD;
        }
        if self.basename {
            let base = match rel.iter().rposition(|&b| b == b'/') {
                Some(i) => &rel[i + 1..],
                None => rel,
            };
            wildmatch(&self.glob, base, flags)
        } else {
            wildmatch(&self.glob, rel, flags | WildmatchFlags::PATHNAME)
        }
    }
}

/// Drops trailing spaces that are not escaped by a backslash. A backslash
/// escapes the byte after it, so `\\ ` ends in an unescaped space.
fn trim_trailing_spaces(line: &[u8]) -> &[u8] {
    let mut end = 0;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'\\' => {
                i = (i + 2).min(line.len());
                end = i;
            }
            b' ' => i += 1,
            _ => {
                i += 1;
                end = i;
            }
        }
    }
    &line[..end]
}

/// The patterns of one source, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatternList {
    /// How `check-ignore -v` names the source; `None` for patterns given on
    /// the command line.
    source: Option<Vec<u8>>,
    /// Directory the patterns are relative to: empty for the top of the
    /// work tree, otherwise with a trailing `/`.
    base: Vec<u8>,
    patterns: Vec<Pattern>,
}

impl PatternList {
    /// An empty list for patterns relative to `base` (empty or ending in
    /// `/`).
    #[must_use]
    pub fn new(source: Option<Vec<u8>>, base: Vec<u8>) -> PatternList {
        PatternList {
            source,
            base,
            patterns: Vec::new(),
        }
    }

    /// Parses the contents of a pattern file.
    #[must_use]
    pub fn from_bytes(data: &[u8], source: Option<Vec<u8>>, base: Vec<u8>) -> PatternList {
        let mut list = PatternList::new(source, base);
        let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
        if !data.is_empty() {
            let data = data.strip_suffix(b"\n").unwrap_or(data);
            for (i, line) in data.split(|&b| b == b'\n').enumerate() {
                list.add_line(line, i + 1);
            }
        }
        list
    }

    /// Adds one line (a pattern given on the command line uses line 0).
    pub fn add_line(&mut self, line: &[u8], line_no: usize) {
        if let Some(p) = Pattern::parse(line, line_no) {
            self.patterns.push(p);
        }
    }

    /// The source name.
    #[must_use]
    pub fn source(&self) -> Option<&[u8]> {
        self.source.as_deref()
    }

    /// The base directory (empty or ending in `/`).
    #[must_use]
    pub fn base(&self) -> &[u8] {
        &self.base
    }

    /// The patterns in file order.
    #[must_use]
    pub fn patterns(&self) -> &[Pattern] {
        &self.patterns
    }

    /// The last pattern that matches the work tree path `path`, if any.
    /// Paths outside the base directory never match.
    #[must_use]
    pub fn last_match(&self, path: &[u8], is_dir: bool, icase: bool) -> Option<&Pattern> {
        let rel = path.strip_prefix(self.base.as_slice())?;
        self.patterns
            .iter()
            .rev()
            .find(|p| p.matches(rel, is_dir, icase))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Pattern {
        Pattern::parse(line.as_bytes(), 1).unwrap()
    }

    #[test]
    fn skipped_lines() {
        for l in ["", "#x", "!", "/", "!/", "   ", "\r"] {
            assert!(Pattern::parse(l.as_bytes(), 1).is_none(), "{l:?}");
        }
    }

    #[test]
    fn trailing_spaces() {
        assert_eq!(p("a  ").text(), b"a");
        assert_eq!(p("a\\ \\ ").text(), b"a\\ \\ ");
        assert_eq!(p("a\\\\ ").text(), b"a\\\\");
        assert_eq!(p("a \\    ").text(), b"a \\ ");
        assert_eq!(p("a\t").text(), b"a\t");
        assert_eq!(p("a\\").text(), b"a\\");
    }

    #[test]
    fn flags() {
        let x = p("!foo/");
        assert!(x.is_negative() && x.is_dir_only() && x.is_basename());
        assert_eq!(x.glob(), b"foo");
        let y = p("/foo");
        assert!(!y.is_basename());
        assert_eq!(y.glob(), b"foo");
        let z = p("a/b/");
        assert!(!z.is_basename() && z.is_dir_only());
    }

    #[test]
    fn matching() {
        assert!(p("*.o").matches(b"a/b/x.o", false, false));
        assert!(!p("/x").matches(b"a/x", false, false));
        assert!(p("/x").matches(b"x", false, false));
        assert!(p("a/*").matches(b"a/b", false, false));
        assert!(!p("a/*").matches(b"a/b/c", false, false));
        assert!(p("**/foo").matches(b"x/y/foo", false, false));
        assert!(!p("foo/").matches(b"foo", false, false));
        assert!(p("foo/").matches(b"foo", true, false));
        assert!(p("FOO").matches(b"foo", false, true));
        assert!(!p("FOO").matches(b"foo", false, false));
    }

    #[test]
    fn list() {
        let l = PatternList::from_bytes(
            b"\xef\xbb\xbfa\r\n# c\n\n!a\nb\n",
            Some(b"x/.gitignore".to_vec()),
            b"x/".to_vec(),
        );
        let lines: Vec<usize> = l.patterns().iter().map(Pattern::line).collect();
        assert_eq!(lines, [1, 4, 5]);
        assert!(l.last_match(b"x/a", false, false).unwrap().is_negative());
        assert!(l.last_match(b"a", false, false).is_none());
    }
}
