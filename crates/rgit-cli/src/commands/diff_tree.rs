// SPDX-License-Identifier: MIT
//! `git diff-tree` (Documentation/git-diff-tree.adoc) for two tree-ishes
//! with optional pathspecs, in all formats of [`crate::diff::output`],
//! and `--merge-base`; for one commit its diff against its parent
//! (`--root`, `--no-commit-id`, `--always`), for a merge with `-m` against
//! every parent and with `-c`/`--cc` the combined diff
//! (`diff/combined.rs`). `--stdin` reads commits (with optional parents)
//! and tree pairs, one per line. With `-v`, `--pretty` or `--format` the
//! commits are shown like `git show` does (the header and the diff).

use std::io::{BufRead, Write as _};

use rgit_hash::ObjectId;
use rgit_object::{Object, ObjectKind};

use crate::diff::collect::TreeDiff;
use crate::diff::options::{DiffOpts, Formats};
use crate::diff::output::Output;
use crate::diff::{self, options::usage_exit};
use crate::messages::diff as dmsg;
use crate::messages::diff_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

/// The commit header options: what `-v`/`--pretty`/`--format` select
/// (forwarded to `show`) and `--always`.
#[derive(Default)]
struct Header {
    /// Options for `show`, in command-line order.
    show_args: Vec<String>,
    /// A pretty format was asked for.
    pretty: bool,
    always: bool,
    /// A diff format was given (else `show` is asked for `--raw`).
    format_given: bool,
}

