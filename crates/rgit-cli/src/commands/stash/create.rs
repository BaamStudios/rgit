// SPDX-License-Identifier: MIT
//! `stash push` (bare `stash`, `save`) and `stash create`: recording the
//! index, the work tree and the untracked files as the commits of a stash
//! entry, then rolling the work tree back.

use rgit_hash::ObjectId;
use rgit_pathspec::{ParseOptions, Pathspec};

use super::child::{Git, TempIndex};
use super::{Head, commit_parts, do_store, head, open, parse, refresh_index, say, write_commit};
use crate::messages::reset as rmsg;
use crate::messages::stash as msg;
use crate::prelude::*;
use crate::attributes::PathspecAttrs;
use crate::repository::{self, Repo};

/// How `push` was invoked; failures exit with different codes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushKind {
    /// `git stash [<options>]`.
    Implicit,
    /// `git stash push`.
    Push,
    /// `git stash save`.
    Save,
}

/// `-u` / `-a`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Untracked {
    None,
    Normal,
    All,
}

pub(crate) struct CreateOptions {
    pub(crate) message: Option<String>,
    pub(crate) untracked: Untracked,
    pub(crate) staged: bool,
    /// Pathspecs as passed to child commands (`:(prefix:<n>)...`).
    pub(crate) pathspec: Vec<String>,
    /// The pathspecs as messages show them.
    pub(crate) shown: Vec<String>,
    /// `--literal-pathspecs` and friends.
    pub(crate) globals: rgit_pathspec::GlobalFlags,
}

/// The commits of a new entry.
pub(crate) struct Created {
    pub(crate) w: ObjectId,
    pub(crate) i: ObjectId,
    /// The untracked files commit.
    pub(crate) u: Option<ObjectId>,
    pub(crate) head: ObjectId,
    /// The reflog message (the subject of `W`).
    pub(crate) message: String,
}

/// A step failed after printing its message; push exits with 1 (128
/// for an explicit `push`).
pub(crate) struct Failed;

