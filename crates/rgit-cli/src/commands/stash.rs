// SPDX-License-Identifier: MIT
//! `git stash` (Documentation/git-stash.adoc, config/stash.adoc).
//!
//! A stash entry is the commit structure the DISCUSSION section of
//! git-stash(1) describes: a work tree commit `W` whose parents are the
//! `HEAD` commit, a commit `I` of the index (child of `HEAD`) and, with
//! `-u`/`-a`, a root commit `U` of the untracked files. The messages
//! (`WIP on <branch>: <abbrev> <subject>`, `index on ...`, `untracked
//! files on ...`) and their newlines were taken from the `git` binary, so
//! the same inputs and dates give the same commit ids. `refs/stash` and
//! its reflog hold the list.
//!
//! Implemented: `push` (also bare `stash` and `save`) with `-k`, `-S`,
//! `-u`, `-a`, `-m`, `-q`, pathspecs and `--pathspec-from-file`; `list`;
//! `show` (`stash.showStat`, `stash.showPatch`,
//! `stash.showIncludeUntracked`, `-u`, `--only-untracked`); `apply` and
//! `pop` with `--index` and the label options; `drop`, `clear`,
//! `branch`, `create`, `store`, `export`, `import`.
//!
//! `export` and `import` are in [`export`].
//!
//! Not yet: `-p`/`--patch` (#109), and `list`
//! with log options other than counts and `--format`/`--pretty=format:`
//! (they need `log -g`).
//!
//! Several steps run rgit's own commands as child processes (see
//! [`child`]): resetting, cleaning, the long status after `apply`, the
//! diff of `show`.

#[path = "stash/apply.rs"]
mod apply;
#[path = "stash/child.rs"]
mod child;
#[path = "stash/create.rs"]
mod create;
#[path = "stash/export.rs"]
mod export;

use std::io::Write;

use rgit_hash::ObjectId;
use rgit_ident::ident::{IdentConfig, Resolver as IdentResolver, Role, Strictness};
use rgit_object::{Object, Signature, SignatureRef};
use rgit_refs::reflog::{Entry as ReflogEntry, FilesReflog};
use rgit_refs::{RefUpdate, Transaction};

use crate::messages::stash as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::Mode;
use crate::repository::Repo;

/// The reference holding the newest entry.
const STASH_REF: &str = "refs/stash";

fn usage_lines() -> Vec<&'static str> {
    vec![
        msg::USAGE_LIST,
        msg::USAGE_SHOW,
        msg::USAGE_DROP,
        msg::USAGE_POP,
        msg::USAGE_APPLY,
        msg::USAGE_BRANCH,
        msg::USAGE_PUSH,
        msg::USAGE_SAVE,
        msg::USAGE_CLEAR,
        msg::USAGE_CREATE,
        msg::USAGE_STORE,
        msg::USAGE_EXPORT,
        msg::USAGE_IMPORT,
    ]
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    let Some(first) = args.first() else {
        return create::push(ctx, &[], create::PushKind::Implicit);
    };
    match first.as_str() {
        "-h" | "--help-all" => {
            let mut parser = Parser::new(&usage_lines(), vec![]);
            if let Err(e) = parser.parse(&["stash".to_owned(), first.clone()]) {
                // Help goes to standard output with exit code 0.
                print!("{}{}", fix_save_usage(&e.stdout), e.stderr);
            }
            Ok(0)
        }
        "push" => create::push(ctx, args, create::PushKind::Push),
        "save" => create::push(ctx, args, create::PushKind::Save),
        "create" => create::create(ctx, args),
        "store" => store_cmd(ctx, args),
        "list" => list(ctx, &args[1..]),
        "show" => show(ctx, &args[1..]),
        "drop" => drop_cmd(ctx, args),
        "clear" => clear(ctx, args),
        "apply" => apply::apply_cmd(ctx, args, false),
        "pop" => apply::apply_cmd(ctx, args, true),
        "branch" => apply::branch(ctx, args),
        "export" => export::export(ctx, args),
        "import" => export::import(ctx, args),
        a if a.starts_with('-') => create::push(ctx, args, create::PushKind::Implicit),
        a => Err(Die::Fatal(msg::no_assumed_push(a))),
    }
}