/// Takes the header options out of `argv` (before `--`).
fn split_header(argv: &[String]) -> (Vec<String>, Header) {
    let mut rest = Vec::new();
    let mut h = Header::default();
    let mut it = argv.iter();
    for a in it.by_ref() {
        let s = a.as_str();
        if s == "--" {
            rest.push(a.clone());
            break;
        }
        match s {
            "-v" => {
                h.pretty = true;
                h.show_args.push("--pretty".into());
            }
            "--always" => h.always = true,
            "--pretty" | "--oneline" => {
                h.pretty = true;
                h.show_args.push(a.clone());
            }
            _ if s.starts_with("--pretty=") || s.starts_with("--format=") => {
                h.pretty = true;
                h.show_args.push(a.clone());
            }
            "--abbrev-commit" | "--no-abbrev-commit" | "--notes" | "--no-notes"
            | "--show-notes" | "--standard-notes" | "--no-standard-notes" | "--expand-tabs"
            | "--no-expand-tabs" => h.show_args.push(a.clone()),
            _ if s.starts_with("--notes=")
                || s.starts_with("--show-notes=")
                || s.starts_with("--encoding=")
                || s.starts_with("--date=") =>
            {
                h.show_args.push(a.clone());
            }
            _ => rest.push(a.clone()),
        }
    }
    rest.extend(it.cloned());
    (rest, h)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let help = format!(
        "usage: {}\n\n{}{}",
        msg::USAGE[0],
        msg::OPTIONS,
        dmsg::COMMON_OPTIONS
    );
    if let Some(code) = diff::help(argv, &help) {
        return Ok(code);
    }
    let usage = || usage_exit(help.clone());
    let (argv_rest, mut header) = split_header(&argv[1..]);
    let mut merge_base = false;
    let mut stdin = false;
    let mut root = false;
    let mut no_commit_id = false;
    let mut merges: Option<&'static str> = None;
    // Diff options as given, for `show` (with a pretty format).
    let mut diff_args: Vec<String> = Vec::new();
    let parsed = diff::parse_args(
        &argv_rest,
        |a| match a {
            "--root" => {
                root = true;
                Ok(true)
            }
            "--no-commit-id" => {
                no_commit_id = true;
                Ok(true)
            }
            "-c" => {
                merges = Some("-c");
                Ok(true)
            }
            "--cc" => {
                merges = Some("--cc");
                Ok(true)
            }
            "-m" => {
                merges = Some("-m");
                Ok(true)
            }
            "--merge-base" => {
                merge_base = true;
                Ok(true)
            }
            "--stdin" => {
                stdin = true;
                Ok(true)
            }
            _ => Ok(false),
        },
        |_| usage(),
    )?;
    let before = parsed.dashdash_at.unwrap_or(parsed.args.len());
    for a in argv_rest.iter().take_while(|a| *a != "--") {
        if a.starts_with('-') && !matches!(a.as_str(), "--root" | "--no-commit-id" | "--stdin") {
            diff_args.push(a.clone());
        }
    }
    let mut opts = parsed.opts;
    let mut args = parsed.args;
    header.format_given = opts.raw
        || opts.patch
        || opts.name_only
        || opts.name_status
        || opts.numstat
        || opts.stat
        || opts.shortstat
        || opts.summary
        || opts.check
        || opts.dirstat.is_some();
    if merge_base && stdin {
        return Err(Die::Fatal(dmsg::STDIN_WITH_MERGE_BASE.to_owned()));
    }
    let flags = Single {
        root,
        no_commit_id,
        merges,
        always: header.always,
    };
    if stdin {
        return run_stdin(ctx, opts, args, before, &flags, &header, &diff_args);
    }
    if merge_base {
        if args[..before].iter().any(|a| a.contains("..")) {
            return Err(Die::Fatal(dmsg::MERGE_BASE_WITH_RANGES.to_owned()));
        }
        if before < 2 {
            return Err(Die::Fatal(dmsg::MERGE_BASE_TWO_COMMITS.to_owned()));
        }
    }
    let revs = before.min(2);
    if revs == 1 || (revs == 2 && !merge_base && args.len() >= 2 && single_commit(ctx, &args)?) {
        let spec = args.remove(0);
        let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
        let id = repo
            .resolve(&spec)
            .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
        let commit_id = repo
            .peel_to(id, ObjectKind::Commit)
            .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
        let formats = commit_formats(&mut opts, &flags)?;
        opts.resolve_objects(&mut repo)?;
        let pathspec = diff::pathspec(ctx, &repo, &args)?;
        let c = Commits {
            repo: &repo,
            opts: &opts,
            formats,
            pathspec: &pathspec,
            flags: &flags,
        };
        let mut sink = Sink::new(&header, &diff_args, &args, &c);
        sink.commit(ctx, &c, commit_id, None)?;
        return sink.finish(ctx, &opts);
    }
    if revs != 2 {
        return Err(Die::Fatal(dmsg::not_supported("diff-tree with one tree-ish")));
    }
    let formats = opts.formats(false)?;
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    if merge_base
        && parsed.dashdash_at.unwrap_or(args.len()) > 2
        && repo.resolve(&args[2]).is_some()
    {
        return Err(Die::Fatal(dmsg::MERGE_BASE_TWO_COMMITS.to_owned()));
    }
    let mut ids = Vec::new();
    for spec in args.drain(..2) {
        let id = repo
            .resolve(&spec)
            .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
        ids.push((spec, id));
    }
    if merge_base {
        ids[0].1 = diff::single_merge_base(ctx, &mut repo, ids[0].1, ids[1].1)?;
    }
    let mut trees = Vec::new();
    for (spec, id) in &ids {
        let id = repo
            .peel_to(*id, ObjectKind::Tree)
            .ok_or_else(|| Die::Fatal(msg::bad_object(spec)))?;
        trees.push(id);
    }
    opts.resolve_objects(&mut repo)?;
    let pathspec = diff::pathspec(ctx, &repo, &args)?;
    diff::check_max_depth(&opts, &pathspec)?;
    let (pathspec, attr_filter) = diff::attr_split(ctx, &mut repo, pathspec)?;
    let repo = repo;
    let mut w = TreeDiff {
        repo: &repo,
        pathspec: &pathspec,
        recursive: recursive(&opts, formats),
        show_trees: opts.show_trees,
        max_depth: opts.max_depth,
        pairs: Vec::new(),
    };
    w.walk(Some(&trees[0]), Some(&trees[1]), b"")?;
    let mut pairs = std::mem::take(&mut w.pairs);
    if opts.copies_harder {
        let files = crate::diff::collect::tree_files(&repo, &trees[0], &pathspec)?;
        pairs = crate::diff::collect::with_unmodified(pairs, files);
    }
    diff::attr_retain(&repo, &mut pairs, attr_filter)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
    out.set_color(&opts)?;
    let (bytes, changed) = out.render(pairs)?;
    Ok(out.finish(&bytes, changed, &opts))
}

/// `-p`, `-t`, `--summary` (and every format that needs contents) imply
/// `-r`.
fn recursive(opts: &DiffOpts, formats: Formats) -> bool {
    opts.recursive
        || formats.patch
        || formats.summary
        || opts.show_trees
        || formats.needs_content()
        || opts.max_depth_given
}

/// The formats of the commit forms (`--cc` implies a patch).
fn commit_formats(opts: &mut DiffOpts, flags: &Single) -> Result<Formats, Die> {
    let other = opts.raw
        || opts.stat
        || opts.numstat
        || opts.shortstat
        || opts.summary
        || opts.name_only
        || opts.name_status
        || opts.dirstat.is_some();
    if flags.merges == Some("--cc") && !other {
        opts.patch = true;
    }
    opts.formats(false)
}

