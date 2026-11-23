// SPDX-License-Identifier: MIT
//! Shared by `rev-list` and `log`: the revision and walk options
//! (Documentation/rev-list-options.adoc), the starting points, the walk
//! over the repository's object database and the output.
//!
//! The walk itself (orderings, limiting, boundary, sides, objects) lives in
//! `rgit_revwalk::walk`; this module wires it to the repository and prints.

mod decorate;
mod follow;
pub mod graph;
pub mod layout;
pub mod mailmap;
mod options;
mod paths;
pub mod pretty;
pub(crate) mod recode;
mod tips;
mod userformat;

use std::collections::HashMap;
use std::io::{IsTerminal, Read, Write};

use rgit_config::ConfigSet;
use rgit_date::DateContext;
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, TreeIter};
use rgit_repo::Setup;
use rgit_revwalk::bisect;
use rgit_revwalk::revision::{self, ObjectLookup, RefLookup, Resolver};
use rgit_revwalk::walk::grep::CommitGrep;
use rgit_revwalk::walk::objects::{ItemKind, ObjectLister, TreeItem, TreeSource};
use rgit_revwalk::walk::{CommitInfo, CommitSource, Shown, Side, Walk};

use crate::messages::rev_list as msg;
use crate::repo::{self, Mode};
use crate::revisions::{self, Config, IndexFile, Objects, Refs, WorkTree};
use crate::signing::verify::{self, Check};
use crate::{Context, Die};
use decorate::{Colors, Decorations, Parts};
pub use options::Options;
use options::{Abbrev, Decorate, MergeDiff, ObjectsMode};
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

    /// The root tree of commit `id`.
    fn tree_of(&self, id: &ObjectId) -> Option<ObjectId> {
        self.read_commit(id)?.0.tree().ok()
    }
}

/// The walk's view of the repository, with the text filters.
struct Commits<'r, 'a> {
    repo: &'r Repo<'a>,
    grep: Option<CommitGrep>,
    /// Patch ids for `--cherry-pick` and `--cherry-mark`.
    patch_ids: Option<crate::patch_id::CommitIds<'r>>,
    /// Path limiting and `--simplify-by-decoration`.
    paths: Option<paths::PathFilter>,
    /// `--author`/`--committer` match the identities mapped through this.
    mailmap: Option<mailmap::Mailmap>,
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
        Ok(match &self.mailmap {
            Some(mm) => grep.matches(&mm.rewrite_object(ObjectKind::Commit, &raw)),
            None => grep.matches(&raw),
        })
    }

    fn patch_equivalent(
        &mut self,
        left: &[ObjectId],
        right: &[ObjectId],
    ) -> Result<std::collections::HashSet<ObjectId>, ObjectId> {
        Ok(match &self.patch_ids {
            Some(ids) => ids.equivalent(left, right),
            None => std::collections::HashSet::new(),
        })
    }
    fn treesame(&mut self, id: &ObjectId, parent: Option<&ObjectId>) -> Result<bool, ObjectId> {
        match &mut self.paths {
            Some(p) => p.treesame(self.repo, id, parent),
            None => Ok(false),
        }
    }

    fn only_added(&mut self, id: &ObjectId, parent: &ObjectId) -> Result<bool, ObjectId> {
        match &mut self.paths {
            Some(p) => p.only_added(self.repo, id, parent),
            None => Ok(false),
        }
    }
}

/// `log --follow` (or `log.follow` with exactly one path): the filter.
fn follow_filter(
    ctx: &Context,
    repo: &Repo<'_>,
    cmd: Command,
    opts: &Options,
    paths: &[String],
) -> Result<Option<follow::Follow>, Die> {
    if cmd != Command::Log {
        return Ok(None);
    }
    let wanted = match opts.follow {
        Some(f) => f,
        None => paths.len() == 1 && repo.config.bool("log.follow").ok().flatten() == Some(true),
    };
    if !wanted {
        return Ok(None);
    }
    if paths.len() != 1 {
        return Err(Die::Fatal(msg::FOLLOW_ONE_PATHSPEC.to_string()));
    }
    let pathspec = paths::parse_pathspec(ctx, repo, paths)?;
    let Some(item) = pathspec.items().first() else {
        return Err(Die::Fatal(msg::FOLLOW_ONE_PATHSPEC.to_string()));
    };
    // Only `top` and `literal` magic can be followed.
    let unsupported: Vec<&str> = item
        .magic()
        .iter()
        .filter(|w| {
            !matches!(
                w,
                rgit_pathspec::MagicWord::Top | rgit_pathspec::MagicWord::Literal
            )
        })
        .map(|w| w.name())
        .collect();
    if !unsupported.is_empty() {
        if opts.follow.is_none() {
            // `log.follow` quietly does not apply.
            return Ok(None);
        }
        return Err(Die::Fatal(msg::follow_magic(&unsupported)));
    }
    let plain = !item.has_wildcard() && !item.is_exclude() && item.magic().is_empty();
    let path = item.pattern().to_vec();
    Ok(Some(follow::Follow::new(
        path,
        (!plain).then_some(pathspec.clone()),
    )))
}

