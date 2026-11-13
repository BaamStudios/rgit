// SPDX-License-Identifier: MIT

//! Attribute names, states, patterns and the lines of an attributes file.
//!
//! Line syntax (gitattributes(5)), as the `git` binary applies it:
//!
//! - A UTF-8 byte order mark at the start of the file is dropped; lines
//!   end at a line feed, a carriage return before it is dropped.
//! - Lines of [`crate::MAX_LINE_LEN`] bytes or more are ignored with a
//!   warning.
//! - Leading blanks (space, tab, carriage return) are skipped; blank lines
//!   and lines starting with `#` are ignored.
//! - The pattern ends at the first blank, unless it starts with `"`: then
//!   it is C-quoted and ends at the closing quote (a line with an
//!   unterminated or malformed quote is ignored).
//! - A pattern starting with `!` is ignored with a warning.
//! - `[attr]<name> <attrs>...` defines a macro; only top-level files may
//!   do that, elsewhere the line is ignored with a message.
//! - Each attribute is `name` (set), `-name` (unset), `!name` (back to
//!   unspecified) or `name=value`. One invalid name (see [`is_valid_name`])
//!   makes the whole line ignored with a message.
//! - Patterns follow gitignore(5) without negation: a trailing `/` limits
//!   the pattern to directories, a pattern without another `/` is compared
//!   with the last path component, otherwise with the whole path relative
//!   to the directory of the file (a leading `/` only anchors). A pattern
//!   matching a directory says nothing about the paths inside it.

use rgit_pathspec::unquote_c_style;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};

use crate::messages;

/// Whether `name` may be used as an attribute name on the command line:
/// ASCII letters, digits, `-`, `_` and `.`, not empty and not starting
/// with `-`.
#[must_use]
pub fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name[0] != b'-'
        && name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Whether `name` is in the namespace reserved for attributes git computes
/// itself (`builtin_*`); such names are rejected in attributes files.
#[must_use]
pub fn is_reserved_name(name: &[u8]) -> bool {
    name.starts_with(b"builtin_")
}

/// The state of an attribute for a path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum State {
    /// Nothing says anything about the attribute (or `!name` reset it).
    #[default]
    Unspecified,
    /// `name`: the attribute is true.
    Set,
    /// `-name`: the attribute is false.
    Unset,
    /// `name=value`.
    Value(Vec<u8>),
}

impl State {
    /// The value of a `name=value` state.
    #[must_use]
    pub fn value(&self) -> Option<&[u8]> {
        match self {
            State::Value(v) => Some(v),
            _ => None,
        }
    }

    /// Whether the state is [`State::Set`].
    #[must_use]
    pub fn is_set(&self) -> bool {
        matches!(self, State::Set)
    }

    /// Whether the state is [`State::Unset`].
    #[must_use]
    pub fn is_unset(&self) -> bool {
        matches!(self, State::Unset)
    }

    /// Whether the state is [`State::Unspecified`].
    #[must_use]
    pub fn is_unspecified(&self) -> bool {
        matches!(self, State::Unspecified)
    }

    /// The text `git check-attr` prints: `unspecified`, `set`, `unset` or
    /// the value.
    #[must_use]
    pub fn display(&self) -> &[u8] {
        match self {
            State::Unspecified => b"unspecified",
            State::Set => b"set",
            State::Unset => b"unset",
            State::Value(v) => v,
        }
    }
}

/// The pattern of an attributes line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    glob: Vec<u8>,
    dir_only: bool,
    basename: bool,
}

impl Pattern {
    /// A pattern from its text (already unquoted).
    #[must_use]
    pub fn new(text: &[u8]) -> Pattern {
        let mut glob = text;
        let dir_only = glob.last() == Some(&b'/');
        if dir_only {
            glob = &glob[..glob.len() - 1];
        }
        let basename = !glob.contains(&b'/');
        if let Some(rest) = glob.strip_prefix(b"/") {
            glob = rest;
        }
        Pattern {
            glob: glob.to_vec(),
            dir_only,
            basename,
        }
    }

