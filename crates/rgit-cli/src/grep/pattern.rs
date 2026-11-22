// SPDX-License-Identifier: MIT
//! The patterns of `git grep` and the boolean expression combining them
//! (Documentation/git-grep.adoc: `-e`, `--and`, `--or`, `--not`, `(`, `)`,
//! `-E`, `-G`, `-F`, `-P`, `-i`, `-w`).
//!
//! Behaviour checked against the `git` binary:
//!
//! - Basic and extended patterns follow POSIX: of the matches starting
//!   at the leftmost position the longest wins (`-E 'H|He'` matches `He`).
//!   They are translated into the syntax of the `regex` crate, which finds
//!   the leftmost start; a DFA run anchored there with "all matches"
//!   semantics then finds the longest end. `-P` keeps the leftmost-first
//!   semantics of Perl.
//! - `-w`: a match counts only when it is neither preceded nor followed
//!   by a word character (ASCII letter, digit, `_`); otherwise the search
//!   goes on from one byte after the start of the rejected match.
//! - Adjacent expressions are combined with "or"; `--or` itself is
//!   accepted anywhere and changes nothing. `--and` binds tighter than the
//!   implicit "or", `--not` tighter than `--and`.
//! - Colouring, `-o` and `--column` look at the matches of all patterns,
//!   also those below `--not`: at each position the leftmost (then the
//!   longest) match of any pattern is taken, and an empty match ends the
//!   line.

use std::cell::RefCell;

use regex::bytes::{Regex, RegexBuilder};
use regex_automata::hybrid::dfa::{Cache, DFA};
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input, MatchKind};

use crate::messages::grep as msg;

/// How patterns are written.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// `-G`, the default: POSIX basic regular expressions.
    #[default]
    Basic,
    /// `-E`: POSIX extended regular expressions.
    Extended,
    /// `-F`: fixed strings.
    Fixed,
    /// `-P`: Perl-compatible regular expressions (the syntax of the
    /// `regex` crate).
    Perl,
}

impl Syntax {
    /// The value of `grep.patternType` (`default` is `None`).
    pub fn from_config(v: &str) -> Option<Option<Syntax>> {
        Some(match v.to_ascii_lowercase().as_str() {
            "default" => None,
            "basic" => Some(Syntax::Basic),
            "extended" => Some(Syntax::Extended),
            "fixed" => Some(Syntax::Fixed),
            "perl" => Some(Syntax::Perl),
            _ => return None,
        })
    }
}

/// One compiled pattern.
pub struct Atom {
    /// Finds the leftmost match.
    first: Regex,
    /// The same pattern with `^`/`$` matching at line ends, to rule out a
    /// whole buffer at once.
    whole: Regex,
    /// Finds the longest match at a given start (POSIX syntaxes).
    longest: Option<(DFA, RefCell<Cache>)>,
    word: bool,
}

/// Whether the character type locale is UTF-8 (`LC_ALL`, else
/// `LC_CTYPE`, else `LANG`). git's regular expressions then work on
/// characters, else on bytes.
pub fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