/// Parses the options of a subcommand; `-h` prints the help to standard
/// output and exits with 0, as `git stash` does.
fn parse(parser: &mut Parser<'_>, args: &[String]) -> Result<Vec<String>, Die> {
    match parser.parse(args) {
        Ok(p) => Ok(p.args),
        Err(mut e) => {
            e.stdout = fix_save_usage(&e.stdout);
            e.stderr = fix_save_usage(&e.stderr);
            if e.code == 129 && args.iter().any(|a| a == "-h" || a == "--help-all") {
                print!("{}{}", e.stdout, e.stderr);
                return Err(Die::Exit(0));
            }
            Err(Die::Output(e))
        }
    }
}

/// git aligns the second line of the `save` usage like the `push` one.
fn fix_save_usage(text: &str) -> String {
    let wide = format!("\n{}[-u", " ".repeat(22));
    let narrow = format!("\n{}[-u", " ".repeat(17));
    text.replace(&wide, &narrow)
}

/// Prints `text` to standard output now (children may write after it).
fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

// --- the repository and its HEAD ---------------------------------------

pub(crate) struct Head {
    pub(crate) commit: ObjectId,
    /// Short branch name or "(no branch)".
    pub(crate) branch: String,
}

fn open(ctx: &Context) -> Result<Repo, Die> {
    Repo::open(ctx, Mode::Required)
}

fn head(repo: &mut Repo) -> Option<Head> {
    let commit = repo.resolve("HEAD")?;
    let commit = repo.peel_to(commit, rgit_object::ObjectKind::Commit)?;
    let branch = match repo.refs.head_target() {
        Some(t) => t.strip_prefix("refs/heads/").unwrap_or(&t).to_owned(),
        None => msg::NO_BRANCH.to_owned(),
    };
    Some(Head { commit, branch })
}

/// The tree and parents of commit `id`.
fn commit_parts(repo: &Repo, id: &ObjectId) -> Option<(ObjectId, Vec<ObjectId>)> {
    match repo.read_object(id)? {
        Object::Commit(c) => Some((c.tree().ok()?, c.parents().ok()?)),
        _ => None,
    }
}

/// The identity for `role`; without a configured one, a stash commit is
/// recorded with a fixed fallback identity instead of failing.
fn signature(resolver: &IdentResolver<'_>, role: Role) -> Result<Signature, Die> {
    let raw = match resolver.ident(role, Strictness::Strict) {
        Ok(ident) => ident.to_bstring().to_vec(),
        Err(_) => {
            let (time, tz) = resolver.date(role).map_err(|e| {
                eprint!("{}", e.stderr_text());
                Die::Exit(rgit_options::EXIT_FATAL)
            })?;
            let sign = if tz < 0 { '-' } else { '+' };
            format!(
                "{} <{}> {time} {sign}{:04}",
                msg::FALLBACK_NAME,
                msg::FALLBACK_EMAIL,
                tz.abs()
            )
            .into_bytes()
        }
    };
    SignatureRef::parse(&raw)
        .and_then(|s| s.to_owned())
        .ok_or_else(|| Die::Fatal(String::from_utf8_lossy(&raw).into_owned()))
}