pub(crate) fn push(ctx: &mut Context, args: &[String], kind: PushKind) -> CmdResult {
    let keep_index = Cell::new(-1);
    let staged = Cell::new(false);
    let patch = Cell::new(false);
    let auto_advance = Cell::new(false);
    let unified = Cell::new(-1);
    let inter_hunk = Cell::new(-1);
    let quiet = Cell::new(0);
    let untracked = Cell::new(0);
    let message = RefCell::new(None);
    let from_file = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut opts = vec![
        Opt::bool('k', "keep-index", &keep_index, msg::HELP_KEEP_INDEX),
        Opt::flag('S', "staged", &staged, msg::HELP_STAGED),
        Opt::flag('p', "patch", &patch, msg::HELP_PATCH),
        Opt::flag(None, "auto-advance", &auto_advance, msg::HELP_AUTO_ADVANCE),
        Opt::integer('U', "unified", &unified, msg::HELP_UNIFIED).noneg(),
        Opt::integer(None, "inter-hunk-context", &inter_hunk, msg::HELP_INTER_HUNK).noneg(),
        Opt::quiet(&quiet, msg::HELP_QUIET_MODE),
        Opt::set_int('u', "include-untracked", &untracked, msg::HELP_INCLUDE_UNTRACKED, 1),
        Opt::set_int('a', "all", &untracked, msg::HELP_ALL, 2),
        Opt::string('m', "message", &message, "message", msg::HELP_MESSAGE),
    ];
    if kind != PushKind::Save {
        opts.push(Opt::string(
            None,
            "pathspec-from-file",
            &from_file,
            "file",
            msg::HELP_PATHSPEC_FROM_FILE,
        ));
        opts.push(Opt::flag(
            None,
            "pathspec-file-nul",
            &file_nul,
            msg::HELP_PATHSPEC_FILE_NUL,
        ));
    }
    let usage = if kind == PushKind::Save {
        msg::USAGE_SAVE
    } else {
        msg::USAGE_PUSH
    };
    // Without a subcommand, errors show the usage of every subcommand.
    let usage_list = if kind == PushKind::Implicit {
        super::usage_lines()
    } else {
        vec![usage]
    };
    let mut parser = Parser::new(&usage_list, opts).flags(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    });
    let mut argv: Vec<String> = Vec::new();
    if kind == PushKind::Implicit {
        argv.push("push".to_owned());
    }
    argv.extend(args.iter().cloned());
    let mut rest = match parse(&mut parser, &argv) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    drop(parser);
    let fail_code = if kind == PushKind::Push { 128 } else { 1 };

    let mut message = message.borrow().clone();
    match kind {
        PushKind::Implicit => {
            if let Some(first) = rest.first() {
                if first != "--" {
                    return Err(Die::Fatal(msg::no_assumed_push(first)));
                }
                rest.remove(0);
            }
        }
        PushKind::Push => {
            if rest.first().is_some_and(|a| a == "--") {
                rest.remove(0);
            }
        }
        PushKind::Save => {
            if rest.first().is_some_and(|a| a == "--") {
                rest.remove(0);
            }
            if !rest.is_empty() {
                message = Some(rest.join(" "));
            }
            rest.clear();
        }
    }

    let from_file = from_file.borrow().clone();
    if patch.get() && from_file.is_some() {
        return Err(Die::Fatal(rmsg::FROM_FILE_WITH_PATCH.to_owned()));
    }
    if let Some(file) = &from_file {
        if !rest.is_empty() {
            return Err(Die::Fatal(rmsg::FROM_FILE_WITH_ARGS.to_owned()));
        }
        let data = if file == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf).map_err(|e| {
                Die::Fatal(rmsg::could_not_open(file, &crate::global::strerror(&e)))
            })?;
            buf
        } else {
            std::fs::read(file).map_err(|e| {
                Die::Fatal(rmsg::could_not_open(file, &crate::global::strerror(&e)))
            })?
        };
        let items = rgit_pathspec::parse_pathspec_file(&data, file_nul.get())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        rest = items
            .into_iter()
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect();
    } else if file_nul.get() {
        return Err(Die::Fatal(rmsg::FILE_NUL_WITHOUT_FROM_FILE.to_owned()));
    }

    let untracked = match untracked.get() {
        1 => Untracked::Normal,
        2 => Untracked::All,
        _ => Untracked::None,
    };
    if staged.get() && untracked != Untracked::None {
        eprintln!("{}", msg::STAGED_WITH_UNTRACKED);
        return Err(Die::Exit(fail_code));
    }

    let mut repo = open(ctx)?;
    repo.work_tree()?;
    let (pathspec, shown) = prefixed_pathspecs(&repo, &rest);
    let opts = CreateOptions {
        message,
        untracked,
        staged: staged.get(),
        pathspec,
        shown,
        globals: repository::pathspec_globals(ctx),
    };
    let quiet = quiet.get() > 0;
    let git = Git::new(ctx, &repo)?;
    match do_push(&mut repo, &git, &opts, keep_index.get() > 0, quiet, patch.get()) {
        Ok(()) => Ok(0),
        Err(Failed) => Err(Die::Exit(fail_code)),
    }
}

/// The pathspecs relative to the top of the work tree, and as git shows
/// them in messages (`:(prefix:<n>)<prefix><arg>`).
fn prefixed_pathspecs(repo: &Repo, args: &[String]) -> (Vec<String>, Vec<String>) {
    let prefix = String::from_utf8_lossy(&repo.prefix()).into_owned();
    let mut specs = Vec::new();
    let mut shown = Vec::new();
    for a in args {
        shown.push(format!(":(prefix:{}){prefix}{a}", prefix.len()));
        if a.starts_with(':') {
            specs.push(a.clone());
            continue;
        }
        match repo.prefix_path(a.as_bytes()) {
            Ok(p) if p.is_empty() => specs.push(".".to_owned()),
            Ok(p) => specs.push(String::from_utf8_lossy(&p).into_owned()),
            Err(()) => specs.push(a.clone()),
        }
    }
    (specs, shown)
}

