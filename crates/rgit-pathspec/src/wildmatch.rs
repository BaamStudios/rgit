// SPDX-License-Identifier: MIT

//! git's glob engine ("wildmatch").
//!
//! Written from the pattern description in gitignore(5) and the pathspec
//! entry of gitglossary(7), and checked against the `git` binary (see the
//! crate's differential tests).
//!
//! | Pattern | Meaning |
//! |---|---|
//! | `?` | any one byte (not `/` with [`WildmatchFlags::PATHNAME`]) |
//! | `*` | any run of bytes (not crossing `/` with `PATHNAME`) |
//! | `**` | with `PATHNAME`, when it forms a whole path component (`**/x`, `x/**`, `a/**/b`): any number of directories, including none. Elsewhere a plain `*`. |
//! | `[...]` | one byte from a set: ranges `a-z`, negation `[!...]` / `[^...]`, POSIX classes `[:alpha:]` etc., `]` first is literal, `\` escapes. Never matches `/` with `PATHNAME`. |
//! | `\x` | the byte `x` literally |
//!
//! Malformed patterns never match: an unterminated `[`, an unknown
//! character class name or a trailing `\`.
//!
//! With [`WildmatchFlags::CASEFOLD`] the text is compared in lower case.
//! Plain pattern letters are lowered too, but escaped letters (`\F`) and
//! bracket members (`[F]`) are taken as written, so they only match when
//! written in lower case. Ranges and `[:upper:]` also accept the other case.
//!
//! The matcher backtracks only as far as needed: a failed `*` that ran out of
//! text gives up the whole match, and a `*` that hit a `/` it may not cross
//! hands control back to the nearest `**`. That keeps patterns like
//! `*a*a*a*a*...` linear-ish instead of exponential.

use std::ops::{BitOr, BitOrAssign};

/// Matching flags for [`wildmatch`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WildmatchFlags(u8);

impl WildmatchFlags {
    /// No flags: `*`, `?` and brackets match `/` like any other byte.
    pub const NONE: Self = Self(0);
    /// Treat `/` as a separator that only a literal `/` or `**` can match.
    pub const PATHNAME: Self = Self(1);
    /// ASCII case-insensitive matching.
    pub const CASEFOLD: Self = Self(2);

    /// Whether all flags in `other` are set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for WildmatchFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for WildmatchFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Matches `text` against the glob `pattern`.
///
/// ```
/// use rgit_pathspec::wildmatch::{wildmatch, WildmatchFlags};
///
/// assert!(wildmatch(b"foo/**/bar", b"foo/bar", WildmatchFlags::PATHNAME));
/// assert!(!wildmatch(b"foo/*", b"foo/a/b", WildmatchFlags::PATHNAME));
/// assert!(wildmatch(b"foo/*", b"foo/a/b", WildmatchFlags::NONE));
/// assert!(wildmatch(b"[[:upper:]]", b"a", WildmatchFlags::CASEFOLD));
/// ```
#[must_use]
pub fn wildmatch(pattern: &[u8], text: &[u8], flags: WildmatchFlags) -> bool {
    let m = Matcher {
        pathname: flags.contains(WildmatchFlags::PATHNAME),
        casefold: flags.contains(WildmatchFlags::CASEFOLD),
    };
    m.run(pattern, 0, text) == Outcome::Match
}

/// Path glob in the gitignore(5) sense: [`wildmatch`] with
/// [`WildmatchFlags::PATHNAME`], optionally case-insensitive.
///
/// This is the simple entry point for callers like `includeIf "gitdir:..."`.
#[must_use]
pub fn match_path_glob(pattern: &[u8], text: &[u8], icase: bool) -> bool {
    let mut flags = WildmatchFlags::PATHNAME;
    if icase {
        flags |= WildmatchFlags::CASEFOLD;
    }
    wildmatch(pattern, text, flags)
}

/// Whether `pattern` contains a byte that [`wildmatch`] treats specially
/// (`*`, `?`, `[` or `\`).
#[must_use]
pub fn has_glob_special(pattern: &[u8]) -> bool {
    first_glob_special(pattern).is_some()
}

/// Offset of the first byte that [`wildmatch`] treats specially.
#[must_use]
pub fn first_glob_special(pattern: &[u8]) -> Option<usize> {
    pattern.iter().position(|b| is_glob_special(*b))
}

pub(crate) fn is_glob_special(b: u8) -> bool {
    matches!(b, b'*' | b'?' | b'[' | b'\\')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Match,
    NoMatch,
    /// No later alignment of any enclosing `*` can succeed.
    AbortAll,
    /// Only an enclosing `**` (which may cross `/`) can still succeed.
    AbortToDoubleStar,
}

struct Matcher {
    pathname: bool,
    casefold: bool,
}

impl Matcher {
    /// The text byte as compared: lowercased with CASEFOLD. Pattern bytes
    /// are lowercased only when they are plain literals outside brackets;
    /// escaped bytes and bracket members are taken as written.
    fn fold(&self, c: u8) -> u8 {
        if self.casefold {
            c.to_ascii_lowercase()
        } else {
            c
        }
    }

