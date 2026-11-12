// SPDX-License-Identifier: MIT
//! Shared by `rev-list` and `log`: the revision and walk options
//! (Documentation/rev-list-options.adoc), the starting points, the walk
//! over the repository's object database and the output.

pub mod mailmap;
pub mod pretty;

use std::io::Write;

use rgit_config::ConfigSet;
use rgit_date::{DateContext, DateMode};
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind};
use rgit_repo::Setup;
use rgit_revwalk::revision::{
    self, Error as RevError, Flags, Hint, ObjectLookup, RefLookup, Resolver, RevItem,
};
use rgit_revwalk::walk::{CommitInfo, CommitSource, Order, Walk, WalkOptions};

use crate::messages::rev_list as msg;
use crate::repo::{self, Mode};
use crate::revisions::{self, Config, IndexFile, Objects, Refs, WorkTree};
use crate::{Context, Die};
use pretty::{Entry, Pretty, Style};

/// The shortest `--abbrev` (smaller values are raised to it).
const MIN_ABBREV: usize = 4;

/// Which command parses the arguments (they differ in error handling and
/// defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    RevList,
    Log,
}

/// `--abbrev` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abbrev {
    /// `core.abbrev` or the automatic length.
    Default,
    /// `--abbrev=<n>`.
    Digits(usize),
    /// `--no-abbrev`.
    Full,
}

/// A starting point as given on the command line.
#[derive(Debug, Clone)]
enum Tip {
    /// A revision argument (`A`, `^A`, `A..B`, `A...B`, `A^!`, ...);
    /// `negate` is set after an odd number of `--not`.
    Rev { arg: String, negate: bool },
    /// `--all`, `--branches`, `--tags`, `--remotes`.
    Refs {
        prefix: &'static str,
        negative: bool,
    },
}

/// Parsed options.
#[derive(Debug)]
pub struct Options {
    pub walk: WalkOptions,
    tips: Vec<Tip>,
    pub count: bool,
    pub parents: bool,
    abbrev_commit: bool,
    abbrev: Abbrev,
    pub pretty: Option<Pretty>,
    date_mode: DateMode,
    /// `--[no-]mailmap`; `None` = the command's default.
    mailmap: Option<bool>,
}

fn parse_count(s: &str) -> Result<usize, Die> {
    match s.parse::<i64>() {
        Ok(n) => Ok(usize::try_from(n).unwrap_or(usize::MAX)),
        Err(_) => Err(Die::Fatal(msg::not_an_integer(s))),
    }
}

fn usage(cmd: Command, arg: &str) -> Die {
    match cmd {
        Command::RevList => {
            let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
            Die::Exit(rgit_options::EXIT_USAGE)
        }
        Command::Log => Die::Fatal(msg::unrecognized_argument(arg)),
    }
}

