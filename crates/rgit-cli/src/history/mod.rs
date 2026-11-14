// SPDX-License-Identifier: MIT
//! Shared by `rev-list` and `log`: the revision and walk options
//! (Documentation/rev-list-options.adoc), the starting points, the walk
//! over the repository's object database and the output.
//!
//! The walk itself (orderings, limiting, boundary, sides, objects) lives in
//! `rgit_revwalk::walk`; this module wires it to the repository and prints.

mod decorate;
mod layout;
pub mod mailmap;
mod options;
pub mod pretty;
mod recode;
mod tips;
mod userformat;

use std::collections::HashMap;
use std::io::{IsTerminal, Read, Write};

use rgit_config::ConfigSet;
use rgit_date::DateContext;
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, TreeIter};
use rgit_repo::Setup;
use rgit_revwalk::revision::{self, ObjectLookup, RefLookup, Resolver};
use rgit_revwalk::walk::grep::CommitGrep;
use rgit_revwalk::walk::objects::{ItemKind, ObjectLister, TreeItem, TreeSource};
use rgit_revwalk::walk::{CommitInfo, CommitSource, Shown, Side, Walk};

use crate::messages::rev_list as msg;
use crate::repo::{self, Mode};
use crate::revisions::{self, Config, IndexFile, Objects, Refs, WorkTree};
use crate::{Context, Die};
use decorate::{Colors, Decorations, Parts};
pub use options::Options;
use options::{Abbrev, Decorate, ObjectsMode};
use pretty::{Entry, Pretty, Style};
use userformat::{BadColor, Item, Lookup, Settings};

/// The default `color.diff.commit`.
const YELLOW: &str = "\x1b[33m";
use tips::Resolved;

/// Which command parses the arguments (they differ in error handling and
/// defaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    RevList,
    Log,
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

    /// `HEAD` of every other work tree of the repository (`--all` includes
    /// them).
    fn other_worktree_heads(&self) -> Vec<ObjectId> {
        let common = self.setup.common_dir.as_path();
        let mut dirs = vec![common.to_path_buf()];
        if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
            let mut wt: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            wt.sort();
            dirs.extend(wt);
        }
        let own = std::path::absolute(&self.setup.git_dir).ok();
        let mut out = Vec::new();
        for dir in dirs {
            if std::path::absolute(&dir).ok() == own {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(dir.join("HEAD")) else {
                continue;
            };
            let text = text.trim_end();
            let id = match text.strip_prefix("ref: ") {
                Some(name) => self.refs.resolve(name).ok().flatten().and_then(|r| r.oid),
                None => ObjectId::from_hex(self.objects.hash_kind(), text.as_bytes()).ok(),
            };
            out.extend(id);
        }
        out
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

    /// The root tree of commit `id`.
    fn tree_of(&self, id: &ObjectId) -> Option<ObjectId> {
        self.read_commit(id)?.0.tree().ok()
    }
}

/// The walk's view of the repository, with the text filters.
struct Commits<'r, 'a> {
    repo: &'r Repo<'a>,
    grep: Option<CommitGrep>,
}

impl CommitSource for Commits<'_, '_> {
    type Error = ObjectId;
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ObjectId> {
        let (commit, _) = self.repo.read_commit(id).ok_or(*id)?;
        let time = commit
            .committer()
            .and_then(|c| c.time())
            .map_or(0, |t| t.seconds);
        Ok(CommitInfo {
            parents: self.repo.parents(id, &commit),
            time,
        })
    }

    fn author_time(&mut self, id: &ObjectId) -> Result<Option<i64>, ObjectId> {
        let (commit, _) = self.repo.read_commit(id).ok_or(*id)?;
        Ok(commit.author().and_then(|a| a.time()).map(|t| t.seconds))
    }

    fn include(&mut self, id: &ObjectId) -> Result<bool, ObjectId> {
        let Some(grep) = &self.grep else {
            return Ok(true);
        };
        let (_, raw) = self.repo.read_commit(id).ok_or(*id)?;
        Ok(grep.matches(&raw))
    }
}