/// Set up path limiting (`<paths>`, `--simplify-by-decoration`) for the
/// walk: the filter, and the walk options it needs.
fn path_filter(
    ctx: &Context,
    repo: &Repo<'_>,
    opts: &Options,
    paths: &[String],
    walk: &mut rgit_revwalk::walk::WalkOptions,
) -> Result<Option<paths::PathFilter>, Die> {
    if paths.is_empty() && !opts.simplify_by_decoration {
        return Ok(None);
    }
    let pathspec = paths::parse_pathspec(ctx, repo, paths)?;
    let decorations = opts.simplify_by_decoration.then(|| {
        // The same decorations `log` shows (`--decorate-refs*`,
        // `--clear-decorations`, `log.excludeDecoration`).
        Decorations::load(repo, &opts.decorate_filter)
    });
    walk.prune = true;
    walk.rewrite_parents |= opts.parents || opts.children || opts.graph;
    Ok(Some(paths::PathFilter::new(pathspec, decorations)))
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

/// `git show` (Documentation/git-show.adoc): `argv` without the command
/// name.
pub fn show_main(ctx: &Context, argv: &[String]) -> Result<i32, Die> {
    run_main(ctx, Command::Log, argv, true)
}

/// `git diff-tree` with `-v`, `--pretty` or `--format`: the given commits
/// shown like `show` does, but with plumbing diffs (`argv` as for `show`).
pub fn diff_tree_show(ctx: &Context, argv: &[String]) -> Result<i32, Die> {
    let mut opts = Options::parse(Command::Log, argv)?;
    opts.show = true;
    opts.plumbing = true;
    run_parsed(ctx, Command::Log, opts)
}

/// Set up the repository, parse `argv` and run `cmd`.
pub fn main(ctx: &Context, cmd: Command, argv: &[String]) -> Result<i32, Die> {
    run_main(ctx, cmd, argv, false)
}

fn run_main(ctx: &Context, cmd: Command, argv: &[String], show: bool) -> Result<i32, Die> {
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
    opts.show = show;
    run_parsed(ctx, cmd, opts)
}

fn run_parsed(ctx: &Context, cmd: Command, mut opts: Options) -> Result<i32, Die> {
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
    if opts.bisect {
        let terms = crate::commands::bisect::Terms::of(&setup);
        let (new, old) = (
            format!("refs/bisect/{}", terms.new),
            format!("refs/bisect/{}", terms.old),
        );
        let (mut has_new, mut has_old) = (false, false);
        let _ = refs.for_each_ref("refs/bisect/", &mut |name, _| {
            has_new |= name.starts_with(&new);
            has_old |= name.starts_with(&old);
        });
        opts.push_bisect_refs(&terms.new, &terms.old, has_new, has_old);
    }
    if opts.has_no_tips() && !opts.stdin && !opts.bisect {
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

/// The commits `cherry-pick` and `revert` work on, in the order they are
/// applied (git-cherry-pick(1)): the arguments are revision arguments of
/// `rev-list`; without a range or an excluded commit nothing is walked and
/// the commits keep their command-line order (`--no-walk=unsorted`),
/// otherwise the walk's output is applied oldest first (`oldest_first`,
/// cherry-pick) or newest first (revert).
///
/// Trees and blobs among the arguments come back in
/// [`SequencerCommits::other`] (name and kind).
pub fn sequencer_commits(
    ctx: &Context,
    argv: &[String],
    oldest_first: bool,
) -> Result<SequencerCommits, Die> {
    // A commit limit (`-<n>`, `-n <n>`, `--max-count`) asks for a walk.
    let mut args = Vec::new();
    let max_count = |a: &String| {
        a == "-n"
            || a.starts_with("--max-count")
            || (a.len() > 1 && a.starts_with('-') && a[1..].bytes().all(|b| b.is_ascii_digit()))
            || (a.starts_with("-n") && a[2..].bytes().all(|b| b.is_ascii_digit()) && a.len() > 2)
    };
    if !argv.iter().any(max_count) {
        args.push("--no-walk=unsorted".to_owned());
    }
    args.extend(argv.iter().cloned());
    let walked = walk_args(ctx, &args)?;
    let mut commits = walked.commits;
    if oldest_first && (walked.limited || !walked.unsorted) {
        commits.reverse();
    }
    Ok(SequencerCommits {
        commits,
        other: walked.other,
    })
}

/// The commits the `rev-list` arguments `args` select, in the order of
/// the walk (newest first unless the arguments say otherwise).
pub fn walk_commits(ctx: &Context, args: &[String]) -> Result<Vec<ObjectId>, Die> {
    Ok(walk_args(ctx, args)?.commits)
}

/// The commits `rev-list <args>` lists, in walk order, each with its
/// parents (for `bisect`).
pub fn walk_with_parents(
    ctx: &Context,
    args: &[String],
) -> Result<Vec<(ObjectId, Vec<ObjectId>)>, Die> {
    let walked = walk_args(ctx, args)?;
    Ok(walked.commits.into_iter().zip(walked.parents).collect())
}

/// The commits `rev-list <args>` lists, in walk order, each with its
/// `--cherry-mark` mark (an equivalent change on the other side).
pub fn walk_marked(ctx: &Context, args: &[String]) -> Result<Vec<(ObjectId, bool)>, Die> {
    let walked = walk_args(ctx, args)?;
    Ok(walked.commits.into_iter().zip(walked.same).collect())
}

/// The result of [`walk_args`].
struct Walked {
    commits: Vec<ObjectId>,
    /// The parents of each commit as the walk read them.
    parents: Vec<Vec<ObjectId>>,
    /// `--cherry-mark`: each commit has an equivalent on the other side.
    same: Vec<bool>,
    other: Vec<(Vec<u8>, ObjectKind)>,
    /// A negative commit limited the walk.
    limited: bool,
    /// `--no-walk=unsorted`.
    unsorted: bool,
}

/// Walk the commits `args` select; an argument that names nothing is a
/// "bad revision".
fn walk_args(ctx: &Context, args: &[String]) -> Result<Walked, Die> {
    let mut opts = Options::parse(Command::Log, args)?;
    let setup = repo::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    opts.apply_grep_config(&config);
    let rev_opts = revisions::options(ctx, &config, kind)?;
    let shallow = setup.common_dir.join("shallow");
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
    // An argument that names nothing is a "bad revision" here.
    if opts.stdin {
        let mut input = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut input);
        opts.read_stdin(&input)?;
    }
    let resolved = tips::resolve_with(&repo, &opts, false).map_err(|e| match e {
        Die::Fatal(m) => {
            let bad = opts.tips.iter().find_map(|t| match t {
                options::Tip::Rev { arg, .. }
                    if m == msg::ambiguous_argument(arg)
                        || m == msg::not_supported("path limiting") =>
                {
                    Some(arg.clone())
                }
                _ => None,
            });
            match bad {
                Some(arg) => Die::Fatal(msg::bad_revision(&arg)),
                None => Die::Fatal(m),
            }
        }
        e => e,
    })?;
    let other = resolved
        .pending
        .iter()
        .map(|p| (p.name.clone(), p.kind))
        .collect();
    let grep = if opts.grep.is_empty() {
        None
    } else {
        Some(CommitGrep::new(&opts.grep).map_err(Die::Fatal)?)
    };
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let limited = resolved.commits.iter().any(|c| c.1);
    let unsorted = opts.walk.no_walk == Some(rgit_revwalk::walk::NoWalk::Unsorted);
    let cherry_repo = if opts.walk.cherry_pick || opts.walk.cherry_mark {
        Some(crate::repository::Repo::open(ctx, Mode::Required)?)
    } else {
        None
    };
    let mut walk_opts = opts.walk.clone();
    // The parents link the selected commits (bisect counts through them).
    walk_opts.rewrite_parents = true;
    let paths = path_filter(ctx, &repo, &opts, &resolved.paths, &mut walk_opts)?;
    let mut walk = Walk::new(
        Commits {
            repo: &repo,
            grep,
            patch_ids: match &cherry_repo {
                Some(r) => Some(
                    crate::patch_id::CommitIds::new(r).with_pathspec(paths::parse_pathspec(
                        ctx,
                        &repo,
                        &resolved.paths,
                    )?),
                ),
                None => None,
            },
            paths,
            mailmap: None,
        },
        walk_opts,
    );
    for (id, negative, side) in &resolved.commits {
        walk.push_side(id, *negative, *side).map_err(walk_error)?;
    }
    let mut commits = Vec::new();
    let mut parents = Vec::new();
    let mut same = Vec::new();
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        commits.push(s.id);
        same.push(s.same);
        parents.push(
            walk.parents(&s.id)
                .map(<[ObjectId]>::to_vec)
                .unwrap_or_default(),
        );
    }
    Ok(Walked {
        commits,
        parents,
        same,
        other,
        limited,
        unsorted,
    })
}

/// One commit of [`walk_formatted`].
pub struct LogItem {
    /// The commit in the output encoding.
    pub commit: Commit,
    /// Each of the requested user formats expanded for this commit.
    pub expanded: Vec<Vec<u8>>,
}

/// The commits `log <args>` shows (from `HEAD` without revisions), each
/// with the user formats `formats` expanded, and the repository's
/// mailmap (`shortlog`).
pub fn walk_formatted(
    ctx: &Context,
    args: &[String],
    formats: &[Vec<u8>],
) -> Result<(Vec<LogItem>, mailmap::Mailmap), Die> {
    let mut opts = Options::parse(Command::Log, args)?;
    let setup = repo::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    opts.apply_config(Command::Log, &config)?;
    let rev_opts = revisions::options(ctx, &config, kind)?;
    let shallow = setup.common_dir.join("shallow");
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
        opts.push_head();
    }
    let resolved = tips::resolve(&repo, &opts)?;
    let grep = if opts.grep.is_empty() {
        None
    } else {
        Some(CommitGrep::new(&opts.grep).map_err(Die::Fatal)?)
    };
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let mut walk = Walk::new(
        Commits {
            repo: &repo,
            grep,
            patch_ids: None,
            paths: None,
            mailmap: None,
        },
        opts.walk.clone(),
    );
    for (id, negative, side) in &resolved.commits {
        walk.push_side(id, *negative, *side).map_err(walk_error)?;
    }
    let hexlen = repo.objects.hash_kind().hex_len();
    let abbrev_len = match opts.abbrev {
        Abbrev::Default => repo.resolver.default_abbrev(),
        Abbrev::Digits(n) => n.min(hexlen),
        Abbrev::Full => hexlen,
    };
    let pretty = Pretty::User {
        format: Vec::new(),
        terminator: true,
    };
    let user = if formats.is_empty() {
        None
    } else {
        Some(user_settings(&repo, &opts, &pretty, Command::Log)?)
    };
    let p = Printer {
        repo: &repo,
        cmd: Command::Log,
        opts: &opts,
        abbrev_len,
        style: Style {
            mailmap: None,
            pretty,
            date_mode: opts.date_mode.clone(),
            date_ctx: DateContext::from_env(),
            commit_color: None,
            zero: false,
        },
        decorations: user
            .as_ref()
            .map(|_| Decorations::load(&repo, &opts.decorate_filter)),
        user,
        decorate: false,
        deco_colors: None,
        notes: if formats.is_empty() {
            None
        } else {
            notes_tree(&repo)
        },
        sources: None,
        encoding: recode::output_encoding(repo.config),
        out: Vec::new(),
        first: true,
        dist: None,
        graph: None,
        ended_line: true,
        child_shown: std::collections::HashSet::new(),
        next: None,
        diff: None,
        store: None,
        usage: None,
        // `%G` placeholders check signatures.
        signing: if formats.iter().any(|f| f.windows(2).any(|w| w == b"%G")) {
            Some(crate::signing::Settings::from_config(repo.config)?)
        } else {
            None
        },
        show_signature: false,
        signature_colors: None,
        signature_cache: std::cell::RefCell::new(None),
    };
    let mut items = Vec::new();
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        let (commit, _) = p.read_shown(&s.id).ok_or_else(|| walk_error(s.id))?;
        let mut expanded = Vec::with_capacity(formats.len());
        if let Some(settings) = &p.user {
            let parents = match walk.parents(&s.id) {
                Some(found) => found.to_vec(),
                None => repo.parents(&s.id, &commit),
            };
            let mark = if s.boundary {
                b'-'
            } else if s.same {
                b'='
            } else if s.side == Some(Side::Left) {
                b'<'
            } else {
                b'>'
            };
            let item = Item {
                id: &s.id,
                commit: &commit,
                parents: &parents,
                mark,
                source: None,
                column: 0,
            };
            for fmt in formats {
                let text = userformat::expand(fmt, settings, &item, &FormatLookup { p: &p })
                    .map_err(|_| Die::Fatal(msg::UNABLE_TO_PARSE_PRETTY.to_string()))?;
                expanded.push(text);
            }
        }
        items.push(LogItem { commit, expanded });
    }
    Ok((items, mailmap::Mailmap::load(&repo)))
}