/// Writes a commit as `commit-tree` would.
fn write_commit(
    repo: &Repo,
    tree: ObjectId,
    parents: &[ObjectId],
    message: &[u8],
) -> Result<ObjectId, Die> {
    let env = rgit_config::SystemEnv::default();
    let icfg = IdentConfig::from_config(&repo.config).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let resolver = IdentResolver::new(&icfg, &env);
    let author = signature(&resolver, Role::Author)?;
    let committer = signature(&resolver, Role::Committer)?;
    let encoding = repo
        .config
        .get("i18n.commitencoding")
        .and_then(|e| e.value.as_ref())
        .map(|v| v.as_slice())
        .filter(|e| !e.eq_ignore_ascii_case(b"utf-8") && !e.eq_ignore_ascii_case(b"utf8"))
        .map(|e| e.to_vec());
    let new = rgit_commit::NewCommit {
        tree,
        parents,
        author: &author,
        committer: &committer,
        encoding: encoding.as_deref(),
        message,
    };
    rgit_commit::create_commit(repo.odb(), &new).map_err(|e| Die::Fatal(e.to_string()))
}

/// Refreshes the stat data of the index and writes it back, as git does
/// before it compares anything. Unmerged paths are reported
/// (`<path>: needs merge`) and fail.
fn refresh_index(repo: &Repo) -> Result<(), ()> {
    refresh(repo, true)
}

/// [`refresh_index`] after an operation: unmerged paths are left alone.
fn refresh_after(repo: &Repo) {
    let _ = refresh(repo, false);
}

fn refresh(repo: &Repo, strict: bool) -> Result<(), ()> {
    let index_path = repo.setup.cwd.join(repo.index_path());
    let lock = match rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(());
        }
    };
    let Ok((mut index, mtime)) = repo.read_index() else {
        return Err(());
    };
    if strict && index.has_conflicts() {
        let mut out = std::io::stdout().lock();
        let mut last: Option<&[u8]> = None;
        for e in index.entries() {
            if e.stage().is_conflict() && last != Some(e.path()) {
                last = Some(e.path());
                let _ = writeln!(out, "{}", msg::needs_merge(&String::from_utf8_lossy(e.path())));
            }
        }
        let _ = out.flush();
        return Err(());
    }
    let Ok(checker) = repo.checker(mtime) else {
        return Err(());
    };
    crate::switch_branch::refresh_stat(&mut index, &checker);
    if let Some(reference) = mtime {
        let _ = rgit_worktree::stat::smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock).map_err(|_| ())
}

// --- refs/stash and its reflog -----------------------------------------

fn reflogs(repo: &Repo) -> FilesReflog {
    FilesReflog::new(
        repo.setup.git_dir.clone(),
        repo.setup.common_dir.clone(),
        repo.setup.format.object_format,
    )
    .with_shared(refs_support::shared_repository(&repo.config))
}

/// The entries of the stash reflog, newest first.
fn stash_entries(repo: &Repo) -> Vec<ReflogEntry> {
    let mut v = reflogs(repo)
        .read(STASH_REF)
        .ok()
        .flatten()
        .unwrap_or_default();
    v.reverse();
    v
}

fn stash_exists(repo: &mut Repo) -> bool {
    repo.resolve(STASH_REF).is_some()
}

/// Points `refs/stash` at `w` with a reflog entry `message`.
fn update_stash_ref(repo: &Repo, w: ObjectId, message: &str) -> Result<(), String> {
    let objects = Objects::open(&repo.setup);
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
    let update = RefUpdate::update(STASH_REF, w, None)
        .message(message)
        .create_reflog(true);
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| e.to_string())
}

/// Checks that `w` looks like a stash commit and stores it.
fn do_store(repo: &Repo, w: ObjectId, message: &str, quiet: bool) -> Result<(), Die> {
    match commit_parts(repo, &w) {
        Some((_, parents)) if parents.len() >= 2 => {}
        _ => return Err(Die::Fatal(msg::not_stash_like(&w.to_hex()))),
    }
    if let Err(e) = update_stash_ref(repo, w, message) {
        if !quiet {
            eprintln!("error: {e}");
        }
        return Err(Die::Exit(1));
    }
    Ok(())
}

