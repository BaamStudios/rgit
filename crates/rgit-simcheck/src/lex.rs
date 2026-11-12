// SPDX-License-Identifier: MIT
//! A small, forgiving lexer for C and Rust.
//!
//! It only has to be good enough to split source text into identifiers,
//! literals and punctuation, to drop whitespace and preprocessor/attribute
//! lines, and to collect comment words separately. It never fails: unknown
//! bytes are skipped.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    C,
    Rust,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ident,
    Number,
    Str,
    Char,
    Punct,
}

#[derive(Clone, Debug)]
pub struct Tok {
    pub kind: Kind,
    /// Identifier/punctuation text, or for `Str` the literal's content in
    /// escaped form (raw strings are converted to escaped form).
    pub text: String,
    pub line: u32,
}

#[derive(Default, Debug)]
pub struct Lexed {
    pub toks: Vec<Tok>,
    /// Lower-cased words of all comments, with their line.
    pub comment_words: Vec<(String, u32)>,
}

const PUNCTS: &[&str] = &[
    ">>=", "<<=", "...", "..=", "->", "=>", "::", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=",
    "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "++", "--", "..",
];

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn add_words(bytes: &[u8], line: u32, out: &mut Vec<(String, u32)>) {
    let text = String::from_utf8_lossy(bytes);
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        if !w.is_empty() {
            out.push((w.to_lowercase(), line));
        }
    }
}

struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
    line: u32,
    lang: Lang,
    out: Lexed,
}

