// SPDX-License-Identifier: MIT
//! `git commit` (Documentation/git-commit.adoc,
//! Documentation/config/commit.adoc), core slice of #45:
//!
//! - message from `-m` (repeatable), `-F <file>` (`-` for stdin) or the
//!   editor with the commented status template (`COMMIT_EDITMSG`,
//!   `--[no-]status`, `commit.status`); `--cleanup` / `commit.cleanup`,
//!   `core.commentChar` / `core.commentString`;
//! - `-a`, `<pathspec>...` with `-o` (default) and `-i`, `--allow-empty`,
//!   `--allow-empty-message`, `-u[<mode>]`;
//! - `--amend` (message and author reuse, `--no-edit`, `--reset-author`),
//!   `--author`, `--date`, `-q`;
//! - the summary (`[main (root-commit) abc1234] subject`, ` Author:`,
//!   ` Date:`, the change counts and create/delete/rename/mode lines), the
//!   "nothing to commit" status and the reflog entries.
//!
//! Hooks are not run (M2); an enabled hook prints a warning instead
//! (decision on #45). Not yet: templates, `-s`, `--trailer`, `-C`/`-c`,
//! `--fixup`/`--squash`, `-v`, `--dry-run` and the status formats, `-S`,
//! `-p`/`--interactive`, concluding merges, similarity renames in the
//! summary (#145), the implicit-committer notice.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_commit::message::{self as cmsg, Cleanup};
use rgit_config::SystemEnv;
use rgit_date::{DateContext, DateMode};
use rgit_hash::ObjectId;
use rgit_ident::ident::{IdentConfig, Resolver, Role, Strictness};
use rgit_index::{AddOptions, Entry, Index, Mode as IndexMode, Stage, WriteOptions};
use rgit_object::{Object, ObjectKind, Signature, SignatureRef};
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_refs::{RefUpdate, Transaction};
use rgit_worktree::stat::smudge_racy_entries;

use super::add::entry_for;
use super::status::Untracked;
use crate::messages::commit as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::tree_changes::{self, Flat, Pair};
use crate::wt_status::{self, Render};

/// A piece of the message in command-line order.
enum Piece {
    Message(String),
    File(String),
}

/// The parsed command line.
struct Args {
    pieces: Vec<Piece>,
    edit: Option<bool>,
    all: bool,
    include: bool,
    only: bool,
    amend: bool,
    quiet: bool,
    allow_empty: bool,
    allow_empty_message: bool,
    reset_author: bool,
    no_verify: bool,
    no_post_rewrite: bool,
    author: Option<String>,
    date: Option<String>,
    cleanup: Option<String>,
    status: Option<bool>,
    untracked: Option<String>,
    paths: Vec<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    let pieces: RefCell<Vec<Piece>> = RefCell::new(Vec::new());
    let edit = Cell::new(-1);
    let status = Cell::new(-1);
    let verify = Cell::new(-1);
    let quiet = Cell::new(false);
    let all = Cell::new(false);
    let include = Cell::new(false);
    let only = Cell::new(false);
    let amend = Cell::new(false);
    let allow_empty = Cell::new(false);
    let allow_empty_message = Cell::new(false);
    let reset_author = Cell::new(false);
    let no_verify = Cell::new(false);
    let no_post_rewrite = Cell::new(false);
    let author = RefCell::new(None);
    let date = RefCell::new(None);
    let cleanup = RefCell::new(None);
    let untracked = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::group(msg::GROUP_MESSAGE),
            Opt::callback('F', "file", Some("file"), msg::HELP_FILE, |v, _| {
                pieces
                    .borrow_mut()
                    .push(Piece::File(v.unwrap_or_default().to_string()));
                Ok(())
            }),
            Opt::string(None, "author", &author, "author", msg::HELP_AUTHOR),
            Opt::string(None, "date", &date, "date", msg::HELP_DATE),
            Opt::callback('m', "message", Some("message"), msg::HELP_MESSAGE, |v, _| {
                pieces
                    .borrow_mut()
                    .push(Piece::Message(v.unwrap_or_default().to_string()));
                Ok(())
            }),
            Opt::flag(None, "reset-author", &reset_author, msg::HELP_RESET_AUTHOR),
            Opt::bool('e', "edit", &edit, msg::HELP_EDIT),
            Opt::string(None, "cleanup", &cleanup, "mode", msg::HELP_CLEANUP),
            Opt::bool(None, "status", &status, msg::HELP_STATUS),
            Opt::group(msg::GROUP_CONTENTS),
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::flag('i', "include", &include, msg::HELP_INCLUDE),
            Opt::flag('o', "only", &only, msg::HELP_ONLY),
            Opt::flag('n', "no-verify", &no_verify, msg::HELP_NO_VERIFY).noneg(),
            Opt::bool(None, "verify", &verify, msg::HELP_VERIFY).noneg(),
            Opt::flag(None, "amend", &amend, msg::HELP_AMEND),
            Opt::flag(None, "no-post-rewrite", &no_post_rewrite, msg::HELP_NO_POST_REWRITE)
                .noneg(),
            Opt::string('u', "untracked-files", &untracked, "mode", msg::HELP_UNTRACKED)
                .optional(Some("all")),
            Opt::flag(None, "allow-empty", &allow_empty, msg::HELP_ALLOW_EMPTY),
            Opt::flag(
                None,
                "allow-empty-message",
                &allow_empty_message,
                msg::HELP_ALLOW_EMPTY_MESSAGE,
            ),
        ],
    );
    let paths = parser.parse(argv)?.args;
    drop(parser);
    Ok(Args {
        pieces: pieces.into_inner(),
        edit: (edit.get() >= 0).then(|| edit.get() == 1),
        all: all.get(),
        include: include.get(),
        only: only.get(),
        amend: amend.get(),
        quiet: quiet.get(),
        allow_empty: allow_empty.get(),
        allow_empty_message: allow_empty_message.get(),
        reset_author: reset_author.get(),
        no_verify: if verify.get() >= 0 { verify.get() == 0 } else { no_verify.get() },
        no_post_rewrite: no_post_rewrite.get(),
        author: author.into_inner(),
        date: date.into_inner(),
        cleanup: cleanup.into_inner(),
        status: (status.get() >= 0).then(|| status.get() == 1),
        untracked: untracked.into_inner(),
        paths,
    })
}