impl Options {
    /// Parse `argv` (without the command name).
    pub fn parse(cmd: Command, argv: &[String]) -> Result<Options, Die> {
        let mut o = Options {
            walk: WalkOptions::default(),
            tips: Vec::new(),
            count: false,
            parents: false,
            abbrev_commit: false,
            abbrev: Abbrev::Default,
            pretty: (cmd == Command::Log).then_some(Pretty::Medium),
            date_mode: DateMode::default(),
            mailmap: None,
        };
        let mut negate = false;
        let mut i = 0;
        let value = |i: &mut usize, name: &str| -> Result<String, Die> {
            *i += 1;
            argv.get(*i)
                .cloned()
                .ok_or_else(|| Die::Fatal(format!("{name} requires an argument")))
        };
        while i < argv.len() {
            let a = argv[i].as_str();
            if a == "--" {
                if i + 1 < argv.len() {
                    return Err(Die::Fatal(msg::not_supported("path limiting")));
                }
                break;
            }
            if !a.starts_with('-') || a == "-" {
                o.push_revision(a, negate);
                i += 1;
                continue;
            }
            match a {
                "--all" => o.tips.push(Tip::Refs {
                    prefix: "",
                    negative: negate,
                }),
                "--branches" => o.tips.push(Tip::Refs {
                    prefix: "refs/heads/",
                    negative: negate,
                }),
                "--tags" => o.tips.push(Tip::Refs {
                    prefix: "refs/tags/",
                    negative: negate,
                }),
                "--remotes" => o.tips.push(Tip::Refs {
                    prefix: "refs/remotes/",
                    negative: negate,
                }),
                "--not" => negate = !negate,
                "--topo-order" => o.walk.order = Order::Topo,
                "--date-order" => o.walk.order = Order::Date,
                "--reverse" => o.walk.reverse = true,
                "--first-parent" => o.walk.first_parent = true,
                "--parents" => o.parents = true,
                "--count" if cmd == Command::RevList => o.count = true,
                "--abbrev-commit" => o.abbrev_commit = true,
                "--no-abbrev-commit" => o.abbrev_commit = false,
                "--abbrev" => o.abbrev = Abbrev::Default,
                "--no-abbrev" => o.abbrev = Abbrev::Full,
                "--oneline" => {
                    o.pretty = Some(Pretty::Oneline);
                    o.abbrev_commit = true;
                }
                "--pretty" => o.pretty = Some(Pretty::Medium),
                "--no-decorate" | "--no-color" | "--no-show-signature" => {}
                "--mailmap" | "--use-mailmap" => o.mailmap = Some(true),
                "--no-mailmap" | "--no-use-mailmap" => o.mailmap = Some(false),
                "-n" | "--max-count" => {
                    o.walk.max_count = Some(parse_count(&value(&mut i, a)?)?);
                }
                "--skip" => o.walk.skip = parse_count(&value(&mut i, a)?)?,
                _ => {
                    if let Some(v) = a.strip_prefix("--max-count=") {
                        o.walk.max_count = Some(parse_count(v)?);
                    } else if let Some(v) = a.strip_prefix("--skip=") {
                        o.walk.skip = parse_count(v)?;
                    } else if let Some(v) = a.strip_prefix("-n") {
                        o.walk.max_count = Some(parse_count(v)?);
                    } else if a.len() > 1 && a[1..].bytes().all(|b| b.is_ascii_digit()) {
                        o.walk.max_count = Some(parse_count(&a[1..])?);
                    } else if let Some(v) = a.strip_prefix("--abbrev=") {
                        let n = parse_count(v)?;
                        o.abbrev = if n == 0 {
                            Abbrev::Full
                        } else {
                            Abbrev::Digits(n.max(MIN_ABBREV))
                        };
                    } else if let Some(v) = a
                        .strip_prefix("--pretty=")
                        .or_else(|| a.strip_prefix("--format="))
                    {
                        o.pretty = Some(parse_pretty(v)?);
                    } else if let Some(v) = a.strip_prefix("--date=") {
                        o.date_mode = rgit_date::parse_date_mode(v, false)
                            .map_err(|e| Die::Fatal(e.to_string()))?;
                    } else if a == "--decorate=no" || a == "--color=never" {
                    } else if a.starts_with("--branches=")
                        || a.starts_with("--tags=")
                        || a.starts_with("--remotes=")
                    {
                        return Err(Die::Fatal(msg::not_supported("a ref pattern")));
                    } else {
                        return Err(usage(cmd, a));
                    }
                }
            }
            i += 1;
        }
        Ok(o)
    }

    fn push_revision(&mut self, arg: &str, negate: bool) {
        self.tips.push(Tip::Rev {
            arg: arg.to_string(),
            negate,
        });
    }

    /// No revision arguments and no ref options.
    pub fn has_no_tips(&self) -> bool {
        self.tips.is_empty()
    }

    /// Start at `HEAD` (log without revisions).
    pub fn push_head(&mut self) {
        self.tips.push(Tip::Rev {
            arg: "HEAD".into(),
            negate: false,
        });
    }
}

