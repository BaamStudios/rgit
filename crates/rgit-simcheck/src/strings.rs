// SPDX-License-Identifier: MIT
//! Verbatim string literal check.
//!
//! Literals are compared after canonicalising format placeholders (`%s`,
//! `%"PRIuMAX"`, `{}`, `{name:>8}` all become one placeholder character), so a
//! C `printf` format and a Rust `format!` string with the same text compare
//! equal.

use crate::lex::{Kind, Lang, Tok};

pub const PLACEHOLDER: char = '\u{1}';

/// String literals of a token stream with their line; adjacent C literals are
/// concatenated like the compiler does.
pub fn literals(toks: &[Tok], lang: Lang) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = Vec::new();
    let mut prev_str = false;
    for t in toks {
        if t.kind == Kind::Str {
            let canon = canonical(&t.text, lang);
            if prev_str && lang == Lang::C {
                if let Some(last) = out.last_mut() {
                    last.0.push_str(&canon);
                }
            } else {
                out.push((canon, t.line));
            }
            prev_str = true;
        } else {
            prev_str = false;
        }
    }
    out
}

pub fn canonical(s: &str, lang: Lang) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match (lang, c) {
            (Lang::C, '%') => {
                if chars.get(i + 1) == Some(&'%') {
                    out.push('%');
                    i += 2;
                    continue;
                }
                let mut j = i + 1;
                while j < chars.len() && "-+ #0".contains(chars[j]) {
                    j += 1;
                }
                while j < chars.len()
                    && (chars[j].is_ascii_digit() || chars[j] == '*' || chars[j] == '.')
                {
                    j += 1;
                }
                while j < chars.len() && "hlLzjt".contains(chars[j]) {
                    j += 1;
                }
                if j < chars.len() && "diouxXeEfgGcspn".contains(chars[j]) {
                    out.push(PLACEHOLDER);
                    i = j + 1;
                } else if j == chars.len() {
                    // `"%" PRIuMAX` style: the conversion follows outside the literal.
                    out.push(PLACEHOLDER);
                    i = j;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            (Lang::Rust, '{') => {
                if chars.get(i + 1) == Some(&'{') {
                    out.push('{');
                    i += 2;
                    continue;
                }
                match chars[i + 1..].iter().position(|&c| c == '}' || c == '{') {
                    Some(off) if chars[i + 1 + off] == '}' => {
                        out.push(PLACEHOLDER);
                        i += off + 2;
                    }
                    _ => {
                        out.push('{');
                        i += 1;
                    }
                }
            }
            (Lang::Rust, '}') if chars.get(i + 1) == Some(&'}') => {
                out.push('}');
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Visible length: characters that are not placeholders.
pub fn visible_len(s: &str) -> usize {
    s.chars().filter(|&c| c != PLACEHOLDER).count()
}

/// Whether a literal takes part in the verbatim check: long enough and not
/// a pure hex constant or escaped byte string (object ids and hash test
/// vectors are data fixed by the format, not expression).
pub fn comparable(s: &str, min_len: usize) -> bool {
    if s.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    // Length without `\xNN` escapes.
    let mut len = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == PLACEHOLDER {
            continue;
        }
        if c == '\\' && chars.peek() == Some(&'x') {
            chars.next();
            chars.next();
            chars.next();
            continue;
        }
        len += 1;
    }
    len >= min_len
}

/// A haystack of many literals with a map back to where each came from.
pub struct Haystack {
    pub text: String,
    /// (byte offset, owner index, line), sorted by offset.
    starts: Vec<(usize, usize, u32)>,
}

impl Haystack {
    pub fn new() -> Haystack {
        Haystack {
            text: String::new(),
            starts: Vec::new(),
        }
    }

    pub fn add(&mut self, s: &str, owner: usize, line: u32) {
        self.starts.push((self.text.len(), owner, line));
        self.text.push_str(s);
        self.text.push('\0');
    }

    pub fn locate(&self, off: usize) -> (usize, u32) {
        let idx = match self.starts.binary_search_by_key(&off, |s| s.0) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (self.starts[idx].1, self.starts[idx].2)
    }

    /// Locations of all occurrences of `needle`, at most `limit`.
    pub fn find_all(&self, needle: &str, limit: usize) -> Vec<(usize, u32, usize)> {
        let mut out = Vec::new();
        for (off, _) in self.text.match_indices(needle) {
            let (owner, line) = self.locate(off);
            if out
                .last()
                .is_some_and(|l: &(usize, u32, usize)| l.0 == owner && l.1 == line)
            {
                continue;
            }
            out.push((owner, line, off));
            if out.len() >= limit {
                break;
            }
        }
        out
    }
}

impl Default for Haystack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_unify() {
        let c = canonical("cannot open '%s': %d%% done", Lang::C);
        let r = canonical("cannot open '{}': {count}% done", Lang::Rust);
        assert_eq!(c, r);
        assert_eq!(canonical("{{x}}", Lang::Rust), "{x}");
        assert_eq!(visible_len(&c), "cannot open '': % done".len());
    }

    #[test]
    fn data_literals_are_not_compared() {
        assert!(!comparable("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391", 32));
        assert!(!comparable(&"\\x12".repeat(20), 32));
        assert!(comparable(
            "this sentence is long enough to be compared",
            32
        ));
        assert!(!comparable("short", 32));
    }

    #[test]
    fn c_concatenation() {
        let toks = crate::lex::lex("x = \"ab\" \"cd\"; y = \"ef\";", Lang::C).toks;
        let l = literals(&toks, Lang::C);
        assert_eq!(l, [("abcd".to_string(), 1), ("ef".to_string(), 1)]);
    }
}