/// Trees for `--objects`.
struct Trees<'r, 'a>(&'r Repo<'a>);

impl TreeSource for Trees<'_, '_> {
    type Error = ObjectId;
    fn tree(&mut self, id: &ObjectId) -> Result<Vec<TreeItem>, ObjectId> {
        let (kind, data) = self.0.objects.read(id).ok().flatten().ok_or(*id)?;
        if kind != ObjectKind::Tree {
            return Err(*id);
        }
        let mut out = Vec::new();
        for entry in TreeIter::new(self.0.objects.hash_kind(), &data) {
            let entry = entry.map_err(|_| *id)?;
            let kind = match entry.mode_bits & 0o170000 {
                0o040000 => ItemKind::Tree,
                0o160000 => ItemKind::Gitlink,
                _ => ItemKind::Blob,
            };
            out.push(TreeItem {
                kind,
                name: entry.name.to_vec(),
                id: entry.oid,
            });
        }
        Ok(out)
    }
}

/// Set up the repository, parse `argv` and run `cmd`.
pub fn main(ctx: &Context, cmd: Command, argv: &[String]) -> Result<i32, Die> {
    // `-h` and `--help-all` work outside a repository and print to stdout.
    if let Some(first @ ("-h" | "--help-all")) = argv.first().map(String::as_str) {
        let text = match (cmd, first) {
            (Command::RevList, _) => msg::REV_LIST_USAGE,
            (Command::Log, "-h") => msg::LOG_USAGE,
            (Command::Log, _) => msg::LOG_USAGE_ALL,
        };
        let _ = std::io::stdout().write_all(text.as_bytes());
        return Ok(0);
    }
    let mut opts = Options::parse(cmd, argv)?;
    let setup = repo::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    opts.apply_config(cmd, &config)?;
    let rev_opts = revisions::options(ctx, &config, kind)?;
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
    if opts.stdin {
        let mut input = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut input);
        opts.read_stdin(&input)?;
    }
    if opts.has_no_tips() && !opts.stdin {
        match cmd {
            Command::RevList => {
                let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
                return Err(Die::Exit(rgit_options::EXIT_USAGE));
            }
            Command::Log => {
                if let Some(branch) = repo.unborn_branch() {
                    crate::pager::setup_for(ctx, &config, "log", true)?;
                    return Err(Die::Fatal(msg::unborn_branch(&branch)));
                }
                opts.push_head();
            }
        }
    }
    run(ctx, &repo, cmd, &opts)
}

/// How commits are printed.
struct Printer<'r, 'a> {
    repo: &'r Repo<'a>,
    cmd: Command,
    opts: &'r Options,
    abbrev_len: usize,
    style: Style,
    /// The settings of user formats.
    user: Option<Settings>,
    decorations: Option<Decorations>,
    /// `--decorate` for the built-in formats.
    decorate: bool,
    /// Colours of decorations in built-in formats.
    deco_colors: Option<Colors>,
    notes: Option<ObjectId>,
    /// `%S`: the revision each commit was reached from (log only).
    sources: Option<HashMap<ObjectId, String>>,
    /// `i18n.logOutputEncoding` (else `i18n.commitEncoding`, UTF-8).
    encoding: Vec<u8>,
    out: Vec<u8>,
    first: bool,
}

/// The `reference` format.
const REFERENCE: &[u8] = b"%C(auto)%h (%s, %ad)";

/// Lookups of the user format placeholders.
struct FormatLookup<'p, 'r, 'a> {
    p: &'p Printer<'r, 'a>,
}

impl Lookup for FormatLookup<'_, '_, '_> {
    fn abbrev(&self, id: &ObjectId) -> String {
        self.p.repo.abbrev(id, self.p.abbrev_len)
    }

    fn decoration(&self, id: &ObjectId, parts: &Parts, colors: Option<&Colors>) -> Vec<u8> {
        match &self.p.decorations {
            Some(d) => d.format(id, parts, colors),
            None => Vec::new(),
        }
    }

    fn notes(&self, id: &ObjectId) -> Option<Vec<u8>> {
        let tree = self.p.notes?;
        note(self.p.repo, &tree, &id.to_hex())
    }
}

