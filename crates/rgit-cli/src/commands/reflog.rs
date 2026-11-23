// SPDX-License-Identifier: MIT
//! `git reflog` (Documentation/git-reflog.adoc): `show` (the default),
//! `list` and `exists`.
//!
//! `show` prints git's default format (`log -g --abbrev-commit
//! --pretty=oneline`) for one reference, with `-n`/`--max-count`,
//! `--abbrev=<n>` and `--no-abbrev-commit`; the other `log` options are
//! `log -g` (#47). `expire` and `delete` (see [`crate::reflog_expire`]),
//! `drop` and `write`.

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_date::{DateContext, DateMode, show_date};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::ObjectKind;
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_root_ref, is_root_ref_syntax, is_valid_refname};
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
        Some("-h" | "--help-all") => {
            print!("{}", msg::USAGE);
            Ok(0)
        }
        Some("show") => show(ctx, &args[1..]),
        Some("list") => list(ctx, args),
        Some("exists") => exists(ctx, args),
        Some("expire") => expire(ctx, args),
        Some("delete") => delete(ctx, args),
        Some("drop") => drop(ctx, args),
        Some("write") => write(ctx, args),
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
        FilesStore::new(&self.common_dir, self.kind)
            .with_worktree(&self.git_dir)
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
        // `main-worktree/<ref>` and `worktrees/<id>/<ref>` name another
        // worktree's references.
        let local = name
            .strip_prefix("main-worktree/")
            .or_else(|| {
                name.strip_prefix("worktrees/")
                    .and_then(|r| r.split_once('/'))
                    .map(|(_, r)| r)
            })
            .unwrap_or(name);
        if local.starts_with("refs/") || is_root_ref_syntax(local) {
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

/// `reflog expire` (see [`crate::reflog_expire`]). `--updateref` and
/// `--stale-fix` are not supported yet (#101).
fn expire(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(false);
    let rewrite = Cell::new(false);
    let updateref = Cell::new(false);
    let verbose = Cell::new(false);
    let expire: RefCell<Option<String>> = RefCell::new(None);
    let expire_unreachable: RefCell<Option<String>> = RefCell::new(None);
    let stale_fix = Cell::new(false);
    let all = Cell::new(false);
    let single_worktree = Cell::new(false);
    let mut parser = Parser::new(
        &[msg::USAGE_EXPIRE],
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag(None, "rewrite", &rewrite, msg::HELP_REWRITE),
            Opt::flag(None, "updateref", &updateref, msg::HELP_UPDATEREF),
            Opt::flag(None, "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::string(None, "expire", &expire, msg::ARGH_TIMESTAMP, msg::HELP_EXPIRE).noneg(),
            Opt::string(
                None,
                "expire-unreachable",
                &expire_unreachable,
                msg::ARGH_TIMESTAMP,
                msg::HELP_EXPIRE_UNREACHABLE,
            )
            .noneg(),
            Opt::flag(None, "stale-fix", &stale_fix, msg::HELP_STALE_FIX),
            Opt::flag(None, "all", &all, msg::HELP_ALL),
            Opt::flag(None, "single-worktree", &single_worktree, msg::HELP_SINGLE_WORKTREE),
        ],
    );
    let args = parser.parse(argv)?.args;
    let date = DateContext::from_env();
    let timestamp = |value: &RefCell<Option<String>>, option: &str| -> Result<Option<u64>, Die> {
        let Some(v) = value.borrow().clone() else {
            return Ok(None);
        };
        rgit_date::parse_expiry_date(&v, &date)
            .map(Some)
            .map_err(|_| Die::Fatal(msg::invalid_timestamp_option(&v, option)))
    };
    let explicit = crate::reflog_expire::Explicit {
        total: timestamp(&expire, "expire")?,
        unreachable: timestamp(&expire_unreachable, "expire-unreachable")?,
    };
    let repo = crate::repository::Repo::open(ctx, Mode::Required)?;
    let settings = crate::reflog_expire::Config::load(&repo.config, explicit, &date)?;
    let git_dir = repo.setup.absolute_git_dir();
    let common = repo.setup.absolute_common_dir();
    let kind = repo.hash;
    let mut logs = Vec::new();
    if all.get() {
        logs = crate::reflog_expire::all_logs(&git_dir, &common, kind, single_worktree.get());
    }
    // Every name is looked up first; the ones without a reflog are
    // reported before anything is pruned.
    let files = FilesReflog::new(&git_dir, &common, kind);
    let mut status = 0;
    for a in &args {
        match log_name(&files, &git_dir, &common, a) {
            Some(full) => logs.push(crate::reflog_expire::Log {
                git_dir: git_dir.clone(),
                name: full,
                listed: false,
            }),
            None => {
                eprintln!("error: {}", msg::reflog_not_found(a));
                status = 255;
            }
        }
    }
    let o = crate::reflog_expire::Options {
        dry_run: dry_run.get(),
        verbose: verbose.get(),
        rewrite: rewrite.get(),
        updateref: updateref.get(),
    };
    if stale_fix.get() && verbose.get() {
        println!("{}", msg::MARKING_REACHABLE);
    }
    let mut stale = stale_fix
        .get()
        .then(|| crate::reflog_expire::StaleFix::new(&repo));
    for log in &logs {
        let rule = crate::reflog_expire::Rule::Age(settings.cutoffs(&log.name));
        crate::reflog_expire::prune(&repo, &common, log, rule, o, stale.as_mut())?;
    }
    Ok(status)
}

/// The full name of the reference whose reflog `name` stands for: the
/// first of the `gitrevisions(7)` candidates that has a log and points to
/// an object.
fn log_name(files: &FilesReflog, git_dir: &Path, common: &Path, name: &str) -> Option<String> {
    let name = if name == "@" { "HEAD" } else { name };
    let mut candidates = vec![name.to_owned()];
    for pattern in ["refs/", "refs/tags/", "refs/heads/", "refs/remotes/"] {
        candidates.push(format!("{pattern}{name}"));
    }
    candidates.push(format!("refs/remotes/{name}/HEAD"));
    candidates
        .into_iter()
        .find(|c| {
            is_valid_refname(c.as_bytes(), RefNameFlags::ONELEVEL)
                && files.exists(c)
                && crate::reflog_expire::tip(git_dir, common, files.hash_kind(), c).is_some()
        })
}

/// `reflog delete <ref>@{<specifier>}...`: removes single entries; each
/// argument applies to the log as the previous ones left it.
fn delete(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(false);
    let rewrite = Cell::new(false);
    let updateref = Cell::new(false);
    let verbose = Cell::new(false);
    let mut parser = Parser::new(
        &[msg::USAGE_DELETE],
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag(None, "rewrite", &rewrite, msg::HELP_REWRITE),
            Opt::flag(None, "updateref", &updateref, msg::HELP_UPDATEREF),
            Opt::flag(None, "verbose", &verbose, msg::HELP_VERBOSE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() {
        return Err(Die::Error(msg::NO_REFLOG_TO_DELETE.to_owned(), 255));
    }
    let repo = crate::repository::Repo::open(ctx, Mode::Required)?;
    let git_dir = repo.setup.absolute_git_dir();
    let common = repo.setup.absolute_common_dir();
    let files = FilesReflog::new(&git_dir, &common, repo.hash);
    let date = DateContext::from_env();
    let o = crate::reflog_expire::Options {
        dry_run: dry_run.get(),
        verbose: verbose.get(),
        rewrite: rewrite.get(),
        updateref: updateref.get(),
    };
    let mut status = 0;
    for a in &args {
        let Some(at) = a.find("@{") else {
            eprintln!("error: {}", msg::not_a_reflog(a));
            status = 255;
            continue;
        };
        let Some(full) = (at > 0)
            .then(|| log_name(&files, &git_dir, &common, &a[..at]))
            .flatten() else {
            eprintln!("error: {}", msg::no_reflog_for(a));
            status = 255;
            continue;
        };
        let spec = &a[at + 2..];
        let count = spec
            .strip_suffix('}')
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        // A count, else a date: the newest entry older than it. A
        // selector that matches nothing still walks the log (`--verbose`).
        let n = match count {
            Some(n) => n.parse::<usize>().unwrap_or(usize::MAX),
            None => {
                let t = rgit_date::approxidate(spec.strip_suffix('}').unwrap_or(spec), &date);
                files
                    .read(&full)
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .position(|e| e.time < t)
                    .unwrap_or(usize::MAX)
            }
        };
        let log = crate::reflog_expire::Log {
            git_dir: git_dir.clone(),
            name: full,
            listed: true,
        };
        let rule = crate::reflog_expire::Rule::Nth(n);
        crate::reflog_expire::prune(&repo, &common, &log, rule, o, None)?;
    }
    Ok(status)
}

/// `reflog drop [--all [--single-worktree] | <refs>...]`: removes whole
/// reflogs.
fn drop(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let all = Cell::new(false);
    let single_worktree = Cell::new(false);
    let mut parser = Parser::new(
        &[msg::USAGE_DROP],
        vec![
            Opt::flag(None, "all", &all, msg::HELP_DROP_ALL),
            Opt::flag(
                None,
                "single-worktree",
                &single_worktree,
                msg::HELP_DROP_SINGLE_WORKTREE,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    if all.get() && !args.is_empty() {
        eprintln!("usage: {}", msg::REFS_WITH_ALL);
        return Err(Die::Exit(129));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let git_dir = setup.absolute_git_dir();
    let common = setup.absolute_common_dir();
    let kind = setup.format.object_format;
    if all.get() {
        let logs = crate::reflog_expire::all_logs(&git_dir, &common, kind, single_worktree.get());
        let mut dirs = vec![common.join("logs")];
        for log in logs {
            let _ = FilesReflog::new(&log.git_dir, &common, kind).delete(&log.name);
            dirs.push(log.git_dir.join("logs"));
        }
        // Like git, the emptied `logs` directories go too.
        for d in dirs {
            remove_empty_dirs(&d);
        }
        return Ok(0);
    }
    // Without arguments nothing is dropped.
    let files = FilesReflog::new(&git_dir, &common, kind);
    let mut status = 0;
    for a in &args {
        match log_name(&files, &git_dir, &common, a) {
            Some(full) => {
                if let Err(e) = files.delete(&full) {
                    eprintln!("error: {e}");
                    status = 255;
                }
            }
            None => {
                eprintln!("error: {}", msg::reflog_not_found(a));
                status = 255;
            }
        }
    }
    Ok(status)
}

/// Removes `dir` and the directories below it if they hold no files;
/// whether `dir` is gone.
fn remove_empty_dirs(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    for e in entries.flatten() {
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if !(is_dir && remove_empty_dirs(&e.path())) {
            empty = false;
        }
    }
    empty && std::fs::remove_dir(dir).is_ok()
}

/// `reflog write <ref> <old-oid> <new-oid> <message>`: appends one entry.
fn write(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_WRITE], vec![]);
    let args = parser.parse(argv)?.args;
    let [name, old, new, message] = args.as_slice() else {
        return Err(parser.usage_error().into());
    };
    let repo = crate::repository::Repo::open(ctx, Mode::Required)?;
    if !(is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL)
        && (name.starts_with("refs/") || is_root_ref(name)))
    {
        return Err(Die::Fatal(msg::invalid_reference_name(name)));
    }
    let parse = |hex: &str, which: &str| -> Result<ObjectId, Die> {
        let id = ObjectId::from_hex(repo.hash, hex.as_bytes())
            .map_err(|_| Die::Fatal(msg::invalid_object_id(which, hex)))?;
        if !id.is_null() && !repo.odb().contains(&id) {
            return Err(Die::Fatal(msg::object_does_not_exist(which, hex)));
        }
        Ok(id)
    };
    let old = parse(old, "old")?;
    let new = parse(new, "new")?;
    let objects = crate::refs_support::Objects::open(&repo.setup);
    crate::refs_support::UpdateHooks::new(&repo.setup, &repo.config, &objects)
        .log(name, old, new, message, true);
    Ok(0)
}

/// Options of `reflog show`.
#[derive(Default)]
struct ShowOptions {
    max_count: Option<usize>,
    /// `Some(None)`: full ids; `Some(Some(n))`: at least `n` digits.
    abbrev: Option<Option<usize>>,
    revs: Vec<String>,
    paths: Vec<String>,
    /// `--color[=<when>]` / `--no-color`.
    color: Option<rgit_config::color::ColorBool>,
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
            "-h" | "--help-all" => {
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
            "--abbrev-commit" | "--oneline" | "--pretty=oneline" | "--format=oneline"
            | "--no-decorate" => {}
            _ if crate::color::parse_arg(a).is_some() => {
                if let Some(c) = crate::color::parse_arg(a) {
                    o.color = Some(c?);
                }
            }
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
    // Everything beyond the plain listing is `log -g` with the reflog's
    // defaults in front.
    let as_log = || {
        let mut argv: Vec<String> = ["-g", "--abbrev-commit", "--pretty=oneline"]
            .map(String::from)
            .to_vec();
        argv.extend(args.iter().cloned());
        crate::history::main(ctx, crate::history::Command::Log, &argv)
    };
    let o = match parse_show(args) {
        Ok(Ok(o)) => o,
        Ok(Err(done)) => return done,
        Err(_) => return as_log(),
    };
    if !o.paths.is_empty() || o.revs.len() > 1 {
        return as_log();
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    crate::pager::setup_for(ctx, &config, "reflog", true)?;
    let color = if crate::color::want(&config, Some("color.diff"), o.color)? {
        Some(crate::color::slot(&config, "color.diff.commit", "\x1b[33m")?)
    } else {
        None
    };
    let arg = o.revs.first().cloned().unwrap_or_else(|| "HEAD".to_string());
    let WalkSource {
        display,
        entries,
        start,
        by_date,
        ..
    } = walk_source(&setup, &arg)?;
    let refs = Refs::new(&setup);
    let kind = refs.kind;
    let date_ctx = DateContext::from_env();
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
        let id = match &color {
            Some(c) => format!("{c}{id}{}", crate::color::RESET),
            None => id,
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

/// The reflog `log -g` and `reflog show` walk for the argument `arg`
/// (`<ref>`, `<ref>@{<n>}`, `<ref>@{<date>}`, `@{-<n>}`, `@{<n>}`); the
/// warnings of a date lookup are printed.
pub(crate) struct WalkSource {
    /// The name as given (`HEAD`, `master`, `refs/heads/master`).
    pub display: String,
    /// Its entries, oldest first.
    pub entries: Vec<rgit_refs::reflog::Entry>,
    /// Entries skipped from the newest (`@{<n>}`, `@{<date>}`).
    pub start: usize,
    /// The selector was a date: entries are named by their date.
    pub by_date: bool,
    /// The selector was a count: entries are named by their index.
    pub by_index: bool,
}

pub(crate) fn walk_source(setup: &rgit_repo::Setup, arg: &str) -> Result<WalkSource, Die> {
    let refs = Refs::new(setup);
    let logs = refs.logs();
    let ambiguous = || Die::Fatal(msg::ambiguous_argument(arg));

    // `<name>@{<selector>}`
    let (base, selector) = match arg.rfind("@{") {
        Some(p) if arg.ends_with('}') => (&arg[..p], Some(&arg[p + 2..arg.len() - 1])),
        _ => (arg, None),
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

    Ok(WalkSource {
        display,
        entries,
        start,
        by_date,
        by_index: selector.is_some() && !by_date,
    })
}
