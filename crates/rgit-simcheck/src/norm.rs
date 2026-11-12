// SPDX-License-Identifier: MIT
//! Token normalisation into a language-neutral alphabet.
//!
//! Identifiers (including type names) become `I`, numbers and character
//! literals `N`, string literals `S`. Control-flow keywords are kept (with the
//! Rust/C spellings unified), declaration noise (`let`, `mut`, `const`,
//! `struct`, `::`, `:` ...) is dropped, and runs of `I` collapse into one, so
//! that `unsigned int x` and `x: u32` both become `I`. The goal is that C code
//! transliterated to Rust normalises to (nearly) the same sequence.

use std::collections::HashMap;

use crate::lex::{Kind, Lang, Tok};

#[derive(Default)]
pub struct Interner {
    map: HashMap<String, u32>,
}

impl Interner {
    pub fn id(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.map.len() as u32;
        self.map.insert(s.to_string(), id);
        id
    }
}

/// A normalised token sequence with the source line of every token.
#[derive(Default, Clone, Debug)]
pub struct Seq {
    pub ids: Vec<u32>,
    pub lines: Vec<u32>,
}

enum Map {
    Keep(&'static str),
    Drop,
    Ident,
}

fn c_keyword(w: &str) -> Map {
    match w {
        "if" => Map::Keep("if"),
        "else" => Map::Keep("else"),
        "while" => Map::Keep("while"),
        "for" => Map::Keep("for"),
        "do" => Map::Keep("do"),
        "return" => Map::Keep("return"),
        "break" => Map::Keep("break"),
        "continue" => Map::Keep("continue"),
        "switch" => Map::Keep("match"),
        "case" => Map::Keep("case"),
        "goto" => Map::Keep("goto"),
        "struct" | "union" | "enum" | "const" | "static" | "inline" | "extern" | "volatile"
        | "register" | "unsigned" | "signed" | "restrict" | "auto" => Map::Drop,
        _ => Map::Ident,
    }
}

fn rust_keyword(w: &str) -> Map {
    match w {
        "if" => Map::Keep("if"),
        "else" => Map::Keep("else"),
        "while" | "loop" => Map::Keep("while"),
        "for" => Map::Keep("for"),
        "return" => Map::Keep("return"),
        "break" => Map::Keep("break"),
        "continue" => Map::Keep("continue"),
        "match" => Map::Keep("match"),
        "let" | "mut" | "pub" | "fn" | "unsafe" | "ref" | "move" | "dyn" | "as" | "const"
        | "static" | "struct" | "enum" | "union" | "where" | "async" | "await" | "extern" => {
            Map::Drop
        }
        _ => Map::Ident,
    }
}

pub fn normalise(toks: &[Tok], lang: Lang, it: &mut Interner) -> Seq {
    let mut out: Vec<(&'static str, u32)> = Vec::with_capacity(toks.len());
    let mut parens: Vec<bool> = Vec::new();
    let mut skip_bang = false;
    let push = |out: &mut Vec<(&'static str, u32)>, s: &'static str, line: u32| {
        let collapse = matches!(
            (s, out.last()),
            ("I", Some(("I", _))) | ("S", Some(("S", _)))
        );
        if !collapse {
            out.push((s, line));
        }
    };
    let mut idx = 0;
    while idx < toks.len() {
        let t = &toks[idx];
        idx += 1;
        let line = t.line;
        match t.kind {
            Kind::Ident => {
                let m = match lang {
                    Lang::C => c_keyword(&t.text),
                    Lang::Rust => rust_keyword(&t.text),
                };
                match m {
                    Map::Keep(k) => push(&mut out, k, line),
                    Map::Drop => {}
                    Map::Ident => {
                        push(&mut out, "I", line);
                        if lang == Lang::Rust
                            && toks.get(idx).is_some_and(|n| n.text == "!")
                            && toks
                                .get(idx + 1)
                                .is_some_and(|n| matches!(n.text.as_str(), "(" | "[" | "{"))
                        {
                            skip_bang = true;
                        }
                    }
                }
            }
            Kind::Number | Kind::Char => push(&mut out, "N", line),
            Kind::Str => push(&mut out, "S", line),
            Kind::Punct => {
                let p = t.text.as_str();
                let cast = match (lang, p) {
                    (Lang::C, "(") => c_cast(
                        toks,
                        idx - 1,
                        matches!(out.last(), Some(("I" | "N" | "S" | ")" | "]", _))),
                    ),
                    _ => None,
                };
                match (lang, p) {
                    (_, "(") if cast.is_some() => {
                        // Casts have no counterpart in Rust (`as` is dropped too).
                        idx = cast.unwrap_or(idx);
                    }
                    (_, "(") => {
                        let drop = lang == Lang::C
                            && matches!(out.last(), Some(("if" | "while" | "match", _)));
                        parens.push(drop);
                        if !drop {
                            push(&mut out, "(", line);
                        }
                    }
                    (_, ")") => {
                        if !parens.pop().unwrap_or(false) {
                            push(&mut out, ")", line);
                        }
                    }
                    (Lang::C, "->") => push(&mut out, ".", line),
                    (Lang::C, "++") => {
                        push(&mut out, "+=", line);
                        push(&mut out, "N", line);
                    }
                    (Lang::C, "--") => {
                        push(&mut out, "-=", line);
                        push(&mut out, "N", line);
                    }
                    (Lang::Rust, "::" | ":" | "->" | "?" | "=>") => {}
                    (Lang::Rust, "!") if skip_bang => skip_bang = false,
                    _ => push(&mut out, intern_punct(p), line),
                }
            }
        }
    }
    let mut seq = Seq::default();
    for (s, line) in out {
        seq.ids.push(it.id(s));
        seq.lines.push(line);
    }
    seq
}

fn intern_punct(p: &str) -> &'static str {
    const ALL: &[&str] = &[
        ">>=", "<<=", "...", "..=", "->", "=>", "::", "==", "!=", "<=", ">=", "&&", "||", "+=",
        "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "++", "--", "..", "{", "}", "[", "]",
        ";", ",", ".", "=", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":",
        "@", "$", "\\", "`",
    ];
    ALL.iter().find(|a| **a == p).copied().unwrap_or("?punct")
}

const C_TYPE_QUALIFIERS: &[&str] = &[
    "struct", "const", "static", "unsigned", "signed", "volatile", "union", "enum",
];

/// A C cast `( Type [*]* )` at `toks[i]` followed by an operand; returns the
/// index after the closing parenthesis. `prev_operand` tells whether the
/// token before `(` ends an operand (then it is a call, not a cast).
pub fn c_cast(toks: &[Tok], i: usize, prev_operand: bool) -> Option<usize> {
    if prev_operand || toks.get(i)?.text != "(" {
        return None;
    }
    if i > 0
        && matches!(
            toks[i - 1].text.as_str(),
            "if" | "while" | "for" | "switch" | "sizeof"
        )
    {
        return None;
    }
    let mut j = i + 1;
    let (mut idents, mut stars) = (0, 0);
    while j < toks.len() && (toks[j].kind == Kind::Ident || toks[j].text == "*") {
        if toks[j].kind == Kind::Ident {
            idents += 1;
        } else {
            stars += 1;
        }
        j += 1;
    }
    if idents == 0 || toks.get(j)?.text != ")" {
        return None;
    }
    let next = toks.get(j + 1)?;
    let typeish =
        stars > 0 || idents >= 2 || C_TYPE_QUALIFIERS.contains(&toks[i + 1].text.as_str());
    let operand = match next.kind {
        Kind::Ident | Kind::Number | Kind::Str | Kind::Char => true,
        Kind::Punct => typeish && matches!(next.text.as_str(), "(" | "&" | "*" | "!" | "-"),
    };
    (operand && (typeish || next.kind != Kind::Punct)).then_some(j + 1)
}

/// Number of distinct token trigrams; a cheap measure of how "specific" a
/// region is. Repetitive regions (tables, `S , S , S`) score low.
pub fn distinct_trigrams(ids: &[u32]) -> usize {
    let mut set = std::collections::HashSet::new();
    for w in ids.windows(3) {
        set.insert((w[0], w[1], w[2]));
    }
    set.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::lex;

    fn norm(src: &str, lang: Lang, it: &mut Interner) -> Vec<u32> {
        normalise(&lex(src, lang).toks, lang, it).ids
    }

    #[test]
    fn c_and_rust_agree() {
        let mut it = Interner::default();
        let c = norm(
            "static int count_widgets(struct widget *w) { unsigned int n = 0; while (w) { if (w->size > 4) n++; w = w->next; } return n; }",
            Lang::C,
            &mut it,
        );
        let r = norm(
            "fn count_widgets(w: *mut Widget) { let mut n: u32 = 0; while w { if w.size > 4 n += 1; w = w.next; } return n; }",
            Lang::Rust,
            &mut it,
        );
        // Signature differs (`(I * I)` vs `(I * I)` modulo `mut`), body must agree.
        assert_eq!(c, r);
    }
}
