// SPDX-License-Identifier: MIT
//! Commit filters by text: `--grep`, `--author`, `--committer` with `-i`,
//! `-E`, `-F`, `-P`, `--all-match` and `--invert-grep`.
//!
//! Written from `Documentation/rev-list-options.adoc` ("Commit Limiting")
//! and the observable behaviour of the `git` binary:
//!
//! - `--grep` patterns are matched line by line against the message (not
//!   the header); a commit matches when one pattern matches some line, or
//!   with `--all-match` when every pattern does. `--invert-grep` keeps the
//!   commits where no pattern matches.
//! - `--author` and `--committer` patterns are matched against the
//!   `Name <email>` part of the header line; several patterns of the same
//!   kind are alternatives, and both kinds and the message patterns must
//!   all be satisfied.
//! - Patterns are POSIX basic regular expressions by default (with the GNU
//!   extensions `\+`, `\?`, `\|`, `\<`, `\>`), extended ones with `-E`,
//!   fixed strings with `-F`; `-P` takes the syntax of the `regex` crate,
//!   which is close to Perl's.

use regex::bytes::{Regex, RegexBuilder};

use super::messages as msg;

/// The pattern syntax.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// POSIX basic regular expressions (the default).
    #[default]
    Basic,
    /// `-E`: POSIX extended regular expressions.
    Extended,
    /// `-F`: fixed strings.
    Fixed,
    /// `-P`: Perl-like regular expressions.
    Perl,
}

/// The text filters of a walk, as given on the command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GrepOptions {
    /// `--grep` patterns.
    pub message: Vec<String>,
    /// `--author` patterns.
    pub author: Vec<String>,
    /// `--committer` patterns.
    pub committer: Vec<String>,
    /// Pattern syntax.
    pub syntax: Syntax,
    /// `-i`.
    pub ignore_case: bool,
    /// `--all-match`.
    pub all_match: bool,
    /// `--invert-grep`.
    pub invert: bool,
}

impl GrepOptions {
    /// No filter is set.
    pub fn is_empty(&self) -> bool {
        self.message.is_empty() && self.author.is_empty() && self.committer.is_empty()
    }
}

/// Compiled text filters.
#[derive(Debug, Clone)]
pub struct CommitGrep {
    message: Vec<Regex>,
    author: Vec<Regex>,
    committer: Vec<Regex>,
    all_match: bool,
    invert: bool,
}

impl CommitGrep {
    /// Compile `opts`. The error is the message for the first pattern
    /// that does not compile.
    pub fn new(opts: &GrepOptions) -> Result<CommitGrep, String> {
        let compile = |pats: &[String], header: bool| -> Result<Vec<Regex>, String> {
            pats.iter()
                .map(|p| {
                    compile(p, opts.syntax, opts.ignore_case)
                        .map_err(|reason| msg::bad_pattern(header, p, reason))
                })
                .collect()
        };
        Ok(CommitGrep {
            message: compile(&opts.message, false)?,
            author: compile(&opts.author, true)?,
            committer: compile(&opts.committer, true)?,
            all_match: opts.all_match,
            invert: opts.invert,
        })
    }

    /// Whether the raw commit object `raw` passes the filters.
    pub fn matches(&self, raw: &[u8]) -> bool {
        let (header, message) = match raw.windows(2).position(|w| w == b"\n\n") {
            Some(i) => (&raw[..=i], &raw[i + 2..]),
            None => (raw, &raw[raw.len()..]),
        };
        if !self.author.is_empty() && !header_matches(header, b"author ", &self.author) {
            return false;
        }
        if !self.committer.is_empty() && !header_matches(header, b"committer ", &self.committer) {
            return false;
        }
        if self.message.is_empty() {
            return true;
        }
        let hit = |re: &Regex| message.split(|&b| b == b'\n').any(|l| re.is_match(l));
        if self.invert {
            !self.message.iter().any(hit)
        } else if self.all_match {
            self.message.iter().all(hit)
        } else {
            self.message.iter().any(hit)
        }
    }
}

/// One of `pats` matches the identity (`Name <email>`) of the header line
/// starting with `key`.
fn header_matches(header: &[u8], key: &[u8], pats: &[Regex]) -> bool {
    header
        .split(|&b| b == b'\n')
        .filter_map(|l| l.strip_prefix(key))
        .any(|ident| {
            let end = ident
                .iter()
                .rposition(|&b| b == b'>')
                .map_or(ident.len(), |i| i + 1);
            let ident = &ident[..end];
            pats.iter().any(|re| re.is_match(ident))
        })
}

