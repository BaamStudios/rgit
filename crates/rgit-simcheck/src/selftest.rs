// SPDX-License-Identifier: MIT
//! Self-test: plant mechanically transliterated upstream fragments and check
//! that the scanner reports them.
//!
//! A fragment of `len` raw C tokens is cut from a random upstream `.c` file
//! and rewritten into Rust syntax token by token (declarations become
//! `let mut name: Type`, `->` becomes `.`, `NULL` becomes `None`, condition
//! parentheses and casts are dropped, `++` becomes `+= 1`, identifiers are
//! renamed, literal values are replaced). The rewritten text only lives in
//! memory and is never printed, so running the self-test does not expose
//! upstream code to anyone.

use std::collections::HashMap;

use crate::lex::{Kind, Tok};
use crate::norm::c_cast;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const SKIP_IDENTS: &[&str] = &[
    "for", "switch", "case", "default", "goto", "do", "sizeof", "typedef", "union", "enum",
];

const DROP_IDENTS: &[&str] = &[
    "struct", "const", "static", "unsigned", "signed", "inline", "extern", "volatile", "register",
];

/// Index of the first token after a statement boundary at or after `from`.
pub fn statement_start(toks: &[Tok], from: usize) -> Option<usize> {
    (from..toks.len()).find(|&i| i > 0 && matches!(toks[i - 1].text.as_str(), ";" | "{" | "}"))
}

/// Whether a window of raw C tokens is suitable for planting: plain
/// statements with balanced parentheses, no constructs that need a
/// structural rewrite.
pub fn suitable(win: &[Tok]) -> bool {
    let has_body = win.iter().any(|t| t.text == ";");
    let mut depth = 0i32;
    for t in win {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    has_body
        && win.iter().all(|t| match t.kind {
            Kind::Ident => !SKIP_IDENTS.contains(&t.text.as_str()),
            Kind::Punct => !matches!(t.text.as_str(), "?" | ":" | "#" | "\\"),
            _ => true,
        })
}

struct Writer {
    out: String,
    names: HashMap<String, String>,
}

impl Writer {
    fn word(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push(' ');
    }

    fn rename(&mut self, s: &str) -> String {
        let n = self.names.len();
        self.names
            .entry(s.to_string())
            .or_insert_with(|| format!("v{n}"))
            .clone()
    }
}

fn is_ident(t: Option<&Tok>) -> bool {
    t.is_some_and(|t| t.kind == Kind::Ident)
}

/// `T [*]* name` followed by `=`, `;`, `,` or `[` at the start of a statement.
/// Returns (index of the name, index after the name).
fn declaration(win: &[Tok], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    let mut idents = 0;
    while j < win.len() && (win[j].kind == Kind::Ident || win[j].text == "*") {
        if win[j].kind == Kind::Ident {
            idents += 1;
        }
        j += 1;
    }
    let end = win.get(j)?;
    const KEYWORDS: &[&str] = &["return", "else", "break", "continue", "if", "while", "goto"];
    if KEYWORDS.contains(&win[i].text.as_str()) {
        return None;
    }
    let ok = idents >= 2
        && j > i
        && win[j - 1].kind == Kind::Ident
        && matches!(end.text.as_str(), "=" | ";" | "," | "[");
    ok.then_some((j - 1, j))
}

/// Rewrite raw C tokens into Rust-looking source text.
pub fn transliterate(win: &[Tok], rng: &mut Rng) -> String {
    let mut w = Writer {
        out: String::from("fn planted() {\n"),
        names: HashMap::new(),
    };
    let mut parens: Vec<bool> = Vec::new();
    let mut stmt_start = true;
    let mut last_kw = false; // last emitted token was `if` / `while`
    let mut prev_operand = false;
    let mut i = 0;
    while i < win.len() {
        let t = &win[i];
        let was_start = stmt_start;
        stmt_start = false;
        let mut kw = false;
        match t.kind {
            Kind::Ident => {
                let decl = if was_start { declaration(win, i) } else { None };
                if let Some((name, after)) = decl {
                    let stars = win[i..name].iter().filter(|t| t.text == "*").count();
                    let ty: Vec<String> = win[i..name]
                        .iter()
                        .filter(|t| {
                            t.kind == Kind::Ident && !DROP_IDENTS.contains(&t.text.as_str())
                        })
                        .map(|t| t.text.clone())
                        .collect();
                    let n = w.rename(&win[name].text);
                    w.word(&format!("let mut {n}:"));
                    for _ in 0..stars {
                        w.word("*mut");
                    }
                    let ty = ty.last().cloned().unwrap_or_else(|| "T".into());
                    let ty = w.rename(&ty);
                    w.word(&ty);
                    i = after;
                    prev_operand = false;
                    continue;
                }
                let s = t.text.as_str();
                match s {
                    "NULL" => w.word("None"),
                    "if" | "while" => {
                        w.word(s);
                        kw = true;
                    }
                    "return" | "break" | "continue" | "else" => w.word(s),
                    _ if DROP_IDENTS.contains(&s) => {}
                    _ => {
                        let n = w.rename(s);
                        w.word(&n);
                    }
                }
                prev_operand = !kw && !matches!(s, "return" | "else");
            }
            Kind::Number => {
                let v = rng.below(1000);
                w.word(&v.to_string());
                prev_operand = true;
            }
            Kind::Char => {
                w.word("b'x'");
                prev_operand = true;
            }
            Kind::Str => {
                if i == 0 || win[i - 1].kind != Kind::Str {
                    w.word("\"planted\"");
                }
                prev_operand = true;
            }
            Kind::Punct => {
                let p = t.text.as_str();
                match p {
                    "(" => {
                        if let Some(after) = c_cast(win, i, prev_operand) {
                            i = after;
                            prev_operand = false;
                            continue;
                        }
                        let drop = last_kw;
                        parens.push(drop);
                        if !drop {
                            w.word("(");
                        }
                        prev_operand = false;
                    }
                    ")" => {
                        if !parens.pop().unwrap_or(false) {
                            w.word(")");
                        }
                        prev_operand = true;
                    }
                    "->" => {
                        w.word(".");
                        prev_operand = false;
                    }
                    "++" | "--" => {
                        let op = if p == "++" { "+= 1" } else { "-= 1" };
                        if !prev_operand && is_ident(win.get(i + 1)) {
                            // Prefix form: `++x` becomes `x += 1`.
                            let n = w.rename(&win[i + 1].text);
                            w.word(&n);
                            i += 1;
                        }
                        w.word(op);
                        prev_operand = true;
                    }
                    ";" | "{" | "}" => {
                        w.word(p);
                        w.out.push('\n');
                        stmt_start = true;
                        prev_operand = false;
                    }
                    _ => {
                        w.word(p);
                        prev_operand = false;
                    }
                }
            }
        }
        last_kw = kw;
        i += 1;
    }
    w.out.push_str("\n}\n");
    w.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{Lang, lex};

    #[test]
    fn rewrites_c_into_rust_shapes() {
        // Hand-written C, not taken from git.
        let c = "unsigned long total = 0; struct node *n = head; while (n != NULL) { total += (unsigned long)n->weight; n++; }";
        let toks = lex(c, Lang::C).toks;
        assert!(suitable(&toks));
        let r = transliterate(&toks, &mut Rng::new(1));
        assert!(r.contains("let mut v0:"), "{r}");
        assert!(r.contains(": *mut"), "{r}");
        assert!(r.contains("None"), "{r}");
        assert!(r.contains("+= 1"), "{r}");
        assert!(!r.contains("->"), "{r}");
        assert!(!r.contains("unsigned"), "{r}");
    }
}