fn parse_pretty(v: &str) -> Result<Pretty, Die> {
    if v.contains('%') || v.starts_with("format:") || v.starts_with("tformat:") {
        return Err(Die::Fatal(msg::not_supported("--format with placeholders")));
    }
    Pretty::from_name(v).ok_or_else(|| Die::Fatal(msg::invalid_pretty(v)))
}

/// The repository as `rev-list` and `log` use it.
pub struct Repo<'a> {
    pub setup: &'a Setup,
    pub config: &'a ConfigSet,
    pub objects: &'a Objects,
    pub refs: &'a Refs,
    pub resolver: &'a Resolver<'a>,
}

impl Repo<'_> {
    /// Read and parse a commit.
    pub fn read_commit(&self, id: &ObjectId) -> Option<(Commit, Vec<u8>)> {
        let (kind, data) = self.objects.read(id).ok()??;
        if kind != ObjectKind::Commit {
            return None;
        }
        let commit = Commit::parse(self.objects.hash_kind(), &data).ok()?;
        Some((commit, data))
    }

    /// The parents the walk uses (none at a shallow boundary).
    pub fn parents(&self, id: &ObjectId, commit: &Commit) -> Vec<ObjectId> {
        if self.objects.is_shallow(id) {
            return Vec::new();
        }
        commit.parents().unwrap_or_default()
    }

    /// Peel `id` through tags; `None` unless it ends at a commit.
    fn peel_to_commit(&self, mut id: ObjectId) -> Option<ObjectId> {
        for _ in 0..64 {
            let (kind, data) = self.objects.read(&id).ok()??;
            match kind {
                ObjectKind::Commit => return Some(id),
                ObjectKind::Tag => {
                    let tag = rgit_object::Tag::parse(self.objects.hash_kind(), &data).ok()?;
                    id = tag.object().ok()?;
                }
                _ => return None,
            }
        }
        None
    }

    /// `id` abbreviated to at least `len` digits, unique in the repository.
    fn abbrev(&self, id: &ObjectId, len: usize) -> String {
        if len >= id.kind().hex_len() {
            return id.to_hex();
        }
        self.resolver
            .unique_abbrev(id, len)
            .unwrap_or_else(|_| id.to_hex_with_len(len))
    }

    /// The branch `HEAD` points to when it is unborn.
    pub fn unborn_branch(&self) -> Option<String> {
        let head = self.refs.resolve("HEAD").ok()??;
        if head.oid.is_some() {
            return None;
        }
        Some(
            head.name
                .strip_prefix("refs/heads/")
                .unwrap_or(&head.name)
                .to_string(),
        )
    }

    /// The work tree has a file `arg` (then git would take it as a path).
    fn is_path(&self, arg: &str) -> bool {
        if self.setup.work_tree.is_none() {
            return false;
        }
        let prefix = self.setup.prefix.clone().unwrap_or_default();
        std::path::Path::new(&prefix)
            .join(arg)
            .symlink_metadata()
            .is_ok()
    }
}

/// The walk's view of the repository.
struct Commits<'r, 'a>(&'r Repo<'a>);

impl CommitSource for Commits<'_, '_> {
    type Error = ObjectId;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ObjectId> {
        let (commit, _) = self.0.read_commit(id).ok_or(*id)?;
        let time = commit
            .committer()
            .and_then(|c| c.time())
            .map_or(0, |t| t.seconds);
        Ok(CommitInfo {
            parents: self.0.parents(id, &commit),
            time,
        })
    }
}

fn print_diagnostics(resolver: &Resolver<'_>) {
    let mut err = std::io::stderr();
    for d in resolver.take_diagnostics() {
        let _ = writeln!(err, "{}", d.render());
    }
}

