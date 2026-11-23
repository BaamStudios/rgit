// SPDX-License-Identifier: MIT
//! `git merge` (Documentation/git-merge.adoc, merge-options.adoc,
//! config/merge.adoc, git-fmt-merge-msg.adoc), part of #57.
//!
//! - Fast-forward (`--ff`, `--no-ff`, `--ff-only`, `merge.ff`), "Already
//!   up to date.", merges into an unborn branch;
//! - true merges with the `ort` engine of `rgit-merge` (also as `-s
//!   recursive`, and as `-s subtree` with the trees shifted), `-s ours`,
//!   `-s resolve` and the trivial in-index merge (`merge/resolve.rs`),
//!   custom strategies (`git-merge-<name>` programs), trying several
//!   strategies and keeping the best result, `-X ours|theirs|
//!   diff-algorithm=...|ignore-*|subtree[=<path>]`,
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
//!   (`--no-verify`) and `post-merge`;
//! - `--autostash` (`merge.autoStash`): local changes are stashed before
//!   the index and work tree move (`MERGE_AUTOSTASH`) and applied when
//!   the merge is done; a merge that stops before its commit leaves the
//!   stash to the `commit` that concludes it, `--abort` applies it,
//!   `--quit` keeps it in the stash list.
//!
//! The behaviour was checked against the `git` binary with differential
//! tests (`tests/merge.rs`).
//!
//! `-S`/`commit.gpgSign` sign the merge commit and `--verify-signatures`
//! checks the merged commits ([`crate::signing`]).
//!
//! Not yet: `-X renormalize` and merge attributes (#153), rerere (#156).
//! The octopus messages are rgit's own approximation.

#[path = "merge/attrs.rs"]
pub(crate) mod attrs;
#[path = "merge/message.rs"]
pub(crate) mod message;
#[path = "merge/resolve.rs"]
pub(crate) mod resolve;

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
use rgit_unpack::{Merge, Porcelain, Worktree};
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
    /// `-S[<key>]` (`Some(Some(_))`) or `--no-gpg-sign` (`Some(None)`).
    gpg_sign: Option<Option<crate::signing::Sign>>,
    /// The resolved signing request (with `commit.gpgSign`).
    sign: Option<crate::signing::Sign>,
    verify_signatures: i32,
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
    let gpg_sign: RefCell<Option<Option<crate::signing::Sign>>> = RefCell::new(None);
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
            Opt::callback('S', "gpg-sign", Some("key-id"), msg::HELP_GPG_SIGN, |v, unset| {
                *gpg_sign.borrow_mut() = Some((!unset).then(|| crate::signing::Sign::from_arg(v)));
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
        gpg_sign: gpg_sign.into_inner(),
        sign: None,
        verify_signatures: verify_signatures.get(),
        autostash: autostash.get(),
        overwrite_ignore: overwrite_ignore.get() != 0,
        signoff: signoff.get(),
        verify: verify.get() != 0,
        heads,
    })
}

/// `@{-<n>}` at the start of `name` replaced by the branch it stands for,
/// as git names the merged commit (`@{-1}~1` is merged as `other~1`).
fn expand_previous(repo: &Repo, name: &str) -> String {
    let Some(rest) = name.strip_prefix("@{-") else {
        return name.to_owned();
    };
    let Some((n, tail)) = rest.split_once('}') else {
        return name.to_owned();
    };
    match n
        .parse::<usize>()
        .ok()
        .filter(|&n| n > 0)
        .and_then(|n| crate::switch_branch::previous_checkout(repo, n))
    {
        Some(branch) => format!("{branch}{tail}"),
        None => name.to_owned(),
    }
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
    /// The strategy at work (the one named in messages).
    strategy: String,
    /// The strategies to try, in order.
    strategies: Vec<String>,
    /// The reflog message and report of the commit when it is not "Merge
    /// made by the '<strategy>' strategy." (the trivial in-index merge).
    made_by: Option<String>,
    show_stat: bool,
    /// `--compact-summary`, `merge.stat=compact`.
    compact: bool,
    /// `--cleanup`, `commit.cleanup`.
    cleanup: Cleanup,
    log: usize,
    /// `--autostash`, `merge.autoStash`.
    autostash: bool,
    /// The stash made for `--autostash`.
    stash: Option<String>,
}