/// The flags of the commit forms.
struct Single {
    root: bool,
    no_commit_id: bool,
    merges: Option<&'static str>,
    always: bool,
}

/// Whether `<a> <b>` is a commit followed by a path (the single-commit
/// form with paths), which git decides by whether `b` names an object.
fn single_commit(ctx: &Context, args: &[String]) -> Result<bool, Die> {
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    Ok(repo.resolve(&args[1]).is_none() && repo.resolve(&args[0]).is_some())
}

/// The tree of a tree or commit id.
fn tree_of(repo: &Repo, id: ObjectId) -> Option<ObjectId> {
    match repo.read_object(&id)? {
        Object::Tree(_) => Some(id),
        Object::Commit(c) => c.tree().ok(),
        _ => None,
    }
}

/// Diffs of commits against their parents.
struct Commits<'a> {
    repo: &'a Repo,
    opts: &'a DiffOpts,
    formats: Formats,
    pathspec: &'a rgit_pathspec::Pathspec,
    flags: &'a Single,
}

impl Commits<'_> {
    /// The rendered diff of `old` against `new` and whether anything differs.
    fn pair(&self, old: Option<ObjectId>, new: &ObjectId) -> Result<(Vec<u8>, bool), Die> {
        self.pair_with(old, new, self.formats)
    }

    fn pair_with(
        &self,
        old: Option<ObjectId>,
        new: &ObjectId,
        formats: Formats,
    ) -> Result<(Vec<u8>, bool), Die> {
        let mut w = TreeDiff {
            repo: self.repo,
            pathspec: self.pathspec,
            recursive: recursive(self.opts, formats),
            show_trees: self.opts.show_trees,
            max_depth: self.opts.max_depth,
            pairs: Vec::new(),
        };
        w.walk(old.as_ref(), Some(new), b"")?;
        let mut out = Output::new(Some(self.repo), self.repo.hash, self.opts, formats, false)?;
        out.set_color(self.opts)?;
        out.render(std::mem::take(&mut w.pairs))
    }

    /// The diffs `commit` gets (one per parent with `-m`), or none when the
    /// commit is not shown (a root without `--root`, a merge without `-m`,
    /// `-c` or `--cc`), and whether it is a combined diff. `parents`
    /// replaces the commit's own parents.
    #[allow(clippy::type_complexity)]
    fn diffs(
        &self,
        commit_id: ObjectId,
        parents: Option<Vec<ObjectId>>,
    ) -> Result<(Vec<(Vec<u8>, bool)>, bool), Die> {
        let repo = self.repo;
        let bad = || Die::Fatal(msg::bad_object(&commit_id.to_hex()));
        let Some(Object::Commit(commit)) = repo.read_object(&commit_id) else {
            return Err(bad());
        };
        let parents = match parents {
            Some(p) => p,
            None => commit.parents().map_err(|_| bad())?,
        };
        let tree = commit.tree().map_err(|_| bad())?;
        let mut parent_trees = Vec::new();
        for p in &parents {
            parent_trees.push(
                tree_of(repo, *p).ok_or_else(|| Die::Fatal(msg::bad_object(&p.to_hex())))?,
            );
        }
        let mut chunks = Vec::new();
        match parents.len() {
            0 => {
                if self.flags.root {
                    chunks.push(self.pair(None, &tree)?);
                }
            }
            1 => chunks.push(self.pair(Some(parent_trees[0]), &tree)?),
            _ => match self.flags.merges {
                None => {}
                Some("-m") => {
                    for t in &parent_trees {
                        chunks.push(self.pair(Some(*t), &tree)?);
                    }
                }
                Some(mode) => {
                    // Formats other than raw and patch compare with the
                    // first parent and come first.
                    let others = Formats {
                        raw: false,
                        patch: false,
                        ..self.formats
                    };
                    let (mut bytes, mut changed) = (Vec::new(), false);
                    if others.any() {
                        (bytes, changed) =
                            self.pair_with(Some(parent_trees[0]), &tree, others)?;
                    }
                    if self.formats.patch || self.formats.raw {
                        let (b, c) = self.combined(&parent_trees, &tree, mode == "--cc");
                        if !bytes.is_empty() && !b.is_empty() {
                            bytes.push(b'\n');
                        }
                        bytes.extend_from_slice(&b);
                        changed |= c;
                    }
                    chunks.push((bytes, changed));
                }
            },
        }
        let combined = parents.len() > 1 && matches!(self.flags.merges, Some("-c" | "--cc"));
        Ok((chunks, combined))
    }

    fn combined(&self, parent_trees: &[ObjectId], tree: &ObjectId, dense: bool) -> (Vec<u8>, bool) {
        let repo = self.repo;
        let files: Vec<_> = crate::diff::combined::collect(repo, parent_trees, tree, false)
            .into_iter()
            .filter(|f| self.pathspec.is_empty() || self.pathspec.matches(&f.path))
            .collect();
        let quote = repo.quote_path();
        let mut bytes = Vec::new();
        if self.formats.raw {
            let abbrev = match self.opts.abbrev {
                Some(Some(n)) => Some(n),
                Some(None) => Some(repo.default_abbrev()),
                None => None,
            };
            crate::diff::combined::raw(repo, &files, abbrev, quote, &mut bytes);
        }
        if self.formats.patch {
            let diff = rgit_diff::DiffOptions::default();
            let popts = crate::diff::combined::PatchOptions {
                dense,
                abbrev: repo.default_abbrev(),
                quote,
                diff: &diff,
            };
            crate::diff::combined::patch(repo, &files, &popts, &mut bytes);
        }
        let changed = !files.is_empty();
        (bytes, changed)
    }
}