/// POSIX character classes as the C library defines them in a UTF-8
/// locale (the `regex` crate's own are ASCII only). The no-break spaces
/// are not spaces there.
const UNICODE_CLASSES: [(&str, &str); 10] = [
    ("[:alpha:]", r"\p{Alphabetic}"),
    ("[:alnum:]", r"\p{Alphabetic}0-9"),
    ("[:upper:]", r"\p{Uppercase}"),
    ("[:lower:]", r"\p{Lowercase}"),
    (
        "[:punct:]",
        r"[[^\p{White_Space}\p{C}\p{Alphabetic}0-9]\u{A0}\u{2007}\u{202F}]",
    ),
    (
        "[:graph:]",
        r"[[^\p{White_Space}\p{C}]\u{A0}\u{2007}\u{202F}]",
    ),
    ("[:print:]", r"[^\p{C}]"),
    ("[:space:]", r"[\s--[\u{A0}\u{2007}\u{202F}]]"),
    ("[:blank:]", r"[\t\p{Zs}--[\u{A0}\u{2007}\u{202F}]]"),
    ("[:cntrl:]", r"\p{Cc}"),
];

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Atom {
    /// Compile `pattern`; the error is the reason git gives.
    /// `utf8`: match characters of a UTF-8 locale instead of bytes.
    pub fn new(
        pattern: &str,
        syn: Syntax,
        icase: bool,
        word: bool,
        utf8: bool,
    ) -> Result<Atom, String> {
        // A pattern with newlines is a list of alternatives.
        let mut pieces = Vec::new();
        for piece in pattern.split('\n') {
            pieces.push(match syn {
                Syntax::Fixed => regex::escape(piece),
                Syntax::Perl => piece.to_string(),
                Syntax::Basic => {
                    rgit_revwalk::walk::grep::translate(piece, false).map_err(str::to_string)?
                }
                Syntax::Extended => {
                    rgit_revwalk::walk::grep::translate(piece, true).map_err(str::to_string)?
                }
            });
        }
        let text = if pieces.len() == 1 {
            pieces.pop().unwrap_or_default()
        } else {
            pieces
                .iter()
                .map(|p| format!("(?:{p})"))
                .collect::<Vec<_>>()
                .join("|")
        };
        let perl = syn == Syntax::Perl;
        let mut text = text;
        if utf8 && syn != Syntax::Fixed {
            for (class, unicode) in UNICODE_CLASSES {
                text = text.replace(class, unicode);
            }
        }
        let unicode = perl || utf8;
        let build = |multi: bool| {
            RegexBuilder::new(&text)
                .case_insensitive(icase)
                .unicode(unicode)
                .multi_line(multi)
                .build()
        };
        let first = build(false).map_err(|e| {
            let e = e.to_string();
            if perl {
                e.lines()
                    .rev()
                    .find(|l| l.starts_with("error: "))
                    .map_or(e.clone(), |l| l["error: ".len()..].to_string())
            } else {
                rgit_revwalk::walk::grep::reason(&e).to_string()
            }
        })?;
        let whole = build(true).map_err(|e| e.to_string())?;
        let longest = if perl {
            None
        } else {
            DFA::builder()
                .syntax(
                    syntax::Config::new()
                        .case_insensitive(icase)
                        .unicode(unicode)
                        .utf8(false),
                )
                .thompson(regex_automata::nfa::thompson::Config::new().utf8(false))
                .configure(DFA::config().match_kind(MatchKind::All))
                .build(&text)
                .ok()
                .map(|dfa| {
                    let cache = dfa.create_cache();
                    (dfa, RefCell::new(cache))
                })
        };
        Ok(Atom {
            first,
            whole,
            longest,
            word,
        })
    }

    /// The leftmost(-longest) match in `line` starting at or after `from`.
    fn raw_find(&self, line: &[u8], from: usize) -> Option<(usize, usize)> {
        let m = self.first.find_at(line, from)?;
        let (s, mut e) = (m.start(), m.end());
        if let Some((dfa, cache)) = &self.longest {
            let input = Input::new(line).range(s..).anchored(Anchored::Yes);
            if let Ok(Some(hm)) = dfa.try_search_fwd(&mut cache.borrow_mut(), &input) {
                e = e.max(hm.offset());
            }
        }
        Some((s, e))
    }

    /// The first match in `line` at or after `from`, honouring `-w`.
    pub fn find(&self, line: &[u8], mut from: usize) -> Option<(usize, usize)> {
        loop {
            if from > line.len() {
                return None;
            }
            let (s, e) = self.raw_find(line, from)?;
            if !self.word {
                return Some((s, e));
            }
            let before = s == 0 || !is_word(line[s - 1]);
            let after = e == line.len() || !is_word(line[e]);
            if before && after {
                return Some((s, e));
            }
            from = s + 1;
        }
    }

    /// Whether the pattern matches somewhere in the line.
    pub fn is_match(&self, line: &[u8]) -> bool {
        if self.word {
            self.find(line, 0).is_some()
        } else {
            self.first.is_match(line)
        }
    }

    /// Whether the pattern may match some line of `buf` (no false
    /// negatives).
    pub fn may_match_buffer(&self, buf: &[u8]) -> bool {
        self.whole.is_match(buf)
    }
}

/// A command line token of the pattern expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Pattern(String),
    And,
    Or,
    Not,
    Open,
    Close,
}

impl Token {
    fn shown(&self) -> &str {
        match self {
            Token::Pattern(p) => p,
            Token::And => "--and",
            Token::Or => "--or",
            Token::Not => "--not",
            Token::Open => "(",
            Token::Close => ")",
        }
    }
}

/// The pattern expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Atom(usize),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

impl Expr {
    fn has_not(&self) -> bool {
        match self {
            Expr::Atom(_) => false,
            Expr::Not(_) => true,
            Expr::And(a, b) | Expr::Or(a, b) => a.has_not() || b.has_not(),
        }
    }