    fn run(&self, p: &[u8], mut pi: usize, t: &[u8]) -> Outcome {
        let mut ti = 0;
        while pi < p.len() {
            let pc = p[pi];
            if ti == t.len() && pc != b'*' {
                return Outcome::AbortAll;
            }
            match pc {
                b'\\' => {
                    pi += 1;
                    let Some(&lit) = p.get(pi) else {
                        // Trailing backslash: malformed.
                        return Outcome::NoMatch;
                    };
                    if lit != self.fold(t[ti]) {
                        return Outcome::NoMatch;
                    }
                }
                b'?' => {
                    if self.pathname && t[ti] == b'/' {
                        return Outcome::NoMatch;
                    }
                }
                b'*' => return self.star(p, pi, t, ti),
                b'[' => match self.bracket(p, pi + 1, t[ti]) {
                    Ok((true, next)) => {
                        pi = next;
                        ti += 1;
                        continue;
                    }
                    Ok((false, _)) => return Outcome::NoMatch,
                    Err(()) => return Outcome::AbortAll,
                },
                _ => {
                    if self.fold(pc) != self.fold(t[ti]) {
                        return Outcome::NoMatch;
                    }
                }
            }
            pi += 1;
            ti += 1;
        }
        if ti == t.len() {
            Outcome::Match
        } else {
            Outcome::NoMatch
        }
    }

    /// Handles a run of `*` starting at `p[start]`, matched at `t[ti..]`.
    fn star(&self, p: &[u8], start: usize, t: &[u8], ti: usize) -> Outcome {
        let mut end = start;
        while end < p.len() && p[end] == b'*' {
            end += 1;
        }
        let crosses_slash = if !self.pathname {
            true
        } else if end - start >= 2
            && (start == 0 || p[start - 1] == b'/')
            && (end == p.len() || p[end] == b'/')
        {
            // `**` as a whole component. `**/` may also match nothing at
            // all, swallowing its slash (`a/**/b` matches `a/b`).
            if end < p.len() && self.run(p, end + 1, &t[ti..]) == Outcome::Match {
                return Outcome::Match;
            }
            true
        } else {
            false
        };

        if end == p.len() {
            // Trailing star: matches the rest unless it would cross a `/`.
            if !crosses_slash && t[ti..].contains(&b'/') {
                return Outcome::AbortToDoubleStar;
            }
            return Outcome::Match;
        }

        let mut k = ti;
        loop {
            let r = self.run(p, end, &t[k..]);
            if r != Outcome::NoMatch && (!crosses_slash || r != Outcome::AbortToDoubleStar) {
                return r;
            }
            if k == t.len() {
                break;
            }
            if !crosses_slash && t[k] == b'/' {
                return Outcome::AbortToDoubleStar;
            }
            k += 1;
        }
        Outcome::AbortAll
    }

    /// Matches `c` against the bracket expression whose body starts at
    /// `p[pi]` (just after `[`). Returns whether it matched and the index
    /// after the closing `]`, or `Err` for a malformed expression.
    fn bracket(&self, p: &[u8], mut pi: usize, raw: u8) -> Result<(bool, usize), ()> {
        let c = self.fold(raw);
        let negated = matches!(p.get(pi), Some(b'!' | b'^'));
        if negated {
            pi += 1;
        }
        let mut matched = false;
        let mut prev: Option<u8> = None;
        let mut first = true;
        loop {
            let Some(&b) = p.get(pi) else {
                return Err(());
            };
            if b == b']' && !first {
                break;
            }
            first = false;
            if b == b'\\' {
                pi += 1;
                let &lit = p.get(pi).ok_or(())?;
                if lit == c {
                    matched = true;
                }
                prev = Some(lit);
            } else if b == b'-' && prev.is_some() && p.get(pi + 1).is_some_and(|&n| n != b']') {
                pi += 1;
                let mut hi = p[pi];
                if hi == b'\\' {
                    pi += 1;
                    hi = *p.get(pi).ok_or(())?;
                }
                let lo = prev.take().unwrap_or(hi);
                if self.in_range(lo, hi, c) {
                    matched = true;
                }
            } else if b == b'[' && p.get(pi + 1) == Some(&b':') {
                match class_at(p, pi) {
                    ClassAt::Class(name, close) => {
                        let class = CharClass::from_name(name).ok_or(())?;
                        if self.in_class(class, c) {
                            matched = true;
                        }
                        pi = close;
                        prev = None;
                    }
                    ClassAt::NotAClass => {
                        if b == c {
                            matched = true;
                        }
                        prev = Some(b);
                    }
                    ClassAt::Unterminated => return Err(()),
                }
            } else {
                if b == c {
                    matched = true;
                }
                prev = Some(b);
            }
            pi += 1;
        }
        let ok = matched != negated && !(self.pathname && raw == b'/');
        Ok((ok, pi + 1))
    }