/// See [`sequencer_commits`].
pub struct SequencerCommits {
    pub commits: Vec<ObjectId>,
    pub other: Vec<(Vec<u8>, ObjectKind)>,
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
    /// `--bisect-all`: the distance shown with the decorations.
    dist: Option<usize>,
    /// `--graph`.
    graph: Option<graph::Graph>,
    /// The last entry ended with a newline (then the separator before the
    /// next one gets a line of the graph).
    ended_line: bool,
    /// The commit shown after the current one (for the graph).
    next: Option<graph::Next>,
    /// Diff output after every commit (`-p`, `--stat`, ...).
    diff: Option<LogDiff>,
    /// `--boundary`: parents of the commits shown so far (boundary
    /// commits are drawn with those of their parents).
    child_shown: std::collections::HashSet<ObjectId>,
    /// The object store, for `--unpacked` and `--disk-usage`.
    store: Option<crate::repo_objects::Repo>,
    /// `--disk-usage`: the bytes of the objects selected so far.
    usage: Option<u64>,
    /// The `gpg.*` settings, when signatures are shown or formatted.
    signing: Option<crate::signing::Settings>,
    /// `--show-signature` (`log.showSignature`).
    show_signature: bool,
    /// The colours of good and bad signatures (when colouring).
    signature_colors: Option<(String, String)>,
    /// The last signature check (`%G?` and friends ask repeatedly).
    signature_cache: std::cell::RefCell<Option<(ObjectId, std::rc::Rc<Check>)>>,
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

    fn signature(&self, id: &ObjectId, commit: &rgit_object::Commit) -> std::rc::Rc<Check> {
        self.p.signature_check(id, &commit.serialize())
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
    /// The signature check of commit `id` (raw object `raw`), cached for
    /// the placeholders of one commit.
    fn signature_check(&self, id: &ObjectId, raw: &[u8]) -> std::rc::Rc<Check> {
        if let Some((cached, c)) = &*self.signature_cache.borrow() {
            if cached == id {
                return c.clone();
            }
        }
        let check = match (
            &self.signing,
            verify::commit_signature(self.repo.objects.hash_kind(), raw),
        ) {
            (Some(settings), Some((payload, sig))) => verify::check(settings, &payload, &sig),
            _ => Check::default(),
        };
        let check = std::rc::Rc::new(check);
        *self.signature_cache.borrow_mut() = Some((*id, check.clone()));
        check
    }

    /// The `--show-signature` lines of commit `id`: the check of its own
    /// signature and of the signed tags it merged (`mergetag`).
    fn signature_block(&self, id: &ObjectId, commit: &rgit_object::Commit, raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.show_signature {
            return out;
        }
        let Some(settings) = &self.signing else {
            return out;
        };
        let min = settings.min_trust_or(crate::signing::Trust::Undefined);
        let color = |good: bool| {
            self.signature_colors
                .as_ref()
                .map(|(g, b)| if good { g.as_str() } else { b.as_str() })
        };
        for tag in commit.mergetags() {
            let mut data = tag.to_vec();
            if !data.ends_with(b"\n") {
                data.push(b'\n');
            }
            let name = rgit_object::Tag::parse(self.repo.objects.hash_kind(), &data)
                .ok()
                .and_then(|t| t.name().map(|n| String::from_utf8_lossy(n).into_owned()))
                .unwrap_or_default();
            let mut text = crate::messages::signing::merged_tag(&name).into_bytes();
            text.push(b'\n');
            let good = match verify::tag_signature(&data) {
                Some((payload, sig)) => {
                    let c = verify::check(settings, &payload, &sig);
                    if c.not_run {
                        text.extend_from_slice(
                            crate::messages::signing::NO_SIGNATURE_SHOWN.as_bytes(),
                        );
                        text.push(b'\n');
                    }
                    text.extend_from_slice(&c.output);
                    c.good(min)
                }
                None => false,
            };
            verify::show_lines(&text, color(good), &mut out);
        }
        if verify::commit_signature(self.repo.objects.hash_kind(), raw).is_some() {
            let c = self.signature_check(id, raw);
            verify::show_lines(&c.output, color(c.good(min)), &mut out);
        }
        out
    }

    fn name(&self, id: &ObjectId, abbreviate: bool) -> String {
        if abbreviate {
            self.repo.abbrev(id, self.abbrev_len)
        } else {
            id.to_hex()
        }
    }

    /// The mark before a commit name: `-` for boundary commits, `<`/`>`
    /// with `--left-right`, `=`/`+` with `--cherry-mark`.
    fn mark(&self, s: &Shown) -> &'static str {
        if s.boundary {
            "-"
        } else if self.opts.walk.cherry_mark && s.same {
            "="
        } else if self.opts.left_right {
            if s.side == Some(Side::Left) { "<" } else { ">" }
        } else if self.opts.walk.cherry_mark {
            "+"
        } else {
            ""
        }
    }