fn parse_pathspec(repo: &Repo, opts: &CreateOptions) -> Result<Pathspec, Failed> {
    let parse = ParseOptions {
        root: repo
            .setup
            .work_tree
            .as_ref()
            .map(|w| w.as_os_str().as_encoded_bytes().to_vec()),
        globals: opts.globals,
        ..Default::default()
    };
    Pathspec::parse(&opts.pathspec, &parse).map_err(|e| {
        eprintln!("fatal: {e}");
        Failed
    })
}

fn do_push(
    repo: &mut Repo,
    git: &Git,
    opts: &CreateOptions,
    keep_index: bool,
    quiet: bool,
    patch: bool,
) -> Result<(), Failed> {
    if refresh_index(repo).is_err() {
        eprintln!("error: {}", msg::COULD_NOT_WRITE_INDEX);
        return Err(Failed);
    }
    let Some(head) = head(repo) else {
        if !quiet {
            eprintln!("{}", msg::NO_INITIAL_COMMIT);
        }
        return Err(Failed);
    };
    if patch {
        eprintln!("fatal: {}", msg::not_supported("--patch"));
        return Err(Failed);
    }
    // With -u/-a a pathspec that matches nothing is not an error.
    if !opts.pathspec.is_empty() && opts.untracked == Untracked::None {
        check_pathspec_matches(repo, opts)?;
    }
    let Some(created) = create_stash(repo, git, &head, opts)? else {
        if !quiet {
            say(msg::NO_LOCAL_CHANGES);
        }
        return Ok(());
    };
    if let Err(e) = do_store(repo, created.w, &created.message, quiet) {
        if let Die::Fatal(m) = e {
            eprintln!("fatal: {m}");
        }
        eprintln!("{}", msg::CANNOT_SAVE_STATUS);
        return Err(Failed);
    }
    if !quiet {
        say(&msg::saved(&created.message));
    }
    revert(repo, git, opts, &created, keep_index)
}

/// Without `-u`/`-a`, every pathspec must match a tracked file.
fn check_pathspec_matches(repo: &Repo, opts: &CreateOptions) -> Result<(), Failed> {
    let ps = parse_pathspec(repo, opts)?;
    let mut seen = vec![false; ps.len()];
    let (index, _) = repo.read_index().map_err(|_| Failed)?;
    let attrs = RefCell::new(PathspecAttrs::new(repo, &index, None));
    for e in index.entries() {
        let _ = spec_matches(&ps, &attrs, e.path(), Some(&mut seen));
    }
    let mut bad = false;
    for item in ps.unmatched(&seen) {
        bad = true;
        let i = ps
            .items()
            .iter()
            .position(|x| std::ptr::eq(x, item))
            .unwrap_or(0);
        let shown = opts
            .shown
            .get(i)
            .cloned()
            .unwrap_or_else(|| String::from_utf8_lossy(item.original()).into_owned());
        eprintln!("error: {}", msg::pathspec_no_match(&shown));
    }
    if bad {
        eprintln!("{}", msg::FORGET_ADD);
        return Err(Failed);
    }
    Ok(())
}