/// The stash of a merge with `--autostash` that is over: `MERGE_AUTOSTASH`
/// is applied (kept in the stash list on conflicts) and removed.
pub(crate) fn apply_merge_autostash(ctx: &Context, git_dir: &Path) -> Result<(), Die> {
    let path = git_dir.join("MERGE_AUTOSTASH");
    let Ok(data) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let _ = std::fs::remove_file(&path);
    if data.trim().is_empty() {
        return Ok(());
    }
    crate::rebase::apply_autostash(ctx, data.trim())
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
        let code = super::reset::run(ctx, &["reset".to_owned(), "--merge".to_owned()])?;
        if code == 0 {
            apply_merge_autostash(ctx, &git_dir)?;
        }
        return Ok(code);
    }
    if args.quit {
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "MERGE_RR", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(git_dir.join(f));
        }
        let stash = git_dir.join("MERGE_AUTOSTASH");
        if let Ok(data) = std::fs::read_to_string(&stash) {
            let _ = std::fs::remove_file(&stash);
            if !data.trim().is_empty() {
                crate::rebase::store_autostash(ctx, data.trim())?;
            }
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
    args.sign = crate::signing::resolve(&repo.config, args.gpg_sign.clone());
    if args.verify_signatures == -1 {
        args.verify_signatures = i32::from(
            repo.config
                .bool("merge.verifysignatures")
                .ok()
                .flatten()
                .unwrap_or(false),
        );
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
    check_strategies(ctx, &args.strategies)?;
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
        strategies: Vec::new(),
        made_by: None,
        show_stat,
        compact,
        cleanup,
        log,
        autostash,
        stash: None,
    };
    s.merge()
}