fn fatal(s: impl Into<String>) -> Die {
    Die::Fatal(s.into())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn config_bool(repo: &Repo, key: &str) -> Option<bool> {
    repo.config.bool(key).ok().flatten()
}

fn config_str(repo: &Repo, key: &str) -> Option<String> {
    repo.config
        .string(key)
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

fn untracked_mode(value: &str) -> Option<Untracked> {
    Some(match value {
        "no" => Untracked::No,
        "normal" => Untracked::Normal,
        "all" => Untracked::All,
        _ => match rgit_config::value::parse_bool(Some(value.as_bytes())) {
            Some(true) => Untracked::Normal,
            Some(false) => Untracked::No,
            None => return None,
        },
    })
}

/// The parsed identity line `Name <email> <time> <tz>`.
fn signature_from(raw: &[u8]) -> Result<Signature, Die> {
    SignatureRef::parse(raw)
        .and_then(|s| s.to_owned())
        .ok_or_else(|| fatal(lossy(raw)))
}

fn ident_line(name: &[u8], email: &[u8], time: u64, tz: i32) -> Vec<u8> {
    let mut v = name.to_vec();
    v.extend_from_slice(b" <");
    v.extend_from_slice(email);
    v.extend_from_slice(b"> ");
    v.extend_from_slice(rgit_date::format_raw(time, tz).as_bytes());
    v
}

/// Seconds and `±hhmm` of a signature.
fn sig_time(s: &Signature) -> (u64, i32) {
    let mins = s.time.offset_minutes.abs();
    let mut tz = mins / 60 * 100 + mins % 60;
    if s.time.negative || s.time.offset_minutes < 0 {
        tz = -tz;
    }
    (s.time.seconds.max(0) as u64, tz)
}

fn show_date(s: &Signature) -> String {
    let (t, tz) = sig_time(s);
    rgit_date::show_date(t, tz, &DateMode::default(), &DateContext::from_env()).unwrap_or_default()
}

fn name_email(s: &Signature) -> String {
    format!("{} <{}>", lossy(&s.name), lossy(&s.email))
}

/// `--date=<date>`: a strict date, else an approximate one in the local
/// zone.
fn parse_force_date(d: &str) -> Result<(u64, i32), Die> {
    let ctx = DateContext::from_env();
    if let Some(r) = rgit_date::parse_date(d, &ctx) {
        return Ok(r);
    }
    let (t, err) = rgit_date::approxidate_careful(d, &ctx);
    if err {
        return Err(fatal(msg::invalid_date(d)));
    }
    Ok((t, ctx.local_tz(t as i64)))
}

/// The author of the most recent reachable commit whose `Name <email>`
/// matches `pattern` (case-insensitive regular expression).
fn find_author(repo: &Repo, pattern: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let re = regex::bytes::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .ok();
    let mut starts: Vec<ObjectId> = repo.refs.list("refs/").into_iter().map(|(_, o)| o).collect();
    starts.extend(repo.refs.resolve("HEAD"));
    let mut seen = std::collections::HashSet::new();
    let mut stack = starts;
    let mut best: Option<(i64, Vec<u8>, Vec<u8>)> = None;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(Object::Commit(c)) = repo.read_object(&id) else {
            continue;
        };
        if let Some(a) = c.author() {
            let mut ne = a.name.to_vec();
            ne.extend_from_slice(b" <");
            ne.extend_from_slice(a.email);
            ne.push(b'>');
            let hit = match &re {
                Some(re) => re.is_match(&ne),
                None => lossy(&ne)
                    .to_lowercase()
                    .contains(&pattern.to_lowercase()),
            };
            let when = c.committer().and_then(|s| s.time()).map_or(0, |t| t.seconds);
            if hit && best.as_ref().is_none_or(|b| when > b.0) {
                best = Some((when, a.name.to_vec(), a.email.to_vec()));
            }
        }
        stack.extend(c.parents().unwrap_or_default());
    }
    best.map(|(_, n, e)| (n, e))
}

/// The comment string (`core.commentString`, `core.commentChar`); `auto`
/// picks a character the message does not use.
fn comment_string(repo: &Repo, text: &[u8]) -> String {
    let v = config_str(repo, "core.commentstring").or_else(|| config_str(repo, "core.commentchar"));
    match v.as_deref() {
        Some("auto") => cmsg::auto_comment_char(text)
            .map(|c| (c as char).to_string())
            .unwrap_or_else(|| "#".into()),
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => "#".into(),
    }
}

fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(rel).as_ref())
    }
}