fn nul_split(data: &[u8]) -> Vec<Vec<u8>> {
    data.split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

fn with_pathspec(mut args: Vec<String>, ps: &[String]) -> Vec<String> {
    args.push("--".to_owned());
    args.extend(ps.iter().cloned());
    args
}

/// The untracked (with `-a` also the ignored) files `opts` selects.
fn untracked_files(repo: &Repo, git: &Git, opts: &CreateOptions) -> Result<Vec<Vec<u8>>, Failed> {
    let mut args = vec!["ls-files".to_owned(), "-z".to_owned(), "--others".to_owned()];
    if opts.untracked == Untracked::Normal {
        args.push("--exclude-standard".to_owned());
    }
    let (code, out) = git.cmd(&args).output().map_err(|_| Failed)?;
    if code != 0 {
        return Err(Failed);
    }
    let mut files = nul_split(&out);
    if !opts.pathspec.is_empty() {
        let spec = parse_pathspec(repo, opts)?;
        let (index, _) = repo.read_index().map_err(|_| Failed)?;
        let attrs = RefCell::new(PathspecAttrs::new(repo, &index, None));
        files.retain(|p| spec_matches(&spec, &attrs, p, None));
    }
    Ok(files)
}

/// Whether `path` is selected by `spec`, evaluating `attr:` magic.
fn spec_matches(
    spec: &Pathspec,
    attrs: &RefCell<PathspecAttrs<'_>>,
    path: &[u8],
    seen: Option<&mut [bool]>,
) -> bool {
    spec.match_path_with(path, seen, &mut |p, a| attrs.borrow_mut().check(p, a))
        .is_some()
}

fn hex_line(out: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(out).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// `<branch>: <abbrev> <subject>`.
fn base_message(repo: &Repo, head: &Head) -> String {
    let objects = crate::refs_support::Objects::open(&repo.setup);
    let abbrev = repo.abbrev(&head.commit, repo.default_abbrev());
    let subject = crate::switch_branch::subject(&objects, &head.commit);
    format!("{}: {abbrev} {subject}", head.branch)
}

/// Records the stash commits; `None` when there is nothing to save.
pub(crate) fn create_stash(
    repo: &Repo,
    git: &Git,
    head: &Head,
    opts: &CreateOptions,
) -> Result<Option<Created>, Failed> {
    let ps = &opts.pathspec;
    let diff_index = git
        .cmd(&with_pathspec(
            vec![
                "diff-index".into(),
                "--quiet".into(),
                "--cached".into(),
                "HEAD".into(),
            ],
            ps,
        ))
        .status()
        .map_err(|_| Failed)?;
    let diff_files = git
        .cmd(&with_pathspec(
            vec![
                "diff-files".into(),
                "--quiet".into(),
            ],
            ps,
        ))
        .status()
        .map_err(|_| Failed)?;
    let untracked = if opts.untracked != Untracked::None {
        untracked_files(repo, git, opts)?
    } else {
        Vec::new()
    };
    if diff_index == 0 && diff_files == 0 && untracked.is_empty() {
        return Ok(None);
    }

    let base = base_message(repo, head);
    let (index, _) = repo.read_index().map_err(|_| Failed)?;
    let i_tree = match rgit_commit::write_tree(repo.odb(), &index, &Default::default()) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("{}", msg::CANNOT_SAVE_INDEX);
            return Err(Failed);
        }
    };
    let i_commit = write_commit(repo, i_tree, &[head.commit], msg::index_on(&base).as_bytes())
        .map_err(report)?;

    let mut u_commit = None;
    if !untracked.is_empty() {
        let tmp = TempIndex::new(repo, "stash-untracked-index");
        let mut list = Vec::new();
        for p in &untracked {
            list.extend_from_slice(p);
            list.push(0);
        }
        let ok = git
            .cmd(&["update-index", "-z", "--add", "--remove", "--stdin"])
            .index(&tmp.path)
            .stdin(list)
            .status()
            .map_err(|_| Failed)?
            == 0;
        let tree = if ok {
            let (code, out) = git
                .cmd(&["write-tree"])
                .index(&tmp.path)
                .output()
                .map_err(|_| Failed)?;
            (code == 0).then(|| hex_line(&out)).flatten()
        } else {
            None
        };
        let Some(tree) = tree.and_then(|t| ObjectId::from_hex(repo.hash, t.as_bytes()).ok())
        else {
            eprintln!("{}", msg::CANNOT_SAVE_UNTRACKED);
            return Err(Failed);
        };
        u_commit = Some(
            write_commit(repo, tree, &[], msg::untracked_on(&base).as_bytes()).map_err(report)?,
        );
    }

    let w_tree = if opts.staged {
        let head_tree = commit_parts(repo, &head.commit).map(|(t, _)| t);
        if head_tree == Some(i_tree) {
            eprintln!("{}", msg::NO_STAGED_CHANGES);
            return Err(Failed);
        }
        i_tree
    } else {
        work_tree_state(repo, git, ps)?
    };

    let message = match &opts.message {
        Some(m) => msg::on_branch(&head.branch, m),
        None => msg::wip_on(&base),
    };
    let mut parents = vec![head.commit, i_commit];
    parents.extend(u_commit);
    let w = write_commit(repo, w_tree, &parents, message.as_bytes()).map_err(report)?;
    Ok(Some(Created {
        w,
        i: i_commit,
        u: u_commit,
        head: head.commit,
        message,
    }))
}