impl Session<'_> {
    /// `--verify-signatures`: every merged commit must have a good
    /// signature of at least `marginal` trust (`gpg.minTrustLevel`).
    fn verify_signatures(&self, heads: &[Head]) -> Result<(), Die> {
        let settings = crate::signing::Settings::from_config(&self.repo.config)?;
        let mut seen = Vec::new();
        for h in heads {
            if seen.contains(&h.commit) {
                continue;
            }
            seen.push(h.commit);
            let raw = self
                .repo
                .odb()
                .read(&h.commit)
                .ok()
                .flatten()
                .map(|o| o.data)
                .unwrap_or_default();
            let abbrev = self.repo.abbrev(&h.commit, self.repo.default_abbrev());
            crate::signing::verify::verify_merge_signature(
                &settings,
                self.repo.hash,
                &raw,
                &abbrev,
                self.args.quiet,
            )?;
        }
        Ok(())
    }

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
            let name = expand_previous(&self.repo, &name);
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
                tag_message: match self.repo.read_object(&id) {
                    Some(rgit_object::Object::Tag(t)) => Some(t.message().to_vec()),
                    _ => None,
                },
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
            if self.args.verify_signatures == 1 {
                self.verify_signatures(&heads)?;
            }
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
        if self.args.verify_signatures == 1 {
            self.verify_signatures(&kept)?;
        }
        self.strategies = self.choose_strategies(kept.len() > 1)?;
        self.strategy = self.strategies[0].clone();
        if kept.len() > 1 {
            if self.args.ff == FF_ONLY {
                let advisor = self.ctx.advisor(&self.repo.config)?;
                advisor
                    .advise(Advice::Diverging, msg::DIVERGING_HINT)
                    .map_err(Die::Fatal)?;
                return Err(fatal(msg::NOT_POSSIBLE_FF));
            }
            if !is_builtin_strategy(&self.strategy) {
                // A custom strategy gets all heads at once.
                let commits = self.commits();
                let mut bases: Vec<ObjectId> = Vec::new();
                for h in &kept {
                    for b in rgit_merge::merge_bases(&commits, &head, &h.commit)
                        .map_err(|e| fatal(e.to_string()))?
                    {
                        if !bases.contains(&b) {
                            bases.push(b);
                        }
                    }
                }
                drop(commits);
                let bases = self.independent(&bases)?;
                return self.true_merge(head, kept, &bases);
            }
            return self.octopus(head, kept);
        }
        let theirs = kept.pop().expect("one head");
        let commits = self.commits();
        let bases = rgit_merge::merge_bases(&commits, &head, &theirs.commit)
            .map_err(|e| fatal(e.to_string()))?;
        drop(commits);
        let ff_possible = bases.len() == 1 && bases[0] == head;
        // git-merge(1): an annotated tag that is not in its natural place
        // in refs/tags/ (from FETCH_HEAD) gets a merge commit, also with
        // --ff (checked against git); only --ff-only fast-forwards.
        let no_ff_tag = self.args.ff == FF_ALLOW && self.untracked_tag(&theirs);
        if ff_possible && self.args.ff != FF_NO && !no_ff_tag && !self.strategies.iter().any(|s| s == "ours") {
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
        self.true_merge(head, vec![theirs], &bases)
    }

    /// The strategies for one head or several (`-s`, else `pull.twohead`
    /// or `pull.octopus`, else `ort` or `octopus`). One head tries them all
    /// in order; several heads use the first one that handles several
    /// heads.
    fn choose_strategies(&mut self, several: bool) -> Result<Vec<String>, Die> {
        let mut names = self.args.strategies.clone();
        if names.is_empty() {
            let key = if several { "pull.octopus" } else { "pull.twohead" };
            if let Some(v) = self.repo.config.get(key).and_then(|e| e.value.as_ref()) {
                names = lossy(v).split_whitespace().map(str::to_owned).collect();
                check_strategies(self.ctx, &names)?;
            }
        }
        if names.is_empty() {
            return Ok(vec![if several { "octopus" } else { "ort" }.to_owned()]);
        }
        if !several {
            return Ok(names);
        }
        if let Some(n) = names
            .iter()
            .find(|n| matches!(n.as_str(), "octopus" | "ours") || !is_builtin_strategy(n))
        {
            return Ok(vec![n.clone()]);
        }
        if names.iter().all(|n| matches!(n.as_str(), "ort" | "recursive")) {
            // These handle two heads only.
            self.create_autostash()?;
            eprintln!("{}", msg::strategy_failed(&names[0]));
            self.restore_autostash()?;
            return Err(Die::Exit(2));
        }
        // resolve, subtree: refused when a real merge is needed.
        Ok(vec![names[0].clone()])
    }

    /// Commit access for the merge engine.
    /// Whether `head` is an annotated tag from `FETCH_HEAD` (not stored
    /// under `refs/tags/`).
    fn untracked_tag(&mut self, head: &Head) -> bool {
        if head.kind != message::Kind::Fetch {
            return false;
        }
        ObjectId::from_hex(self.repo.hash, head.arg.as_bytes())
            .ok()
            .and_then(|o| self.repo.read_object(&o))
            .is_some_and(|o| matches!(o, rgit_object::Object::Tag(_)))
    }

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
        let conv = crate::convert::Converter::checkout(repo, plan.index())?;
        let applied = plan.apply(repo.odb(), &wt, &conv);
        let mut index = applied.index;
        repo.converter()?.use_index(&index);
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
        self.create_autostash()?;
        let old_tree = self.tree_of(&head)?;
        let new_tree = self.tree_of(&theirs.commit)?;
        crate::convert::set_checkout_meta(None, Some(&theirs.commit));
        let moved = self.checkout(old_tree, new_tree, None);
        crate::convert::set_checkout_meta(None, None);
        if let Err(e) = moved {
            self.restore_autostash()?;
            return Err(e);
        }
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

    fn engine_options(&mut self, theirs: &Head) -> Result<rgit_merge::Options, Die> {
        let mut opts = rgit_merge::Options {
            branch1: b"HEAD".to_vec(),
            branch2: theirs.arg.as_bytes().to_vec(),
            style: super::merge_file::config_style(&self.repo.config)?,
            allow_unrelated_histories: self.args.allow_unrelated,
            renormalize: renormalize_config(&self.repo.config),
            ..rgit_merge::Options::default()
        };
        super::merge_tree::apply_rename_config(&self.repo.config, &mut opts);
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
        let their_tree = self.tree_of(&theirs.commit)?;
        let attr_tree = crate::attributes::tree_source(self.ctx, &mut self.repo, None)?;
        opts.path_merge = Some(attrs::RepoPathMerge::open(
            self.ctx,
            &self.repo,
            opts.renormalize,
            Some(&their_tree),
            attr_tree,
        )?);
        Ok(opts)
    }

    /// Prints the paths `staged` that differ between the index and `HEAD`
    /// the way `merge` refuses them.
    fn print_staged(staged: &[Vec<u8>]) {
        let mut text = format!("error: {}\n", msg::LOCAL_CHANGES);
        for p in staged {
            text.push_str("  ");
            text.push_str(&lossy(p));
            text.push('\n');
        }
        eprint!("{text}");
    }

    /// A merge that is not a fast-forward of `theirs` (one head, or
    /// several for a custom strategy) into `head`: the trivial in-index
    /// merge where the strategies allow it, then each strategy in turn
    /// until one merges cleanly; with several strategies the one with the
    /// fewest conflicts prepares the result for resolving by hand.
    fn true_merge(&mut self, head: ObjectId, theirs: Vec<Head>, bases: &[ObjectId]) -> CmdResult {
        self.create_autostash()?;
        let head_tree = self.tree_of(&head)?;
        let several = self.strategies.len() > 1;
        let strategies = self.strategies.clone();
        if !several {
            let staged = self.staged_changes(head_tree)?;
            if !staged.is_empty() {
                Self::print_staged(&staged);
                if is_ort(&self.strategy) {
                    eprintln!("{}", msg::strategy_failed(&self.strategy));
                    self.keep_local_state(head_tree)?;
                }
                return Err(Die::Exit(2));
            }
        }
        let mut parents = vec![head];
        parents.extend(theirs.iter().map(|h| h.commit));

        // The trivial in-index merge (resolve and octopus allow it).
        if theirs.len() == 1
            && bases.len() == 1
            && !self.args.squash
            && self.args.commit != 0
            && strategies.iter().all(|s| s == "resolve" || s == "octopus")
        {
            println!("{}", msg::TRYING_TRIVIAL);
            let base_tree = self.tree_of(&bases[0])?;
            let their_tree = self.tree_of(&theirs[0].commit)?;
            let mut changed = Vec::new();
            let tree = resolve::trivial(&self.repo, &base_tree, &head_tree, &their_tree, &mut changed)?;
            if let Some(p) = self.first_not_uptodate(&changed)? {
                let _ = std::io::stdout().flush();
                eprintln!("error: {}", rgit_unpack::messages::not_uptodate_file(&lossy(&p)));
                println!("{}", msg::NOPE);
            } else if let Some(tree) = tree {
                println!("{}", msg::WONDERFUL);
                self.made_by = Some(msg::IN_INDEX_MERGE.to_owned());
                let result = MergeResult {
                    tree,
                    clean: true,
                    conflicts: Vec::new(),
                    messages: Vec::new(),
                    rename_limit_needed: None,
                };
                return self.finish(head, parents, &theirs, result, None, false);
            } else {
                let _ = std::io::stdout().flush();
                eprintln!("error: {}", msg::FILE_LEVEL_MERGING);
                println!("{}", msg::NOPE);
            }
        }

        // With several strategies git saves local changes first and puts
        // them back before each further attempt.
        let saved = several && self.has_local_changes(head_tree)?;
        // The last attempt that did not fail, and the best one so far
        // (index, conflict count).
        let mut last: Option<(usize, Attempt)> = None;
        let mut best: Option<(usize, usize)> = None;
        for (i, name) in strategies.iter().enumerate() {
            if several {
                if i > 0 {
                    println!("{}", msg::REWINDING);
                    let applied = matches!(last, Some((_, Attempt::Applied(_))));
                    self.restore_state(head, applied, saved)?;
                }
                println!("{}", msg::trying_strategy(name));
            }
            self.strategy = name.clone();
            let attempt = self.attempt(name, head, head_tree, &theirs, bases, several)?;
            let count = match &attempt {
                Attempt::Failed => {
                    if !several {
                        let _ = std::io::stdout().flush();
                        eprintln!("{}", msg::strategy_failed(name));
                        self.restore_autostash()?;
                        return Err(Die::Exit(2));
                    }
                    last = None;
                    continue;
                }
                Attempt::Memory(r, lines) => {
                    if !several {
                        let (r, lines) = (r.clone(), lines.clone());
                        return self.finish(head, parents, &theirs, r, Some(&lines), false);
                    }
                    resolve::print(lines);
                    if r.clean {
                        let r = r.clone();
                        return self.finish(head, parents, &theirs, r, None, false);
                    }
                    r.conflicts
                        .iter()
                        .map(|c| c.stages.iter().flatten().count())
                        .sum()
                }
                Attempt::Applied(clean) => {
                    if !several || *clean {
                        return self.finish_applied(head, parents, &theirs, *clean);
                    }
                    self.unmerged_count()?
                }
            };
            // Of equally good results, the later one wins (as with git).
            if best.is_none_or(|(_, c)| count <= c) {
                best = Some((i, count));
            }
            last = Some((i, attempt));
        }
        let Some((best, _)) = best else {
            let applied = matches!(last, Some((_, Attempt::Applied(_))));
            self.restore_state(head, applied, saved)?;
            let _ = std::io::stdout().flush();
            eprintln!("{}", msg::NO_STRATEGY_HANDLED);
            self.restore_autostash()?;
            return Err(Die::Exit(2));
        };
        let name = strategies[best].clone();
        self.strategy = name.clone();
        let attempt = match last {
            Some((i, a)) if i == best => a,
            last => {
                println!("{}", msg::REWINDING);
                let applied = matches!(last, Some((_, Attempt::Applied(_))));
                self.restore_state(head, applied, saved)?;
                println!("{}", msg::using_strategy(&name));
                let a = self.attempt(&name, head, head_tree, &theirs, bases, true)?;
                if let Attempt::Memory(_, lines) = &a {
                    resolve::print(lines);
                }
                a
            }
        };
        match attempt {
            Attempt::Memory(r, _) => self.finish(head, parents, &theirs, r, None, false),
            Attempt::Applied(clean) => self.finish_applied(head, parents, &theirs, clean),
            Attempt::Failed => {
                let _ = std::io::stdout().flush();
                eprintln!("{}", msg::NO_STRATEGY_HANDLED);
                Err(Die::Exit(2))
            }
        }
    }

    /// One strategy: an in-memory result with its output, the index and
    /// work tree already updated by an external strategy, or a failure
    /// (its output printed).
    fn attempt(
        &mut self,
        name: &str,
        head: ObjectId,
        head_tree: ObjectId,
        theirs: &[Head],
        bases: &[ObjectId],
        several: bool,
    ) -> Result<Attempt, Die> {
        let staged = if several {
            self.staged_changes(head_tree)?
        } else {
            Vec::new()
        };
        match name {
            "ours" => Ok(Attempt::Memory(
                MergeResult {
                    tree: head_tree,
                    clean: true,
                    conflicts: Vec::new(),
                    messages: Vec::new(),
                    rename_limit_needed: None,
                },
                Vec::new(),
            )),
            "ort" | "recursive" | "subtree" => {
                if !staged.is_empty() {
                    Self::print_staged(&staged);
                    return Ok(Attempt::Failed);
                }
                let mut opts = self.engine_options(&theirs[0])?;
                if name == "subtree" && opts.subtree == rgit_merge::Subtree::None {
                    opts.subtree = rgit_merge::Subtree::Guess;
                }
                let commits = self.commits();
                let r = match rgit_merge::merge_commits_with_bases(
                    self.repo.odb(),
                    &commits,
                    &head,
                    &theirs[0].commit,
                    bases,
                    &opts,
                ) {
                    Ok(r) => r,
                    Err(rgit_merge::Error::Driver(m)) => {
                        let _ = std::io::stdout().flush();
                        eprintln!("error: {m}");
                        return Ok(Attempt::Failed);
                    }
                    Err(e @ rgit_merge::Error::Collect(_)) => {
                        let _ = std::io::stdout().flush();
                        for line in e.collect_errors().into_iter().flatten() {
                            eprintln!("error: {line}");
                        }
                        return Ok(Attempt::Failed);
                    }
                    Err(e) => return Err(fatal(e.to_string())),
                };
                drop(commits);
                let lines = engine_lines(&r);
                Ok(Attempt::Memory(r, lines))
            }
            "resolve" => {
                if let Some(x) = self.args.xopts.first() {
                    // git's resolve hands strategy options to `read-tree`,
                    // which rejects them with its usage.
                    let _ = std::io::stdout().flush();
                    let mut ctx = self.ctx.clone();
                    if let Err(e) =
                        super::read_tree::run(&mut ctx, &["read-tree".to_owned(), format!("--{x}")])
                    {
                        e.emit();
                    }
                    return Ok(Attempt::Failed);
                }
                if !staged.is_empty() {
                    let paths: Vec<String> = staged.iter().map(|p| lossy(p)).collect();
                    println!("{}", msg::resolve_local_changes(&paths));
                    return Ok(Attempt::Failed);
                }
                let base_trees = bases
                    .iter()
                    .map(|b| self.tree_of(b))
                    .collect::<Result<Vec<_>, _>>()?;
                let their_tree = self.tree_of(&theirs[0].commit)?;
                let style = super::merge_file::config_style(&self.repo.config)?;
                let (r, lines, changed) =
                    resolve::resolve(&self.repo, &base_trees, &head_tree, &their_tree, style)?;
                if let Some(p) = self.first_not_uptodate(&changed)? {
                    let _ = std::io::stdout().flush();
                    eprintln!("error: {}", rgit_unpack::messages::not_uptodate_file(&lossy(&p)));
                    return Ok(Attempt::Failed);
                }
                Ok(Attempt::Memory(r, lines))
            }
            "octopus" => Ok(Attempt::Failed),
            _ => {
                let code = self.run_custom(name, theirs, bases)?;
                match code {
                    0 => Ok(Attempt::Applied(true)),
                    1 => Ok(Attempt::Applied(false)),
                    _ => {
                        if !several {
                            self.rewind()?;
                        }
                        Ok(Attempt::Failed)
                    }
                }
            }
        }
    }

    /// Runs the custom strategy program for `name` with the options from
    /// `-X` (as `--<option>`), the merge bases, `--`, `HEAD` and the heads,
    /// with `GITHEAD_<id>` naming each head; returns its exit code.
    fn run_custom(&mut self, name: &str, theirs: &[Head], bases: &[ObjectId]) -> Result<i32, Die> {
        let mut cmd = std::process::Command::new(format!("{}{name}", msg::STRATEGY_PROGRAM_PREFIX));
        for x in &self.args.xopts {
            cmd.arg(format!("--{x}"));
        }
        for b in bases {
            cmd.arg(b.to_hex());
        }
        cmd.arg("--").arg("HEAD");
        for h in theirs {
            cmd.arg(h.commit.to_hex());
        }
        for (k, v) in self.ctx.child_env() {
            cmd.env(k, v);
        }
        cmd.env("GIT_REFLOG_ACTION", &self.action);
        for h in theirs {
            cmd.env(format!("GITHEAD_{}", h.commit.to_hex()), &h.arg);
        }
        let _ = std::io::stdout().flush();
        let code = match cmd.status() {
            Ok(status) => status.code().unwrap_or(2),
            Err(_) => 2,
        };
        self.repo = Repo::open(self.ctx, Mode::Required)?;
        Ok(code)
    }

    /// Takes the index and work tree back to `HEAD` after an external
    /// strategy changed them (`read-tree --reset -u HEAD`).
    fn rewind(&mut self) -> Result<(), Die> {
        self.repo = Repo::open(self.ctx, Mode::Required)?;
        let head = self
            .repo
            .resolve("HEAD")
            .and_then(|h| self.repo.peel_to(h, ObjectKind::Commit))
            .ok_or_else(|| fatal(msg::READ_TREE_FAILED))?;
        let tree = self.tree_of(&head)?;
        let repo = &self.repo;
        let work_tree = repo.work_tree()?;
        let index_path = repo.setup.cwd.join(repo.index_path());
        let lock = LockFile::acquire(&index_path, &LockOptions::default()).map_err(lock_failed)?;
        let (index, mtime) = repo.read_index()?;
        let checker = repo.checker(mtime)?;
        let wt = Worktree {
            root: &work_tree,
            checker: &checker,
        };
        let mut opts = rgit_unpack::Options {
            update: true,
            reset: true,
            paths: path_options(repo),
            ..rgit_unpack::Options::default()
        };
        let plan = rgit_unpack::plan(repo.odb(), &index, &Merge::OneWay(tree), Some(&wt), &mut opts)
            .map_err(unpack_failed)?;
        let conv = crate::convert::Converter::checkout(repo, plan.index())?;
        let applied = plan.apply(repo.odb(), &wt, &conv);
        let mut index = applied.index;
        if let Some(reference) = mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        crate::switch_branch::write_index(repo, &index, lock)?;
        Ok(())
    }

    /// Between attempts: the index and work tree go back to `HEAD` when an
    /// external strategy changed them, and saved local changes come back
    /// (git resets to `HEAD`, which shows in the reflog, and applies them).
    fn restore_state(&mut self, head: ObjectId, applied: bool, saved: bool) -> Result<(), Die> {
        if applied {
            self.rewind()?;
        }
        if saved {
            self.update_head(head, Some(head), msg::UPDATING_HEAD)?;
        }
        Ok(())
    }

    /// Whether the index or the work tree differ from `HEAD`.
    fn has_local_changes(&self, head_tree: ObjectId) -> Result<bool, Die> {
        use rgit_worktree::stat::Status;
        if !self.staged_changes(head_tree)?.is_empty() {
            return Ok(true);
        }
        let (index, mtime) = self.repo.read_index()?;
        let checker = self.repo.checker(mtime)?;
        Ok(index.entries().iter().any(|e| {
            e.stage() == Stage::MERGED
                && !e.mode.is_gitlink()
                && !matches!(checker.check(e), Ok(Status::Unchanged | Status::StatOnly))
        }))
    }

    /// The number of unmerged index entries.
    fn unmerged_count(&self) -> Result<usize, Die> {
        let (index, _) = self.repo.read_index()?;
        Ok(index
            .entries()
            .iter()
            .filter(|e| e.stage() != Stage::MERGED)
            .count())
    }

    /// The first of `paths` whose work tree file differs from its index
    /// entry.
    fn first_not_uptodate(&self, paths: &[Vec<u8>]) -> Result<Option<Vec<u8>>, Die> {
        use rgit_worktree::stat::Status;
        if paths.is_empty() {
            return Ok(None);
        }
        let (index, mtime) = self.repo.read_index()?;
        let checker = self.repo.checker(mtime)?;
        for p in paths {
            let Some(e) = index
                .entries()
                .iter()
                .find(|e| e.path() == p.as_slice() && e.stage() == Stage::MERGED)
            else {
                continue;
            };
            if e.mode.is_gitlink() {
                continue;
            }
            if !matches!(checker.check(e), Ok(Status::Unchanged | Status::StatOnly)) {
                return Ok(Some(p.clone()));
            }
        }
        Ok(None)
    }

    /// The end of a merge by an external strategy: the result is what it
    /// left in the index; `clean` is what its exit code said.
    fn finish_applied(
        &mut self,
        head: ObjectId,
        parents: Vec<ObjectId>,
        theirs: &[Head],
        clean: bool,
    ) -> CmdResult {
        let (index, _) = self.repo.read_index()?;
        let mut conflicts: Vec<rgit_merge::ConflictedPath> = Vec::new();
        for e in index.entries() {
            let stage = e.stage().get();
            if stage == 0 {
                continue;
            }
            if conflicts.last().is_none_or(|c| c.path != e.path()) {
                conflicts.push(rgit_merge::ConflictedPath {
                    path: e.path().to_vec(),
                    stages: [None; 3],
                });
            }
            let c = conflicts.last_mut().expect("just pushed");
            c.stages[usize::from(stage) - 1] = Some(rgit_merge::Version {
                mode: e.mode.0,
                oid: e.id,
            });
        }
        let clean = clean && conflicts.is_empty();
        let tree = if clean {
            rgit_commit::write_tree(
                self.repo.odb(),
                &index,
                &rgit_commit::WriteTreeOptions::default(),
            )
            .map_err(|e| fatal(e.to_string()))?
        } else {
            self.tree_of(&head)?
        };
        let result = MergeResult {
            tree,
            clean,
            conflicts,
            messages: Vec::new(),
            rename_limit_needed: None,
        };
        self.finish(head, parents, theirs, result, None, true)
    }

    /// The common end of a merge that was not a fast-forward: the index
    /// and work tree move from `HEAD` (`head`) to `result`, then the
    /// state files, the commit with `parents` (or the stop before it)
    /// and the messages.
    ///
    /// `lines` is the output of the strategy (`None`: printed already);
    /// `applied` when the index and work tree hold the result already.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        head: ObjectId,
        parents: Vec<ObjectId>,
        theirs: &[Head],
        result: MergeResult,
        lines: Option<&[resolve::Line]>,
        applied: bool,
    ) -> CmdResult {
        let head_tree = self.tree_of(&head)?;
        let their_ids: Vec<ObjectId> = theirs.iter().map(|h| h.commit).collect();
        // The index and work tree are updated before the messages appear.
        if !applied {
            if let Err(e) = self.checkout(head_tree, result.tree, Some(&result)) {
                if matches!(e, Die::Exit(1)) {
                    eprintln!("{}", msg::strategy_failed(&self.strategy));
                    self.keep_local_state(head_tree)?;
                    self.restore_autostash()?;
                    return Err(Die::Exit(2));
                }
                return Err(e);
            }
        }
        if let Some(lines) = lines {
            resolve::print(lines);
        }
        super::merge_tree::advise_submodules(self.ctx, &self.repo, &result)?;
        if is_ort(&self.strategy) {
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
            self.autostash_pending();
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
            self.autostash_pending();
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
        self.create_autostash()?;
        let head_tree = self.tree_of(&head)?;
        if self.strategy == "ours" {
            let mut parents = vec![head];
            parents.extend(heads.iter().map(|h| h.commit));
            let result = MergeResult {
                tree: head_tree,
                clean: true,
                conflicts: Vec::new(),
                messages: Vec::new(),
                rename_limit_needed: None,
            };
            return self.finish(head, parents, &heads, result, Some(&[]), false);
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
            let mut opts = self.engine_options(h)?;
            // The octopus strategy merges file by file, without renames.
            opts.renames = false;
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
                rename_limit_needed: None,
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
        let lines = engine_lines(&result);
        self.finish(head, merged, &theirs, result, Some(&lines), false)
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
                    self.repo.setup.original_cwd.join(path)
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
        let conv = crate::convert::Converter::checkout(repo, plan.index())?;
        let applied = plan.apply(repo.odb(), &wt, &conv);
        let mut index = applied.index;
        repo.converter()?.use_index(&index);
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
            max_depth: None,
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
        // A squash leaves the commit (and the autostash) to the user.
        if self.args.squash {
            self.autostash_pending();
        } else {
            crate::maintenance::run_auto_quiet(self.ctx, self.args.quiet);
            if self.stash.is_some() {
                apply_merge_autostash(self.ctx, &self.git_dir)?;
                // Unlike `commit` and `--abort`, a merge that is done
                // leaves no AUTO_MERGE from applying the stash.
                let _ = std::fs::remove_file(self.git_dir.join("AUTO_MERGE"));
            }
        }
        Ok(())
    }

    /// `--autostash`: stashes the local changes, if any, before the index
    /// and work tree move; the stash is recorded in `MERGE_AUTOSTASH`.
    fn create_autostash(&mut self) -> Result<(), Die> {
        if !self.autostash || self.stash.is_some() {
            return Ok(());
        }
        if let Some(hex) = crate::rebase::autostash_local_changes(self.ctx)? {
            self.write_file("MERGE_AUTOSTASH", format!("{hex}\n").as_bytes())?;
            self.stash = Some(hex);
            self.repo = Repo::open(self.ctx, Mode::Required)?;
        }
        Ok(())
    }

    /// The merge failed: the local changes come back from the stash (the
    /// application's `AUTO_MERGE` stays, as with git).
    fn restore_autostash(&mut self) -> Result<(), Die> {
        if let Some(hex) = self.stash.take() {
            let _ = std::fs::remove_file(self.git_dir.join("MERGE_AUTOSTASH"));
            crate::rebase::apply_autostash(self.ctx, &hex)?;
        }
        Ok(())
    }

    /// The merge stopped before its commit: the stash waits for it.
    fn autostash_pending(&self) {
        if self.stash.is_some() {
            println!("{}", msg::APPLY_STASH_WHEN_FINISHED);
        }
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
        let mut new = rgit_object::Commit::new(
            tree,
            parents,
            &author,
            &committer,
            encoding.as_deref(),
            mergetags,
            cleaned,
        );
        if let Some(sign) = &self.args.sign {
            let hash = self.repo.hash;
            match crate::signing::sign_commit(&self.repo.config, hash, &mut new, sign, &committer) {
                Ok(()) => {}
                Err(crate::signing::SignError::Die(d)) => return Err(d),
                Err(crate::signing::SignError::Failed) => {
                    return Err(fatal(crate::messages::signing::FAILED_TO_WRITE_COMMIT));
                }
            }
        }
        let oid = self
            .repo
            .odb()
            .write(ObjectKind::Commit, &new.serialize())
            .map_err(|e| fatal(e.to_string()))?;
        let made_by = self
            .made_by
            .clone()
            .unwrap_or_else(|| msg::made_by(&self.strategy));
        self.update_head(oid, Some(head), &made_by)?;
        for f in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
            let _ = std::fs::remove_file(self.git_dir.join(f));
        }
        if !self.args.quiet {
            println!("{made_by}");
            if self.show_stat {
                self.print_stat(head_tree, tree)?;
            }
        }
        self.post_merge()?;
        Ok(0)
    }
}