/// The note of `hex` in the notes tree `tree` (with fan-out directories).
fn note(repo: &Repo<'_>, tree: &ObjectId, hex: &str) -> Option<Vec<u8>> {
    let (kind, data) = repo.objects.read(tree).ok()??;
    if kind != ObjectKind::Tree {
        return None;
    }
    for entry in TreeIter::new(repo.objects.hash_kind(), &data) {
        let entry = entry.ok()?;
        let name = std::str::from_utf8(entry.name).ok()?;
        if name == hex {
            return match repo.objects.read(&entry.oid).ok()?? {
                (ObjectKind::Blob, blob) => Some(blob),
                _ => None,
            };
        }
        if entry.mode_bits & 0o170000 == 0o040000
            && name.len() == 2
            && let Some(rest) = hex.strip_prefix(name)
            && let Some(found) = note(repo, &entry.oid, rest)
        {
            return Some(found);
        }
    }
    None
}

/// The tree of the notes ref (`GIT_NOTES_REF`, `core.notesRef`, else
/// `refs/notes/commits`).
fn notes_tree(repo: &Repo<'_>) -> Option<ObjectId> {
    let name = std::env::var("GIT_NOTES_REF").ok().or_else(|| {
        repo.config
            .string("core.notesRef")
            .ok()
            .flatten()
            .map(|v| v.to_string())
    });
    let name = name.unwrap_or_else(|| "refs/notes/commits".to_string());
    let id = repo.refs.resolve(&name).ok()??.oid?;
    repo.tree_of(&id)
}

impl Printer<'_, '_> {
    fn name(&self, id: &ObjectId, abbreviate: bool) -> String {
        if abbreviate {
            self.repo.abbrev(id, self.abbrev_len)
        } else {
            id.to_hex()
        }
    }