    /// `--unpacked` leaves out `id`, or `--disk-usage` counts it instead
    /// of showing it.
    fn diverted(&mut self, id: &ObjectId) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        if self.opts.unpacked
            && matches!(
                store.locate(id),
                Some(crate::repo_objects::Location::Packed(..))
            )
        {
            return true;
        }
        if let Some(total) = &mut self.usage {
            *total += store.disk_size(id);
            return true;
        }
        false
    }

    /// `--disk-usage`: the total, in bytes or (`=human`) larger units.
    fn finish_usage(&mut self) {
        let Some(total) = self.usage else {
            return;
        };
        let text = if self.opts.disk_usage == Some(true) {
            human_size(total)
        } else {
            total.to_string()
        };
        self.out.extend_from_slice(text.as_bytes());
        self.out.push(b'\n');
    }

    fn commit<S: CommitSource<Error = ObjectId>>(
        &mut self,
        walk: &Walk<S>,
        s: &Shown,
    ) -> Result<(), Die> {
        let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
        let id = s.id;
        if !s.boundary && self.diverted(&id) {
            return Ok(());
        }
        let opts = self.opts;
        let was_first = self.first;
        let start = self.out.len();
        if self.graph.is_some() {
            let parents = self.graph_parents(walk, &id);
            let mark = self.graph_mark(s);
            if let Some(g) = &mut self.graph {
                g.update(id, parents, mark.as_bytes(), self.next);
            }
        }
        let mark = if self.graph.is_some() {
            ""
        } else {
            self.mark(s)
        };
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
            None if self.cmd == Command::RevList && opts.zero => {
                // `-z`: NUL-terminated fields; the boundary mark is a field
                // of its own after the name (the parents follow it).
                line.extend_from_slice(self.name(&id, opts.abbrev_commit).as_bytes());
                if s.boundary {
                    line.extend_from_slice(b"\0boundary=yes");
                }
                for p in &related {
                    line.push(b' ');
                    line.extend_from_slice(p.to_hex().as_bytes());
                }
                line.push(0);
                self.out.extend_from_slice(&line);
            }
            None => {
                line.extend_from_slice(mark.as_bytes());
                line.extend_from_slice(self.name(&id, opts.abbrev_commit).as_bytes());
                if let Some(d) = self.dist_decoration(&id) {
                    line.extend_from_slice(&d);
                }
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
                    _ if self.dist.is_some() => self.dist_decoration(&id).unwrap_or_default(),
                    (Some(d), true) => d.format(&id, &Parts::default(), self.deco_colors.as_ref()),
                    _ => Vec::new(),
                };
                let signature = self.signature_block(&id, &commit, &raw);
                let entry = Entry {
                    name: &commit_name,
                    shown_parents: &shown,
                    merge_parents: &merge,
                    commit: &commit,
                    raw: &raw,
                    decoration: &decoration,
                    signature: &signature,
                };
                if self.cmd == Command::RevList {
                    // rev-list ends every multi-line entry with an empty line
                    // instead of separating them.
                    pretty::write_entry(&mut self.out, &self.style, &entry, true);
                    if self.style.pretty != Pretty::Oneline {
                        self.out.push(b'\n');
                    }
                } else {
                    let first = self.first || self.graph.is_some();
                    pretty::write_entry(&mut self.out, &self.style, &entry, first);
                }
                self.first = false;
            }
        }
        let diff = self.commit_diff(&id)?;
        if self.graph.is_some() {
            let mut text = self.out.split_off(start);
            // Entries are separated (multi-line formats, `format:`) or
            // terminated (one-line formats, `tformat:`); the terminator is
            // written after the graph.
            let (separated, terminated) = match &self.style.pretty {
                _ if self.cmd == Command::RevList || opts.pretty.is_none() => {
                    text.pop();
                    (false, true)
                }
                Pretty::Oneline => {
                    text.pop();
                    (false, true)
                }
                Pretty::Reference => (false, true),
                Pretty::User { terminator, .. } => (!terminator, *terminator),
                _ => (true, false),
            };
            self.emit_graph(&text, separated && !was_first, terminated, &diff);
        } else {
            self.out.extend_from_slice(&diff);
        }
        self.flush_if_full();
        Ok(())
    }

    /// The parents of `id` that the graph draws: those that are shown
    /// (all of them with `--boundary`, only the first with
    /// `--first-parent`).
    fn graph_parents<S: CommitSource<Error = ObjectId>>(
        &self,
        walk: &Walk<S>,
        id: &ObjectId,
    ) -> Vec<ObjectId> {
        let all = match walk.parents(id) {
            Some(p) => p.to_vec(),
            None => Vec::new(),
        };
        // With `--boundary` the excluded parents of a shown commit are
        // shown as boundary commits. The parents of an excluded (boundary)
        // commit are excluded too; they are drawn when they are shown as
        // boundary commits.
        let excluded = walk.is_uninteresting(id);
        let boundary = walk.options().boundary;
        let shown = |p: &ObjectId| {
            if excluded {
                boundary && self.child_shown.contains(p)
            } else {
                boundary || !walk.is_uninteresting(p)
            }
        };
        let mut out: Vec<ObjectId> = Vec::new();
        for p in all {
            if shown(&p) && !out.contains(&p) && self.graph_shows(&p) {
                out.push(p);
            }
            if self.opts.walk.first_parent {
                break;
            }
        }
        out
    }

    /// Commit `id` passes the filters of the walk (`--grep`, `--author`,
    /// `--merges`, ...), so it is shown when it is reached.
    fn graph_shows(&self, id: &ObjectId) -> bool {
        let w = &self.opts.walk;
        let filtered =
            !self.opts.grep.is_empty() || w.min_parents.is_some() || w.max_parents.is_some();
        if !filtered {
            return true;
        }
        let Some((commit, raw)) = self.repo.read_commit(id) else {
            return true;
        };
        let n = self.repo.parents(id, &commit).len();
        if w.min_parents.is_some_and(|m| n < m) || w.max_parents.is_some_and(|m| n > m) {
            return false;
        }
        if self.opts.grep.is_empty() {
            return true;
        }
        CommitGrep::new(&self.opts.grep).is_ok_and(|g| g.matches(&raw))
    }

    /// The diff output after commit `id`, with the line that separates it
    /// from the message (`---` when a stat comes before the patch).
    fn commit_diff(&self, id: &ObjectId) -> Result<Vec<u8>, Die> {
        let Some(d) = &self.diff else {
            return Ok(Vec::new());
        };
        let walk_error = || Die::Fatal(msg::bad_object(&id.to_hex()));
        let (commit, _) = self.repo.read_commit(id).ok_or_else(walk_error)?;
        let parents = self.repo.parents(id, &commit);
        let tree = commit.tree().map_err(|_| walk_error())?;
        let old = match parents.len() {
            0 if !d.show_root => return Ok(Vec::new()),
            0 => None,
            1 => Some(parents[0]),
            _ => match d.merges {
                MergeDiff::Off => return Ok(Vec::new()),
                MergeDiff::FirstParent => Some(parents[0]),
                MergeDiff::Combined | MergeDiff::Dense if d.formats.patch || d.formats.raw => {
                    return self.combined_diff(d, &parents, &tree, walk_error);
                }
                // Other formats compare with the first parent.
                MergeDiff::Combined | MergeDiff::Dense => Some(parents[0]),
                MergeDiff::Separate => {
                    return Err(Die::Fatal(msg::not_supported("-m")));
                }
            },
        };
        let old_tree = match old {
            Some(p) => Some(self.repo.tree_of(&p).ok_or_else(walk_error)?),
            None => None,
        };
        // diff-tree recurses only when asked to (or for contents).
        let plumbing = self.opts.plumbing;
        let recursive = !plumbing
            || d.opts.recursive
            || d.opts.show_trees
            || d.formats.patch
            || d.formats.summary
            || d.formats.needs_content();
        let mut w = crate::diff::collect::TreeDiff {
            repo: &d.repo,
            pathspec: &d.pathspec,
            recursive,
            show_trees: plumbing && d.opts.show_trees,
            max_depth: None,
            pairs: Vec::new(),
        };
        w.walk(old_tree.as_ref(), Some(&tree), b"")?;
        let mut out = crate::diff::output::Output::new(
            Some(&d.repo),
            d.repo.hash,
            &d.opts,
            d.formats,
            !plumbing,
        )?;
        out.set_color(&d.opts)?;
        let (bytes, _) = out.render(std::mem::take(&mut w.pairs))?;
        if bytes.is_empty() {
            return Ok(bytes);
        }
        let mut text = Vec::new();
        if self.style.pretty == Pretty::Oneline && self.cmd != Command::RevList {
            // Right after the one-line entry.
        } else if d.formats.stat && d.formats.patch {
            text.extend_from_slice(b"---\n");
        } else {
            text.push(b'\n');
        }
        text.extend_from_slice(&bytes);
        Ok(text)
    }

    /// The combined diff (`-c`, `--cc`) of a merge with `parents` and
    /// tree `tree`: raw lines and/or the combined patch.
    fn combined_diff(
        &self,
        d: &LogDiff,
        parents: &[ObjectId],
        tree: &ObjectId,
        walk_error: impl Fn() -> Die,
    ) -> Result<Vec<u8>, Die> {
        let mut trees = Vec::new();
        for p in parents {
            trees.push(self.repo.tree_of(p).ok_or_else(&walk_error)?);
        }
        let renames =
            d.formats.patch && !matches!(d.opts.detect, Some(rgit_diff::diffcore::Detect::Off));
        let files: Vec<_> = crate::diff::combined::collect(&d.repo, &trees, tree, renames)
            .into_iter()
            .filter(|f| d.pathspec.is_empty() || d.pathspec.matches(&f.path))
            .collect();
        let quote = d.repo.quote_path();
        let mut bytes = Vec::new();
        if d.formats.raw {
            // log and show abbreviate raw ids unless --full-index.
            let abbrev = match d.opts.abbrev {
                _ if d.opts.full_index => None,
                // diff-tree: full ids unless --abbrev.
                None if self.opts.plumbing => None,
                Some(Some(n)) => Some(n),
                _ => Some(d.repo.default_abbrev()),
            };
            crate::diff::combined::raw(&d.repo, &files, abbrev, quote, &mut bytes);
        }
        if d.formats.patch {
            if d.formats.raw && !bytes.is_empty() {
                bytes.push(b'\n');
            }
            let diff = rgit_diff::DiffOptions::default();
            let opts = crate::diff::combined::PatchOptions {
                dense: d.merges == MergeDiff::Dense,
                abbrev: d.repo.default_abbrev(),
                quote,
                diff: &diff,
            };
            crate::diff::combined::patch(&d.repo, &files, &opts, &mut bytes);
        }
        let mut text = Vec::new();
        if self.style.pretty == Pretty::Oneline && self.cmd != Command::RevList && !bytes.is_empty()
        {
            // Right after the one-line entry.
        } else {
            // A merge without differences still gets the separator.
            text.push(b'\n');
        }
        text.extend_from_slice(&bytes);
        Ok(text)
    }

    /// The character of a commit in the graph.
    fn graph_mark(&self, s: &Shown) -> &'static str {
        match self.mark(s) {
            "-" => "o",
            "" => "*",
            m => m,
        }
    }

    /// Write the text of one commit with the graph to its left: the lines
    /// of the drawing up to the commit's line, then every text line after
    /// the next line of the drawing, then the rest of the drawing.
    /// `separated`: entries are separated (not terminated), and this is
    /// not the first.
    fn emit_graph(&mut self, text: &[u8], separated: bool, terminated: bool, diff: &[u8]) {
        let Some(g) = self.graph.as_mut() else {
            return;
        };
        let end = if self.opts.zero { 0 } else { b'\n' };
        if separated {
            if self.ended_line && end == b'\n' {
                self.out.extend_from_slice(&g.padding_line());
            }
            self.out.push(end);
        }
        let (before, commit_line) = g.lines_to_commit();
        for l in before {
            self.out.extend_from_slice(&l);
            self.out.push(b'\n');
        }
        self.out.extend_from_slice(&commit_line);
        for (i, seg) in text.split_inclusive(|&b| b == b'\n').enumerate() {
            if i > 0 {
                self.out.extend_from_slice(&g.next_line());
            }
            self.out.extend_from_slice(seg);
        }
        let ended = text.is_empty() || text.ends_with(b"\n");
        while !g.finished() {
            if ended {
                self.out.extend_from_slice(&g.next_line());
                self.out.push(b'\n');
            } else {
                self.out.push(b'\n');
                self.out.extend_from_slice(&g.next_line());
            }
        }
        if terminated {
            if ended && end == b'\n' {
                self.out.extend_from_slice(&g.padding_line());
            }
            self.out.push(end);
        }
        for line in diff.split_inclusive(|&b| b == b'\n') {
            self.out.extend_from_slice(&g.next_line());
            self.out.extend_from_slice(line);
        }
        if !diff.is_empty() {
            self.ended_line = diff.ends_with(b"\n");
            return;
        }
        self.ended_line = ended;
    }

    /// `--bisect-all`: the decorations of `id` followed by its distance,
    /// ` (tag: v1, dist=2)`.
    fn dist_decoration(&self, id: &ObjectId) -> Option<Vec<u8>> {
        let dist = self.dist?;
        let parts = Parts {
            suffix: Vec::new(),
            ..Parts::default()
        };
        let mut out = match &self.decorations {
            Some(d) => d.format(id, &parts, None),
            None => Vec::new(),
        };
        if out.is_empty() {
            out.extend_from_slice(b" (");
        } else {
            out.extend_from_slice(b", ");
        }
        out.extend_from_slice(format!("dist={dist})").as_bytes());
        Some(out)
    }

    /// Read commit `id` for showing, converted to the output encoding.
    fn read_shown(&self, id: &ObjectId) -> Option<(Commit, Vec<u8>)> {
        let (commit, raw) = self.repo.read_commit(id)?;
        let user_format = matches!(self.style.pretty, Pretty::User { .. });
        let converted = if user_format {
            recode::reencode_for_format(&raw, commit.encoding(), &self.encoding)
        } else {
            recode::reencode(&raw, commit.encoding(), &self.encoding)
        };
        match converted {
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
        } else if s.same {
            b'='
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
            // The graph's part of the commit line counts for `%>|(...)`.
            column: layout::width(&prefix)
                + self
                    .graph
                    .as_ref()
                    .map_or(0, |g| layout::width(&g.clone().lines_to_commit().1)),
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
                if !terminator && !self.first && self.graph.is_none() {
                    self.out.push(end);
                }
                if self.show_signature {
                    let block = self.signature_block(&s.id, commit, &commit.serialize());
                    self.out.extend_from_slice(&block);
                }
                self.out.extend_from_slice(&prefix);
                self.out.extend_from_slice(&expanded);
                if terminator && self.graph.is_none() {
                    self.out.push(end);
                }
            }
        }
        self.first = false;
        Ok(())
    }

    fn object(&mut self, id: &ObjectId, name: &[u8]) {
        if self.diverted(id) {
            return;
        }
        self.out.extend_from_slice(id.to_hex().as_bytes());
        if self.cmd == Command::RevList && self.opts.zero {
            self.out.push(0);
            if self.opts.object_names && !name.is_empty() {
                self.out.extend_from_slice(b"path=");
                self.out.extend_from_slice(name);
                self.out.push(0);
            }
            self.flush_if_full();
            return;
        }
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

/// `git show` of single objects, in command-line order: commits as in
/// `log` (each at most once), annotated tags with their header and
/// message followed by the tagged object, trees as `tree <name>` and the
/// names of their entries, blobs as they are.
fn show_objects(p: &mut Printer<'_, '_>) -> Result<i32, Die> {
    let repo = p.repo;
    let opts = p.opts;
    let mut shown = std::collections::HashSet::new();
    let args: Vec<String> = opts
        .tips
        .iter()
        .filter_map(|t| match t {
            options::Tip::Rev { arg, .. } => Some(arg.clone()),
            options::Tip::Refs { .. } => None,
        })
        .collect();
    for arg in &args {
        let mut id = repo
            .resolver
            .resolve(arg)
            .map_err(|_| Die::Fatal(msg::bad_revision(arg)))?;
        let mut name = arg.clone();
        loop {
            let bad = || Die::Fatal(msg::bad_object(&id.to_hex()));
            let (kind, data) = repo.objects.read(&id).ok().flatten().ok_or_else(bad)?;
            match kind {
                ObjectKind::Commit => {
                    if shown.insert(id) {
                        show_commit(p, &id)?;
                    }
                }
                ObjectKind::Tag => {
                    let tag = rgit_object::Tag::parse(repo.objects.hash_kind(), &data)
                        .map_err(|_| bad())?;
                    if !p.first {
                        p.out.push(b'\n');
                    }
                    p.first = false;
                    show_tag_header(p, &tag);
                    let target = tag.object().map_err(|_| bad())?;
                    name = target.to_hex();
                    id = target;
                    continue;
                }
                ObjectKind::Tree => {
                    if !p.first {
                        p.out.push(b'\n');
                    }
                    p.first = false;
                    p.out
                        .extend_from_slice(format!("tree {name}\n\n").as_bytes());
                    for entry in TreeIter::new(repo.objects.hash_kind(), &data) {
                        let entry = entry.map_err(|_| bad())?;
                        p.out.extend_from_slice(entry.name);
                        if entry.mode_bits & 0o170000 == 0o040000 {
                            p.out.push(b'/');
                        }
                        p.out.push(b'\n');
                    }
                }
                ObjectKind::Blob => p.out.extend_from_slice(&data),
            }
            break;
        }
        p.flush_if_full();
    }
    p.flush();
    Ok(0)
}

/// One commit of `show`.
fn show_commit(p: &mut Printer<'_, '_>, id: &ObjectId) -> Result<(), Die> {
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let opts = rgit_revwalk::walk::WalkOptions {
        no_walk: Some(rgit_revwalk::walk::NoWalk::Unsorted),
        ..Default::default()
    };
    let commits = Commits {
        repo: p.repo,
        grep: None,
        patch_ids: None,
        paths: None,
        mailmap: None,
    };
    let mut walk = Walk::new(commits, opts);
    walk.push(id, false).map_err(walk_error)?;
    if let Some(s) = walk.next_shown().map_err(walk_error)? {
        p.commit(&walk, &s)?;
    }
    Ok(())
}

/// `tag <name>`, the tagger (and date) as the format shows them, an empty
/// line and the message.
fn show_tag_header(p: &mut Printer<'_, '_>, tag: &rgit_object::Tag) {
    let out = &mut p.out;
    out.extend_from_slice(b"tag ");
    out.extend_from_slice(tag.name().unwrap_or_default());
    out.push(b'\n');
    let pretty = &p.style.pretty;
    if *pretty != Pretty::Oneline
        && let Some(tagger) = tag.tagger()
    {
        let fuller = *pretty == Pretty::Fuller;
        out.extend_from_slice(if fuller { b"Tagger:     " } else { b"Tagger: " });
        out.extend_from_slice(&pretty::ident(&tagger, p.style.mailmap.as_ref()));
        out.push(b'\n');
        let date = || pretty::date_of(&tagger, &p.style.date_mode, &p.style.date_ctx);
        match pretty {
            Pretty::Medium => {
                out.extend_from_slice(b"Date:   ");
                out.extend_from_slice(date().as_bytes());
                out.push(b'\n');
            }
            Pretty::Fuller => {
                out.extend_from_slice(b"TaggerDate: ");
                out.extend_from_slice(date().as_bytes());
                out.push(b'\n');
            }
            _ => {}
        }
    }
    out.push(b'\n');
    out.extend_from_slice(tag.message());
}

/// The diff settings of `log` and `show`.
struct LogDiff {
    repo: crate::repository::Repo,
    opts: crate::diff::options::DiffOpts,
    formats: crate::diff::options::Formats,
    merges: MergeDiff,
    /// Root commits get a diff against the empty tree.
    show_root: bool,
    /// The paths the diffs are limited to (the walk's, unless
    /// `--full-diff`).
    pathspec: rgit_pathspec::Pathspec,
}

/// The diff `log` shows after every commit: only when a diff format was
/// asked for; merges get none unless `--diff-merges` (or `--first-parent`)
/// says so.
fn log_diff(ctx: &Context, opts: &Options, cmd: Command) -> Result<Option<LogDiff>, Die> {
    if cmd != Command::Log {
        return Ok(None);
    }
    if let Some((regex, _)) = &opts.diff.pickaxe {
        // Limiting the commits by their diff is not there yet.
        return Err(Die::Fatal(msg::not_supported(if *regex {
            "-G"
        } else {
            "-S"
        })));
    }
    let mut d = opts.diff.clone();
    // `-c` and `--cc` imply `-p`.
    let combined = matches!(opts.merges, Some(MergeDiff::Combined | MergeDiff::Dense));
    if (opts.show || combined) && !d.no_output {
        // `show` defaults to a patch.
        let none = !(d.raw
            || d.patch
            || d.name_only
            || d.name_status
            || d.numstat
            || d.stat
            || d.shortstat
            || d.summary);
        d.patch |= none;
    }
    let d = &d;
    let asked = d.raw
        || d.patch
        || d.name_only
        || d.name_status
        || d.numstat
        || d.stat
        || d.shortstat
        || d.summary;
    if !asked {
        return Ok(None);
    }
    let formats = d.formats(false)?;
    if !formats.any() {
        return Ok(None);
    }
    let mut diff_opts = d.clone();
    if diff_opts.color.is_none() {
        diff_opts.color = opts.color;
    }
    let mut repo = crate::repository::Repo::open(ctx, Mode::Required)?;
    diff_opts.resolve_objects(&mut repo)?;
    let merges = opts.merges.unwrap_or(if opts.walk.first_parent {
        MergeDiff::FirstParent
    } else if opts.show {
        MergeDiff::Dense
    } else {
        MergeDiff::Off
    });
    // diff-tree picks the commits to show itself.
    let show_root = opts.root
        || opts.plumbing
        || repo
            .config
            .bool("log.showRoot")
            .ok()
            .flatten()
            .unwrap_or(true);
    Ok(Some(LogDiff {
        repo,
        opts: diff_opts,
        formats,
        merges,
        show_root,
        pathspec: rgit_pathspec::Pathspec::default(),
    }))
}

/// The lane colours of `--graph` (`log.graphColors`, comma-separated
/// colours; invalid ones are skipped with a warning).
fn graph_colors(config: &ConfigSet) -> Vec<String> {
    let Ok(Some(value)) = config.string("log.graphColors") else {
        return graph::default_colors();
    };
    let value = value.to_string();
    let mut out = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        match rgit_config::color::parse_color(part) {
            Some(c) => out.push(c),
            None => {
                let _ = writeln!(
                    std::io::stderr(),
                    "warning: {}",
                    msg::invalid_graph_color(part)
                );
            }
        }
    }
    if out.is_empty() {
        return graph::default_colors();
    }
    out
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
    let mut resolved = tips::resolve(repo, opts)?;
    if resolved.no_revisions && !opts.stdin && !opts.bisect {
        // Only paths: like no arguments at all.
        match cmd {
            Command::RevList => {
                let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
                return Err(Die::Exit(rgit_options::EXIT_USAGE));
            }
            Command::Log => {
                if let Some(branch) = repo.unborn_branch() {
                    crate::pager::setup_for(ctx, repo.config, "log", true)?;
                    return Err(Die::Fatal(msg::unborn_branch(&branch)));
                }
                resolved.push_head(repo)?;
            }
        }
    }
    if cmd == Command::RevList
        && opts.only_alternate_refs()
        && resolved.commits.is_empty()
        && resolved.pending.is_empty()
    {
        let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
        return Err(Die::Exit(rgit_options::EXIT_USAGE));
    }
    let commit_color = match cmd {
        Command::Log => {
            let pager = if opts.show { "show" } else { "log" };
            crate::pager::setup_for(ctx, repo.config, pager, true)?;
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
    let bisecting = cmd == Command::RevList && (opts.bisect || opts.bisect_vars || opts.bisect_all);
    let mut walk_opts = opts.walk.clone();
    if bisecting {
        // The whole selection counts, whatever `-n` or `--reverse` say.
        walk_opts.max_count = None;
        walk_opts.skip = 0;
        walk_opts.reverse = false;
        // The weights count through the rewritten parents.
        walk_opts.rewrite_parents = true;
    }
    // `--max-count-oldest`: the last commits of the whole walk.
    let oldest = opts
        .max_count_oldest
        .then_some(walk_opts.max_count)
        .flatten();
    let reverse = walk_opts.reverse;
    if opts.max_count_oldest {
        walk_opts.max_count = None;
        walk_opts.reverse = false;
    }
    let mut follow = follow_filter(ctx, repo, cmd, opts, &resolved.paths)?;
    let max_count = walk_opts.max_count;
    let paths = if follow.is_some() {
        // `-n` counts the commits the filter shows.
        walk_opts.max_count = None;
        None
    } else {
        path_filter(ctx, repo, opts, &resolved.paths, &mut walk_opts)?
    };
    let cherry_repo = if walk_opts.cherry_pick || walk_opts.cherry_mark {
        Some(crate::repository::Repo::open(ctx, Mode::Required)?)
    } else {
        None
    };
    let patch_ids = match &cherry_repo {
        Some(r) => Some(
            crate::patch_id::CommitIds::new(r).with_pathspec(paths::parse_pathspec(
                ctx,
                repo,
                &resolved.paths,
            )?),
        ),
        None => None,
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
    let ident_grep = !opts.grep.author.is_empty() || !opts.grep.committer.is_empty();
    let mut walk = Walk::new(
        Commits {
            repo,
            grep,
            patch_ids,
            paths,
            mailmap: (use_mailmap && ident_grep).then(|| mailmap::Mailmap::load(repo)),
        },
        walk_opts,
    );
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
    let show_signature = cmd == Command::Log
        && opts.show_signature.unwrap_or_else(|| {
            repo.config
                .bool("log.showsignature")
                .ok()
                .flatten()
                .unwrap_or(false)
        });
    let formats_signature = match &pretty {
        Pretty::User { format, .. } => format.windows(2).any(|w| w == b"%G"),
        _ => false,
    };
    let signing = if show_signature || formats_signature {
        Some(crate::signing::Settings::from_config(repo.config)?)
    } else {
        None
    };
    let signature_colors = match &commit_color {
        Some(_) if show_signature => {
            let colors = crate::diff::color::DiffColors::from_config(repo.config)?;
            Some((colors.frag, colors.whitespace))
        }
        _ => None,
    };
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
        encoding: opts
            .encoding
            .as_ref()
            .map(|e| e.as_bytes().to_vec())
            .unwrap_or_else(|| recode::output_encoding(repo.config)),
        out: Vec::with_capacity(1 << 16),
        first: true,
        dist: None,
        graph: None,
        ended_line: true,
        child_shown: std::collections::HashSet::new(),
        next: None,
        diff: match log_diff(ctx, opts, cmd)? {
            Some(mut d) if !opts.full_diff && !resolved.paths.is_empty() => {
                d.pathspec = paths::parse_pathspec(ctx, repo, &resolved.paths)?;
                Some(d)
            }
            d => d,
        },
        store: None,
        usage: opts.disk_usage.map(|_| 0),
        signing,
        show_signature,
        signature_colors,
        signature_cache: std::cell::RefCell::new(None),
    };
    if opts.graph {
        let palette = p
            .style
            .commit_color
            .as_ref()
            .map(|_| graph_colors(repo.config));
        let mut g = graph::Graph::new(palette);
        let indent = opts.graph_indent.unwrap_or_else(|| {
            repo.config
                .bool("log.graphIndent")
                .ok()
                .flatten()
                .unwrap_or(true)
        });
        g.set_indent(indent);
        p.graph = Some(g);
    }
    if opts.unpacked || opts.disk_usage.is_some() {
        let setup = repo.setup.clone();
        p.store = Some(crate::repo_objects::Repo::open(ctx, setup)?);
    }
    if is_user && cmd == Command::Log {
        let mut map = HashMap::new();
        for ((id, _, _), name) in resolved.commits.iter().zip(&resolved.sources) {
            map.entry(*id).or_insert_with(|| name.clone());
        }
        p.sources = Some(map);
    }
    if let Some(mode) = opts.objects {
        let pathspec = if resolved.paths.is_empty() {
            None
        } else {
            Some(paths::parse_pathspec(ctx, repo, &resolved.paths)?)
        };
        return run_objects(&mut p, &mut walk, &resolved, mode, pathspec);
    }
    // `show` walks only for ranges, exclusions and `-<n>`.
    let single = |t: &options::Tip| {
        matches!(t, options::Tip::Rev { arg, negate: false }
            if !arg.contains("..") && !arg.starts_with('^'))
    };
    if opts.show && opts.walk.max_count.is_none() && opts.tips.iter().all(single) {
        if opts.graph {
            return Err(Die::Fatal(msg::cannot_be_used_together(
                "--no-walk",
                "--graph",
            )));
        }
        return show_objects(&mut p);
    }
    if bisecting {
        return run_bisect(&mut p, &mut walk);
    }
    let mut kept = match oldest {
        Some(n) => Some(keep_oldest(&mut walk, n, reverse)?),
        None => None,
    };
    if p.graph.is_some() && !opts.count {
        // The graph looks two commits ahead.
        let mut queue = std::collections::VecDeque::new();
        let mut done = false;
        loop {
            while !done && queue.len() < 3 {
                match next_shown(&mut walk, &mut kept)? {
                    Some(s) => queue.push_back(s),
                    None => done = true,
                }
            }
            let Some(s) = queue.pop_front() else {
                break;
            };
            if opts.walk.boundary && !s.boundary {
                let parents = walk.parents(&s.id).map(<[ObjectId]>::to_vec);
                p.child_shown.extend(parents.unwrap_or_default());
            }
            p.next = queue.front().map(|n: &Shown| graph::Next {
                id: n.id,
                root: !n.boundary && p.graph_parents(&walk, &n.id).is_empty(),
                last: done && queue.len() == 1,
            });
            p.commit(&walk, &s)?;
        }
        p.flush();
        return Ok(0);
    }
    if opts.maximal_only {
        let mut all = Vec::new();
        while let Some(s) = next_shown(&mut walk, &mut kept)? {
            all.push(s);
        }
        kept = Some(maximal(&walk, all).into());
    }
    let (mut left, mut right, mut same) = (0usize, 0usize, 0usize);
    let mut shown = 0usize;
    while let Some(s) = next_shown(&mut walk, &mut kept)? {
        if let Some(f) = &mut follow {
            if max_count.is_some_and(|m| shown >= m) {
                break;
            }
            let mut parents = walk.parents(&s.id).unwrap_or_default().to_vec();
            if opts.walk.first_parent {
                parents.truncate(1);
            }
            if !f.show(repo, &s.id, &parents).map_err(walk_error)? {
                continue;
            }
            shown += 1;
        }
        if opts.count {
            if opts.walk.cherry_mark && s.same {
                same += 1;
            } else if s.side == Some(Side::Left) {
                left += 1;
            } else {
                right += 1;
            }
            continue;
        }
        p.commit(&walk, &s)?;
    }
    if opts.count {
        let text = match (opts.left_right, opts.walk.cherry_mark) {
            (true, true) => format!("{left}\t{right}\t{same}\n"),
            (true, false) => format!("{left}\t{right}\n"),
            (false, true) => format!("{}\t{same}\n", left + right),
            (false, false) => format!("{}\n", left + right),
        };
        p.out.extend_from_slice(text.as_bytes());
    }
    p.finish_usage();
    p.flush();
    Ok(0)
}

/// The next commit of the walk, or of the list `kept` when there is one.
fn next_shown(
    walk: &mut Walk<Commits<'_, '_>>,
    kept: &mut Option<std::collections::VecDeque<Shown>>,
) -> Result<Option<Shown>, Die> {
    match kept {
        Some(list) => Ok(list.pop_front()),
        None => walk
            .next_shown()
            .map_err(|id| Die::Fatal(msg::bad_object(&id.to_hex()))),
    }
}

/// `--max-count-oldest=<n>`: the last `n` commits of the walk (boundary
/// commits do not count), turned around for `--reverse`.
fn keep_oldest(
    walk: &mut Walk<Commits<'_, '_>>,
    n: usize,
    reverse: bool,
) -> Result<std::collections::VecDeque<Shown>, Die> {
    let mut all = Vec::new();
    while let Some(s) = walk
        .next_shown()
        .map_err(|id| Die::Fatal(msg::bad_object(&id.to_hex())))?
    {
        all.push(s);
    }
    let shown = all.iter().filter(|s| !s.boundary).count();
    let mut skip = shown.saturating_sub(n);
    let mut out: std::collections::VecDeque<Shown> = all
        .into_iter()
        .filter(|s| {
            if s.boundary || skip == 0 {
                return true;
            }
            skip -= 1;
            false
        })
        .collect();
    // Boundary commits only for the commits that are left.
    let mut parents = std::collections::HashSet::new();
    for s in out.iter().filter(|s| !s.boundary) {
        parents.extend(walk.parents(&s.id).unwrap_or_default().iter().copied());
    }
    out.retain(|s| !s.boundary || parents.contains(&s.id));
    if reverse {
        out.make_contiguous().reverse();
    }
    Ok(out)
}

/// `--maximal-only`: the commits of `shown` no other one of them reaches.
fn maximal<S: CommitSource>(walk: &Walk<S>, shown: Vec<Shown>) -> Vec<Shown> {
    let mut reached = std::collections::HashSet::new();
    let mut stack: Vec<ObjectId> = shown
        .iter()
        .flat_map(|s| walk.parents(&s.id).unwrap_or_default().to_vec())
        .collect();
    while let Some(id) = stack.pop() {
        if reached.insert(id) {
            stack.extend(walk.parents(&id).unwrap_or_default().iter().copied());
        }
    }
    shown
        .into_iter()
        .filter(|s| !reached.contains(&s.id))
        .collect()
}

/// A byte count in the largest binary unit it reaches, with two
/// decimals (`--disk-usage=human`).
fn human_size(n: u64) -> String {
    const UNITS: [(u32, &str); 3] = [(30, "GiB"), (20, "MiB"), (10, "KiB")];
    for (shift, unit) in UNITS {
        if n >= 1 << shift {
            // Hundredths, rounded.
            let unit_size = 1u128 << shift;
            let h = (u128::from(n) * 100 + unit_size / 2) / unit_size;
            return format!("{}.{:02} {unit}", h / 100, h % 100);
        }
    }
    if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{n} bytes")
    }
}

/// `--objects`: the commits, then the tags, trees and blobs they need.
/// `rev-list --bisect`, `--bisect-vars` and `--bisect-all`: the commit
/// halfway between the included and the excluded ones (rgit_revwalk::bisect).
fn run_bisect(p: &mut Printer<'_, '_>, walk: &mut Walk<Commits<'_, '_>>) -> Result<i32, Die> {
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let opts = p.opts;
    let mut list = Vec::new();
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        if !s.boundary {
            list.push(s.id);
        }
    }
    let cands = bisect::candidates(
        &list,
        |id| {
            walk.parents(id)
                .map(<[ObjectId]>::to_vec)
                .unwrap_or_default()
        },
        opts.walk.first_parent,
    );
    let found = bisect::find(&cands, opts.bisect_all);
    let all = cands.len();
    if opts.bisect_all {
        let mut order = bisect::sorted(&cands, &found.weights);
        let first = order.first().copied();
        if opts.walk.reverse {
            order.reverse();
        }
        let filter = decorate::Filter {
            clear: true,
            ..Default::default()
        };
        p.decorations = Some(Decorations::load(p.repo, &filter));
        p.decorate = true;
        for i in order {
            p.dist = Some(bisect::distance(found.weights[i], all));
            let s = Shown {
                id: cands[i].id,
                boundary: false,
                side: None,
                same: false,
            };
            p.commit(walk, &s)?;
        }
        p.dist = None;
        if opts.bisect_vars {
            let Some(i) = first else {
                p.flush();
                return Ok(1);
            };
            p.out.extend_from_slice(b"------\n");
            bisect_vars(p, &cands[i].id, bisect::Vars::new(found.weights[i], all));
        }
        p.flush();
        return Ok(0);
    }
    let Some(best) = found.best else {
        p.flush();
        return Ok(if opts.bisect_vars { 1 } else { 0 });
    };
    if opts.bisect_vars {
        bisect_vars(
            p,
            &cands[best].id,
            bisect::Vars::new(found.weights[best], all),
        );
    } else if opts.count {
        p.out.extend_from_slice(b"1\n");
    } else {
        let s = Shown {
            id: cands[best].id,
            boundary: false,
            side: None,
            same: false,
        };
        p.commit(walk, &s)?;
    }
    p.flush();
    Ok(0)
}

