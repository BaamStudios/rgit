// SPDX-License-Identifier: MIT

//! The built-in diff drivers named in gitattributes(5) ("Defining a
//! custom hunk-header"): function-name patterns and word patterns per
//! language.
//!
//! The patterns were written for rgit from the grammars of the languages
//! (what starts a function, class, section, ...) and checked against the
//! hunk headers the `git` binary shows for the same inputs. They use the
//! syntax of the `regex` crate; the first capture group is the text shown
//! in the hunk header (see [`crate::funcname`]).

use regex::bytes::RegexBuilder;

use crate::funcname::{FuncName, FuncPattern};

/// A built-in driver.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    /// The driver name (`diff=<name>`).
    pub name: &'static str,
    /// Function-name patterns, one per entry; `!` in front rejects lines.
    pub funcname: &'static [&'static str],
    /// Whether the patterns ignore case.
    pub ignore_case: bool,
    /// The word pattern for `--word-diff` (without the implicit
    /// "any non-space character" fallback).
    pub word_regex: &'static str,
}

/// Identifier-like runs, numbers with their suffixes and operators made of
/// several characters, shared by C-like languages.
const C_WORDS: &str = r"[A-Za-z_][A-Za-z0-9_]*|[-+]?[0-9]+(?:\.[0-9]*)?(?:[eE][-+]?[0-9]+)?[A-Za-z]*|0[xXbB][0-9a-fA-F_]+[A-Za-z]*|[-+*/%&|^!=<>]=|--|\+\+|&&|\|\||<<=?|>>=?|->|::";

