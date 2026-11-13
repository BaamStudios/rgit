// SPDX-License-Identifier: MIT
//! `git check-attr` (Documentation/git-check-attr.adoc): show the
//! attributes of paths.
//!
//! Arguments: with `--all` all arguments are paths (a leading `--` is
//! skipped, any other `--` is an error); otherwise the arguments before
//! `--` are attributes and the rest paths, and without `--` the first
//! argument is the attribute (all of them with `--stdin`). Paths are
//! printed as given (C-quoted unless `-z`); a trailing `/` makes a path
//! a directory for patterns ending in `/`. `builtin_objectmode` is
//! computed from the work tree file (the index entry with `--cached`).

use std::io::{BufRead, Write};

use rgit_attr::{AttrId, Attributes, BUILTIN_OBJECTMODE, is_valid_name};

use crate::attributes;
use crate::messages::check_attr as msg;
use crate::prelude::*;
use crate::quote::{c_unquote, write_quoted};
use crate::repo::{self, Mode};
use crate::repository::Repo;

/// What to print for each path.
enum Query {
    All,
    Names(Vec<(Vec<u8>, AttrId)>),
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let help_only = argv.len() == 2 && (argv[1] == "-h" || argv[1] == "--help-all");
    let setup = if help_only {
        None
    } else {
        Some(repo::setup(ctx, Mode::Required)?)
    };

    let all = Cell::new(false);
    let cached = Cell::new(false);
    let stdin = Cell::new(false);
    let nul = Cell::new(false);
    let source = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::flag(None, "cached", &cached, msg::HELP_CACHED),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::flag('z', None, &nul, msg::HELP_Z).noneg(),
            Opt::string(None, "source", &source, msg::ARGH_SOURCE, msg::HELP_SOURCE),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        ..ParseFlags::default()
    });
    let args = parser.parse(argv)?.args;
    let Some(setup) = setup else {
        return Ok(0);
    };
    if setup.work_tree.is_none() && !setup.is_bare() {
        setup.require_work_tree().map_err(repo::die)?;
    }

    let dashdash = args.iter().position(|a| a == "--");
    let (attr_count, first_path) = if all.get() {
        match dashdash {
            Some(0) => (0, 1),
            Some(_) => return Err(parser.error_with_usage(msg::ATTRS_AND_ALL).into()),
            None => (0, 0),
        }
    } else if let Some(dd) = dashdash {
        (dd, dd + 1)
    } else if stdin.get() {
        (args.len(), args.len())
    } else {
        (args.len().min(1), args.len().min(1))
    };
    if !all.get() && attr_count == 0 {
        return Err(parser.error_with_usage(msg::NO_ATTRIBUTE).into());
    }
    if stdin.get() {
        if first_path < args.len() {
            return Err(parser.error_with_usage(msg::FILES_WITH_STDIN).into());
        }
    } else if first_path >= args.len() {
        return Err(parser.error_with_usage(msg::NO_FILE).into());
    }
    drop(parser);
    for name in &args[..attr_count] {
        if !is_valid_name(name.as_bytes()) {
            return Err(Die::Error(msg::invalid_attribute(name), 255));
        }
    }

    let mut repo = Repo::from_setup(ctx, setup)?;
    let tree = attributes::tree_source(ctx, &mut repo, source.borrow().as_deref())?;
    let (index, _) = repo.read_index()?;
    let root = repo
        .setup
        .work_tree
        .clone()
        .unwrap_or_else(|| repo.setup.cwd.clone());
    let mut attrs = attributes::open(&repo, &index, tree, cached.get());
    let query = if all.get() {
        Query::All
    } else {
        Query::Names(
            args[..attr_count]
                .iter()
                .map(|n| (n.as_bytes().to_vec(), attrs.id(n.as_bytes())))
                .collect(),
        )
    };
    print_warnings(&mut attrs);

    let mut checker = Checker {
        repo: &repo,
        root: &root,
        index: &index,
        attrs,
        query,
        cached: cached.get(),
        nul: nul.get(),
        quote_high: repo.quote_path(),
        out: std::io::stdout().lock(),
    };
    if stdin.get() {
        let term = if nul.get() { b'\0' } else { b'\n' };
        let mut input = std::io::stdin().lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = input
                .read_until(term, &mut line)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if n == 0 {
                break;
            }
            if line.last() == Some(&term) {
                line.pop();
            }
            let arg = if !nul.get() && line.first() == Some(&b'"') {
                c_unquote(&line).ok_or_else(|| Die::Fatal(msg::BADLY_QUOTED.into()))?
            } else {
                line.clone()
            };
            checker.check(&arg)?;
            let _ = checker.out.flush();
        }
    } else {
        for a in &args[first_path..] {
            checker.check(a.as_bytes())?;
        }
    }
    let _ = checker.out.flush();
    Ok(0)
}

fn print_warnings(attrs: &mut Attributes<'_>) {
    for w in attrs.take_warnings() {
        eprintln!("{w}");
    }
}

struct Checker<'a, W: Write> {
    repo: &'a Repo,
    root: &'a std::path::Path,
    index: &'a rgit_index::Index,
    attrs: Attributes<'a>,
    query: Query,
    cached: bool,
    nul: bool,
    quote_high: bool,
    out: W,
}

impl<W: Write> Checker<'_, W> {
    fn check(&mut self, arg: &[u8]) -> Result<(), Die> {
        let full = self.repo.prefix_path(arg).map_err(|()| {
            let root = self
                .repo
                .setup
                .work_tree
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Die::Fatal(msg::outside_repository(&String::from_utf8_lossy(arg), &root))
        })?;
        let is_dir = full.last() == Some(&b'/');
        let path = full.strip_suffix(b"/").unwrap_or(&full);
        let outcome = self.attrs.lookup(path, is_dir);
        print_warnings(&mut self.attrs);
        let mut rec = Vec::new();
        match &self.query {
            Query::All => {
                for (id, state) in outcome.specified() {
                    self.record(&mut rec, arg, self.attrs.name(id), state.display());
                }
            }
            Query::Names(names) => {
                for (name, id) in names {
                    if name.as_slice() == BUILTIN_OBJECTMODE {
                        let mode =
                            attributes::object_mode(self.root, self.index, path, self.cached)?;
                        let value = mode.as_deref().map_or(&b"unspecified"[..], str::as_bytes);
                        self.record(&mut rec, arg, name, value);
                    } else {
                        self.record(&mut rec, arg, name, outcome.get(*id).display());
                    }
                }
            }
        }
        self.out
            .write_all(&rec)
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    fn record(&self, rec: &mut Vec<u8>, path: &[u8], name: &[u8], value: &[u8]) {
        if self.nul {
            for part in [path, name, value] {
                rec.extend_from_slice(part);
                rec.push(b'\0');
            }
        } else {
            write_quoted(rec, path, self.quote_high);
            rec.extend_from_slice(b": ");
            rec.extend_from_slice(name);
            rec.extend_from_slice(b": ");
            rec.extend_from_slice(value);
            rec.push(b'\n');
        }
    }
}
