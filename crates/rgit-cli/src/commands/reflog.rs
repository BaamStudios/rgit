// SPDX-License-Identifier: MIT
//! `git reflog` (Documentation/git-reflog.adoc): `show` (the default),
//! `list` and `exists`.
//!
//! `show` prints git's default format (`log -g --abbrev-commit
//! --pretty=oneline`) for one reference, with `-n`/`--max-count`,
//! `--abbrev=<n>` and `--no-abbrev-commit`; the other `log` options are
//! `log -g` (#47). `expire`, `delete`, `drop` and `write` are #101.

use std::io::Write;
use std::path::PathBuf;

use rgit_date::{DateContext, DateMode, show_date};
use rgit_hash::{HashKind, ObjectId};
use rgit_pack::ObjectKind;
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_per_worktree, is_root_ref_syntax, is_valid_refname};
use rgit_refs::reflog::lookup::Target as LookupTarget;
use rgit_refs::reflog::{FilesReflog, Selector, nth_prior_checkout, resolve};
use rgit_refs::{MAX_SYMREF_DEPTH, RefStore, Target};

use crate::messages::reflog as msg;
use crate::objects::Objects;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    match args.first().map(String::as_str) {
        Some("-h") => {
            print!("{}", msg::USAGE);
            Ok(0)
        }
        Some("show") => show(ctx, &args[1..]),
        Some("list") => list(ctx, args),
        Some("exists") => exists(ctx, args),
        Some(sub @ ("expire" | "delete" | "drop" | "write")) => {
            Err(Die::Fatal(msg::not_supported(&format!("'reflog {sub}'"))))
        }
        _ => show(ctx, args),
    }
}

/// The references and reflogs of the repository, per worktree.
struct Refs {
    git_dir: PathBuf,
    common_dir: PathBuf,
    kind: HashKind,
}

impl Refs {
    fn new(setup: &rgit_repo::Setup) -> Self {
        Refs {
            git_dir: setup.absolute_git_dir(),
            common_dir: setup.absolute_common_dir(),
            kind: setup.format.object_format,
        }
    }

    fn logs(&self) -> FilesReflog {
        FilesReflog::new(&self.git_dir, &self.common_dir, self.kind)
    }

    fn read_raw(&self, name: &str) -> Option<Target> {
        let dir = if is_per_worktree(name) {
            &self.git_dir
        } else {
            &self.common_dir
        };
        FilesStore::new(dir, self.kind)
            .read_raw(name)
            .ok()
            .flatten()
    }

    /// Follows symbolic references: the last name of the chain and its
    /// value; `None` if `name` does not exist at all.
    fn resolve(&self, name: &str) -> Option<(String, Option<ObjectId>)> {
        let mut current = name.to_string();
        for depth in 0..=MAX_SYMREF_DEPTH {
            match self.read_raw(&current) {
                None if depth == 0 => return None,
                None => return Some((current, None)),
                Some(Target::Object(id)) => return Some((current, Some(id))),
                Some(Target::Symbolic(next)) => current = next,
            }
        }
        None
    }

    /// The full name of the existing reference `name` stands for
    /// (`gitrevisions(7)` order) and its value.
    fn dwim(&self, name: &str) -> Option<(String, ObjectId)> {
        let mut candidates = Vec::new();
        if name.starts_with("refs/") || is_root_ref_syntax(name) {
            candidates.push(name.to_string());
        }
        for pattern in ["refs/", "refs/tags/", "refs/heads/", "refs/remotes/"] {
            candidates.push(format!("{pattern}{name}"));
        }
        candidates.push(format!("refs/remotes/{name}/HEAD"));
        candidates.into_iter().find_map(|c| {
            if !is_valid_refname(c.as_bytes(), RefNameFlags::ONELEVEL) {
                return None;
            }
            match self.resolve(&c) {
                Some((_, Some(id))) => Some((c, id)),
                _ => None,
            }
        })
    }
}

/// `reflog list`.
fn list(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_LIST], vec![]);
    let args = parser.parse(argv)?.args;
    if let Some(a) = args.first() {
        return Err(Die::Error(msg::list_no_arguments(a), 255));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let names = Refs::new(&setup)
        .logs()
        .list()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut out = std::io::stdout().lock();
    for n in names {
        let _ = writeln!(out, "{n}");
    }
    Ok(0)
}