fn compile(pattern: &str, syntax: Syntax, icase: bool) -> Result<Regex, &'static str> {
    let text = match syntax {
        Syntax::Fixed => regex::escape(pattern),
        Syntax::Perl => pattern.to_string(),
        Syntax::Basic => translate(pattern, false)?,
        Syntax::Extended => translate(pattern, true)?,
    };
    RegexBuilder::new(&text)
        .case_insensitive(icase)
        .unicode(false)
        .build()
        .map_err(|e| reason(&e.to_string()))
}

/// The POSIX reason for an error of the `regex` crate.
pub fn reason(error: &str) -> &'static str {
    if error.contains("unclosed group") || error.contains("unopened group") {
        msg::REGEX_UNMATCHED_PAREN
    } else if error.contains("invalid repetition count range") {
        msg::REGEX_BAD_INTERVAL
    } else if error.contains("unclosed counted repetition")
        || error.contains("repetition quantifier expects")
    {
        msg::REGEX_UNMATCHED_BRACE
    } else if error.contains("repetition operator missing expression") {
        msg::REGEX_BAD_PRECEDING
    } else if error.contains("invalid ASCII character class") {
        msg::REGEX_BAD_CLASS
    } else {
        msg::REGEX_INVALID
    }
}

/// Translate a POSIX regular expression (basic or extended) into the
/// syntax of the `regex` crate.
pub fn translate(pattern: &str, extended: bool) -> Result<String, &'static str> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() * 2);
    let mut i = 0;
    // At the start of an expression (`*` is literal there in BRE, `^` is
    // an anchor).
    let mut at_start = true;
    while i < chars.len() {
        let c = chars[i];
        let start = at_start;
        at_start = false;
        match c {
            '\\' if i + 1 < chars.len() => {
                let d = chars[i + 1];
                i += 2;
                match d {
                    '(' | ')' | '{' | '}' | '|' | '+' | '?' if !extended => {
                        out.push(d);
                        at_start = d == '(' || d == '|';
                    }
                    '<' | '>' => out.push_str(r"\b"),
                    'w' | 'W' | 's' | 'S' | 'b' | 'B' => {
                        out.push('\\');
                        out.push(d);
                    }
                    '1'..='9' => {
                        // Back-references are not supported; match the digit.
                        out.push(d);
                    }
                    _ => push_literal(&mut out, d),
                }
                continue;
            }
            '[' => {
                i = bracket(&chars, i, &mut out)?;
                continue;
            }
            '*' if start && !extended => push_literal(&mut out, '*'),
            '^' if start || extended => out.push('^'),
            '^' => push_literal(&mut out, '^'),
            '$' => {
                let next = &chars[i + 1..];
                let anchor = extended
                    || next.is_empty()
                    || next.starts_with(&['\\', ')'])
                    || next.starts_with(&['\\', '|']);
                if anchor {
                    out.push('$');
                } else {
                    push_literal(&mut out, '$');
                }
            }
            '(' | ')' | '{' | '}' | '|' | '+' | '?' if !extended => push_literal(&mut out, c),
            '(' | '|' if extended => {
                out.push(c);
                at_start = true;
            }
            '{' if extended && !chars.get(i + 1).is_some_and(char::is_ascii_digit) => {
                push_literal(&mut out, '{');
            }
            _ => out.push(c),
        }
        i += 1;
    }
    Ok(out)
}

fn push_literal(out: &mut String, c: char) {
    let mut b = [0u8; 4];
    out.push_str(&regex::escape(c.encode_utf8(&mut b)));
}

/// Copy the bracket expression starting at `chars[i] == '['`; returns the
/// index after it.
fn bracket(chars: &[char], i: usize, out: &mut String) -> Result<usize, &'static str> {
    let mut j = i + 1;
    let mut set = String::from("[");
    if chars.get(j) == Some(&'^') {
        set.push('^');
        j += 1;
    }
    let mut first = true;
    while j < chars.len() {
        let c = chars[j];
        if c == ']' && !first {
            set.push(']');
            out.push_str(&set);
            return Ok(j + 1);
        }
        first = false;
        if c == '[' && matches!(chars.get(j + 1), Some(':' | '.' | '=')) {
            let kind = chars[j + 1];
            let close = chars[j + 2..]
                .windows(2)
                .position(|w| w[0] == kind && w[1] == ']');
            if let Some(p) = close {
                let name: String = chars[j + 2..j + 2 + p].iter().collect();
                if kind == ':' {
                    const CLASSES: [&str; 12] = [
                        "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print",
                        "punct", "space", "upper", "xdigit",
                    ];
                    if !CLASSES.contains(&name.as_str()) {
                        return Err(msg::REGEX_BAD_CLASS);
                    }
                    set.push_str(&format!("[:{name}:]"));
                } else {
                    for ch in name.chars() {
                        push_set_char(&mut set, ch);
                    }
                }
                j += p + 4;
                continue;
            }
        }
        if c == '-' && j + 1 < chars.len() && chars[j + 1] != ']' && !set.ends_with('[') {
            set.push('-');
        } else {
            push_set_char(&mut set, c);
        }
        j += 1;
    }
    Err(msg::REGEX_INVALID)
}