fn rev_error(resolver: &Resolver<'_>, e: RevError) -> Die {
    print_diagnostics(resolver);
    match e {
        RevError::Fatal(m) => Die::Fatal(m),
        RevError::Lookup(e) => {
            for line in &e.errors {
                let _ = writeln!(std::io::stderr(), "error: {line}");
            }
            match e.fatal {
                Some(m) => Die::Fatal(m),
                None => Die::Exit(rgit_options::EXIT_FATAL),
            }
        }
        RevError::NotFound => Die::Exit(rgit_options::EXIT_FATAL),
    }
}

/// The objects one revision argument stands for.
fn rev_items(repo: &Repo<'_>, arg: &str) -> Result<Vec<RevItem>, Die> {
    let resolver = repo.resolver;
    let flags = Flags::default().with_hint(Hint::Committish);
    let found = match resolver.try_range(arg, flags) {
        Ok(Some(items)) => Some(items),
        Ok(None) => resolver
            .try_parent_shorthand(arg, flags)
            .map_err(|e| rev_error(resolver, e))?,
        Err(e) => return Err(rev_error(resolver, e)),
    };
    if let Some(items) = found {
        print_diagnostics(resolver);
        return Ok(items);
    }
    let (name, negated) = match arg.strip_prefix('^') {
        Some(n) => (n, true),
        None => (arg, false),
    };
    match resolver.resolve_with(name, flags) {
        Ok(oid) => {
            print_diagnostics(resolver);
            Ok(vec![RevItem {
                oid,
                negated,
                name: Some(name.to_string()),
            }])
        }
        Err(RevError::NotFound) => {
            print_diagnostics(resolver);
            if negated {
                Err(Die::Fatal(msg::bad_revision(arg)))
            } else if repo.is_path(arg) {
                Err(Die::Fatal(msg::not_supported("path limiting")))
            } else {
                Err(Die::Fatal(msg::ambiguous_argument(arg)))
            }
        }
        Err(e) => Err(rev_error(resolver, e)),
    }
}