/// The built-in drivers, sorted by name.
pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "ada",
        funcname: &[
            r"!^(?:.*[ \t])?(?:is[ \t]+new|renames)(?:[ \t].*)?$",
            r"!^[ \t]*with[ \t].*$",
            r"^[ \t]*((?:procedure|function|package|task|protected)[ \t].*)$",
        ],
        ignore_case: true,
        word_regex: r"[A-Za-z][A-Za-z0-9_]*|[0-9][-+0-9#_.eE]*|=>|\.\.|\*\*|:=|/=|>=|<=|<<|>>|<>",
    },
    Builtin {
        name: "bash",
        funcname: &[
            r"^[ \t]*((?:function[ \t]+[A-Za-z_][A-Za-z0-9_]*(?:[ \t]*\([ \t]*\)|[ \t]+)|[A-Za-z_][A-Za-z0-9_]*[ \t]*\([ \t]*\)).*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|\$[A-Za-z_][A-Za-z0-9_]*|\$\{[^}]*\}|[0-9]+|&&|\|\||<<|>>|[-+*/%=!<>]=",
    },
    Builtin {
        name: "bibtex",
        funcname: &[r"^(@[A-Za-z]+[ \t]*[{(][ \t]*[^ \t,]*)"],
        ignore_case: false,
        word_regex: r"[={}\x22]|[^={}\x22 \t]+",
    },
    Builtin {
        name: "cpp",
        funcname: &[
            r"!^[ \t]*[A-Za-z_][A-Za-z0-9_]*[ \t]*:[ \t]*(?:$|//|/\*)",
            r"^((?:::[ \t]*)?[A-Za-z_].*)$",
        ],
        ignore_case: false,
        word_regex: C_WORDS,
    },
    Builtin {
        name: "csharp",
        funcname: &[
            r"^[ \t]*((?:(?:abstract|async|extern|internal|new|override|partial|private|protected|public|readonly|sealed|static|unsafe|virtual|volatile|file|required)[ \t]+)*(?:class|struct|record|interface|enum|namespace)[ \t]+.*)$",
            r"!^[ \t]*(?:do|while|for|foreach|if|else|switch|case|default|return|throw|try|catch|finally|using|lock|fixed|checked|unchecked|yield|await|goto|break|continue|new|var|base|this|get|set|init|add|remove|sizeof|typeof|nameof|delegate)\b",
            r"!^[^(]*=",
            r"!;[ \t]*(?://.*)?$",
            r"^[ \t]*((?:[@A-Za-z_][@A-Za-z0-9_.]*(?:[ \t]*<[^()]*>)?(?:\[[ \t,]*\])*\??[ \t]+)+[@A-Za-z_][@A-Za-z0-9_.]*(?:[ \t]*<[^()]*>)?[ \t]*\(.*)$",
            r"^[ \t]*((?:(?:abstract|async|extern|internal|new|override|partial|private|protected|public|readonly|sealed|static|unsafe|virtual|required)[ \t]+)+[@A-Za-z_][@A-Za-z0-9_.<>,\[\]? \t]*[ \t]+[@A-Za-z_][@A-Za-z0-9_]*(?:[ \t]*\{)?)[ \t]*$",
        ],
        ignore_case: false,
        word_regex: C_WORDS,
    },
    Builtin {
        name: "css",
        funcname: &[r"![:;][ \t]*$", r"^[:\[@.#]?[A-Za-z0-9_].*$"],
        ignore_case: true,
        word_regex: r"-?[_A-Za-z][-_A-Za-z0-9]*|-?[0-9.]+[A-Za-z%]*|#[0-9A-Fa-f]+|[^ \t]",
    },
    Builtin {
        name: "dts",
        funcname: &[
            r"!;",
            r"!=",
            r"^[ \t]*((?:/[ \t]*\{|&[A-Za-z_][A-Za-z0-9_,.+-]*[ \t]*\{|(?:[A-Za-z_][A-Za-z0-9_]*:[ \t]*)*[A-Za-z_][A-Za-z0-9_,.+-]*(?:@[0-9A-Za-z,._+-]+)?(?:[ \t]*/\*.*\*/)?[ \t]*\{).*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_,.+-]*|&[A-Za-z_][A-Za-z0-9_]*|0[xX][0-9a-fA-F]+|[0-9]+|<<|>>|&&|\|\||[=!<>]=",
    },
    Builtin {
        name: "elixir",
        funcname: &[
            r"^[ \t]*((?:(?:def|defp|defmacro|defmacrop|defguard|defguardp|defdelegate|defn|defnp)[ \t]+[^ \t].*|(?:defmodule|defprotocol|defimpl)[ \t]+.*|(?:test|describe)[ \t]+\x22.*)[ \t]+do\b.*)$",
        ],
        ignore_case: false,
        word_regex: r"[@:]?[A-Za-z_][A-Za-z0-9_]*[?!]?|[0-9][0-9_.]*|\|>|<>|<-|->|=>|==|!=|<=|>=|&&|\|\||::|\.\.",
    },
    Builtin {
        name: "fortran",
        funcname: &[
            r"!^(?:[cC*]|[ \t]*!)",
            r"!^[ \t]*module[ \t]+procedure\b",
            r"^[ \t]*((?:(?:pure|elemental|recursive|impure|module|non_recursive)[ \t]+)*(?:end[ \t]+)?(?:program|module|subroutine|block[ \t]+data|(?:[a-z0-9_*()=, \t]*[ \t])?function)[ \t]+.*)$",
        ],
        ignore_case: true,
        word_regex: r"[A-Za-z][A-Za-z0-9_]*|\.[A-Za-z]+\.|[0-9.]+(?:[eEdD][-+]?[0-9]+)?(?:_[A-Za-z0-9]+)?|//|\*\*|::|[/=<>]=|=>",
    },
    Builtin {
        name: "fountain",
        funcname: &[r"^((?:\.[^.]|(?:int|ext|est|int\.?/ext|i/e)[. ]).*)$"],
        ignore_case: true,
        word_regex: r"[^\[\] \t]+|\[\[|\]\]",
    },
    Builtin {
        name: "golang",
        funcname: &[
            r"^[ \t]*(func.*)$",
            r"^[ \t]*(type[ \t].*(?:struct|interface)[ \t]*(?:\{[ \t]*)?)",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+i?|0[xX]?[0-9a-fA-F]+i?|[-+*/%&|^=!<>]=|<-|--|\+\+|&&|\|\||<<=?|>>=?|&\^=?|:=|\.\.\.",
    },
    Builtin {
        name: "html",
        funcname: &[r"^[ \t]*(<[Hh][1-6](?:[ \t].*)?>.*)$"],
        ignore_case: false,
        word_regex: r"[^<>= \t]+",
    },
    Builtin {
        name: "ini",
        funcname: &[r"^([ \t]*\[[^\]]+\])"],
        ignore_case: false,
        word_regex: r"[^\[\]= \t]+|[\[\]=]",
    },
    Builtin {
        name: "java",
        funcname: &[
            r"!^[ \t]*(?:catch|do|for|if|instanceof|new|return|switch|throw|while)\b",
            r"^[ \t]*((?:(?:public|protected|private|static|abstract|final|sealed|non-sealed|strictfp|default)[ \t]+)*(?:class|enum|interface|record|@interface)[ \t]+.*)$",
            r"^[ \t]*((?:[A-Za-z_$<&,?][-A-Za-z0-9_$<>&,.?\[\]]*[ \t]+)+[A-Za-z_$][A-Za-z0-9_$]*[ \t]*\([^;]*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_$][A-Za-z0-9_$]*|[-+0-9.eE]+[fFlLdD]?|0[xXbB]?[0-9a-fA-F_]+[lL]?|[-+*/%&|^!=<>]=|--|\+\+|&&|\|\||<<=?|>>>?=?|->|::",
    },
    Builtin {
        name: "kotlin",
        funcname: &[
            r"^[ \t]*((?:(?:public|private|protected|internal|open|abstract|final|sealed|data|enum|value|inline|annotation|inner|override|suspend|tailrec|operator|infix|external|expect|actual)[ \t]+)*(?:class|interface|fun)\b.*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+[fFL]?|0[xXbB][0-9a-fA-F_]+L?|[-+*/%!=<>]=|--|\+\+|&&|\|\||->|::|\.\.|\?:|\?\.|!!",
    },
    Builtin {
        name: "markdown",
        funcname: &[r"^( {0,3}#{1,6}[ \t].*)$"],
        ignore_case: false,
        word_regex: r"[^<>= \t]+",
    },
    Builtin {
        name: "matlab",
        funcname: &[
            r"^[ \t]*((?:classdef|function)[ \t].*)$",
            r"^[ \t]*((?:%%|##).*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+|[=~<>]=|\.[*/\\^']|\|\||&&",
    },
    Builtin {
        name: "objc",
        funcname: &[
            r"!^[ \t]*(?:do|for|if|else|return|switch|while)\b",
            r"^[ \t]*([-+][ \t]*\(.*)$",
            r"^[ \t]*(@(?:implementation|interface|protocol)[ \t].*)$",
            r"^((?:[A-Za-z_][A-Za-z0-9_]*[ \t*]+)+[A-Za-z_][A-Za-z0-9_]*[ \t]*\(.*)$",
        ],
        ignore_case: false,
        word_regex: C_WORDS,
    },
    Builtin {
        name: "pascal",
        funcname: &[
            r"^[ \t]*(?:(?:class[ \t]+)?(?:procedure|function|constructor|destructor)|interface|implementation|initialization|finalization)[ \t]*.*$",
            r"^(.*=[ \t]*(?:class|record).*)$",
        ],
        ignore_case: true,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+|0[xX]?[0-9a-fA-F]+|<>|<=|>=|:=|\.\.",
    },
    Builtin {
        name: "perl",
        funcname: &[
            r"^package .*$",
            r"^sub[ \t]+[A-Za-z_][A-Za-z0-9_:']*[ \t]*(\([^)]*\))?[ \t]*(?::[^;#]*)?(?:\{[ \t]*)?(?:#.*)?$",
            r"^(BEGIN|END|INIT|CHECK|UNITCHECK|AUTOLOAD|DESTROY)[ \t]*(?:\{[ \t]*)?(?:#.*)?$",
            r"^=head[0-9].*$",
        ],
        ignore_case: false,
        word_regex: r"[$@%&*]?[A-Za-z_][A-Za-z0-9_:]*|[-+0-9.eE]+|0[xXbB]?[0-9a-fA-F_]+|=>|->|::|\.\.\.?|<=>|[-+*/%.&|^!=<>~]=|\*\*|&&|\|\||//|<<|>>|=~|!~",
    },
    Builtin {
        name: "php",
        funcname: &[
            r"^[ \t]*((?:(?:public|protected|private|static|abstract|final)[ \t]+)*function[ \t].*)$",
            r"^[ \t]*((?:(?:abstract|final)[ \t]+)*(?:class|enum|interface|trait)[ \t].*)$",
        ],
        ignore_case: false,
        word_regex: r"\$?[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+|0[xXbB]?[0-9a-fA-F_]+|[-+*/%.&|^!=<>]=|===|!==|--|\+\+|&&|\|\||<<|>>|->|=>|::|\?\?",
    },
    Builtin {
        name: "python",
        funcname: &[r"^[ \t]*((?:class|(?:async[ \t]+)?def)[ \t].*)$"],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+[jJlL]?|0[xXoObB]?[0-9a-fA-F_]+[lL]?|[-+*/%&|^=!<>]=|//=?|\*\*=?|<<=?|>>=?|->|:=",
    },
    Builtin {
        name: "r",
        funcname: &[r"^[ \t]*([A-Za-z_.][A-Za-z0-9_.]*[ \t]*(?:<-|=)[ \t]*function.*)$"],
        ignore_case: false,
        word_regex: r"[A-Za-z_.][A-Za-z0-9_.]*|[-+0-9.eE]+[iL]?|<<-|->>|<-|->|[=!<>]=|&&|\|\||%[^% ]*%",
    },
    Builtin {
        name: "ruby",
        funcname: &[r"^[ \t]*((?:class|module|def)[ \t].*)$"],
        ignore_case: false,
        word_regex: r"[@$]{0,2}[A-Za-z_][A-Za-z0-9_]*[?!]?|:[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+|0[xXbB]?[0-9a-fA-F_]+|[-+*/%&|^!=<>]=|===|<=>|=~|!~|\*\*|&&|\|\||<<|>>|::|\.\.\.?",
    },
    Builtin {
        name: "rust",
        funcname: &[
            r"!;",
            r"^[ \t]*((?:pub(?:[ \t]*\([^)]+\))?[ \t]+)?(?:(?:async|const|unsafe|extern(?:[ \t]+\x22[^\x22]+\x22)?|default)[ \t]+)*(?:fn|struct|enum|union|mod|trait|impl|macro_rules!)[< \t].*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[0-9][0-9_a-fA-Fiosuxz]*(?:\.[0-9_]+)?(?:[eE][-+]?[0-9_]+)?|[-+*/%&|^!=<>]=|<<=?|>>=?|&&|\|\||->|=>|::|\.\.=?|'[A-Za-z_][A-Za-z0-9_]*",
    },
    Builtin {
        name: "scheme",
        funcname: &[
            r"^(\(.*)$",
            r"^ {1,2}(\(def.*)$",
            r"^[ \t]*(\((?:def|define|defmethod|defclass|defstruct|defsyntax|defrecord)[-/*\t ].*)$",
            r"^[ \t]*(\((?:module|library|class|struct)\*?[ \t].*)$",
        ],
        ignore_case: false,
        word_regex: r"[^()\[\]{}'`,; \t]+|[()\[\]{}'`,]|,@",
    },
    Builtin {
        name: "swift",
        funcname: &[
            r"^[ \t]*((?:@[A-Za-z_][A-Za-z0-9_]*(?:\([^)]*\))?[ \t]+)*(?:(?:public|private|fileprivate|internal|open|static|class|final|override|mutating|nonmutating|convenience|required|dynamic|lazy|weak|unowned|indirect|nonisolated|async)[ \t]+)*(?:func|init[?!]?|deinit|subscript|class|struct|enum|protocol|extension|actor)\b.*)$",
        ],
        ignore_case: false,
        word_regex: r"[A-Za-z_][A-Za-z0-9_]*|[-+0-9.eE]+|0[xXbBoO][0-9a-fA-F_]+|[-+*/%&|^!=<>]=|===|!==|->|\.\.\.|\.\.<|&&|\|\||\?\?",
    },
    Builtin {
        name: "tex",
        funcname: &[r"^(\\(?:(?:sub)*section|chapter|part)\*?\{.*)$"],
        ignore_case: false,
        word_regex: r"\\[A-Za-z@]+|\\.|[A-Za-z0-9\x80-\xff]+",
    },
];

/// The built-in driver called `name`.
pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS
        .binary_search_by(|b| b.name.cmp(name))
        .ok()
        .map(|i| &BUILTINS[i])
}

impl Builtin {
    /// The compiled function-name patterns.
    pub fn funcname(&self) -> FuncName {
        let patterns = self
            .funcname
            .iter()
            .map(|p| {
                let (negate, p) = match p.strip_prefix('!') {
                    Some(rest) => (true, rest),
                    None => (false, *p),
                };
                let regex = RegexBuilder::new(p)
                    .unicode(false)
                    .case_insensitive(self.ignore_case)
                    .build()
                    .expect("built-in pattern compiles");
                FuncPattern { negate, regex }
            })
            .collect();
        FuncName::new(patterns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_and_compiling() {
        for w in BUILTINS.windows(2) {
            assert!(w[0].name < w[1].name, "{} < {}", w[0].name, w[1].name);
        }
        for b in BUILTINS {
            let _ = b.funcname();
            regex::bytes::RegexBuilder::new(b.word_regex)
                .unicode(false)
                .build()
                .unwrap();
        }
    }
}