fn store_cmd(ctx: &mut Context, args: &[String]) -> CmdResult {
    let quiet = Cell::new(0);
    let message = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::USAGE_STORE],
        vec![
            Opt::quiet(&quiet, msg::HELP_QUIET_STORE),
            Opt::string('m', "message", &message, "message", msg::HELP_MESSAGE),
        ],
    );
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    drop(parser);
    if rest.len() != 1 {
        eprintln!("{}", msg::STORE_NEEDS_ONE);
        return Err(Die::Exit(128));
    }
    let mut repo = open(ctx)?;
    let Some(w) = repo.resolve(&rest[0]) else {
        return Err(Die::Fatal(msg::not_stash_like(&rest[0])));
    };
    let message = message
        .borrow()
        .clone()
        .unwrap_or_else(|| msg::STORE_DEFAULT_MSG.to_owned());
    do_store(&repo, w, &message, quiet.get() > 0)?;
    Ok(0)
}

// --- naming an entry -----------------------------------------------------

/// A stash entry named on the command line.
pub(crate) struct Info {
    pub(crate) w: ObjectId,
    pub(crate) w_tree: ObjectId,
    pub(crate) b_commit: ObjectId,
    pub(crate) b_tree: ObjectId,
    pub(crate) i_tree: ObjectId,
    pub(crate) u_tree: Option<ObjectId>,
    /// The name as shown in messages (`refs/stash@{0}`, `stash@{1}`).
    pub(crate) revision: String,
    /// The name refers to an entry of the stash reflog.
    pub(crate) is_stash_ref: bool,
}

/// `git stash <cmd> [<stash>]`: the entry named by `args` (at most one).
fn get_info(repo: &mut Repo, args: &[String]) -> Result<Info, Die> {
    if args.len() > 1 {
        let list: String = args.iter().map(|a| format!(" '{a}'")).collect();
        eprintln!("{}", msg::too_many_revisions(&list));
        return Err(Die::Exit(128));
    }
    let revision = match args.first() {
        None => {
            if !stash_exists(repo) {
                eprintln!("{}", msg::NO_ENTRIES);
                return Err(Die::Exit(128));
            }
            format!("{STASH_REF}@{{0}}")
        }
        Some(a) if !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()) => {
            format!("{STASH_REF}@{{{a}}}")
        }
        Some(a) => a.clone(),
    };
    let w = {
        let mut resolver = crate::revision::Resolver::new(&mut *repo);
        match resolver.resolve(&revision) {
            Ok(r) => r.oid,
            Err(crate::revision::Failure::Fatal(m)) => return Err(Die::Fatal(m)),
            Err(_) => {
                return Err(Die::Error(msg::not_valid_reference(&revision), 128));
            }
        }
    };
    let not_like = || Die::Fatal(msg::not_stash_like(&revision));
    let w = repo
        .peel_to(w, rgit_object::ObjectKind::Commit)
        .ok_or_else(not_like)?;
    let (w_tree, parents) = commit_parts(repo, &w).ok_or_else(not_like)?;
    if parents.len() < 2 {
        return Err(not_like());
    }
    let (b_tree, _) = commit_parts(repo, &parents[0]).ok_or_else(not_like)?;
    let (i_tree, _) = commit_parts(repo, &parents[1]).ok_or_else(not_like)?;
    let u_tree = match parents.get(2) {
        Some(u) => Some(commit_parts(repo, u).ok_or_else(not_like)?.0),
        None => None,
    };
    let is_stash_ref = names_stash_ref(repo, &revision);
    Ok(Info {
        w,
        w_tree,
        b_commit: parents[0],
        b_tree,
        i_tree,
        u_tree,
        revision,
        is_stash_ref,
    })
}

/// Whether `rev` (`<ref>@{<n>}` or a reference) names `refs/stash`.
fn names_stash_ref(repo: &Repo, rev: &str) -> bool {
    let base = match rev.find("@{") {
        Some(i) => &rev[..i],
        None => rev,
    };
    let base = if base.is_empty() { "HEAD" } else { base };
    let _ = repo;
    base == STASH_REF || base == "stash"
}