    /// `c` is already folded.
    fn in_range(&self, lo: u8, hi: u8, c: u8) -> bool {
        (lo..=hi).contains(&c)
            || (self.casefold
                && c.is_ascii_lowercase()
                && (lo..=hi).contains(&c.to_ascii_uppercase()))
    }

    /// `c` is already folded.
    fn in_class(&self, class: CharClass, c: u8) -> bool {
        class.contains(c) || (self.casefold && class == CharClass::Upper && c.is_ascii_lowercase())
    }
}

enum ClassAt<'a> {
    /// `[:name:]`, with the index of its closing `]`.
    Class(&'a [u8], usize),
    /// `[` followed by `:` but not a well-formed class; the `[` is literal.
    NotAClass,
    /// No `]` follows at all.
    Unterminated,
}

/// Looks at `p[open..]`, which starts with `[:`.
fn class_at(p: &[u8], open: usize) -> ClassAt<'_> {
    let name_start = open + 2;
    let Some(rel) = p[name_start..].iter().position(|&b| b == b']') else {
        return ClassAt::Unterminated;
    };
    let close = name_start + rel;
    if close > name_start && p[close - 1] == b':' {
        ClassAt::Class(&p[name_start..close - 1], close)
    } else {
        ClassAt::NotAClass
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    Alnum,
    Alpha,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Xdigit,
}

impl CharClass {
    fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"alnum" => Self::Alnum,
            b"alpha" => Self::Alpha,
            b"blank" => Self::Blank,
            b"cntrl" => Self::Cntrl,
            b"digit" => Self::Digit,
            b"graph" => Self::Graph,
            b"lower" => Self::Lower,
            b"print" => Self::Print,
            b"punct" => Self::Punct,
            b"space" => Self::Space,
            b"upper" => Self::Upper,
            b"xdigit" => Self::Xdigit,
            _ => return None,
        })
    }

    /// ASCII-only classification, independent of the locale.
    fn contains(self, c: u8) -> bool {
        match self {
            Self::Alnum => c.is_ascii_alphanumeric(),
            Self::Alpha => c.is_ascii_alphabetic(),
            Self::Blank => c == b' ' || c == b'\t',
            Self::Cntrl => c.is_ascii_control(),
            Self::Digit => c.is_ascii_digit(),
            Self::Graph => c.is_ascii_graphic(),
            Self::Lower => c.is_ascii_lowercase(),
            Self::Print => c.is_ascii_graphic() || c == b' ',
            Self::Punct => c.is_ascii_punctuation(),
            Self::Space => matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
            Self::Upper => c.is_ascii_uppercase(),
            Self::Xdigit => c.is_ascii_hexdigit(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(pattern: &str, text: &str) -> [bool; 4] {
        let p = pattern.as_bytes();
        let t = text.as_bytes();
        [
            wildmatch(p, t, WildmatchFlags::PATHNAME),
            wildmatch(p, t, WildmatchFlags::PATHNAME | WildmatchFlags::CASEFOLD),
            wildmatch(p, t, WildmatchFlags::NONE),
            wildmatch(p, t, WildmatchFlags::CASEFOLD),
        ]
    }

    #[test]
    fn basics() {
        assert_eq!(all("foo", "foo"), [true; 4]);
        assert_eq!(all("*", ""), [true; 4]);
        assert_eq!(all("", ""), [true; 4]);
        assert_eq!(all("", "foo"), [false; 4]);
        assert_eq!(all("foo\\", "foo\\"), [false; 4]);
        assert_eq!(all("foo*bar", "foo/baz/bar"), [false, false, true, true]);
        assert_eq!(all("**/foo", "foo"), [true, true, false, false]);
        assert_eq!(all("a/**", "a/"), [true, true, true, true]);
        assert_eq!(all("[[:spaci:]1]", "1"), [false; 4]);
        assert_eq!(all("[Z-y]", "z"), [false, true, false, true]);
    }

    #[test]
    fn no_exponential_blowup() {
        let text = format!("{}b", "a".repeat(60));
        let pattern = "*a".repeat(16);
        assert!(!wildmatch(
            pattern.as_bytes(),
            text.as_bytes(),
            WildmatchFlags::NONE
        ));
        assert!(!wildmatch(
            pattern.as_bytes(),
            text.as_bytes(),
            WildmatchFlags::PATHNAME
        ));
    }
}