/// The outcome of one strategy.
enum Attempt {
    /// The strategy could not handle the merge (exit code 2).
    Failed,
    /// A result computed in memory and the strategy's output.
    Memory(MergeResult, Vec<resolve::Line>),
    /// An external strategy updated the index and work tree; whether it
    /// merged cleanly.
    Applied(bool),
}

/// The strategies of the `ort` engine (they leave `AUTO_MERGE`).
fn is_ort(name: &str) -> bool {
    matches!(name, "ort" | "recursive" | "subtree")
}

/// The engine's messages as output lines.
fn engine_lines(result: &MergeResult) -> Vec<resolve::Line> {
    result
        .messages
        .iter()
        .map(|m| resolve::Line {
            err: false,
            text: lossy(&m.text),
        })
        .collect()
}

/// `merge.renormalize`.
pub(crate) fn renormalize_config(config: &rgit_config::ConfigSet) -> bool {
    config
        .bool("merge.renormalize")
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// The built-in strategies.
fn is_builtin_strategy(name: &str) -> bool {
    matches!(
        name,
        "ort" | "recursive" | "ours" | "octopus" | "resolve" | "subtree"
    )
}

/// Rejects strategy names that are neither built in nor a `git-merge-<name>`
/// program, listing the custom strategies (`git-merge-*` programs on
/// `$PATH`).
fn check_strategies(ctx: &Context, names: &[String]) -> Result<(), Die> {
    let mut programs = None;
    for s in names {
        if is_builtin_strategy(s) {
            continue;
        }
        let (main, others) = programs.get_or_insert_with(|| {
            let main = crate::cmdlist::main_commands(ctx);
            let others = crate::cmdlist::other_commands(ctx, &main);
            (main, others)
        });
        let program = format!("merge-{s}");
        // Programs that are git commands (merge-index, merge-file, ...)
        // are not strategies.
        let is_command = crate::commands::find(&program).is_some()
            || program == "merge-one-file"
            || program == "merge-index";
        if !s.is_empty()
            && !s.contains(char::is_whitespace)
            && !is_command
            && (main.contains(&program) || others.contains(&program))
        {
            continue;
        }
        let custom: Vec<String> = others
            .iter()
            .filter_map(|o| o.strip_prefix("merge-"))
            .filter(|n| !is_builtin_strategy(n))
            .map(str::to_owned)
            .collect();
        eprintln!("{}", msg::unknown_strategy(s, &custom));
        return Err(Die::Exit(1));
    }
    Ok(())
}

/// The scissors line and its explanation, as comments.
pub(crate) fn scissors_block(comment: &str) -> Vec<u8> {
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
pub(crate) fn add_conflicts(index: &mut Index, result: &MergeResult) -> Result<(), Die> {
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