    /// The terms of the top-level "or" (for `--all-match`).
    fn or_terms(&self) -> Vec<&Expr> {
        match self {
            Expr::Or(a, b) => {
                let mut v = a.or_terms();
                v.extend(b.or_terms());
                v
            }
            e => vec![e],
        }
    }
}

struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    atoms: Vec<String>,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn or(&mut self) -> Result<Expr, String> {
        let left = self.and()?;
        match self.peek() {
            None | Some(Token::Close) => Ok(left),
            Some(_) => Ok(Expr::Or(Box::new(left), Box::new(self.or()?))),
        }
    }

    fn and(&mut self) -> Result<Expr, String> {
        let left = self.not()?;
        if self.peek() == Some(&Token::And) {
            self.pos += 1;
            match self.peek() {
                Some(Token::Pattern(_) | Token::Open | Token::Not) => {}
                _ => return Err(msg::AND_NOT_FOLLOWED.into()),
            }
            return Ok(Expr::And(Box::new(left), Box::new(self.and()?)));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, String> {
        if self.peek() == Some(&Token::Not) {
            self.pos += 1;
            match self.peek() {
                Some(Token::Pattern(_) | Token::Open | Token::Not) => {}
                _ => return Err(msg::NOT_NOT_FOLLOWED.into()),
            }
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Token::Pattern(p)) => {
                self.pos += 1;
                self.atoms.push(p);
                Ok(Expr::Atom(self.atoms.len() - 1))
            }
            Some(Token::Open) => {
                self.pos += 1;
                let e = self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return Err(msg::UNMATCHED_PAREN.into());
                }
                self.pos += 1;
                Ok(e)
            }
            Some(Token::And) => Err(msg::AND_NOT_PRECEDED.into()),
            // Only reachable right after `(`.
            None => Err(msg::UNMATCHED_PAREN.into()),
            _ => Err(msg::NOT_A_PATTERN.into()),
        }
    }
}

/// Parse the tokens into an expression and its patterns (in expression
/// order). `--or` tokens are dropped first.
pub fn parse(tokens: &[Token]) -> Result<(Expr, Vec<String>), String> {
    let tokens: Vec<Token> = tokens
        .iter()
        .filter(|t| **t != Token::Or)
        .cloned()
        .collect();
    if tokens.is_empty() {
        return Err(msg::NO_PATTERN.into());
    }
    let mut p = ExprParser {
        tokens: &tokens,
        pos: 0,
        atoms: Vec::new(),
    };
    let e = p.or()?;
    if let Some(t) = p.peek() {
        return Err(msg::incomplete_group(t.shown()));
    }
    Ok((e, p.atoms))
}

/// Compiled patterns and their expression.
pub struct Matcher {
    pub atoms: Vec<Atom>,
    expr: Expr,
    /// `--all-match`: the top-level "or" terms.
    terms: Option<Vec<Expr>>,
    pub invert: bool,
    has_not: bool,
    /// The pattern is just `^$`: git then also reports the empty line
    /// after the final newline of a file whose last line is not empty.
    pub empty_line_at_end: bool,
}

impl Matcher {
    pub fn new(expr: Expr, atoms: Vec<Atom>, all_match: bool, invert: bool) -> Matcher {
        let terms = all_match.then(|| expr.or_terms().into_iter().cloned().collect());
        let has_not = expr.has_not();
        Matcher {
            atoms,
            expr,
            terms,
            invert,
            has_not,
            empty_line_at_end: false,
        }
    }

    fn eval(&self, e: &Expr, line: &[u8]) -> bool {
        match e {
            Expr::Atom(i) => self.atoms[*i].is_match(line),
            Expr::Not(x) => !self.eval(x, line),
            Expr::And(a, b) => self.eval(a, line) && self.eval(b, line),
            Expr::Or(a, b) => self.eval(a, line) || self.eval(b, line),
        }
    }

    /// Whether the expression matches `line` (before `-v`).
    pub fn matches(&self, line: &[u8]) -> bool {
        self.eval(&self.expr, line)
    }

    /// With `--all-match`, which top-level terms match `line`.
    pub fn term_hits(&self, line: &[u8], hits: &mut [bool]) {
        if let Some(terms) = &self.terms {
            for (t, h) in terms.iter().zip(hits.iter_mut()) {
                if !*h && self.eval(t, line) {
                    *h = true;
                }
            }
        }
    }

    /// The number of `--all-match` terms (0 without the option).
    pub fn term_count(&self) -> usize {
        self.terms.as_ref().map_or(0, Vec::len)
    }