fn report(e: Die) -> Failed {
    match e {
        Die::Fatal(m) => eprintln!("fatal: {m}"),
        Die::Error(m, _) => eprintln!("error: {m}"),
        _ => {}
    }
    Failed
}

/// The tree of the index with the work tree versions of the tracked
/// files that `ps` selects (also files removed from the index that are
/// still in `HEAD`). A file replaced by a directory counts as deleted.
fn work_tree_state(repo: &Repo, git: &Git, ps: &[String]) -> Result<ObjectId, Failed> {
    use rgit_index::{AddOptions, Stage};
    let names = |args: Vec<String>| -> Result<Vec<Vec<u8>>, Failed> {
        let (code, out) = git
            .cmd(&with_pathspec(args, ps))
            .output()
            .map_err(|_| Failed)?;
        if code != 0 {
            eprintln!("{}", msg::CANNOT_SAVE_WORKTREE);
            return Err(Failed);
        }
        Ok(nul_split(&out))
    };
    let mut changed: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    changed.extend(names(vec![
        "diff-files".into(),
        "--name-only".into(),
        "-z".into(),
    ])?);
    changed.extend(names(vec![
        "diff-index".into(),
        "--name-only".into(),
        "-z".into(),
        "HEAD".into(),
    ])?);
    let (mut index, mtime) = repo.read_index().map_err(|_| Failed)?;
    for p in &changed {
        if let Some(e) = index.get(p, Stage::MERGED) {
            if e.flags.intent_to_add {
                eprintln!(
                    "error: {}",
                    msg::entry_not_uptodate(&String::from_utf8_lossy(p))
                );
                eprintln!("{}", msg::CANNOT_SAVE_WORKTREE);
                return Err(Failed);
            }
        }
    }
    let root = repo.work_tree().map_err(|_| Failed)?;
    let checker = repo.checker(mtime).map_err(|_| Failed)?;
    for p in changed {
        let old = index.get(&p, Stage::MERGED).cloned();
        if old
            .as_ref()
            .is_some_and(|e| e.mode.is_gitlink() || e.flags.skip_worktree)
        {
            continue;
        }
        let is_dir = checker
            .lstat(&p)
            .ok()
            .flatten()
            .is_some_and(|m| m.is_dir());
        let entry = if is_dir {
            None
        } else {
            match crate::commands::add::entry_for(repo, &checker, &root, &p, old.as_ref(), false, true)
            {
                Ok(e) => e,
                Err(crate::commands::add::EntryError::Read(m)) => {
                    eprintln!("error: {m}");
                    eprintln!("{}", msg::CANNOT_SAVE_WORKTREE);
                    return Err(Failed);
                }
                Err(crate::commands::add::EntryError::Fatal(d)) => {
                    report(d);
                    return Err(Failed);
                }
            }
        };
        index.remove_path(&p);
        if let Some(e) = entry {
            if index
                .add(
                    e,
                    AddOptions {
                        ok_to_replace: true,
                        ..Default::default()
                    },
                )
                .is_err()
            {
                eprintln!("{}", msg::CANNOT_SAVE_WORKTREE);
                return Err(Failed);
            }
        }
    }
    index.set_cache_tree(None);
    match rgit_commit::write_tree(repo.odb(), &index, &Default::default()) {
        Ok(t) => Ok(t),
        Err(_) => {
            eprintln!("{}", msg::CANNOT_RECORD_WORKTREE);
            Err(Failed)
        }
    }
}