/// `reflog exists <ref>`.
fn exists(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_EXISTS], vec![]);
    let args = parser.parse(argv)?.args;
    let Some(name) = args.first() else {
        return Err(parser.usage_error().into());
    };
    let setup = repo::setup(ctx, Mode::Required)?;
    if !is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL) {
        return Err(Die::Fatal(msg::invalid_ref_format(name)));
    }
    Ok(i32::from(!Refs::new(&setup).logs().exists(name)))
}

/// Options of `reflog show`.
#[derive(Default)]
struct ShowOptions {
    max_count: Option<usize>,
    /// `Some(None)`: full ids; `Some(Some(n))`: at least `n` digits.
    abbrev: Option<Option<usize>>,
    revs: Vec<String>,
    paths: Vec<String>,
}

fn parse_count(v: &str) -> Result<usize, Die> {
    v.parse::<i64>()
        .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
        .map_err(|_| Die::Fatal(msg::not_supported(&format!("the count '{v}'"))))
}

fn parse_show(args: &[String]) -> Result<Result<ShowOptions, CmdResult>, Die> {
    let mut o = ShowOptions::default();
    let mut options_done = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            o.paths.extend(it.by_ref().cloned());
            break;
        }
        if options_done || !a.starts_with('-') || a == "-" {
            o.revs.push(a.clone());
            continue;
        }
        match a.as_str() {
            "-h" => {
                print!("{}", msg::USAGE_SHOW);
                return Ok(Err(Ok(0)));
            }
            "--end-of-options" => options_done = true,
            "-n" | "--max-count" => {
                let v = it
                    .next()
                    .ok_or_else(|| Die::Fatal(msg::not_supported(&format!("'{a}' without a value"))))?;
                o.max_count = Some(parse_count(v)?);
            }
            "--abbrev-commit" | "--oneline" | "--pretty=oneline" | "--format=oneline" => {}
            "--no-abbrev-commit" | "--no-abbrev" => o.abbrev = Some(None),
            _ => {
                if let Some(v) = a.strip_prefix("--max-count=") {
                    o.max_count = Some(parse_count(v)?);
                } else if let Some(v) = a.strip_prefix("--abbrev=") {
                    o.abbrev = Some(Some(parse_count(v)?));
                } else if let Some(v) = a.strip_prefix("-n") {
                    o.max_count = Some(parse_count(v)?);
                } else if a[1..].bytes().all(|b| b.is_ascii_digit()) {
                    o.max_count = Some(parse_count(&a[1..])?);
                } else {
                    return Err(Die::Fatal(msg::not_supported(&format!("the option '{a}'"))));
                }
            }
        }
    }
    Ok(Ok(o))
}

/// Minimum abbreviation from `core.abbrev`; `None` for full ids.
fn configured_abbrev(config: &rgit_config::ConfigSet, kind: HashKind) -> Option<usize> {
    let Some(entry) = config.get("core.abbrev") else {
        return Some(7);
    };
    let value = entry.string().ok().map(|s| s.to_string());
    match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
        None | Some("auto") => Some(7),
        Some("false" | "no" | "off") => None,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= kind.hex_len() => None,
            Ok(n) => Some(n.max(4)),
            Err(_) => Some(7),
        },
    }
}