/// The shell assignments of `--bisect-vars`.
fn bisect_vars(p: &mut Printer<'_, '_>, id: &ObjectId, v: bisect::Vars) {
    let text = format!(
        "bisect_rev='{}'\nbisect_nr={}\nbisect_good={}\nbisect_bad={}\nbisect_all={}\nbisect_steps={}\n",
        id.to_hex(),
        v.nr,
        v.good,
        v.bad,
        v.all,
        v.steps
    );
    p.out.extend_from_slice(text.as_bytes());
}

fn run_objects(
    p: &mut Printer<'_, '_>,
    walk: &mut Walk<Commits<'_, '_>>,
    resolved: &Resolved,
    mode: ObjectsMode,
    pathspec: Option<rgit_pathspec::Pathspec>,
) -> Result<i32, Die> {
    let repo = p.repo;
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    let mut shown = Vec::new();
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        shown.push(s);
    }
    let mut lister = ObjectLister::new(Trees(repo));
    if let Some(ps) = &pathspec {
        let ps = ps.clone();
        lister.set_filter(Box::new(move |path, tree| {
            if tree {
                ps.dir_interest(path) != rgit_pathspec::DirInterest::None
            } else {
                ps.matches(path)
            }
        }));
    }
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
                .list_commit_tree(&tree, &mut |id, name| {
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
            .list_commit_tree(&tree, &mut |id, name| {
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
    p.finish_usage();
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