/// Where commit output goes: written directly with the commit id line,
/// or (with a pretty format) collected and shown by `show`.
struct Sink {
    out: Vec<u8>,
    changed: bool,
    /// `--quiet` found a difference: nothing more is looked at.
    stopped: bool,
    /// The commits for `show` when a pretty format was asked for.
    pretty: Option<Vec<String>>,
    pretty_ids: Vec<ObjectId>,
}

impl Sink {
    fn new(header: &Header, diff_args: &[String], paths: &[String], c: &Commits<'_>) -> Sink {
        let pretty = header.pretty.then(|| {
            // diff-tree is plumbing: none of the porcelain defaults of
            // `show` apply (abbreviated ids, renames, notes, decorations,
            // mailmap, colours); the options given come after them.
            let mut a: Vec<String> = [
                "--no-abbrev",
                "--no-renames",
                "--no-notes",
                "--no-decorate",
                "--no-mailmap",
                "--no-color",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
            if !header.format_given && !c.opts.no_output {
                a.push("--raw".into());
            }
            if c.flags.merges.is_none() {
                a.push("--diff-merges=off".into());
            }
            a.extend(diff_args.iter().cloned());
            if let Some(m) = c.flags.merges {
                a.push(m.into());
            }
            a.extend(header.show_args.iter().cloned());
            a.push("\u{0}PATHS".into());
            a.extend(paths.iter().cloned());
            a
        });
        Sink {
            out: Vec::new(),
            changed: false,
            stopped: false,
            pretty,
            pretty_ids: Vec::new(),
        }
    }

    /// Adds `commit_id`'s output (see [`Commits::diffs`]).
    fn commit(
        &mut self,
        ctx: &Context,
        c: &Commits<'_>,
        commit_id: ObjectId,
        parents: Option<Vec<ObjectId>>,
    ) -> Result<(), Die> {
        if self.stopped {
            return Ok(());
        }
        let (mut chunks, combined) = c.diffs(commit_id, parents)?;
        // A combined diff always shows its commit; --always shows commits
        // without a diff too.
        let shown =
            |changed: bool| changed || combined || c.opts.no_output || c.flags.always;
        if chunks.is_empty() && c.flags.always {
            chunks.push((Vec::new(), false));
        }
        if self.pretty.is_some() {
            self.changed |= chunks.iter().any(|(_, ch)| *ch);
            if chunks.iter().any(|(_, ch)| shown(*ch)) {
                self.pretty_ids.push(commit_id);
            }
        } else {
            for (bytes, changed) in chunks {
                self.changed |= changed;
                if !shown(changed) {
                    continue;
                }
                if !c.flags.no_commit_id {
                    self.out
                        .extend_from_slice(format!("{}\n", commit_id.to_hex()).as_bytes());
                }
                self.out.extend_from_slice(&bytes);
            }
        }
        // --quiet stops at the first difference.
        self.stopped = c.opts.quiet && self.changed;
        if self.out.len() > 1 << 16 {
            self.flush(ctx)?;
        }
        Ok(())
    }

    /// Writes what was collected; with a pretty format runs `show` for the
    /// collected commits first.
    fn flush(&mut self, ctx: &Context) -> Result<(), Die> {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&self.out);
        let _ = stdout.flush();
        drop(stdout);
        self.out.clear();
        if let Some(template) = &self.pretty {
            if self.pretty_ids.is_empty() {
                return Ok(());
            }
            let mut args = Vec::new();
            let mut paths = Vec::new();
            let mut in_paths = false;
            for a in template {
                if a == "\u{0}PATHS" {
                    in_paths = true;
                } else if in_paths {
                    paths.push(a.clone());
                } else {
                    args.push(a.clone());
                }
            }
            args.extend(self.pretty_ids.drain(..).map(|id| id.to_hex()));
            args.push("--".into());
            args.extend(paths);
            let mut sub = ctx.clone();
            if sub.paginate.is_none() {
                sub.paginate = Some(false);
            }
            crate::history::diff_tree_show(&sub, &args)?;
        }
        Ok(())
    }