    /// Whether no line of `buf` can be selected: no pattern matches it
    /// anywhere and the expression only selects lines with matches.
    pub fn rules_out(&self, buf: &[u8]) -> bool {
        !self.invert && !self.has_not && !self.atoms.iter().any(|a| a.may_match_buffer(buf))
    }

    /// The next match of any pattern at or after `from`: leftmost, then
    /// longest. An empty match is returned as such (callers stop there).
    pub fn next_match(&self, line: &[u8], from: usize) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for a in &self.atoms {
            if let Some((s, e)) = a.find(line, from) {
                best = match best {
                    Some((bs, be)) if bs < s || (bs == s && be >= e) => Some((bs, be)),
                    _ => Some((s, e)),
                };
            }
        }
        best
    }

    /// `--column`: one plus the offset of the first match of any pattern,
    /// 1 when there is none.
    pub fn column(&self, line: &[u8]) -> usize {
        self.atoms
            .iter()
            .filter_map(|a| a.find(line, 0).map(|m| m.0))
            .min()
            .map_or(1, |s| s + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(s: &[&str]) -> Vec<Token> {
        s.iter()
            .map(|t| match *t {
                "--and" => Token::And,
                "--or" => Token::Or,
                "--not" => Token::Not,
                "(" => Token::Open,
                ")" => Token::Close,
                p => Token::Pattern(p.to_string()),
            })
            .collect()
    }

    fn matcher(s: &[&str], syn: Syntax) -> Matcher {
        let (e, pats) = parse(&tokens(s)).unwrap();
        let atoms = pats
            .iter()
            .map(|p| Atom::new(p, syn, false, false, false).unwrap())
            .collect();
        Matcher::new(e, atoms, false, false)
    }

    #[test]
    fn longest_match() {
        let m = matcher(&["H|He"], Syntax::Extended);
        assert_eq!(m.next_match(b"Hello", 0), Some((0, 2)));
        let m = matcher(&["He", "Hel"], Syntax::Basic);
        assert_eq!(m.next_match(b"Hello", 0), Some((0, 3)));
        let m = matcher(&["H|He"], Syntax::Perl);
        assert_eq!(m.next_match(b"Hello", 0), Some((0, 1)));
    }

    #[test]
    fn word_retry() {
        let a = Atom::new("x xx* x", Syntax::Basic, false, true, false).unwrap();
        assert_eq!(a.find(b"x x xx x", 0), Some((2, 8)));
        let a = Atom::new("mmap", Syntax::Basic, false, true, false).unwrap();
        assert_eq!(a.find(b"foo_mmap bar mmap", 0), Some((13, 17)));
        let a = Atom::new("foo|foo-bar", Syntax::Extended, false, true, false).unwrap();
        assert_eq!(a.find(b"foo-bar_x", 0), None);
    }

    #[test]
    fn expressions() {
        let m = matcher(&["foo mmap", "--and", "--not", "bar_mmap"], Syntax::Basic);
        assert!(m.matches(b"foo mmap bar"));
        assert!(!m.matches(b"foo mmap bar_mmap"));
        let m = matcher(
            &["(", "foo_", "--or", "baz", ")", "--and", " mmap"],
            Syntax::Basic,
        );
        assert!(m.matches(b"foo_mmap bar mmap"));
        assert!(!m.matches(b"foo mmap bar"));
        let m = matcher(&["A", "--not", "--or", "B"], Syntax::Basic);
        assert!(m.matches(b"x"));
        assert!(!m.matches(b"B"));
    }

    #[test]
    fn expression_errors() {
        let err = |s: &[&str]| parse(&tokens(s)).unwrap_err();
        assert_eq!(err(&["A", "--and"]), msg::AND_NOT_FOLLOWED);
        assert_eq!(err(&["--and", "A"]), msg::AND_NOT_PRECEDED);
        assert_eq!(err(&["("]), msg::UNMATCHED_PAREN);
        assert_eq!(err(&["A", ")"]), msg::incomplete_group(")"));
        assert_eq!(err(&["--not"]), msg::NOT_NOT_FOLLOWED);
        assert_eq!(err(&[]), msg::NO_PATTERN);
        assert!(parse(&tokens(&["A", "--or"])).is_ok());
    }

    #[test]
    fn column_and_empty() {
        let m = matcher(&["bar", "mmap"], Syntax::Basic);
        assert_eq!(m.column(b"foo mmap bar"), 5);
        let m = matcher(&["l*"], Syntax::Basic);
        assert_eq!(m.next_match(b"Hello", 0), Some((0, 0)));
        assert_eq!(m.column(b"Hello"), 1);
    }
}