/// Resolve the starting points: `(commit, negative)` in command-line order.
fn resolve_tips(repo: &Repo<'_>, opts: &Options) -> Result<Vec<(ObjectId, bool)>, Die> {
    let mut out = Vec::new();
    for tip in &opts.tips {
        match tip {
            Tip::Rev { arg, negate } => {
                for item in rev_items(repo, arg)? {
                    if let Some(c) = repo.peel_to_commit(item.oid) {
                        out.push((c, item.negated != *negate));
                    }
                }
            }
            Tip::Refs { prefix, negative } => {
                let iter_prefix = if prefix.is_empty() { "refs/" } else { prefix };
                let mut ids = Vec::new();
                repo.refs
                    .for_each_ref(iter_prefix, &mut |_, id| ids.push(*id))
                    .map_err(|e| Die::Fatal(e.message()))?;
                if prefix.is_empty()
                    && let Ok(Some(head)) = repo.refs.resolve("HEAD")
                    && let Some(id) = head.oid
                {
                    ids.push(id);
                }
                for id in ids {
                    if let Some(c) = repo.peel_to_commit(id) {
                        out.push((c, *negative));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Set up the repository, parse `argv` and run `cmd`.
pub fn main(ctx: &Context, cmd: Command, argv: &[String]) -> Result<i32, Die> {
    let mut opts = Options::parse(cmd, argv)?;
    let setup = repo::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    let rev_opts = revisions::options(&config, kind)?;
    let shallow = std::env::var_os("GIT_SHALLOW_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| setup.common_dir.join("shallow"));
    let objects = Objects::new(revisions::objects_dir(&setup), kind, &shallow);
    let refs = Refs::new(&setup, kind);
    let cfg = Config(&config);
    let index = IndexFile::new(&setup, kind);
    let work_tree = setup.work_tree.clone().map(WorkTree);
    let rev_repo = revision::Repo {
        objects: &objects,
        refs: &refs,
        config: &cfg,
        index: Some(&index),
        work_tree: work_tree
            .as_ref()
            .map(|w| w as &dyn revision::WorkTreeLookup),
        work_tree_info: revisions::work_tree_info(&setup),
    };
    let resolver = Resolver::new(rev_repo, rev_opts);
    let repo = Repo {
        setup: &setup,
        config: &config,
        objects: &objects,
        refs: &refs,
        resolver: &resolver,
    };
    if opts.has_no_tips() {
        match cmd {
            Command::RevList => {
                let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
                return Err(Die::Exit(rgit_options::EXIT_USAGE));
            }
            Command::Log => {
                if let Some(branch) = repo.unborn_branch() {
                    return Err(Die::Fatal(msg::unborn_branch(&branch)));
                }
                opts.push_head();
            }
        }
    }
    run(&repo, cmd, &opts)
}

/// Run the walk and print the result.
fn run(repo: &Repo<'_>, cmd: Command, opts: &Options) -> Result<i32, Die> {
    let tips = resolve_tips(repo, opts)?;
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let mut walk = Walk::new(Commits(repo), opts.walk.clone());
    for (id, negative) in &tips {
        walk.push(id, *negative).map_err(walk_error)?;
    }
    let hexlen = repo.objects.hash_kind().hex_len();
    let abbrev_len = match opts.abbrev {
        Abbrev::Default => repo.resolver.default_abbrev(),
        Abbrev::Digits(n) => n.min(hexlen),
        Abbrev::Full => hexlen,
    };
    let name = |id: &ObjectId, abbreviate: bool| {
        if abbreviate {
            repo.abbrev(id, abbrev_len)
        } else {
            id.to_hex()
        }
    };
    let use_mailmap = opts.mailmap.unwrap_or_else(|| {
        cmd == Command::Log
            && repo
                .config
                .bool("log.mailmap")
                .ok()
                .flatten()
                .unwrap_or(true)
    });
    let style = Style {
        mailmap: use_mailmap.then(|| mailmap::Mailmap::load(repo)),
        pretty: opts.pretty.unwrap_or(Pretty::Medium),
        date_mode: opts.date_mode.clone(),
        date_ctx: DateContext::from_env(),
    };
    let stdout = std::io::stdout();
    let mut out = Vec::with_capacity(1 << 16);
    let mut first = true;
    let mut count = 0usize;
    while let Some(id) = walk.next_commit().map_err(walk_error)? {
        if opts.count {
            count += 1;
            continue;
        }
        match opts.pretty {
            None => {
                out.extend_from_slice(name(&id, opts.abbrev_commit).as_bytes());
                if opts.parents {
                    for p in walk.parents(&id).unwrap_or_default() {
                        out.push(b' ');
                        // rev-list abbreviates only the commit itself.
                        out.extend_from_slice(p.to_hex().as_bytes());
                    }
                }
                out.push(b'\n');
            }
            Some(_) => {
                let (commit, raw) = repo.read_commit(&id).ok_or_else(|| walk_error(id))?;
                let parents = repo.parents(&id, &commit);
                let shown: Vec<String> = if opts.parents {
                    parents
                        .iter()
                        .map(|p| name(p, opts.abbrev_commit))
                        .collect()
                } else {
                    Vec::new()
                };
                let merge: Vec<String> = if parents.len() > 1 {
                    parents.iter().map(|p| name(p, true)).collect()
                } else {
                    Vec::new()
                };
                let commit_name = name(&id, opts.abbrev_commit);
                let entry = Entry {
                    name: &commit_name,
                    shown_parents: &shown,
                    merge_parents: &merge,
                    commit: &commit,
                    raw: &raw,
                };
                pretty::write_entry(&mut out, &style, &entry, first);
                first = false;
            }
        }
        if out.len() > 1 << 15 {
            let _ = stdout.lock().write_all(&out);
            out.clear();
        }
    }
    if opts.count {
        out.extend_from_slice(format!("{count}\n").as_bytes());
    }
    let mut lock = stdout.lock();
    let _ = lock.write_all(&out);
    let _ = lock.flush();
    Ok(0)
}