/// `reflog [show]`.
fn show(ctx: &mut Context, args: &[String]) -> CmdResult {
    let o = match parse_show(args)? {
        Ok(o) => o,
        Err(done) => return done,
    };
    if !o.paths.is_empty() {
        return Err(Die::Fatal(msg::not_supported("limiting to paths")));
    }
    if o.revs.len() > 1 {
        return Err(Die::Fatal(msg::not_supported("showing several reflogs")));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    let refs = Refs::new(&setup);
    let logs = refs.logs();
    let kind = refs.kind;
    let arg = o.revs.first().cloned().unwrap_or_else(|| "HEAD".to_string());
    let ambiguous = || Die::Fatal(msg::ambiguous_argument(&arg));

    // `<name>@{<selector>}`
    let (base, selector) = match arg.rfind("@{") {
        Some(p) if arg.ends_with('}') => (&arg[..p], Some(&arg[p + 2..arg.len() - 1])),
        _ => (arg.as_str(), None),
    };
    let previous = selector
        .and_then(|s| s.strip_prefix('-'))
        .filter(|n| !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()));
    let (display, full, current, selector) = if let Some(n) = previous {
        // `@{-<n>}`: the n-th previously checked out branch.
        if !base.is_empty() {
            return Err(ambiguous());
        }
        let head = logs.read("HEAD").ok().flatten().unwrap_or_default();
        let n = n.parse::<usize>().map_err(|_| ambiguous())?;
        let name = nth_prior_checkout(&head, n)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .ok_or_else(ambiguous)?;
        let (full, id) = refs.dwim(&name).ok_or_else(ambiguous)?;
        (name, full, Some(id), None)
    } else if base.is_empty() && selector.is_some() {
        // `@{<n>}`: the current branch.
        let (name, id) = refs.resolve("HEAD").ok_or_else(ambiguous)?;
        (name.clone(), name, id, selector)
    } else {
        let name = if base == "@" { "HEAD" } else { base };
        match refs.dwim(name) {
            Some((full, id)) => (name.to_string(), full, Some(id), selector),
            None => {
                if name == "HEAD" {
                    if let Some(Target::Symbolic(t)) = refs.read_raw("HEAD") {
                        let branch = t.strip_prefix("refs/heads/").unwrap_or(&t);
                        return Err(Die::Fatal(msg::no_commits_yet(branch)));
                    }
                }
                return Err(ambiguous());
            }
        }
    };

    let entries = logs
        .read(&full)
        .map_err(|e| Die::Fatal(e.to_string()))?
        .unwrap_or_default();
    let date_ctx = DateContext::from_env();
    let mut start = 0;
    let mut by_date = false;
    if let Some(spec) = selector {
        let sel = Selector::parse(spec, &date_ctx).ok_or_else(ambiguous)?;
        // Resolve once like the revision parser: same warnings and errors.
        let target = LookupTarget {
            refname: &full,
            display: &display,
            current,
        };
        match resolve(&entries, sel, target) {
            Ok(r) => {
                let mut err = std::io::stderr().lock();
                for w in &r.warnings {
                    let _ = writeln!(err, "warning: {w}");
                }
            }
            Err(e) => return Err(Die::Fatal(e.to_string())),
        }
        // The walk reads a plain number as a count, anything else as a date.
        if spec.bytes().all(|b| b.is_ascii_digit()) {
            start = spec.parse::<usize>().unwrap_or(usize::MAX);
        } else if let Selector::Date(t) = sel {
            by_date = true;
            start = entries
                .iter()
                .rev()
                .position(|e| e.time <= t)
                .unwrap_or(usize::MAX);
        }
    }

    let min_abbrev = o.abbrev.unwrap_or_else(|| configured_abbrev(&config, kind));
    let object_dir = std::env::var_os("GIT_OBJECT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| refs.common_dir.join("objects"));
    let objects = Objects::open(object_dir, kind);
    let mut out = std::io::stdout().lock();
    let mut shown = 0;
    for (k, e) in entries.iter().rev().enumerate().skip(start) {
        if o.max_count.is_some_and(|m| shown >= m) {
            break;
        }
        // Like `log -g`, entries whose object is not a commit are skipped.
        if objects.kind_of(&e.new) != Some(ObjectKind::Commit) {
            continue;
        }
        let id = match min_abbrev {
            Some(n) => objects.abbrev(&e.new, n),
            None => e.new.to_hex(),
        };
        let label = if by_date {
            show_date(e.time, e.tz, &DateMode::default(), &date_ctx)
                .unwrap_or_else(|_| rgit_date::format_raw(e.time, e.tz))
        } else {
            k.to_string()
        };
        let mut line = format!("{id} {display}@{{{label}}}: ").into_bytes();
        line.extend_from_slice(&e.message);
        line.push(b'\n');
        if out.write_all(&line).is_err() {
            break;
        }
        shown += 1;
    }
    Ok(0)
}
