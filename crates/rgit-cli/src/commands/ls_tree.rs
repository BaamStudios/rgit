// SPDX-License-Identifier: MIT
//! `git ls-tree` (Documentation/git-ls-tree.adoc): list the contents of a
//! tree object.
//!
//! Paths are not pathspecs here but leading-path patterns: an entry is
//! shown when a pattern equals its path or names one of its leading
//! directories; trees that lead to a pattern are entered without being
//! shown (unless `-t`). Only `top` and `literal` magic are accepted, and
//! wildcards match literally.

use std::io::Write;

use rgit_hash::ObjectId;
use rgit_object::{EntryMode, Object, ObjectKind};
use rgit_pathspec::{GlobalFlags, Magic, ParseOptions, Pathspec};

use crate::messages::ls_tree as msg;
use crate::prelude::*;
use crate::quote::{relative_to, write_quoted};
use crate::repo::Mode;
use crate::repository::Repo;
use crate::revision;

/// What `--abbrev` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abbrev {
    /// Full object names (no option, `--no-abbrev`, `--abbrev=0`).
    Full,
    /// `--abbrev` without a value: `core.abbrev` or the automatic length.
    Configured,
    /// `--abbrev=<n>`.
    Digits(usize),
}

/// The format-altering options; only one may be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum Output {
    Default = 0,
    Long = 1,
    NameOnly = 2,
    NameStatus = 3,
    ObjectOnly = 4,
}

impl Output {
    fn from_cmdmode(v: i32) -> Output {
        [Output::Long, Output::NameOnly, Output::NameStatus, Output::ObjectOnly]
            .into_iter()
            .find(|o| *o as i32 == v)
            .unwrap_or(Output::Default)
    }
}

/// A field of `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Mode,
    Type,
    Name,
    Size,
    SizePadded,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(Vec<u8>),
    Field(Field),
}

/// Parse a `--format` string. The error is the complete message.
fn parse_format(fmt: &str) -> Result<Vec<Piece>, String> {
    let mut out = Vec::new();
    let mut lit = Vec::new();
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            lit.push(b[i]);
            i += 1;
            continue;
        }
        let rest = &fmt[i + 1..];
        let rb = rest.as_bytes();
        if rb.first() == Some(&b'%') {
            lit.push(b'%');
            i += 2;
            continue;
        }
        if rb.first() == Some(&b'n') {
            lit.push(b'\n');
            i += 2;
            continue;
        }
        if rb.len() >= 3 && rb[0] == b'x' && rb[1].is_ascii_hexdigit() && rb[2].is_ascii_hexdigit()
        {
            let v = u8::from_str_radix(&rest[1..3], 16).expect("two hex digits");
            lit.push(v);
            i += 4;
            continue;
        }
        if !rest.starts_with('(') {
            return Err(msg::bad_format_open(rest));
        }
        let Some(close) = rest.find(')') else {
            return Err(msg::bad_format_close(rest));
        };
        let field = match &rest[1..close] {
            "objectmode" => Field::Mode,
            "objecttype" => Field::Type,
            "objectname" => Field::Name,
            "objectsize" => Field::Size,
            "objectsize:padded" => Field::SizePadded,
            "path" => Field::Path,
            _ => return Err(msg::bad_format_field(&rest[..=close])),
        };
        if !lit.is_empty() {
            out.push(Piece::Literal(std::mem::take(&mut lit)));
        }
        out.push(Piece::Field(field));
        i += 1 + close + 1;
    }
    if !lit.is_empty() {
        out.push(Piece::Literal(lit));
    }
    Ok(out)
}

/// Everything that decides what is printed.
struct Lister<'a> {
    repo: &'a Repo,
    patterns: Vec<Vec<u8>>,
    recursive: bool,
    show_trees: bool,
    only_trees: bool,
    nul: bool,
    quote_high: bool,
    full_name: bool,
    prefix: Vec<u8>,
    abbrev: usize,
    output: Output,
    format: Option<Result<Vec<Piece>, String>>,
    out: Vec<u8>,
}

