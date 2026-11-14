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
//!   "nothing to commit" status and the reflog entries;
//! - concluding a merge prepared by `git merge --no-commit` or a conflict
//!   (`MERGE_HEAD`, `MERGE_MSG`; reflog `commit (merge): ...`);
//! - concluding a cherry-pick or revert that stopped (#58):
//!   `CHERRY_PICK_HEAD` gives the author, `MERGE_MSG` the message, the
//!   reflog says `commit (cherry-pick): ...` (`GIT_REFLOG_ACTION`
//!   replaces the prefix), and the sequence ends after its last commit;
//! - linked worktrees (HEAD and its reflog in the worktree's git dir).
//!
//! - #128: `-t`/`commit.template`, `-s`, `--trailer`, `-C`/`-c`,
//!   `--fixup[=amend:|reword:]`, `--squash`, `-v`/`commit.verbose`,
//!   `--dry-run` with `--short`/`--porcelain`/`--long`/`-z`/`--branch`,
//!   `--pathspec-from-file`.
//!
//! Hooks (#76): `pre-commit` and `commit-msg` (skipped with
//! `--no-verify`), `prepare-commit-msg`, `post-commit` and `post-rewrite`
//! after `--amend`. Not yet: `-S`/`commit.gpgSign` (#51),
//! `-p`/`--interactive`, similarity renames in
//! the summary (#145), the implicit-committer notice.

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
use crate::messages::reset as rmsg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use rgit_hook::RunOptions;
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::tree_changes::{self, Flat, Pair};
use crate::wt_status::{self, Render};

#[path = "commit/dry_run.rs"]
mod dry_run;
#[path = "commit/verbose.rs"]
mod verbose;

/// A piece of the message in command-line order.
enum Piece {
    Message(String),
    File(String),
}

/// `--fixup=[(amend|reword):]<commit>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixupKind {
    Fixup,
    Amend,
    Reword,
}

/// The status format of `--dry-run` (`--short`, `--porcelain`, `--long`).
const FORMAT_SHORT: i32 = 1;
const FORMAT_PORCELAIN: i32 = 2;
const FORMAT_LONG: i32 = 3;

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
    /// `-v` count; `None` when neither `-v` nor `--no-verbose` was given.
    verbose: Option<i32>,
    /// `-C`/`--reuse-message`.
    reuse: Option<String>,
    /// `-c`/`--reedit-message`.
    reedit: Option<String>,
    fixup: Option<(FixupKind, String)>,
    squash: Option<String>,
    trailers: Vec<String>,
    signoff: bool,
    /// `--no-gpg-sign` was given.
    no_gpg_sign: bool,
    template: Option<String>,
    dry_run: bool,
    /// `FORMAT_*`, 0 when not given.
    format: i32,
    branch: Option<bool>,
    ahead_behind: Option<bool>,
    nul: bool,
    /// `--pathspec-from-file` and `--pathspec-file-nul`.
    pathspec_file: Option<(String, bool)>,
    paths: Vec<String>,
}