impl Lexer<'_> {
    fn peek(&self, off: usize) -> u8 {
        self.b.get(self.i + off).copied().unwrap_or(0)
    }

    fn push(&mut self, kind: Kind, text: String, line: u32) {
        self.out.toks.push(Tok { kind, text, line });
    }

    fn line_comment(&mut self) {
        let start = self.i + 2;
        while self.i < self.b.len() && self.b[self.i] != b'\n' {
            self.i += 1;
        }
        add_words(
            &self.b[start..self.i],
            self.line,
            &mut self.out.comment_words,
        );
    }

    fn block_comment(&mut self) {
        self.i += 2;
        let mut depth = 1;
        let mut seg = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'*' && self.peek(1) == b'/' {
                depth -= 1;
                if depth == 0 {
                    add_words(&self.b[seg..self.i], self.line, &mut self.out.comment_words);
                    self.i += 2;
                    return;
                }
                self.i += 2;
                continue;
            }
            if self.lang == Lang::Rust && c == b'/' && self.peek(1) == b'*' {
                depth += 1;
                self.i += 2;
                continue;
            }
            if c == b'\n' {
                add_words(&self.b[seg..self.i], self.line, &mut self.out.comment_words);
                self.line += 1;
                seg = self.i + 1;
            }
            self.i += 1;
        }
        add_words(&self.b[seg..], self.line, &mut self.out.comment_words);
    }

    /// Skip a C preprocessor line including backslash continuations.
    fn preprocessor(&mut self) {
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'\\' && self.peek(1) == b'\n' {
                self.line += 1;
                self.i += 2;
                continue;
            }
            if c == b'\n' {
                return;
            }
            self.i += 1;
        }
    }

    /// Skip a Rust attribute `#[...]` / `#![...]` with balanced brackets.
    fn attribute(&mut self) {
        while self.i < self.b.len() && self.b[self.i] != b'[' {
            self.i += 1;
        }
        let mut depth = 0usize;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        self.i += 1;
                        return;
                    }
                }
                b'"' => {
                    // Strings inside attributes (e.g. doc = "...") may contain brackets.
                    self.string();
                    self.out.toks.pop();
                    continue;
                }
                b'\n' => self.line += 1,
                _ => {}
            }
            self.i += 1;
        }
    }

    /// Lex a normal (escaped) string starting at the opening quote.
    fn string(&mut self) {
        let line = self.line;
        self.i += 1;
        let mut content = String::new();
        let mut buf: Vec<u8> = Vec::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    let n = self.peek(1);
                    if n == b'\n' || (n == b'\r' && self.peek(2) == b'\n') {
                        // Line continuation: in Rust it also eats leading whitespace.
                        self.i += if n == b'\r' { 3 } else { 2 };
                        self.line += 1;
                        if self.lang == Lang::Rust {
                            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                                if self.b[self.i] == b'\n' {
                                    self.line += 1;
                                }
                                self.i += 1;
                            }
                        }
                        continue;
                    }
                    buf.push(c);
                    if n != 0 {
                        buf.push(n);
                    }
                    self.i += 2;
                }
                b'\n' => {
                    buf.extend_from_slice(b"\\n");
                    self.line += 1;
                    self.i += 1;
                }
                _ => {
                    buf.push(c);
                    self.i += 1;
                }
            }
        }
        content.push_str(&String::from_utf8_lossy(&buf));
        self.push(Kind::Str, content, line);
    }

    /// Lex a Rust raw string; `self.i` points at the first `#` or `"`.
    fn raw_string(&mut self) {
        let line = self.line;
        let mut hashes = 0;
        while self.peek(0) == b'#' {
            hashes += 1;
            self.i += 1;
        }
        self.i += 1; // opening quote
        let mut buf = String::new();
        let start = self.i;
        let mut end = self.b.len();
        while self.i < self.b.len() {
            if self.b[self.i] == b'"' && (1..=hashes).all(|k| self.b.get(self.i + k) == Some(&b'#'))
            {
                end = self.i;
                self.i += 1 + hashes;
                break;
            }
            self.i += 1;
        }
        for ch in String::from_utf8_lossy(&self.b[start..end.min(self.b.len())]).chars() {
            match ch {
                '\\' => buf.push_str("\\\\"),
                '"' => buf.push_str("\\\""),
                '\n' => {
                    buf.push_str("\\n");
                    self.line += 1;
                }
                '\t' => buf.push_str("\\t"),
                _ => buf.push(ch),
            }
        }
        self.push(Kind::Str, buf, line);
    }

    fn char_lit(&mut self) {
        let line = self.line;
        let start = self.i;
        self.i += 1;
        while self.i < self.b.len() {
            match self.b[self.i] {
                b'\\' => self.i += 2,
                b'\'' => {
                    self.i += 1;
                    break;
                }
                b'\n' => break,
                _ => self.i += 1,
            }
        }
        let text = String::from_utf8_lossy(&self.b[start..self.i.min(self.b.len())]).into_owned();
        self.push(Kind::Char, text, line);
    }

    fn run(mut self) -> Lexed {
        let mut at_line_start = true;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'\n' {
                self.line += 1;
                self.i += 1;
                at_line_start = true;
                continue;
            }
            if c.is_ascii_whitespace() {
                self.i += 1;
                continue;
            }
            if c == b'/' && self.peek(1) == b'/' {
                self.line_comment();
                continue;
            }
            if c == b'/' && self.peek(1) == b'*' {
                self.block_comment();
                continue;
            }
            let line_start = at_line_start;
            at_line_start = false;
            if c == b'#' {
                match self.lang {
                    Lang::C if line_start => {
                        self.preprocessor();
                        continue;
                    }
                    Lang::Rust
                        if self.peek(1) == b'['
                            || (self.peek(1) == b'!' && self.peek(2) == b'[') =>
                    {
                        self.attribute();
                        continue;
                    }
                    _ => {}
                }
            }
            if self.lang == Lang::Rust {
                // Raw strings and raw identifiers.
                let (skip, is_raw) = if c == b'r' {
                    (1, true)
                } else if (c == b'b' || c == b'c') && self.peek(1) == b'r' {
                    (2, true)
                } else {
                    (0, false)
                };
                if is_raw {
                    let n = self.peek(skip);
                    if n == b'"' || (n == b'#' && matches!(self.peek(skip + 1), b'"' | b'#')) {
                        self.i += skip;
                        self.raw_string();
                        continue;
                    }
                    if c == b'r' && n == b'#' && is_ident_start(self.peek(2)) {
                        self.i += 2; // raw identifier: lex the identifier below
                    }
                }
            }
            let c = self.b[self.i];
            if c == b'"' {
                self.string();
                continue;
            }
            if c == b'\'' {
                if self.lang == Lang::Rust && is_ident_start(self.peek(1)) && self.peek(2) != b'\''
                {
                    // Lifetime or label: drop it.
                    self.i += 1;
                    while self.i < self.b.len() && is_ident_cont(self.b[self.i]) {
                        self.i += 1;
                    }
                    continue;
                }
                self.char_lit();
                continue;
            }
            if c.is_ascii_digit() {
                let line = self.line;
                let start = self.i;
                while self.i < self.b.len() {
                    let d = self.b[self.i];
                    if is_ident_cont(d) || (d == b'.' && self.peek(1).is_ascii_digit()) {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
                self.push(Kind::Number, text, line);
                continue;
            }
            if is_ident_start(c) {
                let line = self.line;
                let start = self.i;
                while self.i < self.b.len() && is_ident_cont(self.b[self.i]) {
                    self.i += 1;
                }
                let word = &self.b[start..self.i];
                let prefix = match self.lang {
                    Lang::C => matches!(word, b"L" | b"u" | b"U" | b"u8"),
                    Lang::Rust => matches!(word, b"b" | b"c"),
                };
                if prefix && self.peek(0) == b'"' {
                    self.string();
                    continue;
                }
                if prefix && self.peek(0) == b'\'' {
                    self.char_lit();
                    continue;
                }
                let text = String::from_utf8_lossy(word).into_owned();
                self.push(Kind::Ident, text, line);
                continue;
            }
            if !c.is_ascii() {
                self.i += 1;
                continue;
            }
            let line = self.line;
            let rest = &self.b[self.i..];
            let p = PUNCTS
                .iter()
                .find(|p| rest.starts_with(p.as_bytes()))
                .map(|p| p.to_string())
                .unwrap_or_else(|| (c as char).to_string());
            self.i += p.len();
            self.push(Kind::Punct, p, line);
        }
        self.out
    }
}