/// The position (newest first) of the entry `rev` names in the stash
/// reflog.
fn reflog_position(rev: &str, entries: &[ReflogEntry]) -> Option<usize> {
    let start = rev.find("@{")?;
    let spec = rev[start + 2..].strip_suffix('}')?;
    let ctx = rgit_date::DateContext::from_env();
    match rgit_refs::reflog::Selector::parse(spec, &ctx)? {
        rgit_refs::reflog::Selector::Nth(n) => (n < entries.len()).then_some(n),
        sel => {
            let mut oldest_first: Vec<ReflogEntry> = entries.to_vec();
            oldest_first.reverse();
            let target = rgit_refs::reflog::lookup::Target {
                refname: STASH_REF,
                display: STASH_REF,
                current: None,
            };
            let r = rgit_refs::reflog::resolve(&oldest_first, sel, target).ok()?;
            r.index.map(|i| entries.len() - 1 - i)
        }
    }
}

/// Removes the entry `info` names from the stash reflog (and the
/// reference when it was the last one), printing "Dropped ...".
fn drop_entry(repo: &Repo, info: &Info, quiet: bool) -> Result<(), Die> {
    let entries = stash_entries(repo);
    let Some(pos) = reflog_position(&info.revision, &entries) else {
        eprintln!("error: {}", msg::not_a_reflog(&info.revision));
        eprintln!("error: {}", msg::could_not_drop(&info.revision));
        return Err(Die::Exit(128));
    };
    let mut rest = entries;
    rest.remove(pos);
    // Rewrite the old values so the chain stays continuous.
    rest.reverse();
    let zero = repo.hash.null();
    let mut prev = zero;
    for e in rest.iter_mut() {
        e.old = prev;
        prev = e.new;
    }
    let logs = reflogs(repo);
    let store = refs_support::ref_store(&repo.setup, &repo.config);
    let objects = Objects::open(&repo.setup);
    let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects).without_reflogs();
    let fail = |e: String| {
        eprintln!("error: {e}");
        eprintln!("error: {}", msg::could_not_drop(&info.revision));
        Die::Exit(128)
    };
    if rest.is_empty() {
        let mut tx = Transaction::new(&store);
        tx.push(RefUpdate::delete(STASH_REF, repo.hash, None))
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| fail(e.to_string()))?;
        let _ = logs.delete(STASH_REF);
    } else {
        let newest = rest.last().expect("entries").new;
        let mut tx = Transaction::new(&store);
        tx.push(RefUpdate::update(STASH_REF, newest, None))
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| fail(e.to_string()))?;
        let mut data = Vec::new();
        for e in &rest {
            data.extend_from_slice(&e.to_line());
        }
        let path = logs.path(STASH_REF);
        let mut lock = rgit_fs::LockFile::acquire(&path, &rgit_fs::LockOptions::default())
            .map_err(|e| fail(e.to_string()))?;
        lock.write_all(&data).map_err(|e| fail(e.to_string()))?;
        lock.commit().map_err(|e| fail(e.to_string()))?;
    }
    if !quiet {
        say(&msg::dropped(&info.revision, &info.w.to_hex()));
    }
    Ok(())
}

fn drop_cmd(ctx: &mut Context, args: &[String]) -> CmdResult {
    let quiet = Cell::new(0);
    let mut parser = Parser::new(
        &[msg::USAGE_DROP],
        vec![Opt::quiet(&quiet, msg::HELP_QUIET)],
    );
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    drop(parser);
    let mut repo = open(ctx)?;
    let info = get_info(&mut repo, &rest)?;
    if !info.is_stash_ref {
        return Err(Die::Error(msg::not_stash_reference(&info.revision), 128));
    }
    drop_entry(&repo, &info, quiet.get() > 0)?;
    Ok(0)
}

