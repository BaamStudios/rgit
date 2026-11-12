// SPDX-License-Identifier: MIT
//! Allowlist for accepted findings.
//!
//! File format, one entry per line, `#` starts a comment:
//!
//! ```text
//! <kind> <path>[:<first>[-<last>]]   reason...
//! ```
//!
//! `kind` is `file` (everything in the file), `code`, `comment` or `string`.
//! `path` is the rgit path as printed in the report. A line range limits the
//! entry to findings whose rgit lines lie inside it.

use std::fmt;

use crate::lex::{Kind, Tok};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    Code,
    Comment,
    String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Check::Code => "code",
            Check::Comment => "comment",
            Check::String => "string",
        })
    }
}

#[derive(Debug)]
struct Entry {
    kind: Option<Check>,
    path: String,
    range: Option<(u32, u32)>,
}

#[derive(Default, Debug)]
pub struct Allowlist {
    entries: Vec<Entry>,
}

/// Why a finding does not count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    Allowlist,
    Gitoxide,
    Message,
    TestExpectation,
    InterfaceName,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Reason::Allowlist => "allowlist",
            Reason::Gitoxide => "gitoxide provenance header",
            Reason::Message => "user-visible message",
            Reason::TestExpectation => "expected output in a test",
            Reason::InterfaceName => "interface name",
        })
    }
}

/// Files below a `tests/` directory (integration and differential tests).
pub fn is_test_path(path: &str) -> bool {
    path.split('/').any(|c| c == "tests")
}

/// Line ranges of inline test modules (`mod tests { ... }`, or any module
/// whose name starts with `test`).
pub fn test_module_ranges(toks: &[Tok]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < toks.len() {
        let is_mod = toks[i].kind == Kind::Ident
            && toks[i].text == "mod"
            && toks[i + 1].kind == Kind::Ident
            && toks[i + 1].text.starts_with("test")
            && toks[i + 2].text == "{";
        if !is_mod {
            i += 1;
            continue;
        }
        let start = toks[i].line;
        let mut depth = 0usize;
        let mut j = i + 2;
        while j < toks.len() {
            match toks[j].text.as_str() {
                "{" if toks[j].kind == Kind::Punct => depth += 1,
                "}" if toks[j].kind == Kind::Punct => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        let end = toks.get(j).or(toks.last()).map_or(start, |t| t.line);
        out.push((start, end));
        i = j + 1;
    }
    out
}

/// A literal without whitespace, such as an environment variable, config
/// key, ref name or path: an interface name that has to be spelled exactly
/// like git's, not expression.
pub fn is_interface_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@+".contains(c))
}

impl Allowlist {
    pub fn parse(text: &str) -> Result<Allowlist, String> {
        let mut entries = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let kind = match parts.next() {
                Some("file") => None,
                Some("code") => Some(Check::Code),
                Some("comment") => Some(Check::Comment),
                Some("string") => Some(Check::String),
                other => return Err(format!("line {}: unknown kind {:?}", n + 1, other)),
            };
            let target = parts
                .next()
                .ok_or_else(|| format!("line {}: missing path", n + 1))?;
            let (path, range) = match target.rsplit_once(':') {
                Some((p, r)) => {
                    let (a, b) = r.split_once('-').unwrap_or((r, r));
                    let a: u32 = a
                        .parse()
                        .map_err(|_| format!("line {}: bad range", n + 1))?;
                    let b: u32 = b
                        .parse()
                        .map_err(|_| format!("line {}: bad range", n + 1))?;
                    (p.to_string(), Some((a, b)))
                }
                None => (target.to_string(), None),
            };
            entries.push(Entry { kind, path, range });
        }
        Ok(Allowlist { entries })
    }

    pub fn allows(&self, check: Check, path: &str, lines: (u32, u32)) -> bool {
        self.entries.iter().any(|e| {
            e.kind.is_none_or(|k| k == check)
                && e.path == path
                && e.range.is_none_or(|(a, b)| a <= lines.0 && lines.1 <= b)
        })
    }
}

/// A file ported from gitoxide names its source in a header comment
/// (see CLEANROOM.md); such files are MIT/Apache-2.0 derived and exempt.
pub fn has_gitoxide_header(src: &str) -> bool {
    src.lines()
        .take(30)
        .filter(|l| l.trim_start().starts_with("//"))
        .any(|l| l.to_lowercase().contains("gitoxide"))
}

/// Files that collect user-visible messages (CLEANROOM.md: "collect them in
/// one place per crate"): `messages.rs` or anything below a `messages/` dir.
pub fn is_message_file(path: &str) -> bool {
    path.split('/')
        .any(|c| c == "messages" || c == "messages.rs")
}

/// Literal looks like a user-visible message on its own.
pub fn is_message_literal(s: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "fatal: ",
        "error: ",
        "warning: ",
        "hint: ",
        "usage: ",
        "   or: ",
    ];
    PREFIXES.iter().any(|p| s.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_match() {
        let a = Allowlist::parse(
            "# comment\nfile crates/a/src/x.rs  # whole\nstring crates/b/src/y.rs:10-20 reason\n",
        )
        .unwrap();
        assert!(a.allows(Check::Code, "crates/a/src/x.rs", (1, 2)));
        assert!(a.allows(Check::String, "crates/b/src/y.rs", (12, 12)));
        assert!(!a.allows(Check::String, "crates/b/src/y.rs", (21, 21)));
        assert!(!a.allows(Check::Code, "crates/b/src/y.rs", (12, 12)));
        assert!(Allowlist::parse("nope x").is_err());
    }

    #[test]
    fn message_files() {
        assert!(is_message_file("crates/rgit-cli/src/messages.rs"));
        assert!(is_message_file("crates/x/src/messages/refs.rs"));
        assert!(!is_message_file("crates/x/src/mess.rs"));
        assert!(is_test_path("crates/x/tests/cli.rs"));
        assert!(!is_test_path("crates/x/src/testsuite.rs"));
    }

    #[test]
    fn test_modules_and_interface_names() {
        let src = "fn a() { \"x\" }\n#[cfg(test)]\nmod tests {\n fn t() { \"y\"; }\n}\nfn b() {}\n";
        let toks = crate::lex::lex(src, crate::lex::Lang::Rust).toks;
        assert_eq!(test_module_ranges(&toks), [(3, 5)]);
        assert!(is_interface_name("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS"));
        assert!(is_interface_name("refs/remotes/origin/HEAD"));
        assert!(!is_interface_name("could not open file"));
    }
}