/// Rolls the work tree (and index) back after the entry is stored.
fn revert(
    repo: &Repo,
    git: &Git,
    opts: &CreateOptions,
    created: &Created,
    keep_index: bool,
) -> Result<(), Failed> {
    let ps = &opts.pathspec;
    if opts.staged {
        let r = revert_staged(repo, git, created, opts);
        super::refresh_after(repo);
        return r;
    }
    let run = |args: Vec<String>| -> Result<(), Failed> {
        match git.cmd(&args).status() {
            Ok(0) => Ok(()),
            _ => Err(Failed),
        }
    };
    let clean = || -> Result<(), Failed> {
        if opts.untracked == Untracked::None {
            return Ok(());
        }
        let mut args = vec![
            "clean".to_owned(),
            "--force".to_owned(),
            "--quiet".to_owned(),
            "-d".to_owned(),
        ];
        if opts.untracked == Untracked::All {
            args.push("-x".to_owned());
        }
        // With a pathspec only the stashed files go (the pathspec may use
        // magic that `clean` does not evaluate the same way).
        if ps.is_empty() {
            run(args)?;
        }
        remove_stashed_untracked(repo, created)
    };
    if ps.is_empty() {
        run(vec!["reset".into(), "--hard".into(), "-q".into()])?;
        clean()?;
        if keep_index {
            run(vec![
                "read-tree".into(),
                "--reset".into(),
                "-u".into(),
                created.i.to_hex(),
            ])?;
        }
        return Ok(());
    }
    let source = if keep_index { created.i } else { created.head };
    let paths = tracked_matches(repo, opts, created.head)?;
    restore_paths(git, source, &paths)?;
    clean()
}

/// Removes the files recorded in the untracked commit that are still
/// there after `clean` (and the directories that became empty).
fn remove_stashed_untracked(repo: &Repo, created: &Created) -> Result<(), Failed> {
    let Some(u) = created.u else {
        return Ok(());
    };
    let Some((tree, _)) = commit_parts(repo, &u) else {
        return Ok(());
    };
    let root = repo.work_tree().map_err(|_| Failed)?;
    let mut files = Vec::new();
    super::flatten(repo, &tree, b"", &mut files).map_err(|_| Failed)?;
    for (p, _, _) in files {
        let full = root.join(String::from_utf8_lossy(&p).as_ref());
        if std::fs::symlink_metadata(&full).is_ok_and(|m| !m.is_dir()) {
            let _ = std::fs::remove_file(&full);
            rgit_unpack::remove_empty_parents(&root, &p);
        }
    }
    Ok(())
}