    /// Whether the pattern matches `rel`, a path relative to the directory
    /// of the pattern's file. `is_dir`: the path names a directory.
    #[must_use]
    pub fn matches(&self, rel: &[u8], is_dir: bool, icase: bool) -> bool {
        if self.glob.is_empty() || (self.dir_only && !is_dir) {
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

/// What one line of an attributes file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// `pattern attr...`.
    Rule(Pattern, Vec<(Vec<u8>, State)>),
    /// `[attr]name attr...`.
    Macro(Vec<u8>, Vec<(Vec<u8>, State)>),
}

fn is_blank(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn skip_blanks(s: &[u8]) -> &[u8] {
    let n = s.iter().take_while(|&&b| is_blank(b)).count();
    &s[n..]
}

/// Splits off the token at the start of `s` (up to the first blank).
fn token(s: &[u8]) -> (&[u8], &[u8]) {
    let n = s.iter().take_while(|&&b| !is_blank(b)).count();
    (&s[..n], &s[n..])
}

/// Parses the attribute list `s`. On an invalid name returns it.
fn parse_attrs(mut s: &[u8]) -> Result<Vec<(Vec<u8>, State)>, Vec<u8>> {
    let mut out = Vec::new();
    loop {
        s = skip_blanks(s);
        if s.is_empty() {
            return Ok(out);
        }
        let (t, rest) = token(s);
        s = rest;
        let (name, state) = match t[0] {
            b'-' => (&t[1..], State::Unset),
            b'!' => (&t[1..], State::Unspecified),
            _ => match t.iter().position(|&b| b == b'=') {
                Some(eq) => (&t[..eq], State::Value(t[eq + 1..].to_vec())),
                None => (t, State::Set),
            },
        };
        if !is_valid_name(name) || is_reserved_name(name) {
            return Err(name.to_vec());
        }
        out.push((name.to_vec(), state));
    }
}

/// The end of a C-quoted string starting at `s[0] == '"'`: the index of
/// the closing quote.
fn closing_quote(s: &[u8]) -> Option<usize> {
    let mut i = 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Parses one line (without line feed). `None` for lines that say nothing;
/// messages for ignored lines go to `warnings`.
pub(crate) fn parse_line(
    line: &[u8],
    line_no: usize,
    source: &str,
    allow_macros: bool,
    warnings: &mut Vec<String>,
) -> Option<Line> {
    let s = skip_blanks(line);
    if s.is_empty() || s[0] == b'#' {
        return None;
    }
    if let Some(rest) = s.strip_prefix(b"[attr]") {
        if !allow_macros {
            warnings.push(messages::macro_not_allowed(s, source, line_no));
            return None;
        }
        let (name, rest) = token(rest);
        if !is_valid_name(name) || is_reserved_name(name) {
            warnings.push(messages::invalid_name(name, source, line_no));
            return None;
        }
        return match parse_attrs(rest) {
            Ok(attrs) => Some(Line::Macro(name.to_vec(), attrs)),
            Err(bad) => {
                warnings.push(messages::invalid_name(&bad, source, line_no));
                None
            }
        };
    }
    let (pattern, rest) = if s[0] == b'"' {
        let end = closing_quote(s)?;
        (unquote_c_style(&s[..=end])?, &s[end + 1..])
    } else {
        let (p, rest) = token(s);
        (p.to_vec(), rest)
    };
    if pattern.first() == Some(&b'!') {
        warnings.push(messages::NEGATIVE_PATTERN.to_string());
        return None;
    }
    match parse_attrs(rest) {
        Ok(attrs) => Some(Line::Rule(Pattern::new(&pattern), attrs)),
        Err(bad) => {
            warnings.push(messages::invalid_name(&bad, source, line_no));
            None
        }
    }
}

/// The lines of a file with their 1-based numbers: byte order mark
/// dropped, a carriage return before the line feed removed.
pub(crate) fn lines(data: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    let data = data.strip_suffix(b"\n").unwrap_or(data);
    let parts = if data.is_empty() {
        None
    } else {
        Some(data.split(|&b| b == b'\n'))
    };
    parts
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, line)| (i + 1, line.strip_suffix(b"\r").unwrap_or(line)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str, allow: bool) -> (Option<Line>, Vec<String>) {
        let mut w = Vec::new();
        let l = parse_line(line.as_bytes(), 3, "src", allow, &mut w);
        (l, w)
    }

    fn attrs(line: &str) -> Vec<(String, State)> {
        match parse(line, true).0 {
            Some(Line::Rule(_, a) | Line::Macro(_, a)) => a
                .into_iter()
                .map(|(n, s)| (String::from_utf8(n).unwrap(), s))
                .collect(),
            None => panic!("{line:?} ignored"),
        }
    }

    #[test]
    fn names() {
        assert!(is_valid_name(b"a.b-c_D9"));
        assert!(is_valid_name(b"1x"));
        assert!(!is_valid_name(b""));
        assert!(!is_valid_name(b"-x"));
        assert!(!is_valid_name(b"a$"));
        assert!(is_reserved_name(b"builtin_x"));
    }

    #[test]
    fn states() {
        assert_eq!(
            attrs("p a -b !c d=e=f g="),
            vec![
                ("a".into(), State::Set),
                ("b".into(), State::Unset),
                ("c".into(), State::Unspecified),
                ("d".into(), State::Value(b"e=f".to_vec())),
                ("g".into(), State::Value(Vec::new())),
            ]
        );
        assert_eq!(attrs("\t p\ta\r").len(), 1);
    }

    #[test]
    fn ignored_lines() {
        for l in ["", "   ", "# x a", "\"open a", "\"bad\\q\" a"] {
            let (line, w) = parse(l, true);
            assert!(line.is_none() && w.is_empty(), "{l:?}");
        }
        let (line, w) = parse("p a =b", true);
        assert!(line.is_none());
        assert_eq!(w, [" is not a valid attribute name: src:3"]);
        let (_, w) = parse("p builtin_x", true);
        assert_eq!(w, ["builtin_x is not a valid attribute name: src:3"]);
        let (_, w) = parse("!p a", true);
        assert!(w[0].starts_with("warning: Negative patterns"));
        let (_, w) = parse("  [attr]m\tx  ", false);
        assert_eq!(w, ["[attr]m\tx   not allowed: src:3"]);
    }

    #[test]
    fn quoted_pattern() {
        let (l, _) = parse("\"a b\\t\"x y", true);
        match l {
            Some(Line::Rule(p, a)) => {
                assert!(p.matches(b"a b\t", false, false));
                assert_eq!(a.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn patterns() {
        let p = Pattern::new;
        assert!(p(b"*.c").matches(b"x/y.c", false, false));
        assert!(p(b"/x").matches(b"x", false, false));
        assert!(!p(b"/x").matches(b"a/x", false, false));
        assert!(!p(b"d/").matches(b"d", false, false));
        assert!(p(b"d/").matches(b"d", true, false));
        assert!(!p(b"d/**").matches(b"d", true, false));
        assert!(p(b"a**f").matches(b"axf", false, false));
        assert!(!p(b"a**f").matches(b"a/f", false, false));
        assert!(p(b"F").matches(b"f", false, true));
        assert!(!p(b"").matches(b"", false, false));
    }

    #[test]
    fn line_splitting() {
        let got: Vec<(usize, Vec<u8>)> = lines("\u{feff}a\r\n\nb".as_bytes())
            .map(|(n, l)| (n, l.to_vec()))
            .collect();
        assert_eq!(
            got,
            [(1, b"a".to_vec()), (2, Vec::new()), (3, b"b".to_vec())]
        );
        assert_eq!(lines(b"").count(), 0);
        assert_eq!(lines(b"\n").count(), 0);
        assert_eq!(lines(b"\n\n").count(), 2);
    }
}