impl Lister<'_> {
    fn matches(&self, path: &[u8]) -> bool {
        self.patterns.is_empty()
            || self.patterns.iter().any(|p| {
                p.is_empty()
                || path == p.as_slice()
                || (path.starts_with(p) && (p.ends_with(b"/") || path[p.len()] == b'/'))
        })
    }

    /// Some pattern lies strictly below the directory `path`.
    fn leads_to_pattern(&self, path: &[u8]) -> bool {
        self.patterns
            .iter()
            .any(|p| p.len() > path.len() && p.starts_with(path) && p[path.len()] == b'/')
    }

    fn walk(&mut self, tree: &ObjectId, base: &[u8]) -> Result<(), Die> {
        let entries = match revision::read_object(self.repo, tree) {
            Some(Object::Tree(t)) => t.entries,
            _ => return Err(Die::Error(msg::could_not_read(&tree.to_hex()), 1)),
        };
        for e in entries {
            let mut path = base.to_vec();
            path.extend_from_slice(&e.name);
            let mode = e.mode.canonical();
            if mode.is_tree() {
                if self.leads_to_pattern(&path) {
                    if self.show_trees {
                        self.show(&e.oid, &mode, &path)?;
                    }
                    path.push(b'/');
                    self.walk(&e.oid, &path)?;
                } else if self.matches(&path) {
                    if self.recursive {
                        if self.show_trees || self.only_trees {
                            self.show(&e.oid, &mode, &path)?;
                        }
                        path.push(b'/');
                        self.walk(&e.oid, &path)?;
                    } else {
                        self.show(&e.oid, &mode, &path)?;
                    }
                }
            } else if self.matches(&path) && (!self.only_trees || mode == EntryMode::COMMIT) {
                self.show(&e.oid, &mode, &path)?;
            }
        }
        Ok(())
    }

    fn type_of(mode: &EntryMode) -> ObjectKind {
        if mode.is_tree() {
            ObjectKind::Tree
        } else if *mode == EntryMode::COMMIT {
            ObjectKind::Commit
        } else {
            ObjectKind::Blob
        }
    }

    /// The size column: the size of a blob, `BAD` when it cannot be
    /// read, `-` for anything else.
    fn size(&self, id: &ObjectId, kind: ObjectKind) -> String {
        if kind != ObjectKind::Blob {
            return "-".to_string();
        }
        match self.repo.odb().header(id) {
            Ok(Some(h)) => h.size.to_string(),
            _ => "BAD".to_string(),
        }
    }

    fn name(&self, id: &ObjectId) -> String {
        if self.abbrev == 0 {
            id.to_hex()
        } else {
            self.repo.abbrev(id, self.abbrev)
        }
    }

    fn write_path(&mut self, path: &[u8]) {
        let shown = if self.full_name {
            path.to_vec()
        } else {
            relative_to(path, &self.prefix)
        };
        if self.nul {
            self.out.extend_from_slice(&shown);
        } else {
            write_quoted(&mut self.out, &shown, self.quote_high);
        }
    }

    fn show(&mut self, id: &ObjectId, mode: &EntryMode, path: &[u8]) -> Result<(), Die> {
        let kind = Self::type_of(mode);
        if let Some(fmt) = &self.format {
            let pieces = fmt.clone().map_err(Die::Fatal)?;
            for p in pieces {
                match p {
                    Piece::Literal(l) => self.out.extend_from_slice(&l),
                    Piece::Field(Field::Mode) => {
                        let s = format!("{:06o}", mode.bits());
                        self.out.extend_from_slice(s.as_bytes());
                    }
                    Piece::Field(Field::Type) => self.out.extend_from_slice(kind.name().as_bytes()),
                    Piece::Field(Field::Name) => {
                        let n = self.name(id);
                        self.out.extend_from_slice(n.as_bytes());
                    }
                    Piece::Field(f @ (Field::Size | Field::SizePadded)) => {
                        let s = self.size(id, kind);
                        let s = if f == Field::SizePadded {
                            format!("{s:>7}")
                        } else {
                            s
                        };
                        self.out.extend_from_slice(s.as_bytes());
                    }
                    Piece::Field(Field::Path) => self.write_path(path),
                }
            }
        } else {
            match self.output {
                Output::NameOnly | Output::NameStatus => self.write_path(path),
                Output::ObjectOnly => {
                    let n = self.name(id);
                    self.out.extend_from_slice(n.as_bytes());
                }
                _ => {
                    let n = self.name(id);
                    let mut s = format!("{:06o} {} {}", mode.bits(), kind.name(), n);
                    if self.output == Output::Long {
                        s.push_str(&format!(" {:>7}", self.size(id, kind)));
                    }
                    s.push('\t');
                    self.out.extend_from_slice(s.as_bytes());
                    self.write_path(path);
                }
            }
        }
        self.out.push(if self.nul { 0 } else { b'\n' });
        if self.out.len() >= 64 * 1024 {
            self.flush();
        }
        Ok(())
    }

    fn flush(&mut self) {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&self.out);
        let _ = o.flush();
        self.out.clear();
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let only_trees = Cell::new(false);
    let recursive = Cell::new(false);
    let show_trees = Cell::new(false);
    let nul = Cell::new(false);
    let mode = Cell::new(0);
    let full_name = Cell::new(false);
    let full_tree = Cell::new(false);
    let format = RefCell::new(None);
    let abbrev = Cell::new(Abbrev::Full);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('d', None, &only_trees, msg::HELP_D),
            Opt::flag('r', None, &recursive, msg::HELP_R),
            Opt::flag('t', None, &show_trees, msg::HELP_T),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::cmdmode('l', "long", &mode, msg::HELP_LONG, Output::Long as i32),
            Opt::cmdmode(
                None,
                "name-only",
                &mode,
                msg::HELP_NAME_ONLY,
                Output::NameOnly as i32,
            ),
            Opt::cmdmode(
                None,
                "name-status",
                &mode,
                msg::HELP_NAME_ONLY,
                Output::NameStatus as i32,
            ),
            Opt::cmdmode(
                None,
                "object-only",
                &mode,
                msg::HELP_OBJECT_ONLY,
                Output::ObjectOnly as i32,
            ),
            Opt::flag(None, "full-name", &full_name, msg::HELP_FULL_NAME),
            Opt::flag(None, "full-tree", &full_tree, msg::HELP_FULL_TREE),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT).noneg(),
            Opt::callback(None, "abbrev", Some("n"), msg::HELP_ABBREV, |arg, neg| {
                let choice = match (neg, arg) {
                    (true, _) => Abbrev::Full,
                    (false, None) => Abbrev::Configured,
                    (false, Some(v)) => match v.parse::<i64>() {
                        Ok(n) if n <= 0 => Abbrev::Full,
                        Ok(n) => Abbrev::Digits(usize::try_from(n).unwrap_or(usize::MAX)),
                        Err(_) => {
                            return Err(CallbackError::Error(msg::abbrev_not_numeric("abbrev")));
                        }
                    },
                };
                abbrev.set(choice);
                Ok(())
            })
            .optional(None),
        ],
    );
    let args = parser.parse(argv)?.args;
    if format.borrow().is_some() && mode.get() != 0 {
        return Err(parser.usage_msg(msg::FORMAT_EXCLUSIVE).into());
    }
    if args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);

    let repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let full_tree = full_tree.get();
    let prefix = if full_tree { Vec::new() } else { repo.prefix() };

    let spec = &args[0];
    let id = revision::resolve(&repo, spec).map_err(|e| {
        if let Some(err) = e.error {
            eprintln!("error: {err}");
        }
        Die::Fatal(msg::not_valid_object_name(spec))
    })?;
    let tree = revision::peel_to(&repo, id, ObjectKind::Tree)
        .map_err(|_| Die::Fatal(msg::NOT_A_TREE.to_string()))?;

    let root = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let opts = ParseOptions {
        prefix: prefix.clone(),
        root,
        globals: GlobalFlags::default(),
        allowed_magic: Magic::TOP | Magic::LITERAL,
        ..Default::default()
    };
    let mut patterns: Vec<Vec<u8>> = if args.len() > 1 {
        let ps = Pathspec::parse(&args[1..], &opts).map_err(|e| Die::Fatal(e.to_string()))?;
        ps.items().iter().map(|i| i.pattern().to_vec()).collect()
    } else {
        Vec::new()
    };
    if args.len() == 1 && !prefix.is_empty() {
        patterns.push(prefix.clone());
    }

    let abbrev = match abbrev.get() {
        Abbrev::Full => 0,
        Abbrev::Configured => repo.default_abbrev(),
        Abbrev::Digits(n) => n.max(crate::objects::MIN_ABBREV),
    };
    let fmt = format.borrow().clone();
    let mut lister = Lister {
        repo: &repo,
        patterns,
        recursive: recursive.get(),
        show_trees: show_trees.get() || (only_trees.get() && recursive.get()),
        only_trees: only_trees.get(),
        nul: nul.get(),
        quote_high: repo.quote_path(),
        full_name: full_name.get() || full_tree,
        prefix,
        abbrev,
        output: Output::from_cmdmode(mode.get()),
        format: fmt.map(|f| parse_format(&f)),
        out: Vec::new(),
    };
    let result = lister.walk(&tree, b"");
    lister.flush();
    result.map(|()| 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(
            parse_format("%(path)%x09%%").unwrap(),
            vec![
                Piece::Field(Field::Path),
                Piece::Literal(b"\t%".to_vec())
            ]
        );
        assert_eq!(
            parse_format("%x0|%(bogus)").unwrap_err(),
            msg::bad_format_open("x0|%(bogus)")
        );
        assert_eq!(
            parse_format("%(objectname").unwrap_err(),
            msg::bad_format_close("(objectname")
        );
        assert_eq!(
            parse_format("%(objectname x)").unwrap_err(),
            msg::bad_format_field("(objectname x)")
        );
        assert_eq!(parse_format("a%").unwrap_err(), msg::bad_format_open(""));
    }
}