fn clear(ctx: &mut Context, args: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_CLEAR], vec![]);
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    if !rest.is_empty() {
        return Err(Die::Error(msg::CLEAR_WITH_ARGS.to_owned(), 128));
    }
    let mut repo = open(ctx)?;
    if stash_exists(&mut repo) {
        let store = refs_support::ref_store(&repo.setup, &repo.config);
        let objects = Objects::open(&repo.setup);
        let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects).without_reflogs();
        let mut tx = Transaction::new(&store);
        tx.push(RefUpdate::delete(STASH_REF, repo.hash, None))
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| Die::Error(e.to_string(), 1))?;
    }
    let _ = reflogs(&repo).delete(STASH_REF);
    Ok(0)
}

// --- list and show -------------------------------------------------------

/// `git stash list`: `log -g --format="%gd: %gs" refs/stash`. Counts
/// (`-<n>`, `-n <n>`, `--max-count`) and `--format`/`--pretty=format:`
/// are handled here; other log options go to `log` for each entry.
fn list(ctx: &mut Context, args: &[String]) -> CmdResult {
    if args.iter().any(|a| a == "-h") {
        println!("usage: {}\n", msg::USAGE_LIST);
        return Ok(0);
    }
    let mut max: Option<usize> = None;
    let mut format: Option<String> = None;
    let mut patch = false;
    let mut extra: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(n) = a
            .strip_prefix('-')
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            max = n.parse().ok();
        } else if a == "-n" || a == "--max-count" {
            max = it.next().and_then(|n| n.parse().ok());
        } else if let Some(n) = a
            .strip_prefix("--max-count=")
            .or_else(|| a.strip_prefix("-n"))
        {
            max = n.parse().ok();
        } else if let Some(f) = a
            .strip_prefix("--format=")
            .or_else(|| a.strip_prefix("--pretty=format:"))
            .or_else(|| a.strip_prefix("--pretty=tformat:"))
        {
            format = Some(f.to_owned());
        } else if a == "--oneline" {
            format = Some("%h %gD: %gs".to_owned());
        } else if a == "-p" || a == "-u" || a == "--patch" {
            patch = true;
        } else if a == "--" {
            break;
        } else if matches!(
            a.as_str(),
            "--cc" | "-c" | "-m" | "--stat" | "--numstat" | "--name-only" | "--name-status"
        ) || a.starts_with("--diff-merges")
        {
            return Err(Die::Fatal(msg::not_supported(&format!("list {a}"))));
        } else if a.starts_with('-') {
            extra.push(a.clone());
        } else {
            return Err(Die::Fatal(msg::unrecognized_argument(a)));
        }
    }
    let mut repo = open(ctx)?;
    if !stash_exists(&mut repo) {
        return Ok(0);
    }
    let entries = stash_entries(&repo);
    let count = max.unwrap_or(usize::MAX).min(entries.len());
    let format = format.unwrap_or_else(|| "%gd: %gs".to_owned());
    let native = extra.is_empty() && is_simple_format(&format);
    let git = child::Git::new(ctx, &repo)?;
    for (n, e) in entries.iter().take(count).enumerate() {
        let line = expand_reflog_format(&format, n, e);
        if native {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&line);
            let _ = out.write_all(b"\n");
            let _ = out.flush();
        } else {
            // `log` expands the rest of the format; the reflog parts are
            // literal text by now.
            let mut fmt = b"--format=".to_vec();
            fmt.extend_from_slice(&line);
            let mut argv: Vec<std::ffi::OsString> = vec![
                "log".into(),
                "--no-walk".into(),
                "--first-parent".into(),
                os_from_bytes(fmt),
            ];
            argv.extend(extra.iter().map(Into::into));
            argv.push(e.new.to_hex().into());
            argv.push("--".into());
            let code = git.cmd(&argv).status()?;
            if code != 0 {
                return Ok(code);
            }
        }
        if patch {
            let base = format!("{}^1", e.new.to_hex());
            let (_, diff) = git
                .cmd(&["diff-tree", "-p", base.as_str(), &e.new.to_hex()])
                .output()?;
            if !diff.is_empty() {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(b"\n");
                let _ = out.write_all(&diff);
                let _ = out.flush();
            }
        }
    }
    Ok(0)
}