pub fn lex(src: &str, lang: Lang) -> Lexed {
    Lexer {
        b: src.as_bytes(),
        i: 0,
        line: 1,
        lang,
        out: Lexed::default(),
    }
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str, lang: Lang) -> Vec<(Kind, String)> {
        lex(src, lang)
            .toks
            .into_iter()
            .map(|t| (t.kind, t.text))
            .collect()
    }

    #[test]
    fn c_basics() {
        let l = lex(
            "#include <x.h>\n#define A \\\n 1\n/* hello\n world */ int x = 0x1f; // tail\nchar *s = \"a\\\"b\";\n",
            Lang::C,
        );
        let t: Vec<_> = l.toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            t,
            [
                "int", "x", "=", "0x1f", ";", "char", "*", "s", "=", "a\\\"b", ";"
            ]
        );
        assert_eq!(l.toks[0].line, 5);
        assert_eq!(l.toks[5].line, 6);
        let w: Vec<_> = l.comment_words.iter().map(|w| w.0.as_str()).collect();
        assert_eq!(w, ["hello", "world", "tail"]);
    }

    #[test]
    fn rust_basics() {
        let k = kinds(
            "#[derive(Debug)]\nfn f<'a>(x: &'a str) -> u8 { let s = r#\"q\"x\"#; b'c'; 1..2 }",
            Lang::Rust,
        );
        let texts: Vec<_> = k.iter().map(|k| k.1.as_str()).collect();
        assert_eq!(
            texts,
            [
                "fn", "f", "<", ">", "(", "x", ":", "&", "str", ")", "->", "u8", "{", "let", "s",
                "=", "q\\\"x", ";", "'c'", ";", "1", "..", "2", "}"
            ]
        );
        assert_eq!(k[16].0, Kind::Str);
        assert_eq!(k[18].0, Kind::Char);
    }
}
