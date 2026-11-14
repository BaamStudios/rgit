// SPDX-License-Identifier: MIT
//! `git merge` (Documentation/git-merge.adoc, merge-options.adoc,
//! config/merge.adoc, git-fmt-merge-msg.adoc), part of #57.
//!
//! - Fast-forward (`--ff`, `--no-ff`, `--ff-only`, `merge.ff`), "Already
//!   up to date.", merges into an unborn branch;
//! - true merges with the `ort` engine of `rgit-merge` (also as `-s
//!   recursive`), `-s ours`, `-X ours|theirs|diff-algorithm=...`,
//!   `--allow-unrelated-histories`; the index and work tree move from
//!   `HEAD` to the result with a two-way merge (`rgit-unpack`), refusing
//!   with git's messages when local changes or untracked files would be
//!   lost, then the conflicted paths get their stages 1 to 3;
//! - the merge commit with git's default message (fmt-merge-msg rules,
//!   `merge.log`/`--log`, `merge.suppressDest`, `--into-name`), `-m`,
//!   `-F`, `-e`/`--no-edit`, `--cleanup`, `--signoff`, `--no-commit`,
//!   `--squash` (`SQUASH_MSG`);
//! - the state files `MERGE_HEAD`, `MERGE_MSG` (with the "Conflicts:"
//!   block), `MERGE_MODE`, `AUTO_MERGE`, `ORIG_HEAD`; `--abort` (a
//!   `reset --merge`), `--continue` (a `commit`), `--quit`;
//! - the diffstat (`--stat`, `-n`, `--compact-summary`, `merge.stat`),
//!   the reflog entries ("merge <name>: Fast-forward", "... Merge made by
//!   the 'ort' strategy.", `GIT_REFLOG_ACTION`),
//!   `branch.<name>.mergeOptions`, merging the upstream without
//!   arguments (`merge.defaultToUpstream`), `FETCH_HEAD`, suggestions of
//!   remote-tracking branches for unknown names;
//! - several heads: heads contained in `HEAD` or in another head are
//!   dropped; the rest is merged with a simple `octopus` (one head after
//!   the other, fast-forwarding while nothing was merged, conflicts only
//!   in the last one), the strategy from `-s`, `pull.twohead` or
//!   `pull.octopus`;
//! - hooks `pre-merge-commit`, `prepare-commit-msg`, `commit-msg`
//!   (`--no-verify`) and `post-merge`.
//!
//! The behaviour was checked against the `git` binary with differential
//! tests (`tests/merge.rs`).
//!
//! Not yet: rename detection (#151; merges run as with `-X no-renames`),
//! the strategies `resolve` and `subtree` and trying several strategies
//! (#153), `--autostash`, `--verify-signatures` and `-S` (#51), rerere
//! (#156). The octopus messages are rgit's own approximation.

#[path = "merge/message.rs"]
mod message;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rgit_commit::message::{self as cmsg, Cleanup};
use rgit_config::SystemEnv;
use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;
use rgit_hook::RunOptions;
use rgit_ident::ident::{IdentConfig, Resolver as IdentResolver, Role, Strictness};
use rgit_index::{AddOptions, Entry, Index, Stage};
use rgit_merge::{MergeResult, ObjectCommits};
use rgit_object::{ObjectKind, Signature};
use rgit_refs::{RefUpdate, Transaction};
use rgit_unpack::{Identity, Merge, Porcelain, Worktree};
use rgit_worktree::stat::smudge_racy_entries;

use self::message::Head;
use crate::commands::read_tree::{lock_failed, path_options, unpack_failed};
use crate::diff::collect::TreeDiff;
use crate::diff::options::{DiffOpts, Formats};
use crate::diff::output::Output;
use crate::messages::merge as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::Mode;
use crate::repository::Repo;
use crate::revision::Resolver;

const FF_NO: i32 = 0;
const FF_ALLOW: i32 = 1;
const FF_ONLY: i32 = 2;

/// `--log` without a number (and `merge.log=true`).
const DEFAULT_LOG: usize = 20;

/// The command line.
struct Args {
    stat: i32,
    compact: i32,
    log: i32,
    squash: bool,
    commit: i32,
    edit: i32,
    cleanup: Option<String>,
    ff: i32,
    strategies: Vec<String>,
    xopts: Vec<String>,
    messages: Vec<String>,
    file: Option<String>,
    into_name: Option<String>,
    quiet: bool,
    abort: bool,
    quit: bool,
    cont: bool,
    allow_unrelated: bool,
    gpg_sign: bool,
    verify_signatures: bool,
    autostash: i32,
    overwrite_ignore: bool,
    signoff: bool,
    verify: bool,
    heads: Vec<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    let stat = Cell::new(-1);
    let compact = Cell::new(-1);
    let log = Cell::new(-1);
    let squash = Cell::new(-1);
    let commit = Cell::new(-1);
    let edit = Cell::new(-1);
    let cleanup = RefCell::new(None);
    let ff = Cell::new(-1);
    let rerere = Cell::new(-1);
    let verify_signatures = Cell::new(-1);
    let strategies = RefCell::new(Vec::new());
    let xopts = RefCell::new(Vec::new());
    let messages = RefCell::new(Vec::new());
    let file = RefCell::new(None);
    let into_name = RefCell::new(None);
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let abort = Cell::new(0);
    let quit = Cell::new(0);
    let cont = Cell::new(0);
    let allow_unrelated = Cell::new(0);
    let progress = Cell::new(-1);
    let gpg_sign = Cell::new(false);
    let autostash = Cell::new(-1);
    let overwrite_ignore = Cell::new(1);
    let signoff = Cell::new(false);
    let verify = Cell::new(1);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::set_int('n', None, &stat, msg::HELP_N, 0).noneg(),
            Opt::bool(None, "stat", &stat, msg::HELP_STAT),
            Opt::bool(None, "summary", &stat, msg::HELP_SUMMARY),
            Opt::bool(None, "compact-summary", &compact, msg::HELP_COMPACT_SUMMARY),
            Opt::callback(None, "log", Some("n"), msg::HELP_LOG, |v, unset| {
                let n = match (v, unset) {
                    (_, true) => 0,
                    (None, false) => DEFAULT_LOG as i32,
                    (Some(v), false) => v
                        .parse::<i32>()
                        .map_err(|_| CallbackError::Error(msg::LOG_NOT_A_NUMBER.to_owned()))?,
                };
                log.set(n);
                Ok(())
            })
            .optional(None),
            Opt::bool(None, "squash", &squash, msg::HELP_SQUASH),
            Opt::bool(None, "commit", &commit, msg::HELP_COMMIT),
            Opt::bool('e', "edit", &edit, msg::HELP_EDIT),
            Opt::string(None, "cleanup", &cleanup, "mode", msg::HELP_CLEANUP),
            Opt::set_int(None, "ff", &ff, msg::HELP_FF, FF_ALLOW),
            Opt::set_int(None, "ff-only", &ff, msg::HELP_FF_ONLY, FF_ONLY).noneg(),
            Opt::bool(None, "rerere-autoupdate", &rerere, msg::HELP_RERERE),
            Opt::bool(
                None,
                "verify-signatures",
                &verify_signatures,
                msg::HELP_VERIFY_SIGNATURES,
            ),
            Opt::string_list('s', "strategy", &strategies, "strategy", msg::HELP_STRATEGY),
            Opt::string_list(
                'X',
                "strategy-option",
                &xopts,
                "option=value",
                msg::HELP_STRATEGY_OPTION,
            ),
            Opt::string_list('m', "message", &messages, "message", msg::HELP_MESSAGE),
            Opt::string('F', "file", &file, "path", msg::HELP_FILE).noneg(),
            Opt::string(None, "into-name", &into_name, "name", msg::HELP_INTO_NAME),
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::bool(None, "abort", &abort, msg::HELP_ABORT),
            Opt::bool(None, "quit", &quit, msg::HELP_QUIT),
            Opt::bool(None, "continue", &cont, msg::HELP_CONTINUE),
            Opt::bool(
                None,
                "allow-unrelated-histories",
                &allow_unrelated,
                msg::HELP_ALLOW_UNRELATED,
            ),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::callback('S', "gpg-sign", Some("key-id"), msg::HELP_GPG_SIGN, |_, unset| {
                gpg_sign.set(!unset);
                Ok(())
            })
            .optional(None),
            Opt::bool(None, "autostash", &autostash, msg::HELP_AUTOSTASH),
            Opt::bool(
                None,
                "overwrite-ignore",
                &overwrite_ignore,
                msg::HELP_OVERWRITE_IGNORE,
            ),
            Opt::flag(None, "signoff", &signoff, msg::HELP_SIGNOFF),
            Opt::set_int(None, "no-verify", &verify, msg::HELP_NO_VERIFY, 0).noneg(),
            Opt::set_int(None, "verify", &verify, msg::HELP_VERIFY, 1).noneg(),
        ],
    );
    let heads = parser.parse(argv)?.args;
    let usage_error = |m: &str| Die::from(parser.usage_msg(m));
    let (abort, quit, cont) = (abort.get() == 1, quit.get() == 1, cont.get() == 1);
    for (set, name) in [(abort, "--abort"), (quit, "--quit"), (cont, "--continue")] {
        if set && !heads.is_empty() {
            return Err(usage_error(&msg::expects_no_arguments(name)));
        }
    }
    if abort as u8 + quit as u8 + cont as u8 > 1 {
        let name = if abort { "--abort" } else { "--quit" };
        return Err(usage_error(&msg::expects_no_arguments(name)));
    }
    drop(parser);
    Ok(Args {
        stat: stat.get(),
        compact: compact.get(),
        log: log.get(),
        squash: squash.get() == 1,
        commit: commit.get(),
        edit: edit.get(),
        cleanup: cleanup.into_inner(),
        ff: ff.get(),
        strategies: strategies.into_inner(),
        xopts: xopts.into_inner(),
        messages: messages.into_inner(),
        file: file.into_inner(),
        into_name: into_name.into_inner(),
        quiet: quiet.get() > 0,
        abort,
        quit,
        cont,
        allow_unrelated: allow_unrelated.get() == 1,
        gpg_sign: gpg_sign.get(),
        verify_signatures: verify_signatures.get() == 1,
        autostash: autostash.get(),
        overwrite_ignore: overwrite_ignore.get() != 0,
        signoff: signoff.get(),
        verify: verify.get() != 0,
        heads,
    })
}