/// Index entries for the blobs of a flat tree.
fn index_from_flat(hash: rgit_hash::HashKind, flat: &Flat) -> Result<Index, Die> {
    let mut index = Index::new(hash);
    for (path, side) in flat {
        index
            .add(
                Entry::new(path.clone(), Stage::MERGED, IndexMode(side.mode), side.oid),
                AddOptions::default(),
            )
            .map_err(|e| fatal(e.to_string()))?;
    }
    Ok(index)
}

fn write_index(repo: &Repo, index: &mut Index, mtime: Option<rgit_index::Time>) -> Result<(), Die> {
    if let Some(reference) = mtime {
        if let Ok(checker) = repo.checker(mtime) {
            let _ = smudge_racy_entries(index, reference, &checker);
        }
    }
    let path = repo.setup.cwd.join(repo.index_path());
    let skip_hash = config_bool(repo, "index.skiphash").unwrap_or(false);
    index
        .write_file(&path, &rgit_fs::LockOptions::default(), &WriteOptions { skip_hash })
        .map_err(|e| fatal(e.to_string()))
}

/// Take the work tree state of `path` into `index`: re-hash it, or drop
/// it when it is gone.
fn update_path(
    repo: &Repo,
    checker: &rgit_worktree::stat::Checker,
    root: &Path,
    index: &mut Index,
    path: &[u8],
) -> Result<(), Die> {
    let old = index.get(path, Stage::MERGED).cloned();
    if let Some(o) = &old {
        if o.mode.is_gitlink() {
            return Ok(());
        }
    }
    match entry_for(repo, checker, root, path, old.as_ref(), false)? {
        Some(e) => {
            index
                .add(
                    e,
                    AddOptions {
                        ok_to_replace: true,
                        ..Default::default()
                    },
                )
                .map_err(|err| fatal(err.to_string()))?;
            index.invalidate_path(path);
        }
        None => {
            if std::fs::symlink_metadata(os_path(root, path)).is_err() {
                index.remove_path(path);
            }
        }
    }
    Ok(())
}

/// `git add -u` over the whole index (for `-a`).
fn update_all(
    repo: &Repo,
    checker: &rgit_worktree::stat::Checker,
    root: &Path,
    index: &mut Index,
) -> Result<bool, Die> {
    use rgit_worktree::stat::Status;
    let mut paths = Vec::new();
    for e in index.entries() {
        if e.stage() != Stage::MERGED || e.mode.is_gitlink() {
            continue;
        }
        let st = checker
            .check(e)
            .map_err(|err| fatal(format!("{}: {err}", lossy(e.path()))))?;
        if st != Status::Unchanged {
            paths.push(e.path().to_vec());
        }
    }
    for p in &paths {
        update_path(repo, checker, root, index, p)?;
    }
    Ok(!paths.is_empty())
}