/// Tracked paths (in the index or in `HEAD`) that the pathspec selects.
fn tracked_matches(
    repo: &Repo,
    opts: &CreateOptions,
    head: ObjectId,
) -> Result<Vec<Vec<u8>>, Failed> {
    let spec = parse_pathspec(repo, opts)?;
    let mut paths = std::collections::BTreeSet::new();
    let (index, _) = repo.read_index().map_err(|_| Failed)?;
    let attrs = RefCell::new(PathspecAttrs::new(repo, &index, None));
    for e in index.entries() {
        if spec_matches(&spec, &attrs, e.path(), None) {
            paths.insert(e.path().to_vec());
        }
    }
    if let Some((tree, _)) = commit_parts(repo, &head) {
        let mut files = Vec::new();
        super::flatten(repo, &tree, b"", &mut files).map_err(|_| Failed)?;
        for (p, _, _) in files {
            if spec_matches(&spec, &attrs, &p, None) {
                paths.insert(p);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

/// `--staged`: removes the staged changes from the work tree and the
/// index. Files whose work tree version is the staged one go back to
/// `HEAD`; the staged changes of the others are applied in reverse, so
/// that later work tree changes stay.
fn revert_staged(
    repo: &Repo,
    git: &Git,
    created: &Created,
    opts: &CreateOptions,
) -> Result<(), Failed> {
    use rgit_index::Stage;
    use rgit_worktree::stat::Status;
    let spec = if opts.pathspec.is_empty() {
        None
    } else {
        Some(parse_pathspec(repo, opts)?)
    };
    let (index0, _) = repo.read_index().map_err(|_| Failed)?;
    let attrs = RefCell::new(PathspecAttrs::new(repo, &index0, None));
    let wanted = |p: &[u8]| {
        spec.as_ref()
            .is_none_or(|s| spec_matches(s, &attrs, p, None))
    };
    let mut head_files = Vec::new();
    let mut index_files = Vec::new();
    if let Some((t, _)) = commit_parts(repo, &created.head) {
        super::flatten(repo, &t, b"", &mut head_files).map_err(|_| Failed)?;
    }
    if let Some((t, _)) = commit_parts(repo, &created.i) {
        super::flatten(repo, &t, b"", &mut index_files).map_err(|_| Failed)?;
    }
    let mut versions: std::collections::BTreeMap<Vec<u8>, [Option<(u32, ObjectId)>; 2]> =
        std::collections::BTreeMap::new();
    for (p, m, id) in head_files {
        versions.entry(p).or_default()[0] = Some((m, id));
    }
    for (p, m, id) in index_files {
        versions.entry(p).or_default()[1] = Some((m, id));
    }
    let (index, mtime) = repo.read_index().map_err(|_| Failed)?;
    let checker = repo.checker(mtime).map_err(|_| Failed)?;
    let mut clean = Vec::new();
    let mut dirty = Vec::new();
    for (p, [h, i]) in versions {
        if h == i || !wanted(&p) {
            continue;
        }
        let same = match index.get(&p, Stage::MERGED) {
            Some(e) => matches!(checker.check(e), Ok(Status::Unchanged | Status::StatOnly)),
            None => checker.lstat(&p).ok().flatten().is_none(),
        };
        if same {
            clean.push(p);
        } else {
            dirty.push(String::from_utf8_lossy(&p).into_owned());
        }
    }
    if !dirty.is_empty() {
        let mut args = vec![
            "diff-tree".to_owned(),
            "-p".to_owned(),
            "--full-index".to_owned(),
            created.head.to_hex(),
            format!("{}^{{tree}}", created.i.to_hex()),
        ];
        args.push("--".to_owned());
        args.extend(dirty);
        let (code, patch) = git
            .cmd(&args)
            .env("GIT_LITERAL_PATHSPECS", "1")
            .output()
            .map_err(|_| Failed)?;
        if code != 0 {
            eprintln!("{}", msg::CANNOT_REMOVE_WORKTREE);
            return Err(Failed);
        }
        for args in [&["apply", "-R"][..], &["apply", "-R", "--cached"][..]] {
            let code = git
                .cmd(args)
                .stdin(patch.clone())
                .status()
                .map_err(|_| Failed)?;
            if code != 0 {
                eprintln!("{}", msg::CANNOT_REMOVE_WORKTREE);
                return Err(Failed);
            }
        }
    }
    restore_paths(git, created.head, &clean)
}

/// `restore --source=<source> --staged --worktree` for exactly `paths`.
fn restore_paths(git: &Git, source: ObjectId, paths: &[Vec<u8>]) -> Result<(), Failed> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut list = Vec::new();
    for p in paths {
        list.extend_from_slice(p);
        list.push(0);
    }
    let code = git
        .cmd(&[
            "restore".to_owned(),
            format!("--source={}", source.to_hex()),
            "--staged".to_owned(),
            "--worktree".to_owned(),
            "--pathspec-from-file=-".to_owned(),
            "--pathspec-file-nul".to_owned(),
        ])
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(list)
        .status()
        .map_err(|_| Failed)?;
    if code != 0 {
        return Err(Failed);
    }
    Ok(())
}

/// `git stash create [<message>]`: prints the commit of a new entry
/// without storing it.
pub(crate) fn create(ctx: &mut Context, args: &[String]) -> CmdResult {
    let message = args[1..].join(" ");
    let mut repo = open(ctx)?;
    repo.work_tree()?;
    let git = Git::new(ctx, &repo)?;
    if refresh_index(&repo).is_err() {
        eprintln!("error: {}", msg::COULD_NOT_WRITE_INDEX);
        return Err(Die::Exit(1));
    }
    let Some(head) = head(&mut repo) else {
        eprintln!("{}", msg::NO_INITIAL_COMMIT);
        return Err(Die::Exit(128));
    };
    let opts = CreateOptions {
        message: (!message.is_empty()).then_some(message),
        untracked: Untracked::None,
        staged: false,
        pathspec: Vec::new(),
        shown: Vec::new(),
        globals: repository::pathspec_globals(ctx),
    };
    match create_stash(&repo, &git, &head, &opts) {
        Ok(Some(c)) => {
            say(&c.w.to_hex());
            Ok(0)
        }
        Ok(None) => Ok(0),
        Err(Failed) => Err(Die::Exit(1)),
    }
}