fn fatal(s: impl Into<String>) -> Die {
    Die::Fatal(s.into())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Everything a merge needs besides the arguments.
struct Session<'a> {
    ctx: &'a Context,
    repo: Repo,
    args: Args,
    git_dir: PathBuf,
    /// `HEAD`'s branch (full name), `None` when detached.
    branch: Option<String>,
    /// `GIT_REFLOG_ACTION` or "merge <names>".
    action: String,
    strategy: String,
    show_stat: bool,
    /// `--compact-summary`, `merge.stat=compact`.
    compact: bool,
    /// `--cleanup`, `commit.cleanup`.
    cleanup: Cleanup,
    log: usize,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.len() == 2 && (argv[1] == "-h" || argv[1] == "--help-all") {
        parse_args(argv)?;
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    let branch = repo.refs.head_target();
    // branch.<name>.mergeOptions come before the command line.
    let mut full = vec![argv[0].clone()];
    if let Some(b) = branch.as_deref().and_then(|b| b.strip_prefix("refs/heads/")) {
        let key = format!("branch.{b}.mergeoptions");
        if let Some(v) = repo.config.get(&key).and_then(|e| e.value.as_ref()) {
            full.extend(lossy(v).split_whitespace().map(str::to_owned));
        }
    }
    full.extend(argv.iter().skip(1).cloned());
    let args = parse_args(&full)?;
    repo.work_tree()?;
    let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);

    if args.abort {
        if !git_dir.join("MERGE_HEAD").exists() {
            return Err(fatal(msg::NO_MERGE_TO_ABORT));
        }
        drop(repo);
        return super::reset::run(ctx, &["reset".to_owned(), "--merge".to_owned()]);
    }
    if args.quit {
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "MERGE_RR", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(git_dir.join(f));
        }
        return Ok(0);
    }
    if args.cont {
        if !git_dir.join("MERGE_HEAD").exists() {
            return Err(fatal(msg::NO_MERGE_IN_PROGRESS));
        }
        drop(repo);
        return super::commit::run(ctx, &["commit".to_owned()]);
    }

    let ff_config = match repo.config.get("merge.ff").and_then(|e| e.value.as_ref()) {
        Some(v) if v.eq_ignore_ascii_case(b"only") => FF_ONLY,
        Some(v) => match rgit_config::value::parse_bool(Some(v)) {
            Some(false) => FF_NO,
            _ => FF_ALLOW,
        },
        None => FF_ALLOW,
    };
    let mut args = args;
    if args.ff < 0 {
        args.ff = ff_config;
    }
    if args.squash {
        if args.ff == FF_NO {
            return Err(fatal(msg::incompatible("--squash", "--no-ff")));
        }
        if args.commit == 1 {
            return Err(fatal(msg::incompatible("--squash", "--commit")));
        }
    }
    if args.gpg_sign {
        return Err(fatal(msg::option_not_supported("-S/--gpg-sign")));
    }
    let verify_signatures = args.verify_signatures
        || repo
            .config
            .bool("merge.verifysignatures")
            .ok()
            .flatten()
            .unwrap_or(false);
    if verify_signatures {
        return Err(fatal(msg::option_not_supported("--verify-signatures")));
    }
    let autostash = match args.autostash {
        -1 => repo
            .config
            .bool("merge.autostash")
            .ok()
            .flatten()
            .unwrap_or(false),
        v => v == 1,
    };
    if autostash {
        return Err(fatal(msg::option_not_supported("--autostash")));
    }

    // A merge or cherry-pick in progress, or conflicts in the index.
    let (index, _) = repo.read_index()?;
    if index.has_conflicts() {
        eprintln!("error: {}", msg::UNMERGED);
        ctx.advisor(&repo.config)?
            .advise_without_footer(Advice::ResolveConflict, crate::messages::commit::UNMERGED_HINT)
            .map_err(Die::Fatal)?;
        return Err(fatal(crate::messages::commit::UNRESOLVED));
    }
    drop(index);
    if git_dir.join("MERGE_HEAD").exists() {
        return Err(fatal(msg::NOT_CONCLUDED));
    }
    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return Err(fatal(msg::NOT_CONCLUDED_CHERRY_PICK));
    }

    // The strategy.
    check_strategies(&args.strategies)?;
    let strategy = "ort".to_owned();

    // Any value but a false one shows the diffstat, "compact" the compact
    // summary (config/merge.adoc).
    let (stat_config, compact_config) =
        match repo.config.get("merge.stat").and_then(|e| e.value.as_ref()) {
            Some(v) if v.eq_ignore_ascii_case(b"compact") => (true, true),
            Some(v) => (
                !matches!(rgit_config::value::parse_bool(Some(v)), Some(false)),
                false,
            ),
            None => (true, false),
        };
    let (mut show_stat, mut compact) = match args.stat {
        -1 => (stat_config, compact_config),
        v => (v == 1, false),
    };
    match args.compact {
        1 => (show_stat, compact) = (true, true),
        0 => compact = false,
        _ => {}
    }
    let log = match args.log {
        -1 => match repo.config.get("merge.log").and_then(|e| e.value.as_ref()) {
            // A number, or a boolean (true means 20).
            Some(v) => match lossy(v).trim().parse::<i64>() {
                Ok(n) => usize::try_from(n).unwrap_or(0),
                Err(_) => match rgit_config::value::parse_bool(Some(v)) {
                    Some(true) => DEFAULT_LOG,
                    _ => 0,
                },
            },
            None if repo.config.get("merge.log").is_some() => DEFAULT_LOG,
            None => 0,
        },
        v => usize::try_from(v).unwrap_or(0),
    };

    let cleanup = match args
        .cleanup
        .clone()
        .or_else(|| {
            repo.config
                .get("commit.cleanup")
                .and_then(|e| e.value.as_ref())
                .map(|v| lossy(v))
        })
        .as_deref()
    {
        None => Cleanup::Default,
        Some(m) => {
            Cleanup::parse(m).ok_or_else(|| fatal(crate::messages::commit::invalid_cleanup(m)))?
        }
    };
    let mut s = Session {
        ctx,
        repo,
        args,
        git_dir,
        branch,
        action: String::new(),
        strategy,
        show_stat,
        compact,
        cleanup,
        log,
    };
    s.merge()
}