fn hook_warnings(repo: &Repo, args: &Args) {
    let dir = match config_str(repo, "core.hookspath") {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                repo.setup
                    .work_tree
                    .clone()
                    .unwrap_or_else(|| repo.setup.git_dir.clone())
                    .join(p)
            }
        }
        None => repo.setup.common_dir.join("hooks"),
    };
    let mut names = Vec::new();
    if !args.no_verify {
        names.push("pre-commit");
    }
    names.push("prepare-commit-msg");
    if !args.no_verify {
        names.push("commit-msg");
    }
    names.push("post-commit");
    if args.amend && !args.no_post_rewrite {
        names.push("post-rewrite");
    }
    for n in names {
        let p = repo.setup.cwd.join(dir.join(n));
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        #[cfg(not(unix))]
        if !meta.is_file() {
            continue;
        }
        eprintln!("{}", msg::hook_not_run(n));
    }
}

/// Lines of a blob for the change counts; `None` for binary content.
fn blob_text(repo: &Repo, side: Option<tree_changes::Side>) -> Option<Vec<u8>> {
    let Some(s) = side else {
        return Some(Vec::new());
    };
    if s.mode & 0o170000 == 0o160000 {
        return Some(format!("Subproject commit {}\n", s.oid.to_hex()).into_bytes());
    }
    let data = match repo.read_object(&s.oid) {
        Some(Object::Blob(b)) => b.data,
        _ => Vec::new(),
    };
    (!rgit_diff::is_binary(&data)).then_some(data)
}