    fn finish(mut self, ctx: &Context, opts: &DiffOpts) -> CmdResult {
        self.flush(ctx)?;
        Ok(i32::from(opts.wants_exit_code() && self.changed))
    }
}

/// `--stdin`: commits (`<commit> [<parent>...]`) and tree pairs
/// (`<tree> <tree>`), one per line; other lines are copied.
fn run_stdin(
    ctx: &Context,
    mut opts: DiffOpts,
    args: Vec<String>,
    before: usize,
    flags: &Single,
    header: &Header,
    diff_args: &[String],
) -> CmdResult {
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    // Leading arguments that name objects are processed first; the rest
    // are paths.
    let mut revs = Vec::new();
    let mut i = 0;
    while i < before {
        match repo.resolve(&args[i]) {
            Some(id) => revs.extend(repo.peel_to(id, ObjectKind::Commit)),
            None => break,
        }
        i += 1;
    }
    let paths: Vec<String> = args[i..].to_vec();
    let formats = commit_formats(&mut opts, flags)?;
    let tree_formats = opts.formats(false)?;
    opts.resolve_objects(&mut repo)?;
    let pathspec = diff::pathspec(ctx, &repo, &paths)?;
    let repo = repo;
    let c = Commits {
        repo: &repo,
        opts: &opts,
        formats,
        pathspec: &pathspec,
        flags,
    };
    let mut sink = Sink::new(header, diff_args, &paths, &c);
    for id in revs {
        sink.commit(ctx, &c, id, None)?;
    }
    let hex_len = repo.hash.hex_len();
    let stdin = std::io::stdin();
    for line in stdin.lock().split(b'\n') {
        let Ok(line) = line else { break };
        let text = String::from_utf8_lossy(&line).into_owned();
        let mut words = text.split(' ');
        let first = words.next().unwrap_or("");
        let id = (first.len() == hex_len)
            .then(|| ObjectId::from_hex(repo.hash, first.as_bytes()).ok())
            .flatten();
        let Some(id) = id else {
            sink.out.extend_from_slice(&line);
            sink.out.push(b'\n');
            continue;
        };
        match repo.read_object(&id) {
            Some(Object::Commit(_)) => {
                let parents: Vec<ObjectId> = words
                    .map_while(|w| {
                        (w.len() == hex_len)
                            .then(|| ObjectId::from_hex(repo.hash, w.as_bytes()).ok())
                            .flatten()
                    })
                    .collect();
                let parents = (!parents.is_empty()).then_some(parents);
                sink.commit(ctx, &c, id, parents)?;
            }
            Some(Object::Tree(_)) => {
                let rest: Vec<&str> = words.collect();
                let other = match rest.as_slice() {
                    [w] if w.len() == hex_len => ObjectId::from_hex(repo.hash, w.as_bytes())
                        .ok()
                        .and_then(|o| tree_of(&repo, o)),
                    _ => None,
                };
                let Some(other) = other else {
                    sink.flush(ctx)?;
                    crate::messages::print_error(msg::NEED_TWO_TREES);
                    continue;
                };
                let tc = Commits {
                    repo: c.repo,
                    opts: c.opts,
                    formats: tree_formats,
                    pathspec: c.pathspec,
                    flags: c.flags,
                };
                let (bytes, changed) = tc.pair(Some(id), &other)?;
                sink.changed |= changed;
                sink.out.extend_from_slice(&line);
                sink.out.push(b'\n');
                sink.out.extend_from_slice(&bytes);
            }
            Some(o) => {
                sink.flush(ctx)?;
                crate::messages::print_error(&msg::not_commit_or_tree(&id.to_hex(), o.kind().name()));
            }
            None => {
                sink.flush(ctx)?;
                crate::messages::print_error(&msg::not_commit_or_tree(&id.to_hex(), "missing"));
            }
        }
    }
    sink.finish(ctx, &opts)
}