impl Session<'_> {
    /// The commits named on the command line (or the upstream).
    fn heads(&mut self) -> Result<Vec<Head>, Die> {
        let names = if self.args.heads.is_empty() {
            vec![self.upstream()?]
        } else {
            self.args.heads.clone()
        };
        let mut heads = Vec::new();
        for name in names {
            if name == "FETCH_HEAD" {
                heads.extend(self.fetch_heads()?);
                continue;
            }
            let given = name;
            let name = self.expand_upstream(&given)?;
            let mut r = Resolver::new(&mut self.repo);
            let found = r.resolve(&name).ok().and_then(|res| {
                r.peel_to(res.oid, ObjectKind::Commit, Some(&name))
                    .map(|c| (res.oid, c))
            });
            let Some((object, commit)) = found else {
                eprintln!("{}", msg::not_something_we_can_merge(&name));
                self.suggest(&name);
                return Err(Die::Exit(1));
            };
            let mut head = message::name_head(&self.repo, &name, object, commit);
            // The reflog and the conflict labels keep the name as given.
            head.arg = given;
            heads.push(head);
        }
        Ok(heads)
    }

    /// `[<branch>]@{upstream}` (`@{u}`) at the start of `name` replaced by
    /// the short name of the upstream, as the reference `git` names the
    /// merged commit (`new@{u}~1` is merged as `origin/side~1`). `name`
    /// unchanged when it has no such part; a branch without a usable
    /// upstream is fatal.
    fn expand_upstream(&mut self, name: &str) -> Result<String, Die> {
        let unchanged = || Ok(name.to_owned());
        let Some(at) = name.find("@{") else {
            return unchanged();
        };
        let Some(close) = name[at..].find('}') else {
            return unchanged();
        };
        let mark = name[at + 2..at + close].to_ascii_lowercase();
        if mark != "u" && mark != "upstream" {
            return unchanged();
        }
        let branch = match &name[..at] {
            "" => match self
                .branch
                .as_deref()
                .and_then(|b| b.strip_prefix("refs/heads/"))
            {
                Some(b) => b.to_owned(),
                None => return unchanged(),
            },
            b => b.to_owned(),
        };
        if self.repo.resolve(&format!("refs/heads/{branch}")).is_none() {
            return Err(fatal(rgit_revwalk::revision::messages::no_such_branch(
                &branch,
            )));
        }
        let config = &self.repo.config;
        let b = rgit_remote::BranchConfig::from_config(config, branch.as_bytes())
            .map_err(|e| fatal(e.to_string()))?;
        let remotes = rgit_remote::Remotes::from_config(config).map_err(|e| fatal(e.to_string()))?;
        let full = rgit_remote::upstream_ref(&remotes, Some(&self.repo.setup.git_dir), Some(&b))
            .map_err(|e| fatal(e.to_string()))?
            .to_string();
        let short = ["refs/heads/", "refs/tags/", "refs/remotes/"]
            .iter()
            .find_map(|p| full.strip_prefix(p))
            .unwrap_or(&full);
        Ok(format!("{short}{}", &name[at + close + 1..]))
    }

    /// Remote-tracking branches with the name the user may have meant.
    fn suggest(&self, name: &str) {
        let suffix = format!("/{name}");
        let found: Vec<String> = self
            .repo
            .refs
            .list("refs/remotes/")
            .into_iter()
            .map(|(r, _)| r)
            .filter(|r| r.ends_with(&suffix))
            .map(|full| {
                let short = full.trim_start_matches("refs/remotes/");
                if message::dwim(&self.repo, short).as_deref() == Some(full.as_str()) {
                    short.to_owned()
                } else {
                    full.trim_start_matches("refs/").to_owned()
                }
            })
            .collect();
        if found.is_empty() {
            return;
        }
        let mut text = String::from("\n");
        text.push_str(if found.len() == 1 {
            msg::DID_YOU_MEAN_ONE
        } else {
            msg::DID_YOU_MEAN_MANY
        });
        for f in &found {
            text.push_str("\n\t");
            text.push_str(f);
        }
        eprintln!("{text}");
    }

    /// The heads of `FETCH_HEAD` marked for merging.
    fn fetch_heads(&mut self) -> Result<Vec<Head>, Die> {
        let path = self.git_dir.join("FETCH_HEAD");
        let data = std::fs::read(&path).map_err(|e| {
            let shown = self.repo.setup.git_dir.join("FETCH_HEAD");
            fatal(msg::could_not_open_for_reading(
                &shown.to_string_lossy(),
                &crate::global::strerror(&e),
            ))
        })?;
        let mut heads = Vec::new();
        for line in data.split(|&b| b == b'\n') {
            let mut parts = line.splitn(3, |&b| b == b'\t');
            let (Some(id), Some(flag), Some(desc)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if !flag.is_empty() {
                continue;
            }
            let Ok(id) = ObjectId::from_hex(self.repo.hash, id) else {
                continue;
            };
            let Some(commit) = self.repo.peel_to(id, ObjectKind::Commit) else {
                eprintln!("{}", msg::not_something_we_can_merge(&id.to_hex()));
                return Err(Die::Exit(1));
            };
            heads.push(Head {
                arg: id.to_hex(),
                name: lossy(desc),
                kind: message::Kind::Fetch,
                commit,
                tag_message: None,
                signed_tag: message::signed_tag(&self.repo, id, commit),
            });
        }
        Ok(heads)
    }

    /// `merge` without arguments: the upstream of the current branch.
    fn upstream(&self) -> Result<String, Die> {
        let enabled = self
            .repo
            .config
            .bool("merge.defaulttoupstream")
            .ok()
            .flatten()
            .unwrap_or(true);
        if !enabled {
            return Err(fatal(msg::NO_COMMIT_SPECIFIED));
        }
        let Some(branch) = self
            .branch
            .as_deref()
            .and_then(|b| b.strip_prefix("refs/heads/"))
        else {
            return Err(fatal(msg::NO_CURRENT_BRANCH));
        };
        let config = &self.repo.config;
        let b = rgit_remote::BranchConfig::from_config(config, branch.as_bytes())
            .map_err(|e| fatal(e.to_string()))?;
        let Some(remote) = &b.remote else {
            return Err(fatal(msg::NO_REMOTE));
        };
        let Some(merge) = b.merge.first() else {
            return Err(fatal(msg::NO_DEFAULT_UPSTREAM));
        };
        let remotes = rgit_remote::Remotes::from_config(config).map_err(|e| fatal(e.to_string()))?;
        match rgit_remote::upstream_ref(&remotes, Some(&self.repo.setup.git_dir), Some(&b)) {
            Ok(r) => Ok(r.to_string()),
            Err(_) => Err(fatal(msg::no_remote_tracking_branch(
                &merge.to_string(),
                &remote.to_string(),
            ))),
        }
    }

    fn merge(&mut self) -> CmdResult {
        let mut heads = self.heads()?;
        self.action = std::env::var("GIT_REFLOG_ACTION").unwrap_or_else(|_| {
            let mut a = "merge".to_owned();
            let mut seen = Vec::new();
            for h in &heads {
                if !seen.contains(&h.commit) {
                    seen.push(h.commit);
                    a.push(' ');
                    a.push_str(&h.arg);
                }
            }
            a
        });
        let head = self
            .repo
            .resolve("HEAD")
            .filter(|_| self.repo.refs.resolve("HEAD").is_some())
            .and_then(|h| self.repo.peel_to(h, ObjectKind::Commit));
        let Some(head) = head else {
            return self.merge_into_unborn(&heads);
        };
        self.write_orig_head(head)?;

        // Only the heads not contained in HEAD or in another head count
        // (and HEAD itself when it is not contained in a head).
        let mut tips: Vec<ObjectId> = vec![head];
        for h in &heads {
            if !tips.contains(&h.commit) {
                tips.push(h.commit);
            }
        }
        let independent = self.independent(&tips)?;
        let mut kept: Vec<Head> = Vec::new();
        for h in heads.drain(..) {
            if h.commit != head
                && independent.contains(&h.commit)
                && !kept.iter().any(|k| k.commit == h.commit)
            {
                kept.push(h);
            }
        }
        if kept.is_empty() {
            if !self.args.quiet {
                println!("{}", msg::ALREADY_UP_TO_DATE);
            }
            return Ok(0);
        }
        self.strategy = self.choose_strategy(kept.len() > 1)?;
        if kept.len() > 1 {
            if self.args.ff == FF_ONLY {
                let advisor = self.ctx.advisor(&self.repo.config)?;
                advisor
                    .advise(Advice::Diverging, msg::DIVERGING_HINT)
                    .map_err(Die::Fatal)?;
                return Err(fatal(msg::NOT_POSSIBLE_FF));
            }
            return self.octopus(head, kept);
        }
        let theirs = kept.pop().expect("one head");
        let commits = self.commits();
        let bases = rgit_merge::merge_bases(&commits, &head, &theirs.commit)
            .map_err(|e| fatal(e.to_string()))?;
        drop(commits);
        let ff_possible = bases.len() == 1 && bases[0] == head;
        if ff_possible && self.args.ff != FF_NO && self.strategy != "ours" {
            return self.fast_forward(head, &theirs);
        }
        if self.args.ff == FF_ONLY {
            let advisor = self.ctx.advisor(&self.repo.config)?;
            advisor
                .advise(Advice::Diverging, msg::DIVERGING_HINT)
                .map_err(Die::Fatal)?;
            return Err(fatal(msg::NOT_POSSIBLE_FF));
        }
        if bases.is_empty() && !self.args.allow_unrelated {
            return Err(fatal(msg::UNRELATED_HISTORIES));
        }
        self.true_merge(head, &theirs, &bases)
    }

    /// The strategy for one head or several (`-s`, else `pull.twohead` or
    /// `pull.octopus`, else `ort` or `octopus`). Of several candidates
    /// the first one rgit can carry out is used (git tries them all and
    /// keeps the best result).
    fn choose_strategy(&self, several: bool) -> Result<String, Die> {
        let mut names = self.args.strategies.clone();
        if names.is_empty() {
            let key = if several { "pull.octopus" } else { "pull.twohead" };
            if let Some(v) = self.repo.config.get(key).and_then(|e| e.value.as_ref()) {
                names = lossy(v).split_whitespace().map(str::to_owned).collect();
                check_strategies(&names)?;
            }
        }
        if names.is_empty() {
            return Ok(if several { "octopus" } else { "ort" }.to_owned());
        }
        let usable: &[&str] = if several {
            &["octopus", "ours"]
        } else {
            &["ort", "recursive", "ours"]
        };
        if let Some(n) = names.iter().find(|n| usable.contains(&n.as_str())) {
            return Ok(n.clone());
        }
        if several && names.iter().all(|n| matches!(n.as_str(), "ort" | "recursive")) {
            // These handle two heads only.
            eprintln!("{}", msg::strategy_failed(&names[0]));
            return Err(Die::Exit(2));
        }
        // resolve, subtree: refused when a real merge is needed.
        Ok(names[0].clone())
    }

    /// Commit access for the merge engine.
    fn commits(&self) -> ObjectCommits<'_> {
        let shallow = std::fs::read_to_string(self.repo.setup.common_dir.join("shallow"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| ObjectId::from_hex(self.repo.hash, l.trim().as_bytes()).ok())
            .collect();
        let len = self.repo.default_abbrev();
        let repo = &self.repo;
        ObjectCommits {
            objects: repo.odb(),
            shallow,
            label: Box::new(move |id| repo.abbrev(id, len).into_bytes()),
        }
    }

    fn tree_of(&self, commit: &ObjectId) -> Result<ObjectId, Die> {
        crate::switch_branch::commit_tree(&self.repo, commit)
            .ok_or_else(|| fatal(rgit_merge::messages::not_a_commit(&commit.to_hex())))
    }

    /// `git merge <commit>` on an unborn branch: check the commit out and
    /// point the branch at it.
    fn merge_into_unborn(&mut self, heads: &[Head]) -> CmdResult {
        if self.args.squash {
            return Err(fatal(msg::SQUASH_INTO_EMPTY));
        }
        if heads.len() != 1 {
            return Err(fatal(msg::ONLY_ONE_INTO_EMPTY));
        }
        let commit = heads[0].commit;
        let tree = self.tree_of(&commit)?;
        let repo = &self.repo;
        let work_tree = repo.work_tree()?;
        let index_path = repo.setup.cwd.join(repo.index_path());
        let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
        let (index, mtime) = repo.read_index()?;
        let checker = repo.checker(mtime)?;
        let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
        let wt = Worktree {
            root: &work_tree,
            checker: &checker,
        };
        let mut opts = rgit_unpack::Options {
            update: true,
            paths: path_options(repo),
            excludes: Some(&mut excludes),
            ..rgit_unpack::Options::default()
        };
        let plan = match rgit_unpack::plan(repo.odb(), &index, &Merge::OneWay(tree), Some(&wt), &mut opts) {
            Ok(p) => p,
            Err(rgit_unpack::Error::Rejected(r)) => {
                for m in r.plumbing_messages().into_iter().take(1) {
                    eprintln!("error: {m}");
                }
                return Err(fatal(msg::READ_TREE_FAILED));
            }
            Err(e) => return Err(unpack_failed(e)),
        };
        let applied = plan.apply(repo.odb(), &wt, &Identity);
        let mut index = applied.index;
        if let Some(reference) = mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        crate::switch_branch::write_index(repo, &index, lock)?;
        if !applied.errors.is_empty() {
            for e in &applied.errors {
                eprintln!("error: {e}");
            }
            return Err(Die::Exit(128));
        }
        self.update_head(commit, None, msg::REFLOG_INITIAL)?;
        Ok(0)
    }

    /// Moves `HEAD` (its branch) to `new` with the reflog message `what`.
    fn update_head(&self, new: ObjectId, old: Option<ObjectId>, what: &str) -> Result<(), Die> {
        let setup = &self.repo.setup;
        let config = refs_support::load_config(self.ctx, setup)?;
        let store = refs_support::ref_store(setup, &config);
        let objects = Objects::open(setup);
        let mut hooks = UpdateHooks::new(setup, &config, &objects)
            .with_transaction_hook(crate::hooks::for_repo(self.ctx, setup, &config)?);
        let update = match old {
            Some(o) => RefUpdate::update("HEAD", new, Some(o)),
            None => RefUpdate::create("HEAD", new),
        };
        let message = if old.is_some() {
            msg::reflog(&self.action, what)
        } else {
            what.to_owned()
        };
        let mut tx = Transaction::new(&store);
        tx.push(update.message(message))
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| fatal(e.to_string()))
    }

    /// Records `ORIG_HEAD`.
    fn write_orig_head(&self, head: ObjectId) -> Result<(), Die> {
        let setup = &self.repo.setup;
        let config = refs_support::load_config(self.ctx, setup)?;
        let store = refs_support::ref_store(setup, &config);
        let objects = Objects::open(setup);
        let mut hooks = UpdateHooks::new(setup, &config, &objects)
            .with_transaction_hook(crate::hooks::for_repo(self.ctx, setup, &config)?);
        let mut tx = Transaction::new(&store);
        tx.push(
            RefUpdate::update("ORIG_HEAD", head, None)
                .no_deref(true)
                .message(crate::messages::reset::UPDATING_ORIG_HEAD),
        )
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| fatal(e.to_string()))
    }

    fn fast_forward(&mut self, head: ObjectId, theirs: &Head) -> CmdResult {
        let abbrev = self.repo.default_abbrev();
        if !self.args.quiet {
            println!(
                "{}",
                msg::updating(
                    &self.repo.abbrev(&head, abbrev),
                    &self.repo.abbrev(&theirs.commit, abbrev)
                )
            );
            let _ = std::io::stdout().flush();
        }
        let old_tree = self.tree_of(&head)?;
        let new_tree = self.tree_of(&theirs.commit)?;
        self.checkout(old_tree, new_tree, None)?;
        if !self.args.quiet {
            println!("{}", msg::FAST_FORWARD);
        }
        if self.args.squash {
            if !self.args.quiet {
                println!("{}", msg::SQUASH_NOT_UPDATING);
            }
            self.write_squash_msg(&[theirs.commit], head)?;
        } else {
            self.update_head(theirs.commit, Some(head), msg::FAST_FORWARD)?;
        }
        if self.show_stat && !self.args.quiet {
            self.print_stat(old_tree, new_tree)?;
        }
        self.post_merge()?;
        Ok(0)
    }

    /// The index differs from `HEAD`'s tree: the paths that differ.
    fn staged_changes(&self, head_tree: ObjectId) -> Result<Vec<Vec<u8>>, Die> {
        let tree = rgit_unpack::tree_entries(self.repo.odb(), &head_tree).map_err(unpack_failed)?;
        let (index, _) = self.repo.read_index()?;
        let mut out = Vec::new();
        let entries: Vec<&Entry> = index.entries().iter().collect();
        let (mut i, mut t) = (0, 0);
        while i < entries.len() || t < tree.len() {
            let order = match (entries.get(i), tree.get(t)) {
                (Some(e), Some(x)) => e.path().cmp(x.path()),
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Greater,
            };
            match order {
                std::cmp::Ordering::Less => {
                    out.push(entries[i].path().to_vec());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(tree[t].path().to_vec());
                    t += 1;
                }
                std::cmp::Ordering::Equal => {
                    let (e, x) = (entries[i], &tree[t]);
                    if e.mode != x.mode || e.id != x.id || e.flags.intent_to_add {
                        out.push(e.path().to_vec());
                    }
                    i += 1;
                    t += 1;
                }
            }
        }
        Ok(out)
    }

    fn engine_options(&self, theirs: &Head) -> Result<rgit_merge::Options, Die> {
        let mut opts = rgit_merge::Options {
            branch1: b"HEAD".to_vec(),
            branch2: theirs.arg.as_bytes().to_vec(),
            style: super::merge_file::config_style(&self.repo.config)?,
            allow_unrelated_histories: self.args.allow_unrelated,
            ..rgit_merge::Options::default()
        };
        for x in &self.args.xopts {
            use super::merge_tree::StrategyOptionError as E;
            match super::merge_tree::apply_strategy_option(&mut opts, x) {
                Ok(()) => {}
                Err(E::Unknown) => {
                    return Err(fatal(crate::messages::merge_tree::unknown_strategy_option(x)));
                }
                Err(E::Unsupported) => {
                    return Err(fatal(msg::option_not_supported(&format!("-X{x}"))));
                }
            }
        }
        Ok(opts)
    }

    fn true_merge(&mut self, head: ObjectId, theirs: &Head, bases: &[ObjectId]) -> CmdResult {
        if !matches!(self.strategy.as_str(), "ort" | "recursive" | "ours") {
            return Err(fatal(msg::strategy_not_supported(&self.strategy)));
        }
        let head_tree = self.tree_of(&head)?;
        let staged = self.staged_changes(head_tree)?;
        if !staged.is_empty() {
            let mut text = format!("error: {}\n", msg::LOCAL_CHANGES);
            for p in &staged {
                text.push_str("  ");
                text.push_str(&lossy(p));
                text.push('\n');
            }
            eprint!("{text}");
            eprintln!("{}", msg::strategy_failed(&self.strategy));
            self.keep_local_state(head_tree)?;
            return Err(Die::Exit(2));
        }
        let result = if self.strategy == "ours" {
            MergeResult {
                tree: head_tree,
                clean: true,
                conflicts: Vec::new(),
                messages: Vec::new(),
            }
        } else {
            let opts = self.engine_options(theirs)?;
            let commits = self.commits();
            let r = rgit_merge::merge_commits_with_bases(
                self.repo.odb(),
                &commits,
                &head,
                &theirs.commit,
                bases,
                &opts,
            )
            .map_err(|e| fatal(e.to_string()))?;
            drop(commits);
            r
        };
        self.finish(head, vec![head, theirs.commit], std::slice::from_ref(theirs), result)
    }

    /// The common end of a merge that was not a fast-forward: the index
    /// and work tree move from `HEAD` (`head`) to `result`, then the
    /// state files, the commit with `parents` (or the stop before it)
    /// and the messages.
    fn finish(
        &mut self,
        head: ObjectId,
        parents: Vec<ObjectId>,
        theirs: &[Head],
        result: MergeResult,
    ) -> CmdResult {
        let head_tree = self.tree_of(&head)?;
        let their_ids: Vec<ObjectId> = theirs.iter().map(|h| h.commit).collect();
        // The index and work tree are updated before the messages appear.
        if let Err(e) = self.checkout(head_tree, result.tree, Some(&result)) {
            if matches!(e, Die::Exit(1)) {
                eprintln!("{}", msg::strategy_failed(&self.strategy));
                self.keep_local_state(head_tree)?;
                return Err(Die::Exit(2));
            }
            return Err(e);
        }
        let mut out = Vec::new();
        for m in &result.messages {
            out.extend_from_slice(&m.text);
            out.push(b'\n');
        }
        {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
        super::merge_tree::advise_submodules(self.ctx, &self.repo, &result)?;
        if self.strategy != "ours" {
            self.write_file("AUTO_MERGE", format!("{}\n", result.tree.to_hex()).as_bytes())?;
        }

        let comment = super::commit::comment_string(&self.repo, b"")?;
        let mut message = self.message(theirs, head)?;
        if self.args.signoff {
            message = self.signoff(&message, &comment)?;
        }
        if !result.clean {
            if parents[0] != head {
                // An octopus fast-forwarded to its first head before the
                // conflict.
                self.update_head(parents[0], Some(head), msg::FAST_FORWARD)?;
            }
            let paths: Vec<Vec<u8>> = result.conflicts.iter().map(|c| c.path.clone()).collect();
            let mut block = Vec::new();
            if self.cleanup == Cleanup::Scissors {
                // config/merge: scissors go before the conflicts so that the
                // block is cut off when the message is edited.
                block.push(b'\n');
                block.extend_from_slice(&scissors_block(&comment));
                block.extend_from_slice(comment.as_bytes());
            }
            block.extend_from_slice(&message::conflicts_block(
                &comment,
                &paths,
                self.repo.quote_path(),
            ));
            if self.args.squash {
                self.write_squash_msg(&their_ids, head)?;
                self.write_file("MERGE_MSG", &block)?;
                println!("{}", msg::SQUASH_NOT_UPDATING);
            } else {
                let mut m = message.clone();
                m.extend_from_slice(&block);
                self.write_merge_state(&parents[1..], &m)?;
            }
            println!("{}", msg::AUTOMATIC_FAILED);
            return Ok(1);
        }
        if self.args.squash {
            self.write_squash_msg(&their_ids, head)?;
            if !self.args.quiet {
                println!("{}", msg::SQUASH_NOT_UPDATING);
            }
            self.post_merge()?;
            eprintln!("{}", msg::WENT_WELL);
            return Ok(0);
        }
        if self.args.commit == 0 {
            self.write_merge_state(&parents[1..], &message)?;
            eprintln!("{}", msg::WENT_WELL);
            return Ok(0);
        }
        let mergetags = message::mergetags(theirs, &parents);
        self.commit(head, head_tree, &parents, result.tree, message, &comment, mergetags)
    }

    /// The commits of `tips` that are not ancestors of another one, in
    /// their order.
    fn independent(&self, tips: &[ObjectId]) -> Result<Vec<ObjectId>, Die> {
        let commits = self.commits();
        let mut out = Vec::new();
        'outer: for (i, a) in tips.iter().enumerate() {
            for (j, b) in tips.iter().enumerate() {
                if i == j || a == b {
                    continue;
                }
                let bases =
                    rgit_merge::merge_bases(&commits, a, b).map_err(|e| fatal(e.to_string()))?;
                if bases.contains(a) {
                    continue 'outer;
                }
            }
            out.push(*a);
        }
        Ok(out)
    }

    /// The octopus strategy: the heads are merged one after the other
    /// (fast-forwarding while nothing was merged yet); only the last one
    /// may leave conflicts (git-merge(1), MERGE STRATEGIES).
    fn octopus(&mut self, head: ObjectId, heads: Vec<Head>) -> CmdResult {
        let head_tree = self.tree_of(&head)?;
        if self.strategy == "ours" {
            let mut parents = vec![head];
            parents.extend(heads.iter().map(|h| h.commit));
            let result = MergeResult {
                tree: head_tree,
                clean: true,
                conflicts: Vec::new(),
                messages: Vec::new(),
            };
            return self.finish(head, parents, &heads, result);
        }
        let staged = self.staged_changes(head_tree)?;
        if !staged.is_empty() {
            let mut text = format!("error: {}\n", msg::LOCAL_CHANGES);
            for p in &staged {
                text.push_str("  ");
                text.push_str(&lossy(p));
                text.push('\n');
            }
            eprint!("{text}");
            eprintln!("{}", msg::strategy_failed(&self.strategy));
            return Err(Die::Exit(2));
        }
        let mut merged: Vec<ObjectId> = vec![head];
        let mut tree = head_tree;
        let mut trees_merged = false;
        let mut last: Option<MergeResult> = None;
        let mut out = Vec::new();
        for (i, h) in heads.iter().enumerate() {
            let commits = self.commits();
            let mut bases: Vec<ObjectId> = Vec::new();
            for m in &merged {
                for b in rgit_merge::merge_bases(&commits, m, &h.commit)
                    .map_err(|e| fatal(e.to_string()))?
                {
                    if !bases.contains(&b) {
                        bases.push(b);
                    }
                }
            }
            drop(commits);
            let bases = self.independent(&bases)?;
            if bases.contains(&h.commit) {
                out.extend_from_slice(msg::octopus_up_to_date(&h.arg).as_bytes());
                continue;
            }
            if !trees_merged && self.args.ff != FF_NO && bases == merged {
                out.extend_from_slice(msg::octopus_fast_forward(&h.arg).as_bytes());
                tree = self.tree_of(&h.commit)?;
                merged = vec![h.commit];
                continue;
            }
            out.extend_from_slice(msg::octopus_trying(&h.arg).as_bytes());
            trees_merged = true;
            let base_tree = match bases.first() {
                Some(b) => self.tree_of(b)?,
                None => ObjectId::empty_tree(self.repo.hash),
            };
            let their_tree = self.tree_of(&h.commit)?;
            let opts = self.engine_options(h)?;
            let r = rgit_merge::merge_trees(self.repo.odb(), &base_tree, &tree, &their_tree, &opts)
                .map_err(|e| fatal(e.to_string()))?;
            for m in &r.messages {
                out.extend_from_slice(&m.text);
                out.push(b'\n');
            }
            merged.push(h.commit);
            tree = r.tree;
            if !r.clean {
                if i + 1 < heads.len() {
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(&out);
                    let _ = o.write_all(msg::OCTOPUS_STOP.as_bytes());
                    let _ = o.flush();
                    drop(o);
                    eprintln!("fatal: {}", msg::MERGE_PROGRAM_FAILED);
                    eprintln!("{}", msg::strategy_failed(&self.strategy));
                    return Err(Die::Exit(2));
                }
                last = Some(r);
                break;
            }
        }
        {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
        let result = match last {
            Some(r) => r,
            None => MergeResult {
                tree,
                clean: true,
                conflicts: Vec::new(),
                messages: Vec::new(),
            },
        };
        if merged.len() == 1 {
            // Every head was a fast-forward.
            let theirs = heads
                .into_iter()
                .find(|h| h.commit == merged[0])
                .expect("the head fast-forwarded to");
            return self.fast_forward(head, &theirs);
        }
        let theirs: Vec<Head> = heads
            .into_iter()
            .filter(|h| merged.contains(&h.commit))
            .collect();
        self.finish(head, merged, &theirs, result)
    }

    /// After a merge refused because of local changes, git leaves what it
    /// saved of them behind (observed with the `git` binary): `AUTO_MERGE`
    /// names the tree of the tracked files as they are in the work tree,
    /// and the index entries of the changed files lose their stat data.
    /// Nothing happens without local changes.
    fn keep_local_state(&self, head_tree: ObjectId) -> Result<(), Die> {
        use rgit_worktree::stat::Status;
        let repo = &self.repo;
        let root = repo.work_tree()?;
        let (mut index, mtime) = repo.read_index()?;
        let checker = repo.checker(mtime)?;
        crate::switch_branch::refresh_stat(&mut index, &checker);
        let mut snapshot = index.clone();
        let mut dirty = Vec::new();
        for (pos, e) in index.entries().iter().enumerate() {
            if e.stage() != Stage::MERGED || e.mode.is_gitlink() {
                continue;
            }
            match checker.check(e) {
                Ok(Status::Unchanged) | Err(_) => {}
                Ok(Status::Deleted) => {
                    dirty.push(pos);
                    snapshot.remove(e.path(), Stage::MERGED);
                }
                Ok(_) => {
                    dirty.push(pos);
                    if let Ok(Some(n)) = super::add::entry_for(repo, &checker, &root, e.path(), Some(e), false, true) {
                        let _ = snapshot.add(n, AddOptions { ok_to_replace: true, ..Default::default() });
                    }
                    snapshot.invalidate_path(e.path());
                }
            }
        }
        snapshot.set_cache_tree(None);
        let tree = super::write_tree::write_tree(repo, &snapshot, true, None)?;
        if dirty.is_empty() && tree == head_tree {
            return Ok(());
        }
        self.write_file("AUTO_MERGE", format!("{}\n", tree.to_hex()).as_bytes())?;
        if dirty.is_empty() {
            return Ok(());
        }
        for pos in dirty {
            if let Some(e) = index.entry_mut(pos) {
                e.stat = Default::default();
            }
        }
        let path = repo.setup.cwd.join(repo.index_path());
        let skip_hash = repo.config.bool("index.skiphash").ok().flatten().unwrap_or(false);
        index
            .write_file(&path, &repo.index_lock_options(), &rgit_index::WriteOptions { skip_hash })
            .map_err(|e| fatal(e.to_string()))
    }

    /// The message of the merge commit before editing.
    fn message(&self, theirs: &[Head], head: ObjectId) -> Result<Vec<u8>, Die> {
        let mut m = Vec::new();
        if !self.args.messages.is_empty() {
            for (n, p) in self.args.messages.iter().enumerate() {
                if n > 0 {
                    m.push(b'\n');
                }
                m.extend_from_slice(p.as_bytes());
                if !m.ends_with(b"\n") {
                    m.push(b'\n');
                }
            }
            m.extend_from_slice(&message::log_part(&self.repo, theirs, Some(&head), self.log));
            return Ok(m);
        }
        if let Some(f) = &self.args.file {
            if f == "-" {
                use std::io::Read as _;
                std::io::stdin()
                    .read_to_end(&mut m)
                    .map_err(|e| fatal(e.to_string()))?;
            } else {
                let path = Path::new(f);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.repo.setup.cwd.join(path)
                };
                m = std::fs::read(&path).map_err(|e| {
                    fatal(crate::messages::commit::could_not_read_log(
                        f,
                        &crate::global::strerror(&e),
                    ))
                })?;
            }
            m.extend_from_slice(&message::log_part(&self.repo, theirs, Some(&head), self.log));
            return Ok(m);
        }
        let into = match &self.args.into_name {
            Some(n) => n.clone(),
            None => match self.branch.as_deref() {
                Some(b) => b.strip_prefix("refs/heads/").unwrap_or(b).to_owned(),
                None => "HEAD".to_owned(),
            },
        };
        Ok(message::default_message(
            &self.repo,
            theirs,
            &into,
            Some(&head),
            self.log,
        ))
    }

    fn committer(&self) -> Result<Signature, Die> {
        self.ident(Role::Committer)
    }

    fn ident(&self, role: Role) -> Result<Signature, Die> {
        let env = SystemEnv::default();
        let icfg = IdentConfig::from_config(&self.repo.config).map_err(|p| {
            p.print();
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let resolver = IdentResolver::new(&icfg, &env);
        let ident = resolver.ident(role, Strictness::Strict).map_err(|e| {
            eprint!("{}", e.stderr_text());
            Die::Exit(rgit_options::EXIT_FATAL)
        })?;
        let raw = ident.to_bstring();
        rgit_object::SignatureRef::parse(&raw)
            .and_then(|s| s.to_owned())
            .ok_or_else(|| fatal(lossy(&raw)))
    }

    fn signoff(&self, message: &[u8], comment: &str) -> Result<Vec<u8>, Die> {
        let committer = self.committer()?;
        let mut line = b"Signed-off-by: ".to_vec();
        line.extend_from_slice(&committer.name);
        line.extend_from_slice(b" <");
        line.extend_from_slice(&committer.email);
        line.push(b'>');
        let (conf, warnings) =
            rgit_trailer::Config::from_config(&self.repo.config).map_err(|entry| {
                let name = String::from_utf8_lossy(&entry.name).into_owned();
                crate::comment::bad_entry(&entry, &crate::messages::comment::missing_value(&name))
            })?;
        for w in warnings {
            eprintln!("warning: {w}");
        }
        Ok(rgit_trailer::append_signoff(
            message,
            &line,
            &conf,
            comment.as_bytes(),
        ))
    }

    fn write_file(&self, name: &str, data: &[u8]) -> Result<(), Die> {
        std::fs::write(self.git_dir.join(name), data).map_err(|e| fatal(e.to_string()))
    }

    /// `MERGE_HEAD`, `MERGE_MSG` and `MERGE_MODE`.
    fn write_merge_state(&self, theirs: &[ObjectId], message: &[u8]) -> Result<(), Die> {
        let ids: String = theirs.iter().map(|t| format!("{}\n", t.to_hex())).collect();
        self.write_file("MERGE_HEAD", ids.as_bytes())?;
        self.write_file("MERGE_MSG", message)?;
        let mode: &[u8] = if self.args.ff == FF_NO { b"no-ff" } else { b"" };
        self.write_file("MERGE_MODE", mode)
    }

    fn write_squash_msg(&self, theirs: &[ObjectId], head: ObjectId) -> Result<(), Die> {
        let m = message::squash_message(&self.repo, theirs, Some(&head));
        self.write_file("SQUASH_MSG", &m)
    }

    /// Moves the index and work tree from `old` to `new` (trees) and, for a
    /// merge result, records the conflicted paths' stages. Refusals are
    /// printed and end in `Die::Exit(1)`.
    fn checkout(&self, old: ObjectId, new: ObjectId, result: Option<&MergeResult>) -> Result<(), Die> {
        let repo = &self.repo;
        let work_tree = repo.work_tree()?;
        let index_path = repo.setup.cwd.join(repo.index_path());
        let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
        let (mut index, mtime) = repo.read_index()?;
        let checker = repo.checker(mtime)?;
        crate::switch_branch::refresh_stat(&mut index, &checker);
        let mut excludes = crate::commands::add::build_excludes(repo, &work_tree);
        let wt = Worktree {
            root: &work_tree,
            checker: &checker,
        };
        let mut opts = rgit_unpack::Options {
            update: true,
            show_all_errors: true,
            paths: path_options(repo),
            excludes: self.args.overwrite_ignore.then_some(&mut excludes),
            ..rgit_unpack::Options::default()
        };
        let merge = Merge::TwoWay { old, new };
        let plan = match rgit_unpack::plan(repo.odb(), &index, &merge, Some(&wt), &mut opts) {
            Ok(p) => p,
            Err(rgit_unpack::Error::Rejected(r)) => {
                for m in r.porcelain_messages(Porcelain::Merge) {
                    eprintln!("error: {m}");
                }
                eprintln!("{}", rgit_unpack::messages::ABORTING);
                return Err(Die::Exit(1));
            }
            Err(e) => return Err(unpack_failed(e)),
        };
        let applied = plan.apply(repo.odb(), &wt, &Identity);
        let mut index = applied.index;
        if let Some(result) = result {
            add_conflicts(&mut index, result)?;
        }
        if let Some(reference) = mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        crate::switch_branch::write_index(repo, &index, lock)?;
        if !applied.errors.is_empty() {
            for e in &applied.errors {
                eprintln!("error: {e}");
            }
            return Err(Die::Exit(128));
        }
        Ok(())
    }

    /// `diff --stat --summary <old> <new>`.
    fn print_stat(&self, old: ObjectId, new: ObjectId) -> Result<(), Die> {
        let pathspec = rgit_pathspec::Pathspec::default();
        let mut w = TreeDiff {
            repo: &self.repo,
            pathspec: &pathspec,
            recursive: true,
            show_trees: false,
            pairs: Vec::new(),
        };
        w.walk(Some(&old), Some(&new), b"")?;
        let opts = DiffOpts {
            stat: true,
            summary: true,
            ..Default::default()
        };
        let formats = Formats {
            stat: true,
            summary: true,
            ..Default::default()
        };
        let mut out = Output::new(Some(&self.repo), self.repo.hash, &opts, formats, true)?;
        out.compact_summary = self.compact;
        let (bytes, _) = out.render(w.pairs)?;
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&bytes);
        let _ = o.flush();
        Ok(())
    }

    /// Hook options with git's environment for merge hooks.
    fn hook_opts<'h>(&self, args: &[&str], editor: Option<bool>, index: bool) -> RunOptions<'h> {
        let mut o = RunOptions::with_args(args.iter().copied());
        o.env.push(("GIT_REFLOG_ACTION".into(), self.action.clone().into()));
        if editor == Some(false) {
            o.env.push(("GIT_EDITOR".into(), ":".into()));
        }
        if index {
            o.env
                .push(("GIT_INDEX_FILE".into(), self.repo.index_path().into()));
        }
        o
    }

    fn post_merge(&self) -> Result<(), Die> {
        let _ = std::io::stdout().flush();
        let hooks = crate::hooks::for_repo(self.ctx, &self.repo.setup, &self.repo.config)?;
        let flag = if self.args.squash { "1" } else { "0" };
        crate::hooks::run(&hooks, "post-merge", &self.hook_opts(&[flag], None, false))?;
        Ok(())
    }

    /// Whether the merge commit is edited: `-e`/`--no-edit`, else when
    /// no message was given and standard input and output are terminals
    /// (unless `GIT_MERGE_AUTOEDIT=no`).
    fn use_editor(&self) -> bool {
        match self.args.edit {
            -1 => {
                use std::io::IsTerminal as _;
                self.args.messages.is_empty()
                    && self.args.file.is_none()
                    && std::io::stdin().is_terminal()
                    && std::io::stdout().is_terminal()
                    && std::env::var("GIT_MERGE_AUTOEDIT").as_deref() != Ok("no")
            }
            v => v == 1,
        }
    }

    /// Creates the merge commit of a clean merge, with the `mergetag`
    /// headers of merged signed tags.
    #[allow(clippy::too_many_arguments)]
    fn commit(
        &mut self,
        head: ObjectId,
        head_tree: ObjectId,
        parents: &[ObjectId],
        tree: ObjectId,
        message: Vec<u8>,
        comment: &str,
        mergetags: Vec<rgit_object::Header>,
    ) -> CmdResult {
        let use_editor = self.use_editor();
        let hooks = crate::hooks::for_repo(self.ctx, &self.repo.setup, &self.repo.config)?;
        let _ = std::io::stdout().flush();
        if self.args.verify {
            let outcome = crate::hooks::run(
                &hooks,
                "pre-merge-commit",
                &self.hook_opts(&[], Some(use_editor), true),
            )?;
            if !outcome.ok() {
                self.write_merge_state(&parents[1..], &message)?;
                eprintln!("{}", msg::NOT_COMMITTING);
                return Ok(1);
            }
        }
        let mut file = message.clone();
        if use_editor {
            if self.cleanup == Cleanup::Scissors {
                file.extend_from_slice(&scissors_block(comment));
            }
            file.extend_from_slice(msg::edit_template(comment).as_bytes());
        }
        self.write_merge_state(&parents[1..], &file)?;
        let merge_msg = self.repo.setup.git_dir.join("MERGE_MSG");
        let merge_msg_arg = merge_msg.to_string_lossy().into_owned();
        let outcome = crate::hooks::run(
            &hooks,
            "prepare-commit-msg",
            &self.hook_opts(&[&merge_msg_arg, "merge"], Some(use_editor), true),
        )?;
        if !outcome.ok() {
            eprintln!("{}", msg::NOT_COMMITTING);
            return Ok(1);
        }
        if use_editor {
            let env = SystemEnv::default();
            let editor = rgit_ident::editor::editor(&self.repo.config, &env).map_err(|p| {
                p.print();
                Die::Exit(rgit_options::EXIT_FATAL)
            })?;
            let advisor = self.ctx.advisor(&self.repo.config)?;
            let opts = rgit_ident::editor::LaunchOptions {
                hint: advisor
                    .enabled(Advice::WaitingForEditor)
                    .map_err(Die::Fatal)?,
                env: self.ctx.child_env(),
                cwd: None,
            };
            let path = self.git_dir.join("MERGE_MSG");
            if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &path, &opts) {
                e.print();
                eprintln!("{}", msg::NOT_COMMITTING);
                return Ok(1);
            }
        }
        if self.args.verify {
            let outcome = crate::hooks::run(
                &hooks,
                "commit-msg",
                &self.hook_opts(&[&merge_msg_arg], Some(use_editor), true),
            )?;
            if !outcome.ok() {
                eprintln!("{}", msg::NOT_COMMITTING);
                return Ok(1);
            }
        }
        let edited = std::fs::read(self.git_dir.join("MERGE_MSG")).map_err(|e| fatal(e.to_string()))?;
        let cleanup = self.cleanup;
        let mode = cleanup.resolve(use_editor);
        let mut cleaned = edited;
        if use_editor && cleanup == Cleanup::Scissors {
            cmsg::cut_at_scissors(&mut cleaned, comment);
        }
        let cleaned = cmsg::cleanup(&cleaned, mode, comment);
        let strip = (mode == Cleanup::Strip).then_some(comment.as_bytes());
        if cmsg::is_empty(&cleaned, strip) {
            eprintln!("error: {}", msg::EMPTY_MESSAGE);
            eprintln!("{}", msg::NOT_COMMITTING);
            return Ok(1);
        }

        let author = self.ident(Role::Author)?;
        let committer = self.committer()?;
        let encoding = self
            .repo
            .config
            .get("i18n.commitencoding")
            .and_then(|e| e.value.as_ref())
            .map(|v| v.to_vec())
            .filter(|e| !e.eq_ignore_ascii_case(b"utf-8") && !e.eq_ignore_ascii_case(b"utf8"));
        let new = rgit_object::Commit::new(
            tree,
            parents,
            &author,
            &committer,
            encoding.as_deref(),
            mergetags,
            cleaned,
        );
        let oid = self
            .repo
            .odb()
            .write(ObjectKind::Commit, &new.serialize())
            .map_err(|e| fatal(e.to_string()))?;
        self.update_head(oid, Some(head), &msg::made_by(&self.strategy))?;
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(self.git_dir.join(f));
        }
        if !self.args.quiet {
            println!("{}", msg::made_by(&self.strategy));
            if self.show_stat {
                self.print_stat(head_tree, tree)?;
            }
        }
        self.post_merge()?;
        Ok(0)
    }
}