#[cfg(unix)]
fn os_from_bytes(b: Vec<u8>) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(b)
}

#[cfg(not(unix))]
fn os_from_bytes(b: Vec<u8>) -> std::ffi::OsString {
    String::from_utf8_lossy(&b).into_owned().into()
}

/// A format that uses only the reflog placeholders.
fn is_simple_format(f: &str) -> bool {
    let b = f.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            match b.get(i + 1) {
                Some(b'%') | Some(b'n') => i += 2,
                Some(b'g') if matches!(b.get(i + 2), Some(b'd' | b'D' | b's')) => i += 3,
                _ => return false,
            }
        } else {
            i += 1;
        }
    }
    true
}

/// Replaces `%gd`, `%gD`, `%gs` (and, for the simple path, `%n`, `%%`)
/// in `f` for entry `n`. Other placeholders are kept; a `%` in the
/// replacement text is doubled when other placeholders remain.
fn expand_reflog_format(f: &str, n: usize, e: &ReflogEntry) -> Vec<u8> {
    let simple = is_simple_format(f);
    let b = f.as_bytes();
    let mut out = Vec::new();
    let lit = |out: &mut Vec<u8>, text: &[u8]| {
        for &c in text {
            if c == b'%' && !simple {
                out.push(b'%');
            }
            out.push(c);
        }
    };
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            match (b.get(i + 1), b.get(i + 2)) {
                (Some(b'g'), Some(b'd')) => {
                    lit(&mut out, format!("stash@{{{n}}}").as_bytes());
                    i += 3;
                    continue;
                }
                (Some(b'g'), Some(b'D')) => {
                    lit(&mut out, format!("{STASH_REF}@{{{n}}}").as_bytes());
                    i += 3;
                    continue;
                }
                (Some(b'g'), Some(b's')) => {
                    lit(&mut out, &e.message);
                    i += 3;
                    continue;
                }
                (Some(b'n'), _) if simple => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                (Some(b'%'), _) if simple => {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                (Some(&c), _) => {
                    out.push(b'%');
                    out.push(c);
                    i += 2;
                    continue;
                }
                (None, _) => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `git stash show`: `diff` between the base commit and the stash.
fn show(ctx: &mut Context, args: &[String]) -> CmdResult {
    if args.first().is_some_and(|a| a == "-h") {
        print!("{}", show_usage());
        return Ok(0);
    }
    #[derive(PartialEq, Clone, Copy)]
    enum Untracked {
        Default,
        No,
        Include,
        Only,
    }
    let mut untracked = Untracked::Default;
    let mut diff_opts: Vec<String> = Vec::new();
    let mut revs: Vec<String> = Vec::new();
    let mut dashdash = false;
    for a in args {
        if dashdash {
            revs.push(a.clone());
            continue;
        }
        match a.as_str() {
            "-u" | "--include-untracked" => untracked = Untracked::Include,
            "--no-include-untracked" => untracked = Untracked::No,
            "--only-untracked" => untracked = Untracked::Only,
            "--" => dashdash = true,
            s if s.starts_with('-') => diff_opts.push(a.clone()),
            _ => revs.push(a.clone()),
        }
    }
    let mut repo = open(ctx)?;
    let info = get_info(&mut repo, &revs)?;
    let cfg_bool = |k: &str, d: bool| repo.config.bool(k).ok().flatten().unwrap_or(d);
    if untracked == Untracked::Default {
        untracked = if cfg_bool("stash.showincludeuntracked", false) {
            Untracked::Include
        } else {
            Untracked::No
        };
    }
    if diff_opts.is_empty() {
        if cfg_bool("stash.showstat", true) {
            diff_opts.push("--stat".to_owned());
        }
        if cfg_bool("stash.showpatch", false) {
            diff_opts.push("-p".to_owned());
        }
        if diff_opts.is_empty() {
            return Ok(0);
        }
    }
    // With untracked files the stashed state is put into a temporary
    // index and compared with `diff --cached`, so that no objects are
    // written.
    let git = child::Git::new(ctx, &repo)?;
    let mut argv: Vec<String> = vec!["diff".to_owned()];
    argv.extend(diff_opts);
    let tmp = child::TempIndex::new(&repo, "stash-show-index");
    let index = match (untracked, info.u_tree) {
        (Untracked::Only, Some(u)) => Some(untracked_index(&repo, None, u)?),
        (Untracked::Only, None) => return Ok(0),
        (Untracked::Include, Some(u)) => Some(untracked_index(&repo, Some(info.w_tree), u)?),
        _ => None,
    };
    match index {
        Some(index) => {
            index
                .write_file(&tmp.path, &rgit_fs::LockOptions::default(), &Default::default())
                .map_err(|e| Die::Fatal(e.to_string()))?;
            argv.push("--cached".to_owned());
            argv.push(if untracked == Untracked::Only {
                ObjectId::empty_tree(repo.hash).to_hex()
            } else {
                info.b_tree.to_hex()
            });
        }
        None => {
            argv.push(info.b_commit.to_hex());
            argv.push(info.w.to_hex());
        }
    }
    argv.push("--".to_owned());
    let mut call = git.cmd(&argv);
    if tmp.path.exists() {
        call = call.index(&tmp.path);
    }
    let (code, err) = call.status_stderr()?;
    if code == rgit_options::EXIT_USAGE {
        // An option `diff` does not know: the usage of `stash show`.
        eprint!("{}", show_usage());
    } else {
        let mut e = std::io::stderr().lock();
        let _ = e.write_all(&err);
    }
    Ok(code)
}

fn show_usage() -> String {
    let cells = (Cell::new(false), Cell::new(false));
    let parser = Parser::new(
        &[msg::USAGE_SHOW],
        vec![
            Opt::flag('u', "include-untracked", &cells.0, msg::HELP_SHOW_UNTRACKED),
            Opt::flag(None, "only-untracked", &cells.1, msg::HELP_ONLY_UNTRACKED).noneg(),
        ],
    );
    parser.usage_text(false)
}

/// An index holding the files of `w` (if any) and of the untracked
/// tree `u`.
fn untracked_index(
    repo: &Repo,
    w: Option<ObjectId>,
    u: ObjectId,
) -> Result<rgit_index::Index, Die> {
    use rgit_index::{AddOptions, Entry, Index, Mode as IndexMode, Stage};
    let mut files = Vec::new();
    if let Some(w) = w {
        flatten(repo, &w, b"", &mut files)?;
    }
    let mut extra = Vec::new();
    flatten(repo, &u, b"", &mut extra)?;
    let mut index = Index::new(repo.hash);
    let opts = AddOptions {
        ok_to_replace: true,
        ..Default::default()
    };
    for (path, mode, id) in files {
        let _ = index.add(Entry::new(path, Stage::MERGED, IndexMode(mode), id), opts);
    }
    for (path, mode, id) in extra {
        if index.get(&path, Stage::MERGED).is_some() {
            return Err(Die::Error(
                msg::duplicate_untracked(&String::from_utf8_lossy(&path)),
                128,
            ));
        }
        let _ = index.add(Entry::new(path, Stage::MERGED, IndexMode(mode), id), opts);
    }
    Ok(index)
}

/// The files of `tree` with their full paths.
pub(crate) fn flatten(
    repo: &Repo,
    tree: &ObjectId,
    base: &[u8],
    out: &mut Vec<(Vec<u8>, u32, ObjectId)>,
) -> Result<(), Die> {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return Err(Die::Fatal(msg::not_stash_like(&tree.to_hex())));
    };
    for e in t.entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical();
        if mode.is_tree() {
            path.push(b'/');
            flatten(repo, &e.oid, &path, out)?;
        } else {
            out.push((path, mode.bits(), e.oid));
        }
    }
    Ok(())
}