    /// The mark before a commit name: `-` for boundary commits, `<`/`>`
    /// with `--left-right`.
    fn mark(&self, s: &Shown) -> &'static str {
        if s.boundary {
            "-"
        } else if self.opts.left_right {
            if s.side == Some(Side::Left) { "<" } else { ">" }
        } else {
            ""
        }
    }

    fn commit<S: CommitSource<Error = ObjectId>>(
        &mut self,
        walk: &Walk<S>,
        s: &Shown,
    ) -> Result<(), Die> {
        let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
        let id = s.id;
        let opts = self.opts;
        let mark = self.mark(s);
        let mut line = Vec::new();
        if opts.timestamp {
            line.extend_from_slice(format!("{} ", walk.time(&id).unwrap_or(0)).as_bytes());
        }
        let related: Vec<ObjectId> = if opts.parents {
            walk.parents(&id).unwrap_or_default().to_vec()
        } else if opts.children {
            walk.children(&id).to_vec()
        } else {
            Vec::new()
        };
        match opts.pretty {
            None => {
                line.extend_from_slice(mark.as_bytes());
                line.extend_from_slice(self.name(&id, opts.abbrev_commit).as_bytes());
                for p in &related {
                    line.push(b' ');
                    // rev-list abbreviates only the commit itself.
                    line.extend_from_slice(p.to_hex().as_bytes());
                }
                line.push(b'\n');
                self.out.extend_from_slice(&line);
                if opts.header {
                    let (_, raw) = self.repo.read_commit(&id).ok_or_else(|| walk_error(id))?;
                    write_header(&mut self.out, &raw);
                }
            }
            Some(Pretty::User { .. } | Pretty::Reference) => {
                let (commit, _) = self.read_shown(&id).ok_or_else(|| walk_error(id))?;
                self.user_entry(walk, s, &commit, &related, line)?;
            }
            Some(_) => {
                let (commit, raw) = self.read_shown(&id).ok_or_else(|| walk_error(id))?;
                let parents = self.repo.parents(&id, &commit);
                let shown: Vec<String> = related
                    .iter()
                    .map(|p| self.name(p, opts.abbrev_commit))
                    .collect();
                let merge: Vec<String> = if parents.len() > 1 {
                    parents.iter().map(|p| self.name(p, true)).collect()
                } else {
                    Vec::new()
                };
                let base = self.name(&id, opts.abbrev_commit);
                let commit_name = match (self.cmd, mark) {
                    (_, "") => base,
                    (Command::Log, m) => format!("{m} {base}"),
                    (Command::RevList, m) => format!("{m}{base}"),
                };
                let ts = String::from_utf8(line).unwrap_or_default();
                let commit_name = format!("{ts}{commit_name}");
                let decoration = match (&self.decorations, self.decorate) {
                    (Some(d), true) => d.format(&id, &Parts::default(), self.deco_colors.as_ref()),
                    _ => Vec::new(),
                };
                let entry = Entry {
                    name: &commit_name,
                    shown_parents: &shown,
                    merge_parents: &merge,
                    commit: &commit,
                    raw: &raw,
                    decoration: &decoration,
                };
                if self.cmd == Command::RevList {
                    // rev-list ends every multi-line entry with an empty line
                    // instead of separating them.
                    pretty::write_entry(&mut self.out, &self.style, &entry, true);
                    if self.style.pretty != Pretty::Oneline {
                        self.out.push(b'\n');
                    }
                } else {
                    pretty::write_entry(&mut self.out, &self.style, &entry, self.first);
                }
                self.first = false;
            }
        }
        self.flush_if_full();
        Ok(())
    }

    /// Read commit `id` for showing, converted to the output encoding.
    fn read_shown(&self, id: &ObjectId) -> Option<(Commit, Vec<u8>)> {
        let (commit, raw) = self.repo.read_commit(id)?;
        match recode::reencode(&raw, commit.encoding(), &self.encoding) {
            Some(data) => {
                let converted = Commit::parse(self.repo.objects.hash_kind(), &data).ok()?;
                Some((converted, raw))
            }
            None => Some((commit, raw)),
        }
    }

    /// One commit in a user format.
    fn user_entry<S: CommitSource<Error = ObjectId>>(
        &mut self,
        walk: &Walk<S>,
        s: &Shown,
        commit: &Commit,
        related: &[ObjectId],
        prefix: Vec<u8>,
    ) -> Result<(), Die> {
        let (fmt, terminator): (&[u8], bool) = match &self.style.pretty {
            Pretty::User { format, terminator } => (format, *terminator),
            _ => (REFERENCE, true),
        };
        let fmt = fmt.to_vec();
        let parents = match walk.parents(&s.id) {
            Some(p) => p.to_vec(),
            None => self.repo.parents(&s.id, commit),
        };
        let mark = if s.boundary {
            b'-'
        } else if s.side == Some(Side::Left) {
            b'<'
        } else {
            b'>'
        };
        let source = match &mut self.sources {
            Some(map) => {
                let source = map.get(&s.id).cloned();
                if let Some(src) = &source {
                    for p in &parents {
                        map.entry(*p).or_insert_with(|| src.clone());
                    }
                }
                source
            }
            None => None,
        };
        let item = Item {
            id: &s.id,
            commit,
            parents: &parents,
            mark,
            source: source.as_deref(),
        };
        let settings = self.user.as_ref().expect("user format settings");
        let expanded = userformat::expand(&fmt, settings, &item, &FormatLookup { p: self })
            .map_err(|BadColor(v)| {
                let _ = writeln!(
                    std::io::stderr(),
                    "error: {}",
                    crate::messages::config::invalid_color(&v)
                );
                Die::Fatal(msg::UNABLE_TO_PARSE_PRETTY.to_string())
            })?;
        match self.cmd {
            Command::RevList => {
                if self.opts.commit_header {
                    self.out.extend_from_slice(&prefix);
                    self.out.extend_from_slice(b"commit ");
                    self.out.extend_from_slice(self.mark(s).as_bytes());
                    let name = self.name(&s.id, self.opts.abbrev_commit);
                    self.out.extend_from_slice(name.as_bytes());
                    for p in related {
                        self.out.push(b' ');
                        // Parents are never abbreviated here.
                        self.out.extend_from_slice(p.to_hex().as_bytes());
                    }
                    self.out.push(b'\n');
                }
                if !expanded.is_empty() {
                    self.out.extend_from_slice(&expanded);
                    self.out.push(b'\n');
                }
            }
            Command::Log => {
                if fmt.is_empty() && terminator {
                    return Ok(());
                }
                let end = if self.opts.zero { 0 } else { b'\n' };
                if !terminator && !self.first {
                    self.out.push(end);
                }
                self.out.extend_from_slice(&prefix);
                self.out.extend_from_slice(&expanded);
                if terminator {
                    self.out.push(end);
                }
            }
        }
        self.first = false;
        Ok(())
    }

    fn object(&mut self, id: &ObjectId, name: &[u8]) {
        self.out.extend_from_slice(id.to_hex().as_bytes());
        if self.opts.object_names {
            self.out.push(b' ');
            // Names are cut at a newline.
            let end = name.iter().position(|&b| b == b'\n').unwrap_or(name.len());
            self.out.extend_from_slice(&name[..end]);
        }
        self.out.push(b'\n');
        self.flush_if_full();
    }

    fn flush_if_full(&mut self) {
        if self.out.len() > 1 << 15 {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if !self.opts.quiet {
            let mut lock = std::io::stdout().lock();
            let _ = lock.write_all(&self.out);
            let _ = lock.flush();
        }
        self.out.clear();
    }
}