/// Rejects strategy names git does not know.
fn check_strategies(names: &[String]) -> Result<(), Die> {
    for s in names {
        if !matches!(
            s.as_str(),
            "ort" | "recursive" | "ours" | "octopus" | "resolve" | "subtree"
        ) {
            eprintln!("{}", msg::unknown_strategy(s));
            return Err(Die::Exit(1));
        }
    }
    Ok(())
}

/// The scissors line and its explanation, as comments.
fn scissors_block(comment: &str) -> Vec<u8> {
    let mut out = cmsg::scissors_line(comment).into_bytes();
    out.push(b'\n');
    for l in crate::messages::commit::SCISSORS_HELP.lines() {
        out.extend_from_slice(comment.as_bytes());
        out.push(b' ');
        out.extend_from_slice(l.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Replaces the result's conflicted paths in `index` by their stages.
fn add_conflicts(index: &mut Index, result: &MergeResult) -> Result<(), Die> {
    let add = AddOptions {
        ok_to_replace: false,
        skip_df_check: true,
        skip_path_check: true,
    };
    for c in &result.conflicts {
        index.remove_path(&c.path);
        index.invalidate_path(&c.path);
        for (i, v) in c.stages.iter().enumerate() {
            if let Some(v) = v {
                let stage = Stage::new(i as u8 + 1).expect("stages 1 to 3");
                index
                    .add(
                        Entry::new(c.path.clone(), stage, rgit_index::Mode(v.mode), v.oid),
                        add,
                    )
                    .map_err(|e| fatal(e.to_string()))?;
            }
        }
    }
    Ok(())
}
