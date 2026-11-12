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
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Reason::Allowlist => "allowlist",
            Reason::Gitoxide => "gitoxide provenance header",
            Reason::Message => "user-visible message",
        })
    }
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
    }
}