/// `--header`: the raw commit with the message indented, ended by NUL.
fn write_header(out: &mut Vec<u8>, raw: &[u8]) {
    let (head, message) = match raw.windows(2).position(|w| w == b"\n\n") {
        Some(i) => (&raw[..=i], &raw[i + 2..]),
        None => (raw, &raw[raw.len()..]),
    };
    out.extend_from_slice(head);
    out.push(b'\n');
    let message = message.strip_suffix(b"\n").unwrap_or(message);
    if !message.is_empty() {
        for line in message.split(|&b| b == b'\n') {
            out.extend_from_slice(b"    ");
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    out.push(0);
}

/// Whether the built-in formats of `log` show decorations
/// (`--decorate`, `log.decorate`; `auto` decorates on a terminal).
fn want_decorate(config: &ConfigSet, opt: Option<Decorate>) -> bool {
    match opt {
        Some(Decorate::No) => false,
        Some(Decorate::Short | Decorate::Full) => true,
        // `[log] decorate` without a value is "true".
        None if config
            .get("log.decorate")
            .is_some_and(|e| e.value.is_none()) =>
        {
            true
        }
        Some(Decorate::Auto) | None => match config.string("log.decorate") {
            Ok(Some(v)) if v == "short" || v == "full" => true,
            Ok(Some(v)) if v != "auto" => {
                rgit_config::value::parse_bool(Some(v.as_ref())).unwrap_or(false)
            }
            _ => std::io::stdout().is_terminal(),
        },
    }
}

/// The settings of a user format.
fn user_settings(
    repo: &Repo<'_>,
    opts: &Options,
    pretty: &Pretty,
    cmd: Command,
) -> Result<Settings, Die> {
    let color = crate::color::want(repo.config, Some("color.diff"), opts.color)?;
    let colors = if color {
        let commit = crate::color::slot(repo.config, "color.diff.commit", YELLOW)?;
        Some(Colors::load(repo.config, commit)?)
    } else {
        None
    };
    let date_mode = if *pretty == Pretty::Reference && !opts.date_given {
        rgit_date::DateMode::new(rgit_date::DateStyle::Short)
    } else {
        opts.date_mode.clone()
    };
    let trailers = rgit_trailer::Config::from_config(repo.config)
        .map(|(c, _)| c)
        .unwrap_or_default();
    let columns = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80);
    Ok(Settings {
        mailmap: mailmap::Mailmap::load(repo),
        date_mode,
        date_ctx: DateContext::from_env(),
        color,
        colors,
        trailers,
        columns,
        notes: opts.notes && cmd == Command::Log,
        source: cmd == Command::Log,
    })
}

/// Run the walk and print the result.
fn run(ctx: &Context, repo: &Repo<'_>, cmd: Command, opts: &Options) -> Result<i32, Die> {
    let resolved = tips::resolve(repo, opts)?;
    let commit_color = match cmd {
        Command::Log => {
            crate::pager::setup_for(ctx, repo.config, "log", true)?;
            let on = crate::color::want(repo.config, Some("color.diff"), opts.color)?;
            if on {
                Some(crate::color::slot(
                    repo.config,
                    "color.diff.commit",
                    YELLOW,
                )?)
            } else {
                None
            }
        }
        Command::RevList => None,
    };
    if opts.walk.ancestry_path && opts.ancestry.is_empty() && !resolved.commits.iter().any(|c| c.1)
    {
        return Err(Die::Fatal(msg::NO_BOTTOM_COMMITS.to_string()));
    }
    let grep = if opts.grep.is_empty() {
        None
    } else {
        Some(CommitGrep::new(&opts.grep).map_err(Die::Fatal)?)
    };
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let mut walk = Walk::new(Commits { repo, grep }, opts.walk.clone());
    for (id, negative, side) in &resolved.commits {
        walk.push_side(id, *negative, *side).map_err(walk_error)?;
    }
    for arg in &opts.ancestry {
        let id = repo
            .resolver
            .resolve(arg)
            .and_then(|id| repo.resolver.peel_to_commit(&id))
            .map_err(|_| Die::Fatal(msg::bad_revision(arg)))?;
        walk.ancestry_target(&id).map_err(walk_error)?;
    }
    let hexlen = repo.objects.hash_kind().hex_len();
    let abbrev_len = match opts.abbrev {
        Abbrev::Default => repo.resolver.default_abbrev(),
        Abbrev::Digits(n) => n.min(hexlen),
        Abbrev::Full => hexlen,
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
    let pretty = opts.pretty.clone().unwrap_or(Pretty::Medium);
    let is_user = matches!(pretty, Pretty::User { .. } | Pretty::Reference);
    // `log.decorate` does not apply to the raw format, only `--decorate`.
    let decorate = cmd == Command::Log
        && !is_user
        && match (&pretty, opts.decorate) {
            (Pretty::Raw, None) => false,
            (_, d) => want_decorate(repo.config, d),
        };
    let mut filter = opts.decorate_filter.clone();
    filter.full = match opts.decorate {
        Some(Decorate::Full) => Some(true),
        Some(Decorate::Short) => Some(false),
        _ => None,
    };
    let decorations = (decorate || is_user).then(|| Decorations::load(repo, &filter));
    let deco_colors = match &commit_color {
        Some(c) if decorate => Some(Colors::load(repo.config, c.clone())?),
        _ => None,
    };
    let user = if is_user {
        Some(user_settings(repo, opts, &pretty, cmd)?)
    } else {
        None
    };
    let notes = if is_user { notes_tree(repo) } else { None };
    let style = Style {
        mailmap: use_mailmap.then(|| mailmap::Mailmap::load(repo)),
        pretty,
        date_mode: opts.date_mode.clone(),
        date_ctx: DateContext::from_env(),
        commit_color,
        zero: opts.zero,
    };
    let mut p = Printer {
        repo,
        cmd,
        opts,
        abbrev_len,
        style,
        user,
        decorations,
        decorate,
        deco_colors,
        notes,
        sources: None,
        encoding: recode::output_encoding(repo.config),
        out: Vec::with_capacity(1 << 16),
        first: true,
    };
    if is_user && cmd == Command::Log {
        let mut map = HashMap::new();
        for ((id, _, _), name) in resolved.commits.iter().zip(&resolved.sources) {
            map.entry(*id).or_insert_with(|| name.clone());
        }
        p.sources = Some(map);
    }
    if let Some(mode) = opts.objects {
        return run_objects(&mut p, &mut walk, &resolved, mode);
    }
    let (mut left, mut right) = (0usize, 0usize);
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        if opts.count {
            if s.side == Some(Side::Left) {
                left += 1;
            } else {
                right += 1;
            }
            continue;
        }
        p.commit(&walk, &s)?;
    }
    if opts.count {
        let text = if opts.left_right {
            format!("{left}\t{right}\n")
        } else {
            format!("{}\n", left + right)
        };
        p.out.extend_from_slice(text.as_bytes());
    }
    p.flush();
    Ok(0)
}

/// `--objects`: the commits, then the tags, trees and blobs they need.
fn run_objects(
    p: &mut Printer<'_, '_>,
    walk: &mut Walk<Commits<'_, '_>>,
    resolved: &Resolved,
    mode: ObjectsMode,
) -> Result<i32, Die> {
    let repo = p.repo;
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let mut shown = Vec::new();
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        shown.push(s);
    }
    let mut lister = ObjectLister::new(Trees(repo));
    let edges = walk.edges();
    for e in &edges {
        if let Some(tree) = repo.tree_of(e) {
            lister.exclude_tree(&tree).map_err(walk_error)?;
        }
    }
    for (id, kind) in &resolved.excluded {
        match kind {
            ObjectKind::Tree => lister.exclude_tree(id).map_err(walk_error)?,
            _ => lister.exclude(id),
        }
    }
    let mut count = 0usize;
    let counting = p.opts.count;
    if mode.edge && !counting {
        for e in &edges {
            p.out.push(b'-');
            p.out.extend_from_slice(e.to_hex().as_bytes());
            p.out.push(b'\n');
        }
    }
    let mut pending_done = false;
    let list_pending = |p: &mut Printer<'_, '_>,
                        lister: &mut ObjectLister<Trees<'_, '_>>,
                        count: &mut usize|
     -> Result<(), Die> {
        for obj in &resolved.pending {
            match obj.kind {
                ObjectKind::Tree => {
                    lister
                        .list_tree(&obj.id, &obj.name, &mut |id, name| {
                            *count += 1;
                            if !counting {
                                p.object(id, name);
                            }
                        })
                        .map_err(walk_error)?;
                }
                _ => {
                    if lister.take(&obj.id) {
                        *count += 1;
                        if !counting {
                            p.object(&obj.id, &obj.name);
                        }
                    }
                }
            }
        }
        Ok(())
    };
    let mut trees = Vec::new();
    for s in &shown {
        count += 1;
        if !counting {
            p.commit(walk, s)?;
        }
        if s.boundary {
            continue;
        }
        let tree = repo.tree_of(&s.id).ok_or_else(|| walk_error(s.id))?;
        if mode.in_commit_order {
            if !pending_done {
                list_pending(p, &mut lister, &mut count)?;
                pending_done = true;
            }
            lister
                .list_tree(&tree, b"", &mut |id, name| {
                    count += 1;
                    if !counting {
                        p.object(id, name);
                    }
                })
                .map_err(walk_error)?;
        } else {
            trees.push(tree);
        }
    }
    if !pending_done {
        list_pending(p, &mut lister, &mut count)?;
    }
    for tree in trees {
        lister
            .list_tree(&tree, b"", &mut |id, name| {
                count += 1;
                if !counting {
                    p.object(id, name);
                }
            })
            .map_err(walk_error)?;
    }
    if counting {
        p.out.extend_from_slice(format!("{count}\n").as_bytes());
    }
    p.flush();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::write_header;

    #[test]
    fn header_indents_every_message_line() {
        let mut out = Vec::new();
        write_header(&mut out, b"tree t\n\nsubj\n\n\tbody\n");
        assert_eq!(out, b"tree t\n\n    subj\n    \n    \tbody\n\0");
    }
}