fn push_set_char(set: &mut String, c: char) {
    if matches!(c, '\\' | '[' | ']' | '&' | '~' | '-' | '^') {
        set.push('\\');
    }
    set.push(c);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grep(f: impl FnOnce(&mut GrepOptions)) -> CommitGrep {
        let mut o = GrepOptions::default();
        f(&mut o);
        CommitGrep::new(&o).unwrap()
    }

    const RAW: &[u8] = b"tree 0000\nauthor Ann Bee <ann@ex.com> 1 +0000\ncommitter Carl <c@ex.com> 2 +0000\n\nfirst subject\n\nbody foo+bar x*y\n";

    #[test]
    fn basic_translation() {
        let t = |p: &str, e: bool| translate(p, e).unwrap();
        assert_eq!(t(r"\(a\|b\)\+c", false), "(a|b)+c");
        assert_eq!(t("(a)+?{", false), r"\(a\)\+\?\{");
        assert_eq!(t("*a^b$c$", false), r"\*a\^b\$c$");
        assert_eq!(t(r"\<w\>", false), r"\bw\b");
        assert_eq!(t("[]a-z[:digit:]]", false), r"[\]a-z[:digit:]]");
        assert_eq!(t("(a|b)+x{2}y{", true), r"(a|b)+x{2}y\{");
        assert_eq!(translate("[a", false), Err(msg::REGEX_INVALID));
    }

    #[test]
    fn message_and_identity() {
        assert!(grep(|o| o.message = vec!["^body".into()]).matches(RAW));
        assert!(!grep(|o| o.message = vec!["subject.body".into()]).matches(RAW));
        assert!(!grep(|o| o.message = vec!["^tree".into()]).matches(RAW));
        assert!(grep(|o| o.message = vec!["o+b".into()]).matches(RAW));
        assert!(
            !grep(|o| {
                o.message = vec!["o+b".into()];
                o.syntax = Syntax::Extended;
            })
            .matches(RAW)
        );
        assert!(
            grep(|o| {
                o.message = vec!["x*y".into()];
                o.syntax = Syntax::Fixed;
            })
            .matches(RAW)
        );
        assert!(grep(|o| o.author = vec!["^Ann".into(), "nobody".into()]).matches(RAW));
        assert!(grep(|o| o.author = vec!["com>$".into()]).matches(RAW));
        assert!(!grep(|o| o.author = vec!["^author".into()]).matches(RAW));
        assert!(!grep(|o| o.committer = vec!["Ann".into()]).matches(RAW));
        assert!(
            grep(|o| {
                o.author = vec!["ann bee".into()];
                o.ignore_case = true;
            })
            .matches(RAW)
        );
    }

    #[test]
    fn errors() {
        let err = |p: &str, syntax: Syntax| {
            CommitGrep::new(&GrepOptions {
                message: vec![p.into()],
                syntax,
                ..Default::default()
            })
            .unwrap_err()
        };
        assert_eq!(
            err(r"\(a", Syntax::Basic),
            "command line, '\\(a': Unmatched ( or \\("
        );
        assert_eq!(
            err("a{1", Syntax::Extended),
            "command line, 'a{1': Unmatched \\{"
        );
        assert_eq!(
            err("[[:foo:]]", Syntax::Basic),
            "command line, '[[:foo:]]': Invalid character class name"
        );
        let header = CommitGrep::new(&GrepOptions {
            author: vec!["[".into()],
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(header, "header, '[': Invalid regular expression");
    }

    #[test]
    fn all_match_and_invert() {
        let pats = || vec!["first".to_string(), "nothing".to_string()];
        assert!(grep(|o| o.message = pats()).matches(RAW));
        assert!(
            !grep(|o| {
                o.message = pats();
                o.all_match = true;
            })
            .matches(RAW)
        );
        assert!(
            !grep(|o| {
                o.message = pats();
                o.all_match = true;
                o.invert = true;
            })
            .matches(RAW)
        );
        assert!(
            grep(|o| {
                o.message = vec!["nothing".into()];
                o.invert = true;
            })
            .matches(RAW)
        );
    }
}