/// Split `--fixup=<value>` into its kind and commit; an unknown
/// `<word>:` prefix is an error.
fn parse_fixup(value: &str) -> Result<(FixupKind, String), Die> {
    if let Some((word, rest)) = value.split_once(':') {
        match word {
            "amend" => return Ok((FixupKind::Amend, rest.to_owned())),
            "reword" => return Ok((FixupKind::Reword, rest.to_owned())),
            w if !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphabetic()) => {
                return Err(fatal(msg::unknown_fixup(value)));
            }
            _ => {}
        }
    }
    Ok((FixupKind::Fixup, value.to_owned()))
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    let pieces: RefCell<Vec<Piece>> = RefCell::new(Vec::new());
    let edit = Cell::new(-1);
    let status = Cell::new(-1);
    let verify = Cell::new(-1);
    let post_rewrite = Cell::new(-1);
    let verbose = Cell::new(-1);
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
    let signoff = Cell::new(false);
    let dry_run = Cell::new(false);
    let nul = Cell::new(false);
    let interactive = Cell::new(false);
    let patch = Cell::new(false);
    let format = Cell::new(0);
    let branch = Cell::new(-1);
    let ahead_behind = Cell::new(-1);
    let context = Cell::new(-1);
    let interhunk = Cell::new(-1);
    let author = RefCell::new(None);
    let date = RefCell::new(None);
    let cleanup = RefCell::new(None);
    let untracked = RefCell::new(None);
    let reuse = RefCell::new(None);
    let reedit = RefCell::new(None);
    let fixup: RefCell<Option<String>> = RefCell::new(None);
    let squash = RefCell::new(None);
    let template = RefCell::new(None);
    let gpg_sign = Cell::new(-1);
    let trailers = RefCell::new(Vec::new());
    let from_file = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::countup('v', "verbose", &verbose, msg::HELP_VERBOSE),
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
            Opt::string('c', "reedit-message", &reedit, "commit", msg::HELP_REEDIT),
            Opt::string('C', "reuse-message", &reuse, "commit", msg::HELP_REUSE),
            Opt::string(None, "fixup", &fixup, msg::ARGH_FIXUP, msg::HELP_FIXUP).literal_argh(),
            Opt::string(None, "squash", &squash, "commit", msg::HELP_SQUASH),
            Opt::flag(None, "reset-author", &reset_author, msg::HELP_RESET_AUTHOR),
            Opt::string_list(None, "trailer", &trailers, "trailer", msg::HELP_TRAILER),
            Opt::flag('s', "signoff", &signoff, msg::HELP_SIGNOFF),
            Opt::string('t', "template", &template, "file", msg::HELP_TEMPLATE),
            Opt::bool('e', "edit", &edit, msg::HELP_EDIT),
            Opt::string(None, "cleanup", &cleanup, "mode", msg::HELP_CLEANUP),
            Opt::bool(None, "status", &status, msg::HELP_STATUS),
            Opt::callback('S', "gpg-sign", Some("key-id"), msg::HELP_GPG_SIGN, |_, unset| {
                gpg_sign.set(i32::from(!unset));
                Ok(())
            })
            .optional(None),
            Opt::group(msg::GROUP_CONTENTS),
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::flag('i', "include", &include, msg::HELP_INCLUDE),
            Opt::flag(None, "interactive", &interactive, msg::HELP_INTERACTIVE),
            Opt::flag('p', "patch", &patch, msg::HELP_PATCH),
            Opt::integer('U', "unified", &context, msg::HELP_UNIFIED).noneg(),
            Opt::integer(None, "inter-hunk-context", &interhunk, msg::HELP_INTER_HUNK)
                .noneg(),
            Opt::flag('o', "only", &only, msg::HELP_ONLY),
            Opt::flag('n', "no-verify", &no_verify, msg::HELP_NO_VERIFY).noneg(),
            Opt::bool(None, "verify", &verify, msg::HELP_VERIFY).noneg(),
            Opt::flag(None, "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::set_int(None, "short", &format, msg::HELP_SHORT, FORMAT_SHORT),
            Opt::bool(None, "branch", &branch, msg::HELP_BRANCH),
            Opt::bool(None, "ahead-behind", &ahead_behind, msg::HELP_AHEAD_BEHIND),
            Opt::set_int(None, "porcelain", &format, msg::HELP_PORCELAIN, FORMAT_PORCELAIN),
            Opt::set_int(None, "long", &format, msg::HELP_LONG, FORMAT_LONG),
            Opt::flag('z', "null", &nul, msg::HELP_NULL),
            Opt::flag(None, "amend", &amend, msg::HELP_AMEND),
            Opt::flag(None, "no-post-rewrite", &no_post_rewrite, msg::HELP_NO_POST_REWRITE)
                .noneg(),
            Opt::bool(None, "post-rewrite", &post_rewrite, msg::HELP_POST_REWRITE).noneg(),
            Opt::string('u', "untracked-files", &untracked, "mode", msg::HELP_UNTRACKED)
                .optional(Some("all")),
            Opt::flag(None, "allow-empty", &allow_empty, msg::HELP_ALLOW_EMPTY).hidden(),
            Opt::flag(
                None,
                "allow-empty-message",
                &allow_empty_message,
                msg::HELP_ALLOW_EMPTY_MESSAGE,
            )
            .hidden(),
            Opt::string(
                None,
                "pathspec-from-file",
                &from_file,
                "file",
                rmsg::HELP_PATHSPEC_FROM_FILE,
            ),
            Opt::flag(None, "pathspec-file-nul", &file_nul, rmsg::HELP_PATHSPEC_FILE_NUL),
        ],
    );
    let paths = parser.parse(argv)?.args;
    drop(parser);
    let from_file = from_file.into_inner();
    if from_file.is_some() {
        if interactive.get() || patch.get() {
            return Err(fatal(msg::incompatible(
                "--pathspec-from-file",
                "--interactive/--patch",
            )));
        }
        if all.get() {
            return Err(fatal(msg::incompatible("--pathspec-from-file", "-a")));
        }
        if !paths.is_empty() {
            return Err(fatal(rmsg::FROM_FILE_WITH_ARGS));
        }
    } else if file_nul.get() {
        return Err(fatal(rmsg::FILE_NUL_WITHOUT_FROM_FILE));
    }
    if context.get() >= 0 && !(interactive.get() || patch.get()) {
        return Err(fatal(msg::requires_interactive("--unified")));
    }
    if interhunk.get() >= 0 && !(interactive.get() || patch.get()) {
        return Err(fatal(msg::requires_interactive("--inter-hunk-context")));
    }
    if gpg_sign.get() == 1 {
        return Err(fatal(msg::not_supported("-S/--gpg-sign")));
    }
    if interactive.get() || patch.get() {
        return Err(fatal(msg::not_supported("--interactive/--patch")));
    }
    let fixup = match fixup.into_inner() {
        Some(v) => Some(parse_fixup(&v)?),
        None => None,
    };
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
        no_post_rewrite: if post_rewrite.get() >= 0 {
            post_rewrite.get() == 0
        } else {
            no_post_rewrite.get()
        },
        author: author.into_inner(),
        date: date.into_inner(),
        cleanup: cleanup.into_inner(),
        status: (status.get() >= 0).then(|| status.get() == 1),
        untracked: untracked.into_inner(),
        verbose: (verbose.get() >= 0).then(|| verbose.get()),
        reuse: reuse.into_inner(),
        reedit: reedit.into_inner(),
        fixup,
        squash: squash.into_inner(),
        trailers: trailers.into_inner(),
        signoff: signoff.get(),
        no_gpg_sign: gpg_sign.get() == 0,
        template: template.into_inner(),
        dry_run: dry_run.get(),
        format: format.get(),
        branch: (branch.get() >= 0).then(|| branch.get() == 1),
        ahead_behind: (ahead_behind.get() >= 0).then(|| ahead_behind.get() == 1),
        nul: nul.get(),
        pathspec_file: from_file.map(|f| (f, file_nul.get())),
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
pub(crate) fn signature_from(raw: &[u8]) -> Result<Signature, Die> {
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

pub(crate) fn show_date(s: &Signature) -> String {
    let (t, tz) = sig_time(s);
    rgit_date::show_date(t, tz, &DateMode::default(), &DateContext::from_env()).unwrap_or_default()
}

pub(crate) fn name_email(s: &Signature) -> String {
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
pub(crate) fn comment_string(repo: &Repo, text: &[u8]) -> Result<String, Die> {
    // The last of core.commentChar and core.commentString wins.
    let v = repo
        .config
        .entries()
        .iter()
        .rev()
        .find(|e| e.name == "core.commentchar" || e.name == "core.commentstring")
        .and_then(|e| e.value.as_ref())
        .map(|v| String::from_utf8_lossy(v).into_owned());
    Ok(match v.as_deref() {
        Some("auto") => cmsg::auto_comment_char(text)
            .map(|c| (c as char).to_string())
            .ok_or_else(|| fatal(msg::NO_COMMENT_CHAR))?,
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => "#".into(),
    })
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
        .write_file(&path, &repo.index_lock_options(), &WriteOptions { skip_hash })
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
    let unmerged = index
        .entries_for_path(path)
        .iter()
        .any(|e| e.stage().is_conflict());
    match entry_for(repo, checker, root, path, old.as_ref(), false, false).map_err(Die::from)? {
        Some(e) => {
            if unmerged {
                // The resolution replaces the conflict stages.
                index.remove_path(path);
            }
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
    let mut gitlinks = Vec::new();
    for e in index.entries() {
        if e.stage() != Stage::MERGED {
            // A conflicted path takes the work tree file (or goes away).
            if paths.last().map(Vec::as_slice) != Some(e.path()) {
                paths.push(e.path().to_vec());
            }
            continue;
        }
        if e.mode.is_gitlink() {
            // A submodule: record the commit checked out there.
            let full = os_path(root, e.path());
            if let Some(head) = repository::submodule_head(&full, repo.hash) {
                if head != e.id {
                    let mut n = Entry::new(e.path().to_vec(), Stage::MERGED, IndexMode::GITLINK, head);
                    if let Ok(m) = std::fs::symlink_metadata(&full) {
                        n.stat = rgit_worktree::stat::stat_from_metadata(&m);
                    }
                    n.flags = e.flags;
                    gitlinks.push(n);
                }
            }
            continue;
        }
        let st = checker
            .check(e)
            .map_err(|err| fatal(format!("{}: {err}", lossy(e.path()))))?;
        // Racily clean files are converted again, as git does.
        if st != Status::Unchanged || (checker.is_racy(e) && repo.converter()?.converts(e.path())) {
            paths.push(e.path().to_vec());
        }
    }
    for p in &paths {
        update_path(repo, checker, root, index, p)?;
    }
    let changed = !paths.is_empty() || !gitlinks.is_empty();
    for n in gitlinks {
        let path = n.path().to_vec();
        index
            .add(
                n,
                AddOptions {
                    ok_to_replace: true,
                    ..Default::default()
                },
            )
            .map_err(|err| fatal(err.to_string()))?;
        index.invalidate_path(&path);
    }
    Ok(changed)
}

/// The index files the commit hooks see while the command holds
/// `index.lock`: the updated index in the lock file, and for a partial
/// commit the index to be committed in a temporary file. Both go away on
/// drop.
#[derive(Default)]
struct HookIndex {
    lock: Option<rgit_fs::LockFile>,
    temp: Option<PathBuf>,
}

impl HookIndex {
    /// Writes the files and returns the one the hooks get as
    /// `GIT_INDEX_FILE` (absolute).
    fn prepare(&mut self, repo: &Repo, index: &Index, commit: Option<&Index>) -> Result<PathBuf, Die> {
        let opts = WriteOptions {
            skip_hash: config_bool(repo, "index.skiphash").unwrap_or(false),
        };
        let path = repo.setup.cwd.join(repo.index_path());
        let mut lock = rgit_fs::LockFile::acquire(&path, &repo.index_lock_options())
            .map_err(|e| fatal(e.to_string()))?;
        lock.file_mut()
            .write_all(&index.to_bytes(&opts))
            .map_err(|e| fatal(e.to_string()))?;
        let lock_path = lock.lock_path().to_path_buf();
        self.lock = Some(lock);
        let Some(commit) = commit else {
            return Ok(lock_path);
        };
        let temp = repo
            .setup
            .cwd
            .join(&repo.setup.git_dir)
            .join(format!("next-index-{}.lock", std::process::id()));
        std::fs::write(&temp, commit.to_bytes(&opts)).map_err(|e| fatal(e.to_string()))?;
        self.temp = Some(temp.clone());
        Ok(temp)
    }
}

impl Drop for HookIndex {
    fn drop(&mut self) {
        if let Some(t) = self.temp.take() {
            let _ = std::fs::remove_file(t);
        }
        if let Some(l) = self.lock.take() {
            l.rollback();
        }
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
pub(crate) fn diff_summary(repo: &Repo, pairs: &[Pair], quote_high: bool) -> Vec<u8> {
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
                    out.extend_from_slice(
                        format!(" ({}%)\n", p.score * 100 / rgit_diff::diffcore::MAX_SCORE)
                            .as_bytes(),
                    );
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

/// A commit named on the command line (`-C`, `-c`, `--fixup`, `--squash`).
fn lookup_commit(repo: &mut Repo, name: &str) -> Result<(ObjectId, rgit_object::Commit), Die> {
    let id = repo
        .resolve(name)
        .and_then(|i| repo.peel_to(i, ObjectKind::Commit))
        .ok_or_else(|| fatal(msg::could_not_lookup(name)))?;
    match repo.read_object(&id) {
        Some(Object::Commit(c)) => Ok((id, c)),
        _ => Err(fatal(msg::could_not_lookup(name))),
    }
}

/// The template file: `-t` or `commit.template` (`~` expanded); `None`
/// for none or a missing `:(optional)` one.
fn template_path(repo: &Repo, args: &Args, root: &Path, prefix: &[u8]) -> Result<Option<PathBuf>, Die> {
    let env = SystemEnv::default();
    let raw: Vec<u8> = match &args.template {
        Some(t) => t.as_bytes().to_vec(),
        None => match repo.config.get("commit.template") {
            None => return Ok(None),
            Some(e) => match &e.value {
                Some(v) => v.to_vec(),
                None => {
                    return Err(crate::comment::bad_entry(
                        e,
                        &crate::messages::comment::missing_value("commit.template"),
                    ));
                }
            },
        },
    };
    if raw.is_empty() {
        // `-t ''` means no template; an empty `commit.template` names the
        // file ''.
        return Ok(args.template.is_none().then(PathBuf::new));
    }
    let path = rgit_config::value::interpolate_path(&raw, &env)
        .map_err(|_| fatal(msg::could_not_read_template(&lossy(&raw), "")))?;
    Ok(path.map(|p| {
        if p.is_absolute() || args.template.is_none() {
            p
        } else {
            os_path(root, prefix).join(p)
        }
    }))
}

/// The message without its first line and the empty lines after it (the
/// body of an `amend!` message).
fn without_first_line(text: &[u8]) -> &[u8] {
    let mut rest = match text.iter().position(|&b| b == b'\n') {
        Some(p) => &text[p + 1..],
        None => &[][..],
    };
    while let Some(r) = rest.strip_prefix(b"\n") {
        rest = r;
    }
    rest
}

/// The `Signed-off-by` line of the committer.
fn signoff_line(committer: &Signature) -> Vec<u8> {
    let mut v = b"Signed-off-by: ".to_vec();
    v.extend_from_slice(name_email(committer).as_bytes());
    v
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut args = parse_args(argv)?;
    let mut repo = Repo::open(ctx, Mode::Required)?;
    let advisor = ctx.advisor(&repo.config)?;
    advisor.check_env().map_err(Die::Fatal)?;
    if let Some((name, nul)) = &args.pathspec_file {
        args.paths = super::restore::read_pathspec_file(&repo, name, *nul)?
            .iter()
            .map(|p| lossy(p))
            .collect();
    }
    let head = repo.resolve("HEAD").filter(|_| repo.refs.resolve("HEAD").is_some());
    let head = head.and_then(|h| repo.peel_to(h, ObjectKind::Commit));
    // The configuration is read before the options are checked.
    let template_file = {
        let root = repo.work_tree()?;
        template_path(&repo, &args, &root, &repo.prefix())?
    };

    // The status format of a dry run.
    if args.format == FORMAT_LONG && args.nul {
        return Err(fatal(msg::incompatible("--long", "-z")));
    }
    if args.nul && args.format == 0 {
        args.format = FORMAT_PORCELAIN;
    }
    if args.format != 0 {
        args.dry_run = true;
    }
    let show_branch = args.branch.unwrap_or(false);
    let branch_info = if args.dry_run && show_branch && matches!(args.format, FORMAT_SHORT | FORMAT_PORCELAIN) {
        let revs = crate::refs_support::Revs::new(ctx, &repo.setup)?;
        let count = args.ahead_behind.unwrap_or(true);
        Some(super::status::branch_info(&mut repo, &revs, count)?)
    } else {
        None
    };

    // Option checks, in git's order.
    if args.amend && head.is_none() {
        return Err(fatal(msg::NOTHING_TO_AMEND));
    }
    let has_m = args.pieces.iter().any(|p| matches!(p, Piece::Message(_)));
    let has_f = args.pieces.iter().any(|p| matches!(p, Piece::File(_)));
    if has_m && has_f {
        return Err(fatal(msg::M_AND_F));
    }
    if args.squash.is_some() && args.fixup.is_some() {
        return Err(fatal(msg::incompatible("--squash", "--fixup")));
    }
    let sources: Vec<&str> = [
        (args.reuse.is_some(), "-C"),
        (args.reedit.is_some(), "-c"),
        (has_f, "-F"),
        (args.fixup.is_some(), "--fixup"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect();
    if sources.len() > 1 {
        return Err(fatal(msg::incompatible(sources[0], sources[1])));
    }
    if has_m {
        if args.reuse.is_some() {
            return Err(fatal(msg::incompatible("-m", "-C")));
        }
        if args.reedit.is_some() {
            return Err(fatal(msg::incompatible("-m", "-c")));
        }
    }
    if matches!(args.fixup, Some((FixupKind::Reword, _))) {
        if args.all || args.include || args.only {
            return Err(fatal(msg::REWORD_WITH_CONTENTS));
        }
        if let Some(p) = args.paths.first() {
            return Err(fatal(msg::reword_with_path(p)));
        }
        // A reword commit takes only a message.
        args.only = true;
    }
    // amend! and reword commits may leave the tree alone.
    if matches!(args.fixup, Some((FixupKind::Amend | FixupKind::Reword, _))) {
        args.allow_empty = true;
    }
    if args.reset_author && args.author.is_some() {
        return Err(fatal(msg::RESET_AUTHOR_AND_AUTHOR));
    }
    let picking = std::fs::read_to_string(repo.setup.cwd.join(&repo.setup.git_dir).join("CHERRY_PICK_HEAD"))
        .ok()
        .and_then(|s| ObjectId::from_hex(repo.hash, s.trim_end().as_bytes()).ok())
        .is_some();
    if args.reset_author
        && !(args.amend || args.reuse.is_some() || args.reedit.is_some() || picking)
    {
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
    if !args.no_gpg_sign && config_bool(&repo, "commit.gpgsign") == Some(true) {
        return Err(fatal(msg::not_supported("commit.gpgSign")));
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
    // The commits named on the command line.
    let reuse_name = args.reuse.clone().or_else(|| args.reedit.clone());
    let reused = match &reuse_name {
        Some(n) => Some(lookup_commit(&mut repo, n)?),
        None => None,
    };
    // git looks these up only once the index is prepared.
    let fixup_lookup = match &args.fixup {
        Some((kind, n)) => lookup_commit(&mut repo, n).map(|c| Some((*kind, c.1))),
        None => Ok(None),
    };
    let squash_lookup = match &args.squash {
        Some(n) => lookup_commit(&mut repo, n).map(Some),
        None => Ok(None),
    };
    let repo = repo;
    let verbose = args.verbose.unwrap_or_else(|| {
        repo.config
            .get("commit.verbose")
            .and_then(|e| e.value.as_ref())
            .map_or(0, |v| {
                // A boolean or an integer (git_config_bool_or_int).
                rgit_config::value::parse_int(v, rgit_config::value::IntWidth::I32)
                    .ok()
                    .and_then(|n| i32::try_from(n).ok())
                    .or_else(|| rgit_config::value::parse_bool(Some(v)).map(i32::from))
                    .unwrap_or(0)
            })
    });
    let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
    // Concluding a cherry-pick: CHERRY_PICK_HEAD names the picked commit,
    // whose author the commit keeps.
    let pseudo_head = |name: &str| -> Option<ObjectId> {
        std::fs::read_to_string(git_dir.join(name))
            .ok()
            .and_then(|s| ObjectId::from_hex(repo.hash, s.trim_end().as_bytes()).ok())
    };
    let cherry_pick_head = pseudo_head("CHERRY_PICK_HEAD");
    let reverting = pseudo_head("REVERT_HEAD").is_some();
    if cherry_pick_head.is_some() && args.amend {
        return Err(fatal(msg::AMEND_DURING_CHERRY_PICK));
    }
    if cherry_pick_head.is_some() && !args.paths.is_empty() && !args.include {
        return Err(fatal(msg::PARTIAL_DURING_CHERRY_PICK));
    }
    let picked_commit = match cherry_pick_head {
        Some(id) => match repo.read_object(&id) {
            Some(Object::Commit(c)) => Some(c),
            _ => None,
        },
        None => None,
    };
    // Concluding a merge: MERGE_HEAD names the other parents.
    let merge_heads: Vec<ObjectId> = std::fs::read(git_dir.join("MERGE_HEAD"))
        .map(|d| {
            d.split(|&b| b == b'\n')
                .filter_map(|l| ObjectId::from_hex(repo.hash, l.trim_ascii()).ok())
                .collect()
        })
        .unwrap_or_default();
    let merging = !merge_heads.is_empty();
    if merging && args.amend {
        return Err(fatal(msg::AMEND_DURING_MERGE));
    }
    if merging && !args.paths.is_empty() && !args.include {
        return Err(fatal(msg::PARTIAL_DURING_MERGE));
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
        None => head.into_iter().chain(merge_heads.iter().copied()).collect(),
    };
    let tree_id_of = |id: &ObjectId| -> Option<ObjectId> {
        match repo.read_object(id) {
            Some(Object::Commit(c)) => c.tree().ok(),
            _ => None,
        }
    };
    let tree_of = |id: &ObjectId| -> Result<Flat, Die> {
        let mut flat = Flat::new();
        if let Some(t) = tree_id_of(id) {
            tree_changes::flatten_tree(&repo, &t, &mut flat)?;
        }
        Ok(flat)
    };
    let base = match parents.first() {
        Some(p) => tree_of(p)?,
        None => Flat::new(),
    };
    let base_tree = parents
        .first()
        .and_then(tree_id_of)
        .unwrap_or_else(|| ObjectId::empty_tree(repo.hash));
    let head_flat = match head {
        Some(h) => tree_of(&h)?,
        None => Flat::new(),
    };

    // Identities. The author comes from the amended commit or the one of
    // -C/-c, unless --reset-author.
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
    let author_commit = reused
        .as_ref()
        .map(|(_, c)| c)
        .or(old_commit.as_ref())
        .or(picked_commit.as_ref());
    let author_from_commit = author_commit.is_some() && !args.reset_author;
    let mut author_parts: (Vec<u8>, Vec<u8>, u64, i32) = match author_commit {
        Some(c) if !args.reset_author => {
            let a = c
                .author()
                .and_then(|a| a.to_owned())
                .ok_or_else(|| fatal(msg::NOTHING_TO_AMEND))?;
            if a.name.is_empty() {
                return Err(fatal(rgit_ident::messages::empty_ident_name(&lossy(&a.email))));
            }
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
    // A bad --date fails only after the trees are written (as in git).
    let forced_date = args.date.as_deref().map(parse_force_date);
    if let Some(Ok((t, tz))) = &forced_date {
        author_parts.2 = *t;
        author_parts.3 = *tz;
    }
    let plain_fixup = matches!(args.fixup, Some((FixupKind::Fixup, _)));
    let use_editor = args
        .edit
        .unwrap_or(!(has_m || has_f || args.reuse.is_some() || plain_fixup));
    // The hooks see the author and, without an editor, `GIT_EDITOR=:`.
    let git_hooks = crate::hooks::for_repo(ctx, &repo.setup, &repo.config)?;
    let mut hook_env: Vec<(String, String)> = vec![
        ("GIT_AUTHOR_NAME".into(), lossy(&author_parts.0)),
        ("GIT_AUTHOR_EMAIL".into(), lossy(&author_parts.1)),
        (
            "GIT_AUTHOR_DATE".into(),
            format!("@{}", rgit_date::format_raw(author_parts.2, author_parts.3)),
        ),
    ];
    if !use_editor {
        hook_env.push(("GIT_EDITOR".into(), ":".into()));
    }
    let hook_opts = |index_file: &Path, hook_args: &[&str]| {
        let mut o = RunOptions::with_args(hook_args.iter().copied());
        o.env = hook_env
            .iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        o.env.push(("GIT_INDEX_FILE".into(), index_file.into()));
        o
    };
    // The index to commit.
    let root = repo.work_tree()?;
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;
    let mut commit_index: Option<Index> = None;
    // With -a, -i and -o the updated index is only written once the
    // commit is made (git keeps it locked until then).
    let mut deferred = false;
    if args.all {
        deferred = update_all(&repo, &checker, &root, &mut index)?;
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
        for p in &matched {
            update_path(&repo, &checker, &root, &mut index, p)?;
        }
        deferred = true;
        if !args.include {
            let mut temp = index_from_flat(repo.hash, &head_flat)?;
            for p in &matched {
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
    // git writes the trees of the index it keeps and records them in its
    // cache tree (rev-list --indexed-objects and later write-trees use it).
    if !index.has_conflicts() {
        let cached = rgit_commit::write_tree_cached(repo.odb(), &index, &Default::default())
            .ok()
            .and_then(|(_, c)| c);
        index.set_cache_tree(cached);
    }
    if commit_index.as_ref().unwrap_or(&index).has_conflicts() {
        let mut listed = Vec::new();
        let mut last: Option<&[u8]> = None;
        for e in commit_index.as_ref().unwrap_or(&index).entries() {
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
        advisor
            .advise_without_footer(Advice::ResolveConflict, msg::UNMERGED_HINT)
            .map_err(Die::Fatal)?;
        return Err(fatal(msg::UNRESOLVED));
    }

    // An amend! message comes from the target commit (checked once the
    // index is prepared, as in git).
    if has_m {
        match &args.fixup {
            Some((FixupKind::Amend, _)) => {
                return Err(fatal(msg::incompatible("-m", "--fixup:amend")));
            }
            Some((FixupKind::Reword, _)) => {
                return Err(fatal(msg::incompatible("-m", "--fixup:reword")));
            }
            _ => {}
        }
    }
    let fixup_target = fixup_lookup?;
    let squash_target = squash_lookup?;

    // git writes the index (refreshed) and the trees before it looks
    // for changes and asks for the message.
    if !deferred && !args.all && !args.only {
        write_index(&repo, &mut index.clone(), index_mtime)?;
    }

    let renames_key = if repo.config.get("status.renames").is_some() {
        "status.renames"
    } else {
        "diff.renames"
    };
    let renames = !matches!(config_bool(&repo, renames_key), Some(false));
    let hints = advisor.enabled(Advice::StatusHints).map_err(Die::Fatal)?;
    let mut header = wt_status::header(&repo, head);
    header.merging = merging;
    header.amend = args.amend;
    let prefix = repo.prefix();
    let quote_high = repo.quote_path();

    if args.dry_run {
        let commit_index = commit_index.as_ref().unwrap_or(&index);
        let staged = tree_changes::diff(&repo, &base, &wt_status::index_flat(commit_index), renames);
        let committable = !staged.is_empty() || parents.len() > 1;
        let collected =
            wt_status::collect(&repo, &base, commit_index, index_mtime, untracked, renames)?;
        let mut out = Vec::new();
        if args.format == FORMAT_SHORT || args.format == FORMAT_PORCELAIN {
            let relative = args.format == FORMAT_SHORT
                && !args.nul
                && config_bool(&repo, "status.relativepaths").unwrap_or(true);
            out = dry_run::short(
                &collected,
                branch_info.as_ref(),
                args.nul,
                quote_high,
                if relative { prefix.clone() } else { Vec::new() },
            );
        } else {
            let last = (!committable).then(|| {
                wt_status::nothing_line(&collected, &header, untracked, args.amend, hints)
            });
            out.extend_from_slice(&wt_status::render(
                &collected,
                &header,
                &Render {
                    hints,
                    branch_hints: hints,
                    comment: None,
                    prefix: &prefix,
                    quote_high,
                    last,
                    untracked_not_listed: untracked == Untracked::No,
                },
            ));
            if verbose > 0 {
                out.extend_from_slice(&verbose::staged_diff(
                    &repo,
                    &base_tree,
                    commit_index,
                    false,
                )?);
            }
        }
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&out);
        let _ = o.flush();
        return Ok(if committable { 0 } else { 1 });
    }

    // The hooks read (and may change) the index being committed: the
    // index file itself, or, while the command holds `index.lock`, a copy
    // of the updated index in the lock file (`-a`, `-i`) or a temporary
    // index of the partial commit (`-o`).
    let verify = !args.no_verify;
    let wants = |e: &str| git_hooks.exists(e);
    let need_index = (verify && (wants("pre-commit") || wants("commit-msg")))
        || wants("prepare-commit-msg");
    let mut hook_index = HookIndex::default();
    let hook_index_file = if need_index && (deferred || commit_index.is_some()) {
        hook_index.prepare(&repo, &index, commit_index.as_ref())?
    } else {
        repo.index_path()
    };
    if verify {
        let outcome = crate::hooks::run(&git_hooks, "pre-commit", &hook_opts(&hook_index_file, &[]))?;
        if outcome.ran > 0 {
            if !outcome.ok() {
                return Ok(1);
            }
            // Take over what the hook changed.
            let reread = |p: &Path| {
                Index::read_file(p, repo.hash, &Default::default())
                    .map_err(|e| fatal(e.to_string()))
            };
            if let Some(temp) = &hook_index.temp {
                commit_index = Some(reread(temp)?);
            } else if hook_index.lock.is_some() {
                index = reread(&hook_index_file)?;
            } else {
                index = repo.read_index()?.0;
            }
        }
    }

    // The message, from the first source that has one.
    let mut message: Vec<u8> = Vec::new();
    // The second and third argument of prepare-commit-msg.
    let mut source: Vec<String> = Vec::new();
    let mut template: Option<Vec<u8>> = None;
    if let Some((_, sq)) = &squash_target {
        message.extend_from_slice(b"squash! ");
        message.extend_from_slice(&cmsg::subject(sq.message()));
        message.extend_from_slice(b"\n\n");
        source.push("message".into());
    }
    match &fixup_target {
        Some((FixupKind::Fixup, c)) => {
            message.extend_from_slice(b"fixup! ");
            message.extend_from_slice(&cmsg::subject(c.message()));
            message.extend_from_slice(b"\n\n");
            source.push("message".into());
        }
        Some((_, c)) => {
            message.extend_from_slice(b"amend! ");
            message.extend_from_slice(&cmsg::subject(c.message()));
            message.extend_from_slice(b"\n\n");
            let body = c.message();
            if body.starts_with(b"amend! ") {
                message.extend_from_slice(without_first_line(body));
            } else {
                message.extend_from_slice(body);
            }
            source.push("message".into());
        }
        None => {}
    }
    let mut given: Vec<u8> = Vec::new();
    for piece in &args.pieces {
        match piece {
            Piece::Message(m) => {
                if !given.is_empty() {
                    given.push(b'\n');
                }
                given.extend_from_slice(m.as_bytes());
                if !given.is_empty() && !given.ends_with(b"\n") {
                    given.push(b'\n');
                }
            }
            Piece::File(f) => {
                if f == "-" {
                    std::io::stdin()
                        .read_to_end(&mut given)
                        .map_err(|_| fatal(msg::COULD_NOT_READ_STDIN))?;
                } else {
                    let path = Path::new(f);
                    let path = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        os_path(&root, &prefix).join(path)
                    };
                    let data = std::fs::read(path).map_err(|e| {
                        fatal(msg::could_not_read_log(f, &crate::global::strerror(&e)))
                    })?;
                    given.extend_from_slice(&data);
                }
            }
        }
    }
    if !args.pieces.is_empty() {
        message.extend_from_slice(&given);
        if source.is_empty() {
            source.push("message".into());
        }
    } else if let Some((id, c)) = &reused {
        // `--squash X -C X` does not repeat the message.
        if squash_target.as_ref().is_none_or(|(sq, _)| sq != id) {
            message.extend_from_slice(c.message());
        }
        if source.is_empty() {
            source = vec!["commit".into(), reuse_name.clone().unwrap_or_default()];
        }
    } else if fixup_target.is_some() {
    } else if let Some(c) = &old_commit {
        message.extend_from_slice(c.message());
        source = vec!["commit".into(), "HEAD".into()];
    } else if merging {
        message.extend(std::fs::read(git_dir.join("MERGE_MSG")).unwrap_or_default());
        source.push("merge".into());
    } else if let Ok(squash) = std::fs::read(git_dir.join("SQUASH_MSG")) {
        // `merge --squash` left the message (and, after conflicts, the
        // "Conflicts:" block in MERGE_MSG).
        message.extend(squash);
        message.extend(std::fs::read(git_dir.join("MERGE_MSG")).unwrap_or_default());
        source.push("squash".into());
    } else if let Ok(data) = std::fs::read(git_dir.join("MERGE_MSG")) {
        // A cherry-pick or revert left its message.
        message.extend(data);
        source.push("merge".into());
    } else if use_editor {
        if let Some(path) = &template_file {
            let data = std::fs::read(path).map_err(|e| {
                let shown = match &args.template {
                    Some(t) => t.clone(),
                    None => path.display().to_string(),
                };
                fatal(msg::could_not_read_template(
                    &shown,
                    &crate::global::strerror(&e),
                ))
            })?;
            message.extend_from_slice(&data);
            if source.is_empty() {
                source.push("template".into());
            }
            template = Some(data);
        }
    }

    let commit_index = commit_index.as_ref().unwrap_or(&index);
    let tree = super::write_tree::write_tree(&repo, commit_index, false, None)?;
    if let Some(Err(e)) = forced_date {
        return Err(e);
    }

    let author = signature_from(&ident_line(
        &author_parts.0,
        &author_parts.1,
        author_parts.2,
        author_parts.3,
    ))?;

    // An amended or reused commit keeps its author date; it is shown like
    // a differing one (observed with the git binary).
    let show_author_date = author_from_commit || args.date.is_some();

    let staged = tree_changes::diff(&repo, &base, &wt_status::index_flat(commit_index), renames);
    // A merge (new or amended) is made even when its tree equals the first
    // parent's.
    let committable = !staged.is_empty() || parents.len() > 1;


    let mode = cleanup.resolve(use_editor);
    if use_editor && cleanup != Cleanup::Verbatim && template.is_none() {
        message = cmsg::stripspace(&message, None);
    }
    let comment = if use_editor {
        comment_string(&repo, &message)?
    } else {
        comment_string(&repo, b"")?
    };
    let trailer_conf = if args.signoff || !args.trailers.is_empty() {
        let (conf, warnings) = rgit_trailer::Config::from_config(&repo.config).map_err(|entry| {
            let name = String::from_utf8_lossy(&entry.name).into_owned();
            crate::comment::bad_entry(&entry, &crate::messages::comment::missing_value(&name))
        })?;
        for w in warnings {
            eprintln!("warning: {w}");
        }
        conf
    } else {
        rgit_trailer::Config::default()
    };
    if args.signoff {
        message = rgit_trailer::append_signoff(
            &message,
            &signoff_line(&committer),
            &trailer_conf,
            comment.as_bytes(),
        );
    }
    let editmsg = git_dir.join("COMMIT_EDITMSG");

    let include_status = args
        .status
        .or_else(|| config_bool(&repo, "commit.status"))
        .unwrap_or(true);
    if use_editor {
        let mut file = message.clone();
        let notice = if merging {
            Some(msg::MERGE_NOTICE)
        } else if cherry_pick_head.is_some() {
            Some(msg::CHERRY_PICK_NOTICE)
        } else {
            None
        };
        if let (true, Some(notice)) = (include_status, notice) {
            for l in notice.lines() {
                file.extend_from_slice(comment.as_bytes());
                if !l.is_empty() && !l.starts_with('\t') {
                    file.push(b' ');
                }
                file.extend_from_slice(l.as_bytes());
                file.push(b'\n');
            }
            file.push(b'\n');
        }
        if include_status {
            file.push(b'\n');
            let mut lines: Vec<String> = Vec::new();
            if cleanup == Cleanup::Scissors {
                lines.push(cmsg::SCISSORS.trim_start().to_owned());
                lines.extend(msg::SCISSORS_HELP.lines().map(str::to_owned));
            } else if mode == Cleanup::Strip {
                lines.extend(msg::instructions_strip(&comment, args.allow_empty_message).lines().map(str::to_owned));
            } else {
                lines.extend(msg::instructions_keep(&comment, args.allow_empty_message).lines().map(str::to_owned));
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
            if verbose > 0 {
                // With --cleanup=scissors the line is already at the top.
                if cleanup != Cleanup::Scissors {
                    file.extend_from_slice(cmsg::scissors_line(&comment).as_bytes());
                    file.push(b'\n');
                    for l in msg::SCISSORS_HELP.lines() {
                        file.extend_from_slice(comment.as_bytes());
                        file.push(b' ');
                        file.extend_from_slice(l.as_bytes());
                        file.push(b'\n');
                    }
                }
                file.extend_from_slice(&verbose::template_diff(
                    &repo,
                    &base_tree,
                    commit_index,
                    verbose,
                    &comment,
                )?);
            }
        }
        std::fs::write(&editmsg, &file).map_err(|e| fatal(e.to_string()))?;
    } else {
        // Comment lines go only once the message is read back (git
        // writes them, e.g. the "Conflicts:" block of a cherry-pick).
        let written = match mode {
            Cleanup::Strip | Cleanup::Scissors => cmsg::stripspace(&message, None),
            m => cmsg::cleanup(&message, m, &comment),
        };
        std::fs::write(&editmsg, &written).map_err(|e| fatal(e.to_string()))?;
    }
    if !args.trailers.is_empty() {
        let data = std::fs::read(&editmsg).map_err(|e| fatal(e.to_string()))?;
        let new: Vec<rgit_trailer::NewTrailer> = args
            .trailers
            .iter()
            .map(|t| rgit_trailer::NewTrailer::new(t.as_bytes()))
            .collect();
        let opts = rgit_trailer::Options {
            no_divider: true,
            comment: comment.as_bytes().into(),
            ..Default::default()
        };
        let mut on_error = |m: &str| eprintln!("error: {m}");
        let out = rgit_trailer::process(&data, &trailer_conf, &opts, &new, &mut on_error);
        std::fs::write(&editmsg, &out).map_err(|e| fatal(e.to_string()))?;
    }
    // Nothing to commit: git has written COMMIT_EDITMSG by now.
    if !committable && !args.allow_empty {
        let collected = wt_status::collect(&repo, &base, commit_index, index_mtime, untracked, renames)?;
        if args.amend {
            eprintln!("{}", msg::AMEND_EMPTY);
        }
        if cherry_pick_head.is_some() {
            eprintln!("{}\n{}", msg::EMPTY_CHERRY_PICK, msg::EMPTY_CHERRY_PICK_SKIP);
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
    let editmsg_shown = repo.setup.git_dir.join("COMMIT_EDITMSG");
    let editmsg_arg = editmsg_shown.to_string_lossy().into_owned();
    {
        let mut pcm_args = vec![editmsg_arg.as_str()];
        pcm_args.extend(source.iter().map(String::as_str));
        let outcome = crate::hooks::run(
            &git_hooks,
            "prepare-commit-msg",
            &hook_opts(&hook_index_file, &pcm_args),
        )?;
        if !outcome.ok() {
            return Ok(1);
        }
    }
    if use_editor {
        let editor = rgit_ident::editor::editor(&repo.config, &env).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let opts = rgit_ident::editor::LaunchOptions {
            hint: advisor
                .enabled(Advice::WaitingForEditor)
                .map_err(Die::Fatal)?,
            env: ctx.child_env(),
            cwd: None,
        };
        if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &editmsg, &opts) {
            e.print();
            eprintln!("{}", msg::SUPPLY_MESSAGE);
            return Ok(1);
        }
    }
    if verify {
        let outcome = crate::hooks::run(
            &git_hooks,
            "commit-msg",
            &hook_opts(&hook_index_file, &[editmsg_arg.as_str()]),
        )?;
        if !outcome.ok() {
            return Ok(1);
        }
    }
    let message = std::fs::read(&editmsg).map_err(|e| fatal(e.to_string()))?;
    drop(hook_index);

    let mut cleaned = message.clone();
    if use_editor && (cleanup == Cleanup::Scissors || (verbose > 0 && include_status)) {
        cmsg::cut_at_scissors(&mut cleaned, &comment);
    }
    let cleaned = cmsg::cleanup(&cleaned, mode, &comment);
    let strip_comments = (mode == Cleanup::Strip).then_some(comment.as_bytes());
    if let Some(t) = &template {
        if cleanup != Cleanup::Verbatim {
            let t = cmsg::cleanup(t, mode, &comment);
            if let Some(rest) = cleaned.strip_prefix(&t[..]) {
                if cmsg::is_empty(rest, strip_comments) {
                    eprintln!("{}", msg::TEMPLATE_UNTOUCHED);
                    return Ok(1);
                }
            }
        }
    }
    // A verbatim message counts as empty only when it has no bytes at all.
    let empty = if mode == Cleanup::Verbatim {
        cleaned.is_empty()
    } else {
        cmsg::is_empty(&cleaned, strip_comments)
    };
    if !args.allow_empty_message && empty {
        eprintln!("{}", msg::EMPTY_MESSAGE);
        return Ok(1);
    }
    if matches!(fixup_target, Some((FixupKind::Amend | FixupKind::Reword, _)))
        && !args.allow_empty_message
        && cleaned.starts_with(b"amend! ")
        && cmsg::is_empty(without_first_line(&cleaned), strip_comments)
    {
        eprintln!("{}", msg::EMPTY_BODY);
        return Ok(1);
    }

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
    } else if merging {
        msg::REFLOG_MERGE
    } else if cherry_pick_head.is_some() {
        msg::REFLOG_CHERRY_PICK
    } else if parents.is_empty() {
        msg::REFLOG_INITIAL
    } else {
        msg::REFLOG_COMMIT
    };
    // GIT_REFLOG_ACTION replaces the "commit (...)" prefix (the sequencer
    // sets it when it lets `git commit` edit a picked commit).
    let action_env = std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .filter(|a| !a.is_empty());
    let kind = action_env.as_deref().unwrap_or(kind);
    let reflog = msg::reflog_message(kind, &lossy(cmsg::first_line(&cleaned)));
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects);
    let linked = setup.git_dir != setup.common_dir;
    let head_target = repo.refs.head_target();
    // In a linked worktree HEAD lives in the worktree's git directory,
    // which the reference store does not know about yet (#104): update
    // the branch (or the detached HEAD) directly and log HEAD by hand.
    let (name, wt_store) = match (&head_target, linked) {
        (Some(t), true) => (t.clone(), None),
        (None, true) => (
            "HEAD".to_owned(),
            Some(rgit_refs::files::FilesStore::new(
                setup.git_dir.clone(),
                setup.format.object_format,
            )),
        ),
        _ => ("HEAD".to_owned(), None),
    };
    let update = match head {
        Some(h) => RefUpdate::update(name.clone(), oid, Some(h)),
        None => RefUpdate::create(name.clone(), oid),
    }
    .message(reflog.clone());
    let update = if wt_store.is_some() {
        update.no_deref(true)
    } else {
        update
    };
    let mut tx = match &wt_store {
        Some(w) => Transaction::new(w),
        None => Transaction::new(&store),
    };
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| fatal(e.to_string()))?;
    if linked && head_target.is_some() {
        let logs = rgit_refs::reflog::FilesReflog::new(
            setup.git_dir.clone(),
            setup.common_dir.clone(),
            setup.format.object_format,
        )
        .with_shared(refs_support::shared_repository(&config));
        let mut ident = committer_ident.name.to_vec();
        ident.extend_from_slice(b" <");
        ident.extend_from_slice(&committer_ident.email);
        ident.push(b'>');
        let entry = rgit_refs::reflog::Entry {
            old: head.unwrap_or_else(|| ObjectId::null(repo.hash)),
            new: oid,
            ident,
            time: committer_ident.time,
            tz: committer_ident.tz,
            message: rgit_refs::reflog::normalize_message(reflog.as_bytes()),
        };
        logs.append("HEAD", &entry, true)
            .map_err(|e| fatal(e.to_string()))?;
    }

    if merging {
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(git_dir.join(f));
        }
    }
    if cherry_pick_head.is_some() || reverting || git_dir.join("MERGE_MSG").exists() {
        for f in ["MERGE_MSG", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(git_dir.join(f));
        }
        crate::sequencer::post_commit_cleanup(&git_dir);
    }
    if git_dir.join("SQUASH_MSG").exists() {
        for f in ["SQUASH_MSG", "MERGE_MSG", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(git_dir.join(f));
        }
    }
    if deferred {
        write_index(&repo, &mut index.clone(), index_mtime)?;
    }
    crate::hooks::run(&git_hooks, "post-commit", &hook_opts(&repo.index_path(), &[]))?;
    if let (Some(old), false) = (head.filter(|_| args.amend), args.no_post_rewrite) {
        // githooks(5): `<old> <new>` lines on standard input, argument
        // `amend`.
        let input = format!("{} {}\n", old.to_hex(), oid.to_hex());
        crate::hooks::run(
            &git_hooks,
            "post-rewrite",
            &RunOptions::with_args(["amend"]).stdin(rgit_hook::Input::Data(input.as_bytes())),
        )?;
    }
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
    let pairs = tree_changes::diff(&repo, &base, &new_flat, true);
    if parents.len() < 2 {
        out.extend_from_slice(&diff_summary(&repo, &pairs, quote_high));
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&out);
    let _ = o.flush();
    Ok(0)
}