/// The change counts and create/delete/rename/mode lines of the summary.
fn diff_summary(repo: &Repo, pairs: &[Pair], quote_high: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if pairs.is_empty() {
        return out;
    }
    let (mut ins, mut del) = (0, 0);
    for p in pairs {
        if p.is_rename() && p.old == p.new {
            continue;
        }
        let (Some(a), Some(b)) = (blob_text(repo, p.old), blob_text(repo, p.new)) else {
            continue;
        };
        let d = rgit_diff::diff_text(&a, &b, &rgit_diff::DiffOptions::default());
        ins += d.stats.insertions;
        del += d.stats.deletions;
    }
    out.extend_from_slice(msg::files_changed(pairs.len(), ins, del).as_bytes());
    out.push(b'\n');
    let q = |p: &[u8]| {
        let mut v = Vec::new();
        crate::quote::write_quoted(&mut v, p, quote_high);
        v
    };
    for p in pairs {
        match (p.old, p.new) {
            (None, Some(n)) => {
                out.extend_from_slice(format!(" create mode {:06o} ", n.mode).as_bytes());
                out.extend_from_slice(&q(&p.path));
                out.push(b'\n');
            }
            (Some(o), None) => {
                out.extend_from_slice(format!(" delete mode {:06o} ", o.mode).as_bytes());
                out.extend_from_slice(&q(&p.path));
                out.push(b'\n');
            }
            (Some(o), Some(n)) => {
                if let Some(old) = &p.old_path {
                    out.extend_from_slice(b" rename ");
                    out.extend_from_slice(&tree_changes::rename_text(&q(old), &q(&p.path)));
                    out.extend_from_slice(b" (100%)\n");
                } else if o.mode != n.mode && !p.type_changed() {
                    out.extend_from_slice(
                        format!(" mode change {:06o} => {:06o} ", o.mode, n.mode).as_bytes(),
                    );
                    out.extend_from_slice(&q(&p.path));
                    out.push(b'\n');
                }
            }
            (None, None) => {}
        }
    }
    out
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = parse_args(argv)?;
    let mut repo = Repo::open(ctx, Mode::Required)?;
    let head = repo.resolve("HEAD").filter(|_| repo.refs.resolve("HEAD").is_some());
    let head = head.and_then(|h| repo.peel_to(h, ObjectKind::Commit));
    let repo = repo;

    // Option checks, in git's order.
    if args.amend && head.is_none() {
        return Err(fatal(msg::NOTHING_TO_AMEND));
    }
    let has_m = args.pieces.iter().any(|p| matches!(p, Piece::Message(_)));
    let has_f = args.pieces.iter().any(|p| matches!(p, Piece::File(_)));
    if has_m && has_f {
        return Err(fatal(msg::M_AND_F));
    }
    if args.reset_author && args.author.is_some() {
        return Err(fatal(msg::RESET_AUTHOR_AND_AUTHOR));
    }
    if args.reset_author && !args.amend {
        return Err(fatal(msg::RESET_AUTHOR_ONLY_WITH));
    }
    if args.include && args.only {
        return Err(fatal(msg::INCLUDE_AND_ONLY));
    }
    if args.all && args.include {
        return Err(fatal(msg::INCLUDE_AND_ALL));
    }
    if args.all && args.only {
        return Err(fatal(msg::ONLY_AND_ALL));
    }
    if args.paths.is_empty()
        && (args.include || (args.only && !(args.amend || args.allow_empty)))
    {
        return Err(fatal(msg::NO_PATHS));
    }
    if args.all && !args.paths.is_empty() {
        return Err(fatal(msg::paths_with_all(&args.paths[0])));
    }
    let cleanup_cfg = args
        .cleanup
        .clone()
        .or_else(|| config_str(&repo, "commit.cleanup"));
    let cleanup = match cleanup_cfg.as_deref() {
        None => Cleanup::Default,
        Some(m) => Cleanup::parse(m).ok_or_else(|| fatal(msg::invalid_cleanup(m)))?,
    };
    let untracked = match args
        .untracked
        .clone()
        .or_else(|| config_str(&repo, "status.showuntrackedfiles"))
    {
        None => Untracked::Normal,
        Some(m) => untracked_mode(&m).ok_or_else(|| fatal(msg::invalid_untracked(&m)))?,
    };
    if config_str(&repo, "core.commentchar").as_deref() == Some("auto")
        || config_str(&repo, "core.commentstring").as_deref() == Some("auto")
    {
        eprintln!("warning: Support for 'core.commentChar=auto' is deprecated and will be removed in Git 3.0");
    }
    let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
    for f in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        if git_dir.join(f).exists() {
            return Err(fatal(msg::MERGE_NOT_SUPPORTED));
        }
    }

    // The commit being replaced (amend) and the parents.
    let old_commit = match (args.amend, head) {
        (true, Some(h)) => match repo.read_object(&h) {
            Some(Object::Commit(c)) => Some(c),
            _ => return Err(fatal(msg::NOTHING_TO_AMEND)),
        },
        _ => None,
    };
    let parents: Vec<ObjectId> = match &old_commit {
        Some(c) => c.parents().unwrap_or_default(),
        None => head.into_iter().collect(),
    };
    let tree_of = |id: &ObjectId| -> Result<Flat, Die> {
        let mut flat = Flat::new();
        if let Some(Object::Commit(c)) = repo.read_object(id) {
            if let Ok(t) = c.tree() {
                tree_changes::flatten_tree(&repo, &t, &mut flat)?;
            }
        }
        Ok(flat)
    };
    let base = match parents.first() {
        Some(p) => tree_of(p)?,
        None => Flat::new(),
    };
    let head_flat = match head {
        Some(h) => tree_of(&h)?,
        None => Flat::new(),
    };

    // Identities.
    let env = SystemEnv::default();
    let icfg = IdentConfig::from_config(&repo.config).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let resolver = Resolver::new(&icfg, &env);
    let ident = |role| {
        resolver.ident(role, Strictness::Strict).map_err(|e| {
            eprint!("{}", e.stderr_text());
            Die::Exit(rgit_options::EXIT_FATAL)
        })
    };
    let committer_ident = ident(Role::Committer)?;
    let committer = signature_from(&committer_ident.to_bstring())?;
    let mut author_parts: (Vec<u8>, Vec<u8>, u64, i32) = match &old_commit {
        Some(c) if !args.reset_author => {
            let a = c
                .author()
                .and_then(|a| a.to_owned())
                .ok_or_else(|| fatal(msg::NOTHING_TO_AMEND))?;
            let (t, tz) = sig_time(&a);
            (a.name, a.email, t, tz)
        }
        _ => {
            let a = ident(Role::Author)?;
            (a.name.to_vec(), a.email.to_vec(), a.time, a.tz)
        }
    };
    if let Some(a) = &args.author {
        let parsed = a
            .find('<')
            .and_then(|lt| a[lt..].find('>').map(|gt| (lt, lt + gt)))
            .map(|(lt, gt)| {
                (
                    rgit_ident::ident::sanitize(&a.as_bytes()[..lt]).to_vec(),
                    rgit_ident::ident::sanitize(&a.as_bytes()[lt + 1..gt]).to_vec(),
                )
            });
        let (n, e) = match parsed {
            Some(p) => p,
            None => find_author(&repo, a).ok_or_else(|| fatal(msg::no_existing_author(a)))?,
        };
        author_parts.0 = n;
        author_parts.1 = e;
    }
    // The index to commit.
    let root = repo.work_tree()?;
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;
    let mut commit_index: Option<Index> = None;
    let mut index_written = false;
    let mut real_untouched = false;
    if args.all {
        if update_all(&repo, &checker, &root, &mut index)? {
            write_index(&repo, &mut index, index_mtime)?;
            index_written = true;
        }
    } else if !args.paths.is_empty() {
        let opts = ParseOptions {
            prefix: repo.prefix(),
            root: Some(root.as_os_str().as_encoded_bytes().to_vec()),
            globals: repository::pathspec_globals(ctx),
            ..Default::default()
        };
        let pathspec = Pathspec::parse(&args.paths, &opts).map_err(|e| fatal(e.to_string()))?;
        let mut candidates: BTreeSet<Vec<u8>> = index
            .entries()
            .iter()
            .map(|e| e.path().to_vec())
            .collect();
        candidates.extend(head_flat.keys().cloned());
        let mut seen = vec![false; pathspec.items().len()];
        let matched: Vec<Vec<u8>> = candidates
            .into_iter()
            .filter(|p| pathspec.match_path_seen(p, &mut seen).is_some())
            .collect();
        if let Some(item) = pathspec.unmatched(&seen).next() {
            return Err(Die::Error(
                rgit_pathspec::messages::did_not_match_known_to_git(item.original()),
                if args.include { 128 } else { 1 },
            ));
        }
        // Paths only in HEAD (removed from the index) are matched, but a
        // file still on disk is not added back (observed with git).
        let kept_from_head: Vec<&Vec<u8>> = matched
            .iter()
            .filter(|p| {
                index.path_range(p).is_empty()
                    && std::fs::symlink_metadata(os_path(&root, p)).is_ok()
            })
            .collect();
        for p in &matched {
            if !kept_from_head.contains(&p) {
                update_path(&repo, &checker, &root, &mut index, p)?;
            }
        }
        // With nothing to take from the work tree git leaves the index
        // file (and its cache tree) alone.
        if matched.len() == kept_from_head.len() {
            real_untouched = true;
        } else {
            write_index(&repo, &mut index, index_mtime)?;
        }
        index_written = true;
        if !args.include {
            let mut temp = index_from_flat(repo.hash, &head_flat)?;
            for p in &matched {
                if kept_from_head.contains(&p) {
                    continue;
                }
                temp.remove_path(p);
                for e in index.entries_for_path(p) {
                    temp.add(e.clone(), AddOptions::default())
                        .map_err(|err| fatal(err.to_string()))?;
                }
            }
            commit_index = Some(temp);
        }
    } else if args.only {
        commit_index = Some(index_from_flat(repo.hash, &head_flat)?);
    }
    let commit_index = commit_index.as_ref().unwrap_or(&index);
    if commit_index.has_conflicts() {
        let mut listed = Vec::new();
        let mut last: Option<&[u8]> = None;
        for e in commit_index.entries() {
            if e.stage().get() != 0 && last != Some(e.path()) {
                listed.extend_from_slice(b"U\t");
                listed.extend_from_slice(&crate::quote::relative_to(e.path(), &repo.prefix()));
                listed.push(b'\n');
                last = Some(e.path());
            }
        }
        let _ = std::io::stdout().write_all(&listed);
        let _ = std::io::stdout().flush();
        eprintln!("error: {}", msg::UNMERGED);
        eprintln!("{}", msg::UNMERGED_HINT);
        return Err(fatal(msg::UNRESOLVED));
    }

    // git writes the index (refreshed) and the trees before it looks
    // for changes and asks for the message.
    if !index_written {
        write_index(&repo, &mut index.clone(), index_mtime)?;
    }
    if !real_untouched && !index.has_conflicts() {
        let _ = rgit_commit::write_tree(repo.odb(), &index, &Default::default());
    }
    let tree = super::write_tree::write_tree(&repo, commit_index, false, None)?;

    if let Some(d) = &args.date {
        let (t, tz) = parse_force_date(d)?;
        author_parts.2 = t;
        author_parts.3 = tz;
    }
    let author = signature_from(&ident_line(
        &author_parts.0,
        &author_parts.1,
        author_parts.2,
        author_parts.3,
    ))?;

    // An amended commit keeps its author date; it is shown like a
    // differing one (observed with the git binary).
    let show_author_date = sig_time(&author) != sig_time(&committer)
        || (args.amend && !args.reset_author)
        || args.date.is_some();

    let renames = !matches!(config_bool(&repo, "diff.renames"), Some(false));
    let hints = config_bool(&repo, "advice.statushints").unwrap_or(true)
        && !matches!(
            std::env::var("GIT_ADVICE").ok().as_deref().map(|v| rgit_config::value::parse_bool(Some(v.as_bytes()))),
            Some(Some(false))
        );
    let header = wt_status::header(&repo, head);
    let staged = tree_changes::diff(&base, &wt_status::index_flat(commit_index), renames);
    let committable = !staged.is_empty();
    let prefix = repo.prefix();
    let quote_high = repo.quote_path();

    if !committable && !args.allow_empty {
        let collected = wt_status::collect(&repo, &base, commit_index, index_mtime, untracked, renames)?;
        if args.amend {
            eprintln!("{}", msg::AMEND_EMPTY);
        }
        let last = wt_status::nothing_line(&collected, &header, untracked, args.amend, hints);
        let text = wt_status::render(
            &collected,
            &header,
            &Render {
                hints,
                branch_hints: hints,
                comment: None,
                prefix: &prefix,
                quote_high,
                last: Some(last),
                untracked_not_listed: false,
            },
        );
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&text);
        let _ = o.flush();
        return Ok(1);
    }

    // The message.
    let mut message: Vec<u8> = Vec::new();
    for piece in &args.pieces {
        match piece {
            Piece::Message(m) => {
                if !message.is_empty() {
                    message.push(b'\n');
                }
                message.extend_from_slice(m.as_bytes());
                if !message.is_empty() && !message.ends_with(b"\n") {
                    message.push(b'\n');
                }
            }
            Piece::File(f) => {
                if f == "-" {
                    std::io::stdin()
                        .read_to_end(&mut message)
                        .map_err(|_| fatal(msg::COULD_NOT_READ_STDIN))?;
                } else {
                    let path = Path::new(f);
                    let path = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        os_path(&root, &prefix).join(path)
                    };
                    let data = std::fs::read(&path).map_err(|e| {
                        fatal(msg::could_not_read_log(f, &crate::global::strerror(&e)))
                    })?;
                    message.extend_from_slice(&data);
                }
            }
        }
    }
    let have_message = !args.pieces.is_empty();
    if !have_message {
        if let Some(c) = &old_commit {
            message = c.message().to_vec();
        }
    }
    let use_editor = args.edit.unwrap_or(!have_message && !args.amend)
        || (args.edit.is_none() && args.amend && !have_message);
    let mode = cleanup.resolve(use_editor);
    let comment = comment_string(&repo, &message);
    let editmsg = git_dir.join("COMMIT_EDITMSG");

    if use_editor {
        let include_status = args
            .status
            .or_else(|| config_bool(&repo, "commit.status"))
            .unwrap_or(true);
        let mut file = message.clone();
        if include_status {
            file.push(b'\n');
            let mut lines: Vec<String> = Vec::new();
            if cleanup == Cleanup::Scissors {
                lines.push(cmsg::SCISSORS.trim_start().to_owned());
                lines.extend(msg::SCISSORS_HELP.lines().map(str::to_owned));
            } else if mode == Cleanup::Strip {
                lines.extend(msg::instructions_strip(&comment).lines().map(str::to_owned));
            } else {
                lines.extend(msg::instructions_keep(&comment).lines().map(str::to_owned));
            }
            let mut idents = Vec::new();
            if author.name != committer.name || author.email != committer.email {
                idents.push(msg::template_author(&name_email(&author)));
            }
            if show_author_date {
                idents.push(msg::template_date(&show_date(&author)));
            }
            if !idents.is_empty() {
                lines.push(String::new());
                lines.extend(idents);
            }
            lines.push(String::new());
            for l in lines {
                file.extend_from_slice(comment.as_bytes());
                if !l.is_empty() {
                    file.push(b' ');
                }
                file.extend_from_slice(l.as_bytes());
                file.push(b'\n');
            }
            let collected =
                wt_status::collect(&repo, &base, commit_index, index_mtime, untracked, renames)?;
            file.extend_from_slice(&wt_status::render(
                &collected,
                &header,
                &Render {
                    hints: false,
                    branch_hints: hints,
                    comment: Some(&comment),
                    prefix: &prefix,
                    quote_high,
                    last: None,
                    untracked_not_listed: untracked == Untracked::No,
                },
            ));
        }
        std::fs::write(&editmsg, &file).map_err(|e| fatal(e.to_string()))?;
        let editor = rgit_ident::editor::editor(&repo.config, &env).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let opts = rgit_ident::editor::LaunchOptions {
            hint: rgit_ident::editor::waiting_hint_enabled(&repo.config, &env) && !ctx.no_advice,
            env: ctx.child_env(),
            cwd: None,
        };
        if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &editmsg, &opts) {
            e.print();
            eprintln!("{}", msg::SUPPLY_MESSAGE);
            return Ok(1);
        }
        message = std::fs::read(&editmsg).map_err(|e| fatal(e.to_string()))?;
    }

    let mut cleaned = message.clone();
    if use_editor && cleanup == Cleanup::Scissors {
        cmsg::cut_at_scissors(&mut cleaned, &comment);
    }
    let cleaned = cmsg::cleanup(&cleaned, mode, &comment);
    if !use_editor {
        std::fs::write(&editmsg, &cleaned).map_err(|e| fatal(e.to_string()))?;
    }
    let strip_comments = (mode == Cleanup::Strip).then_some(comment.as_bytes());
    if !args.allow_empty_message && cmsg::is_empty(&cleaned, strip_comments) {
        eprintln!("{}", msg::EMPTY_MESSAGE);
        return Ok(1);
    }

    hook_warnings(&repo, &args);

    // The objects and the ref update.
    let encoding = repo
        .config
        .get("i18n.commitencoding")
        .and_then(|e| e.value.as_ref())
        .map(|v| v.as_slice())
        .filter(|e| !e.eq_ignore_ascii_case(b"utf-8") && !e.eq_ignore_ascii_case(b"utf8"))
        .map(|e| e.to_vec());
    let new = rgit_commit::NewCommit {
        tree,
        parents: &parents,
        author: &author,
        committer: &committer,
        encoding: encoding.as_deref(),
        message: &cleaned,
    };
    let oid = rgit_commit::create_commit(repo.odb(), &new).map_err(|e| fatal(e.to_string()))?;

    let kind = if args.amend {
        msg::REFLOG_AMEND
    } else if parents.is_empty() {
        msg::REFLOG_INITIAL
    } else {
        msg::REFLOG_COMMIT
    };
    let reflog = msg::reflog_message(kind, &lossy(cmsg::first_line(&cleaned)));
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects);
    let update = match head {
        Some(h) => RefUpdate::update("HEAD", oid, Some(h)),
        None => RefUpdate::create("HEAD", oid),
    }
    .message(reflog);
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| fatal(e.to_string()))?;

    if args.quiet {
        return Ok(0);
    }
    let mut out = Vec::new();
    let branch = match &header.head {
        wt_status::HeadState::Branch(b) => b.clone(),
        wt_status::HeadState::Detached(_) => msg::DETACHED_HEAD.to_owned(),
    };
    out.push(b'[');
    out.extend_from_slice(branch.as_bytes());
    if head.is_none() {
        out.extend_from_slice(msg::ROOT_COMMIT.as_bytes());
    }
    out.push(b' ');
    out.extend_from_slice(repo.abbrev(&oid, repo.default_abbrev()).as_bytes());
    out.extend_from_slice(b"] ");
    out.extend_from_slice(&cmsg::subject(&cleaned));
    out.push(b'\n');
    if author.name != committer.name || author.email != committer.email {
        out.extend_from_slice(msg::summary_author(&name_email(&author)).as_bytes());
        out.push(b'\n');
    }
    if show_author_date {
        out.extend_from_slice(msg::summary_date(&show_date(&author)).as_bytes());
        out.push(b'\n');
    }
    let new_flat = wt_status::index_flat(commit_index);
    // The summary detects renames even with diff.renames=false.
    let pairs = tree_changes::diff(&base, &new_flat, true);
    out.extend_from_slice(&diff_summary(&repo, &pairs, quote_high));
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&out);
    let _ = o.flush();
    Ok(0)
}
