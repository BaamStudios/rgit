// SPDX-License-Identifier: MIT
//! `git pull` (Documentation/git-pull.adoc, config/pull.adoc,
//! merge-options.adoc, fetch-options.adoc), part of #167.
//!
//! As git-pull(1) describes it, `pull` runs `fetch` (with
//! `--update-head-ok` and the fetch options) and then integrates the
//! commits `FETCH_HEAD` marks for merging with `merge` (the merge options
//! passed on, `FETCH_HEAD` as the commit). Both run as child processes of
//! the same binary at the top of the work tree, with `GIT_REFLOG_ACTION`
//! set to `pull <arguments>`.
//!
//! Supported:
//! - `--ff`, `--no-ff`, `--ff-only`, `pull.ff`, `--[no-]rebase[=<mode>]`,
//!   `branch.<name>.rebase`, `pull.rebase` and their precedence (an
//!   explicit `--ff-only` wins over any rebase setting, `pull.ff=only`
//!   only over a configured one); the hint and error for divergent
//!   branches when nothing says how to reconcile them;
//! - the messages when nothing was fetched for merging (wildcard
//!   refspecs, detached `HEAD`, no upstream, upstream not fetched, a
//!   remote that is not the branch's);
//! - a fetch that moves the checked-out branch: the work tree follows;
//! - pulling into an unborn branch (index and work tree moved from the
//!   empty tree, keeping staged new files), several heads (octopus);
//! - refusing to start with unmerged files or an unfinished merge.
//!
//! - `--rebase` (also from the configuration): a fast-forward is done as
//!   `merge --ff-only` (with the rebase's `--[no-]autostash`), anything
//!   else runs `rebase --onto <fetched head> <upstream>` as a child (the work is done by [`crate::rebase`]), with
//!   the verbosity, `--stat`/`-n`, `--strategy`, `--strategy-option`,
//!   `--signoff`, `--gpg-sign` and `--[no-]autostash` (`pull.autostash`,
//!   else `rebase.autoStash`) passed on. The upstream is the fork point of
//!   the branch in the reflog of the remote-tracking branch (computed
//!   before fetching, as `merge-base --fork-point`), so that commits the
//!   upstream rewrote are not replayed; the fetched head when there is
//!   none or it is already part of the fetched history.
//!   `--rebase=merges` adds `--rebase-merges`, `--rebase=interactive`
//!   adds `--interactive` (after the verbosity, as git).

use std::path::PathBuf;
use std::process::{Command, Stdio};

use rgit_hash::ObjectId;
use rgit_merge::ObjectCommits;
use rgit_object::ObjectKind;
use rgit_refs::{RefUpdate, Transaction};
use rgit_remote::{BranchConfig, Remotes};
use rgit_unpack::Porcelain;
use rgit_worktree::stat::Status;

use crate::messages::pull as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::Mode;
use crate::repository::Repo;

/// How the fetched commits are integrated (`--rebase=<mode>`,
/// `pull.rebase`, `branch.<name>.rebase`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rebase {
    False,
    True,
    Merges,
    Interactive,
}

/// `true`, `false` (any boolean), `merges`/`m`, `interactive`/`i`.
fn parse_rebase(value: &str) -> Option<Rebase> {
    match value {
        "merges" | "m" => return Some(Rebase::Merges),
        "interactive" | "i" => return Some(Rebase::Interactive),
        _ => {}
    }
    match rgit_config::value::parse_bool(Some(value.as_bytes())) {
        Some(true) => Some(Rebase::True),
        Some(false) => Some(Rebase::False),
        None => None,
    }
}

/// One option passed on as typed (`OPT_PASSTHRU`).
type Pass = RefCell<Option<String>>;

/// The command line.
struct Args {
    verbosity: i32,
    progress: Option<String>,
    recurse_submodules: Option<String>,
    rebase: Option<Rebase>,
    // Merge options.
    diffstat: Option<String>,
    compact_summary: Option<String>,
    log: Option<String>,
    signoff: Option<String>,
    squash: Option<String>,
    commit: Option<String>,
    edit: Option<String>,
    cleanup: Option<String>,
    ff: Option<String>,
    verify: Option<String>,
    verify_signatures: Option<String>,
    autostash: i32,
    strategies: Vec<String>,
    strategy_opts: Vec<String>,
    gpg_sign: Option<String>,
    allow_unrelated: Option<String>,
    // Fetch options.
    all: Option<String>,
    append: Option<String>,
    upload_pack: Option<String>,
    force: Option<String>,
    tags: Option<String>,
    prune: Option<String>,
    jobs: Option<String>,
    dry_run: bool,
    dry_run_text: Option<String>,
    keep: Option<String>,
    depth: Option<String>,
    shallow_since: Option<String>,
    shallow_exclude: Option<String>,
    deepen: Option<String>,
    unshallow: Option<String>,
    update_shallow: Option<String>,
    refmap: Option<String>,
    server_options: Vec<String>,
    ipv4: Option<String>,
    ipv6: Option<String>,
    negotiation_tips: Vec<String>,
    negotiation_include: Vec<String>,
    show_forced_updates: Option<String>,
    set_upstream: Option<String>,
    /// `[<repository> [<refspec>...]]`.
    rest: Vec<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    use crate::messages::fetch as fmsg;
    use crate::messages::merge as mmsg;
    // One counter, as git's verbosity options: -v counts up from 0, -q
    // down from 0, --no-verbose/--no-quiet reset it.
    let verbosity = Cell::new(0);
    let p: Vec<Pass> = (0..40).map(|_| RefCell::new(None)).collect();
    let rebase: Cell<Option<Rebase>> = Cell::new(None);
    let autostash = Cell::new(-1);
    let dry_run = Cell::new(0);
    let strategies = RefCell::new(Vec::new());
    let strategy_opts = RefCell::new(Vec::new());
    let server_options = RefCell::new(Vec::new());
    let negotiation_tips = RefCell::new(Vec::new());
    let negotiation_include = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::callback('v', "verbose", None, mmsg::HELP_VERBOSE, |_, unset| {
                verbosity.set(if unset { 0 } else { verbosity.get().max(0) + 1 });
                Ok(())
            })
            .noarg(),
            Opt::callback('q', "quiet", None, mmsg::HELP_QUIET, |_, unset| {
                verbosity.set(if unset { 0 } else { verbosity.get().min(0) - 1 });
                Ok(())
            })
            .noarg(),
            Opt::passthru(None, "progress", &p[0], None, mmsg::HELP_PROGRESS).noarg(),
            Opt::passthru(
                None,
                "recurse-submodules",
                &p[1],
                Some("on-demand"),
                msg::HELP_RECURSE_SUBMODULES,
            )
            .optional(None),
            Opt::group(msg::GROUP_MERGING),
            Opt::callback(
                'r',
                "rebase",
                Some("(false|true|merges|interactive)"),
                msg::HELP_REBASE,
                |v, unset| {
                    let mode = match (v, unset) {
                        (_, true) => Rebase::False,
                        (None, false) => Rebase::True,
                        (Some(v), false) => parse_rebase(v).ok_or_else(|| {
                            CallbackError::Error(msg::invalid_option_value("--rebase", v))
                        })?,
                    };
                    rebase.set(Some(mode));
                    Ok(())
                },
            )
            .optional(None)
            .literal_argh(),
            Opt::passthru('n', None, &p[2], None, mmsg::HELP_N).noneg().noarg(),
            Opt::passthru(None, "stat", &p[2], None, mmsg::HELP_STAT).noarg(),
            Opt::passthru(None, "summary", &p[2], None, mmsg::HELP_SUMMARY)
                .noarg()
                .hidden(),
            Opt::passthru(
                None,
                "compact-summary",
                &p[3],
                None,
                mmsg::HELP_COMPACT_SUMMARY,
            )
            .noarg(),
            Opt::passthru(None, "log", &p[4], Some("n"), mmsg::HELP_LOG).optional(None),
            Opt::passthru(None, "signoff", &p[5], Some("..."), msg::HELP_SIGNOFF)
                .optional(None)
                .literal_argh(),
            Opt::passthru(None, "squash", &p[6], None, mmsg::HELP_SQUASH).noarg(),
            Opt::passthru(None, "commit", &p[7], None, mmsg::HELP_COMMIT).noarg(),
            Opt::passthru(None, "edit", &p[8], None, msg::HELP_EDIT).noarg(),
            Opt::passthru(None, "cleanup", &p[9], Some("mode"), mmsg::HELP_CLEANUP),
            Opt::passthru(None, "ff", &p[10], None, msg::HELP_FF).noarg(),
            Opt::passthru(None, "ff-only", &p[10], None, mmsg::HELP_FF_ONLY)
                .noarg()
                .noneg(),
            Opt::passthru(None, "verify", &p[11], None, msg::HELP_VERIFY).noarg(),
            Opt::passthru(
                None,
                "verify-signatures",
                &p[12],
                None,
                mmsg::HELP_VERIFY_SIGNATURES,
            )
            .noarg(),
            Opt::bool(None, "autostash", &autostash, mmsg::HELP_AUTOSTASH),
            Opt::passthru_argv('s', "strategy", &strategies, Some("strategy"), mmsg::HELP_STRATEGY),
            Opt::passthru_argv(
                'X',
                "strategy-option",
                &strategy_opts,
                Some("option=value"),
                mmsg::HELP_STRATEGY_OPTION,
            ),
            Opt::passthru('S', "gpg-sign", &p[13], Some("key-id"), mmsg::HELP_GPG_SIGN)
                .optional(None),
            Opt::passthru(
                None,
                "allow-unrelated-histories",
                &p[14],
                None,
                msg::HELP_ALLOW_UNRELATED,
            )
            .noarg(),
            Opt::group(msg::GROUP_FETCHING),
            Opt::passthru(None, "all", &p[15], None, msg::HELP_ALL).noarg(),
            Opt::passthru('a', "append", &p[16], None, fmsg::HELP_APPEND).noarg(),
            Opt::passthru(None, "upload-pack", &p[17], Some("path"), fmsg::HELP_UPLOAD_PACK),
            Opt::passthru('f', "force", &p[18], None, msg::HELP_FORCE).noarg(),
            Opt::passthru('t', "tags", &p[19], None, fmsg::HELP_TAGS).noarg(),
            Opt::passthru('p', "prune", &p[20], None, fmsg::HELP_PRUNE).noarg(),
            Opt::passthru('j', "jobs", &p[21], Some("n"), msg::HELP_JOBS).optional(None),
            Opt::bool(None, "dry-run", &dry_run, fmsg::HELP_DRY_RUN),
            Opt::passthru('k', "keep", &p[22], None, fmsg::HELP_KEEP).noarg(),
            Opt::passthru(None, "depth", &p[23], Some("depth"), msg::HELP_DEPTH),
            Opt::passthru(
                None,
                "shallow-since",
                &p[24],
                Some("time"),
                msg::HELP_SHALLOW_SINCE,
            ),
            Opt::passthru(
                None,
                "shallow-exclude",
                &p[25],
                Some("ref"),
                msg::HELP_SHALLOW_EXCLUDE,
            ),
            Opt::passthru(None, "deepen", &p[26], Some("n"), msg::HELP_DEPTH),
            Opt::passthru(None, "unshallow", &p[27], None, msg::HELP_UNSHALLOW)
                .noarg()
                .noneg(),
            Opt::passthru(
                None,
                "update-shallow",
                &p[28],
                None,
                msg::HELP_UPDATE_SHALLOW,
            )
            .noarg(),
            Opt::passthru(None, "refmap", &p[29], Some("refmap"), msg::HELP_REFMAP).noneg(),
            Opt::passthru_argv(
                'o',
                "server-option",
                &server_options,
                Some("server-specific"),
                fmsg::HELP_SERVER_OPTION,
            ),
            Opt::passthru('4', "ipv4", &p[30], None, fmsg::HELP_IPV4).noarg(),
            Opt::passthru('6', "ipv6", &p[31], None, fmsg::HELP_IPV6).noarg(),
            Opt::passthru_argv(
                None,
                "negotiation-restrict",
                &negotiation_tips,
                Some("revision"),
                msg::HELP_NEGOTIATION_RESTRICT,
            ),
            Opt::passthru_argv(
                None,
                "negotiation-tip",
                &negotiation_tips,
                Some("revision"),
                msg::HELP_NEGOTIATION_TIP,
            ),
            Opt::passthru_argv(
                None,
                "negotiation-include",
                &negotiation_include,
                Some("revision"),
                msg::HELP_NEGOTIATION_INCLUDE,
            ),
            Opt::passthru(
                None,
                "show-forced-updates",
                &p[32],
                None,
                fmsg::HELP_SHOW_FORCED,
            )
            .noarg(),
            Opt::passthru(None, "set-upstream", &p[33], None, msg::HELP_SET_UPSTREAM).noarg(),
        ],
    );
    let rest = parser.parse(argv)?.args;
    drop(parser);
    let take = |i: usize| p[i].borrow_mut().take();
    let verbosity = verbosity.get();
    Ok(Args {
        verbosity,
        progress: take(0),
        recurse_submodules: take(1),
        rebase: rebase.get(),
        diffstat: take(2),
        compact_summary: take(3),
        log: take(4),
        signoff: take(5),
        squash: take(6),
        commit: take(7),
        edit: take(8),
        cleanup: take(9),
        ff: take(10),
        verify: take(11),
        verify_signatures: take(12),
        autostash: autostash.get(),
        strategies: strategies.into_inner(),
        strategy_opts: strategy_opts.into_inner(),
        gpg_sign: take(13),
        allow_unrelated: take(14),
        all: take(15),
        append: take(16),
        upload_pack: take(17),
        force: take(18),
        tags: take(19),
        prune: take(20),
        jobs: take(21),
        dry_run: dry_run.get() == 1,
        dry_run_text: match dry_run.get() {
            1 => Some("--dry-run".to_owned()),
            0 => Some("--no-dry-run".to_owned()),
            _ => None,
        },
        keep: take(22),
        depth: take(23),
        shallow_since: take(24),
        shallow_exclude: take(25),
        deepen: take(26),
        unshallow: take(27),
        update_shallow: take(28),
        refmap: take(29),
        server_options: server_options.into_inner(),
        ipv4: take(30),
        ipv6: take(31),
        negotiation_tips: negotiation_tips.into_inner(),
        negotiation_include: negotiation_include.into_inner(),
        show_forced_updates: take(32),
        set_upstream: take(33),
        rest,
    })
}

impl Args {
    /// `-q` or `-v` as often as given.
    fn verbosity_args(&self, out: &mut Vec<String>) {
        let (flag, n) = if self.verbosity < 0 {
            ("-q", -self.verbosity)
        } else {
            ("-v", self.verbosity)
        };
        for _ in 0..n {
            out.push(flag.to_owned());
        }
    }

    fn fetch_argv(&self) -> Vec<String> {
        let mut a = vec!["fetch".to_owned(), "--update-head-ok".to_owned()];
        self.verbosity_args(&mut a);
        let opts = [
            &self.progress,
            &self.recurse_submodules,
            &self.all,
            &self.append,
            &self.upload_pack,
            &self.force,
            &self.tags,
            &self.prune,
            &self.jobs,
            &self.dry_run_text,
            &self.keep,
            &self.depth,
            &self.shallow_since,
            &self.shallow_exclude,
            &self.deepen,
            &self.unshallow,
            &self.update_shallow,
            &self.refmap,
            &self.ipv4,
            &self.ipv6,
        ];
        a.extend(opts.into_iter().flatten().cloned());
        a.extend(self.negotiation_tips.iter().cloned());
        a.extend(self.negotiation_include.iter().cloned());
        a.extend(self.show_forced_updates.iter().cloned());
        a.extend(self.set_upstream.iter().cloned());
        a.extend(self.server_options.iter().cloned());
        a.extend(self.rest.iter().cloned());
        a
    }

    /// `rebase` with the options a rebasing pull passes on.
    fn rebase_argv(
        &self,
        autostash: bool,
        mode: Option<&str>,
        onto: &ObjectId,
        upstream: &ObjectId,
    ) -> Vec<String> {
        let mut a = vec!["rebase".to_owned()];
        self.verbosity_args(&mut a);
        a.extend(mode.map(str::to_owned));
        a.extend(self.diffstat.iter().cloned());
        a.extend(self.strategies.iter().cloned());
        a.extend(self.strategy_opts.iter().cloned());
        a.extend(self.signoff.iter().cloned());
        a.extend(self.gpg_sign.iter().cloned());
        a.push(if autostash { "--autostash" } else { "--no-autostash" }.to_owned());
        a.push("--onto".to_owned());
        a.push(onto.to_hex());
        a.push(upstream.to_hex());
        a
    }

    fn merge_argv(&self, ff: Option<&str>, autostash: Option<bool>) -> Vec<String> {
        let mut a = vec!["merge".to_owned()];
        self.verbosity_args(&mut a);
        let opts = [
            &self.progress,
            &self.diffstat,
            &self.compact_summary,
            &self.log,
            &self.signoff,
            &self.squash,
            &self.commit,
            &self.edit,
            &self.cleanup,
        ];
        a.extend(opts.into_iter().flatten().cloned());
        a.extend(ff.map(str::to_owned));
        a.extend(self.verify.iter().cloned());
        a.extend(self.verify_signatures.iter().cloned());
        match autostash {
            Some(true) => a.push("--autostash".to_owned()),
            Some(false) => a.push("--no-autostash".to_owned()),
            None => {}
        }
        a.extend(self.strategies.iter().cloned());
        a.extend(self.strategy_opts.iter().cloned());
        a.extend(self.gpg_sign.iter().cloned());
        a.extend(self.allow_unrelated.iter().cloned());
        a.push("FETCH_HEAD".to_owned());
        a
    }
}

fn fatal(s: impl Into<String>) -> Die {
    Die::Fatal(s.into())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// The last value of `key`, `None` when unset (a value-less key is
/// "true").
fn config_value(repo: &Repo, key: &str) -> Option<String> {
    let e = repo.config.get(key)?;
    Some(e.value.as_ref().map_or_else(|| "true".to_owned(), |v| lossy(v)))
}

/// `branch.<name>.rebase`, else `pull.rebase`.
fn configured_rebase(repo: &Repo, branch: Option<&str>) -> Result<Option<Rebase>, Die> {
    let mut keys = Vec::new();
    if let Some(b) = branch {
        keys.push(format!("branch.{b}.rebase"));
    }
    keys.push("pull.rebase".to_owned());
    for key in keys {
        if let Some(v) = config_value(repo, &key) {
            return parse_rebase(&v)
                .map(Some)
                .ok_or_else(|| fatal(msg::invalid_option_value(&key, &v)));
        }
    }
    Ok(None)
}

/// `pull.ff` as the option it stands for.
fn configured_ff(repo: &Repo) -> Result<Option<String>, Die> {
    let Some(v) = config_value(repo, "pull.ff") else {
        return Ok(None);
    };
    if v == "only" {
        return Ok(Some("--ff-only".to_owned()));
    }
    match rgit_config::value::parse_bool(Some(v.as_bytes())) {
        Some(true) => Ok(Some("--ff".to_owned())),
        Some(false) => Ok(Some("--no-ff".to_owned())),
        None => Err(fatal(msg::invalid_option_value("pull.ff", &v))),
    }
}

/// `pull.autostash`, overridden by `--[no-]autostash`.
fn autostash(repo: &Repo, args: &Args) -> Option<bool> {
    match args.autostash {
        -1 => repo.config.bool("pull.autostash").ok().flatten(),
        v => Some(v == 1),
    }
}

/// Children: the same binary at the top of the work tree, seeing the
/// global options, with `GIT_REFLOG_ACTION` set.
struct Children {
    exe: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    dir: PathBuf,
}

impl Children {
    fn new(ctx: &Context, repo: &Repo, action: &str) -> Result<Children, Die> {
        let exe = std::env::current_exe().map_err(|e| fatal(e.to_string()))?;
        let mut env = ctx.child_env();
        if ctx.git_dir.is_some() || ctx.work_tree.is_some() {
            let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
            env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
            env.push(("GIT_DIR".into(), git_dir.into_os_string()));
            if let Some(wt) = &repo.setup.work_tree {
                env.push(("GIT_WORK_TREE".into(), wt.clone().into_os_string()));
            }
        }
        env.push(("GIT_REFLOG_ACTION".into(), action.into()));
        Ok(Children {
            exe,
            env,
            dir: repo.work_tree()?,
        })
    }

    fn run(&self, argv: &[String]) -> Result<i32, Die> {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let status = Command::new(&self.exe)
            .args(argv)
            .current_dir(&self.dir)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::inherit())
            .status()
            .map_err(|e| fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = parse_args(argv)?;
    let repo = Repo::open(ctx, Mode::Required)?;
    repo.work_tree()?;
    let branch = repo
        .refs
        .head_target()
        .and_then(|b| b.strip_prefix("refs/heads/").map(str::to_owned));

    // How to integrate: an explicit --ff-only wins over any rebase
    // setting, pull.ff=only only over a configured one.
    let mut rebase = args.rebase;
    let rebase_given = rebase.is_some();
    if rebase.is_none() {
        rebase = configured_rebase(&repo, branch.as_deref())?;
    }
    // A rebase was asked for (even if --ff-only overrides it below): the
    // work tree must be clean before anything is fetched.
    let rebase_requested = matches!(
        rebase,
        Some(Rebase::True | Rebase::Merges | Rebase::Interactive)
    );
    if let Some(mode) = args.cleanup.as_deref().and_then(|c| c.strip_prefix("--cleanup=")) {
        if rgit_commit::message::Cleanup::parse(mode).is_none() {
            return Err(fatal(crate::messages::commit::invalid_cleanup(mode)));
        }
    }
    let mut ff = args.ff.clone();
    let ff_given = ff.is_some();
    if ff.is_none() {
        ff = configured_ff(&repo)?;
    }
    if ff.as_deref() == Some("--ff-only") {
        if ff_given || !rebase_given {
            rebase = Some(Rebase::False);
        } else {
            ff = None;
        }
    }
    let rebasing = matches!(
        rebase,
        Some(Rebase::True | Rebase::Merges | Rebase::Interactive)
    );
    if rebasing && args.verify_signatures.as_deref() == Some("--verify-signatures") {
        eprintln!("warning: {}", msg::IGNORING_VERIFY_SIGNATURES);
    }

    // Nothing may be in the way of a merge.
    let git_dir = repo.setup.cwd.join(&repo.setup.git_dir);
    let (index, _) = repo.read_index()?;
    if index.has_conflicts() {
        eprintln!("error: {}", msg::UNMERGED);
        ctx.advisor(&repo.config)?
            .advise_without_footer(
                Advice::ResolveConflict,
                crate::messages::commit::UNMERGED_HINT,
            )
            .map_err(Die::Fatal)?;
        return Err(fatal(msg::UNRESOLVED));
    }
    drop(index);
    if git_dir.join("MERGE_HEAD").exists() {
        eprintln!("error: {}", msg::MERGE_IN_PROGRESS);
        ctx.advisor(&repo.config)?
            .advise_without_footer(Advice::ResolveConflict, msg::MERGE_IN_PROGRESS_HINT)
            .map_err(Die::Fatal)?;
        return Err(fatal(msg::UNFINISHED_MERGE));
    }
    let orig_head = head_commit(&repo);
    let rebase_autostash = match autostash(&repo, &args) {
        Some(v) => v,
        None => repo
            .config
            .bool("rebase.autostash")
            .ok()
            .flatten()
            .unwrap_or(false),
    };
    if rebase_requested {
        match orig_head {
            None => {
                if !repo.read_index()?.0.entries().is_empty() {
                    return Err(fatal(msg::UNBORN_WITH_STAGED));
                }
            }
            Some(head) if !rebase_autostash => require_clean_work_tree(&repo, head)?,
            Some(_) => {}
        }
    }

    // The fork point is taken before the fetch moves the remote-tracking
    // branch.
    let fork = match orig_head {
        Some(head) if rebase_requested => rebase_fork_point(&repo, &args, branch.as_deref(), head),
        _ => None,
    };

    let action = std::env::var("GIT_REFLOG_ACTION")
        .ok()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| {
            let mut a = String::from("pull");
            for arg in &argv[1..] {
                a.push(' ');
                a.push_str(arg);
            }
            a
        });
    let children = Children::new(ctx, &repo, &action)?;
    // `--recurse-submodules` (or `submodule.recurse`): the submodules
    // follow the merge or rebase (`submodule update --recursive`).
    let update_submodules = match args.recurse_submodules.as_deref() {
        None => rgit_submodule::settings::recurse(&repo.config),
        Some(a) => !matches!(
            a,
            "--no-recurse-submodules"
                | "--recurse-submodules=no"
                | "--recurse-submodules=false"
                | "--recurse-submodules=off"
                | "--recurse-submodules=0"
        ),
    };
    drop(repo);
    let finish = |code: i32, how: &str| -> Result<i32, Die> {
        if code != 0 || !update_submodules {
            return Ok(code);
        }
        let argv: Vec<String> = ["submodule", "update", "--recursive", how]
            .map(String::from)
            .into();
        children.run(&argv)
    };

    // A failed fetch ends `pull` with status 1, whatever fetch's own was.
    if children.run(&args.fetch_argv())? != 0 {
        return Ok(1);
    }
    if args.dry_run {
        return Ok(0);
    }

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let curr_head = head_commit(&repo);
    if let (Some(orig), Some(curr)) = (orig_head, curr_head) {
        if orig != curr {
            follow_fetched_head(&repo, orig, curr)?;
        }
    }

    let merge_heads = fetch_heads(&repo, &git_dir);
    if merge_heads.is_empty() {
        eprintln!("{}", no_candidates(&repo, &args, rebasing, branch.as_deref())?);
        return Ok(1);
    }

    let (Some(_), Some(head)) = (orig_head, curr_head) else {
        if merge_heads.len() > 1 {
            return Err(fatal(msg::MULTIPLE_INTO_EMPTY));
        }
        return pull_into_void(ctx, &mut repo, merge_heads[0], curr_head);
    };

    if merge_heads.len() > 1 {
        if rebasing {
            return Err(fatal(msg::REBASE_MULTIPLE));
        }
        if ff.as_deref() == Some("--ff-only") {
            return Err(fatal(msg::FF_MULTIPLE));
        }
    }

    let commits: Vec<ObjectId> = merge_heads
        .iter()
        .filter_map(|id| repo.peel_to(*id, ObjectKind::Commit))
        .collect();
    let all_commits = commits.len() == merge_heads.len();
    let can_ff = all_commits && commits.len() == 1 && is_ancestor(&repo, &head, &commits[0])?;
    let up_to_date = all_commits && {
        let mut all = true;
        for c in &commits {
            all &= is_ancestor(&repo, c, &head)?;
        }
        all
    };

    if rebase.is_none() && ff.is_none() && !can_ff && !up_to_date {
        let advisor = ctx.advisor(&repo.config)?;
        eprint!("{}", advisor.hint(msg::DIVERGENT_HINT));
        return Err(fatal(msg::NEED_RECONCILE));
    }
    if ff.as_deref() == Some("--ff-only") && !rebasing && !can_ff && !up_to_date {
        let mmsg = crate::messages::merge::DIVERGING_HINT;
        ctx.advisor(&repo.config)?
            .advise(Advice::Diverging, mmsg)
            .map_err(Die::Fatal)?;
        return Err(fatal(crate::messages::merge::NOT_POSSIBLE_FF));
    }

    let stash = autostash(&repo, &args);
    if rebasing {
        if can_ff {
            drop(repo);
            let code = children.run(&args.merge_argv(Some("--ff-only"), Some(rebase_autostash)))?;
            return finish(code, "--rebase");
        }
        let onto = merge_heads[0];
        // A fork point the fetched history already contains gives the
        // same commits as the fetched head itself.
        let upstream = match (fork, commits.first()) {
            (Some(f), Some(c)) if !is_ancestor(&repo, &f, c)? => f,
            _ => onto,
        };
        if update_submodules && crate::fetch_submodules::touches_submodules(&repo, &upstream, &head) {
            return Err(fatal(msg::LOCAL_SUBMODULE_CHANGES));
        }
        drop(repo);
        let mode = match rebase {
            Some(Rebase::Merges) => Some("--rebase-merges"),
            Some(Rebase::Interactive) => Some("--interactive"),
            _ => None,
        };
        let code = children.run(&args.rebase_argv(rebase_autostash, mode, &onto, &upstream))?;
        return finish(code, "--rebase");
    }
    drop(repo);
    let code = children.run(&args.merge_argv(ff.as_deref(), stash))?;
    finish(code, "--checkout")
}

/// The fork point of `head` in the reflog of the remote-tracking branch
/// of what is pulled: `refs/remotes/<repository>/<branch>` for a refspec
/// given on the command line (`refs/heads/<branch>` for the repository
/// `.`), the upstream of the current branch
/// otherwise. `None` when there is none (or no current branch).
fn rebase_fork_point(
    repo: &Repo,
    args: &Args,
    branch: Option<&str>,
    head: ObjectId,
) -> Option<ObjectId> {
    let branch = branch?;
    let tracking = match (args.rest.first(), args.rest.get(1)) {
        (Some(remote), Some(spec)) => {
            let spec = spec.strip_prefix('+').unwrap_or(spec);
            let src = spec.split(':').next().unwrap_or_default();
            let src = if src.is_empty() { "HEAD" } else { src };
            let src = src.strip_prefix("refs/heads/").unwrap_or(src);
            if remote == "." {
                format!("refs/heads/{src}")
            } else {
                format!("refs/remotes/{remote}/{src}")
            }
        }
        _ => {
            let config = &repo.config;
            let b = BranchConfig::from_config(config, branch.as_bytes()).ok()?;
            let remotes = Remotes::from_config(config).ok()?;
            lossy(&rgit_remote::upstream_ref(&remotes, Some(&repo.setup.git_dir), Some(&b)).ok()?)
        }
    };
    let flags = rgit_refs::name::RefNameFlags::STRICT;
    rgit_refs::name::check_refname_format(tracking.as_bytes(), flags).ok()?;
    crate::rebase::fork_point(repo, &tracking, &head).ok().flatten()
}

/// Before a rebasing pull: no changes in the work tree or the index
/// (submodules ignored).
fn require_clean_work_tree(repo: &Repo, head: ObjectId) -> Result<(), Die> {
    let (index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    let mut unstaged = false;
    for e in index.entries() {
        if e.mode.is_gitlink() || e.stage().is_conflict() {
            continue;
        }
        let st = checker.check(e).map_err(|err| fatal(err.to_string()))?;
        if !matches!(st, Status::Unchanged | Status::StatOnly) {
            unstaged = true;
            break;
        }
    }
    let staged = match crate::switch_branch::commit_tree(repo, &head) {
        Some(tree) => index_differs(repo, &index, tree)?,
        None => true,
    };
    if !unstaged && !staged {
        return Ok(());
    }
    if unstaged {
        eprintln!("error: {}", msg::cannot_rebase(msg::UNSTAGED));
        if staged {
            eprintln!("error: {}", msg::ADDITIONALLY_STAGED);
        }
    } else {
        eprintln!("error: {}", msg::cannot_rebase(msg::STAGED));
    }
    eprintln!("error: {}", msg::COMMIT_OR_STASH);
    Err(Die::Exit(128))
}

/// Whether the index (submodules aside) differs from `tree`.
fn index_differs(repo: &Repo, index: &rgit_index::Index, tree: ObjectId) -> Result<bool, Die> {
    let tree = rgit_unpack::tree_entries(repo.odb(), &tree)
        .map_err(crate::commands::read_tree::unpack_failed)?;
    let entries: Vec<_> = index.entries().iter().collect();
    if entries.len() != tree.len() {
        return Ok(true);
    }
    Ok(entries.iter().zip(tree.iter()).any(|(e, t)| {
        e.path() != t.path()
            || e.flags.intent_to_add
            || (!(e.mode.is_gitlink() && t.mode.is_gitlink()) && (e.mode != t.mode || e.id != t.id))
    }))
}

/// The commit `HEAD` points to, `None` on an unborn branch.
fn head_commit(repo: &Repo) -> Option<ObjectId> {
    repo.refs.resolve("HEAD")
}

/// Whether `a` is an ancestor of (or equal to) `b`.
fn is_ancestor(repo: &Repo, a: &ObjectId, b: &ObjectId) -> Result<bool, Die> {
    if a == b {
        return Ok(true);
    }
    let shallow = std::fs::read_to_string(repo.setup.common_dir.join("shallow"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| ObjectId::from_hex(repo.hash, l.trim().as_bytes()).ok())
        .collect();
    let commits = ObjectCommits {
        objects: repo.odb(),
        shallow,
        label: Box::new(|id| id.to_hex().into_bytes()),
    };
    let bases = rgit_merge::merge_bases(&commits, a, b).map_err(|e| fatal(e.to_string()))?;
    Ok(bases.contains(a))
}

/// The objects of `FETCH_HEAD` marked for merging.
fn fetch_heads(repo: &Repo, git_dir: &std::path::Path) -> Vec<ObjectId> {
    let data = std::fs::read(git_dir.join("FETCH_HEAD")).unwrap_or_default();
    let mut heads = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        let mut parts = line.splitn(3, |&b| b == b'\t');
        let (Some(id), Some(flag)) = (parts.next(), parts.next()) else {
            continue;
        };
        if !flag.is_empty() {
            continue;
        }
        if let Ok(id) = ObjectId::from_hex(repo.hash, id) {
            heads.push(id);
        }
    }
    heads
}

/// Why nothing was fetched for merging.
fn no_candidates(
    repo: &Repo,
    args: &Args,
    rebase: bool,
    branch: Option<&str>,
) -> Result<String, Die> {
    if args.rest.len() > 1 {
        return Ok(msg::no_candidates(rebase));
    }
    let config = &repo.config;
    let b = match branch {
        Some(b) => Some(
            BranchConfig::from_config(config, b.as_bytes()).map_err(|e| fatal(e.to_string()))?,
        ),
        None => None,
    };
    let branch_remote = b.as_ref().and_then(|b| b.remote.as_ref()).map(|r| lossy(r));
    if let Some(given) = args.rest.first() {
        if branch_remote.as_deref() != Some(given.as_str()) {
            return Ok(msg::not_default_remote(given));
        }
    }
    let Some(b) = b else {
        return Ok(msg::not_on_a_branch(rebase));
    };
    let Some(merge) = b.merge.first() else {
        let remotes = Remotes::from_config(config).map_err(|e| fatal(e.to_string()))?;
        let name = remotes.default_remote_name(b.remote.as_ref().map(|r| r.as_ref()));
        let configured = remotes
            .get(&name, Some(&repo.setup.git_dir))
            .is_ok_and(|r| r.is_configured());
        let shown = if configured || rebase {
            lossy(&name)
        } else {
            "<remote>".to_owned()
        };
        return Ok(msg::no_tracking_information(
            rebase,
            &shown,
            branch.unwrap_or_default(),
        ));
    };
    Ok(msg::upstream_not_fetched(&lossy(merge)))
}

/// The fetch moved the checked-out branch from `orig` to `curr`: move the
/// index and work tree along.
fn follow_fetched_head(repo: &Repo, orig: ObjectId, curr: ObjectId) -> Result<(), Die> {
    let hex = orig.to_hex();
    eprintln!("warning: {}", msg::fetch_updated_head(&hex));
    let old_tree = crate::switch_branch::commit_tree(repo, &orig);
    let new_tree = crate::switch_branch::commit_tree(repo, &curr);
    let moved = match (old_tree, new_tree) {
        (Some(o), Some(n)) => {
            crate::switch_branch::move_index_with(repo, o, n, false, Porcelain::Merge).is_ok()
        }
        _ => false,
    };
    if !moved {
        return Err(fatal(msg::cannot_fast_forward_work_tree(&hex)));
    }
    Ok(())
}

/// Pull into an unborn branch: check `head` out (from the empty tree, so
/// staged new files stay) and point the branch at it.
fn pull_into_void(
    ctx: &Context,
    repo: &mut Repo,
    head: ObjectId,
    curr: Option<ObjectId>,
) -> CmdResult {
    let Some(commit) = repo.peel_to(head, ObjectKind::Commit) else {
        return Err(fatal(crate::messages::merge::not_something_we_can_merge(
            &head.to_hex(),
        )));
    };
    let tree = crate::switch_branch::commit_tree(repo, &commit)
        .ok_or_else(|| fatal(rgit_merge::messages::not_a_commit(&commit.to_hex())))?;
    let empty = ObjectId::empty_tree(repo.hash);
    crate::switch_branch::move_index_with(repo, empty, tree, false, Porcelain::Merge)?;
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects)
        .with_transaction_hook(crate::hooks::for_repo(ctx, setup, &config)?);
    let update = match curr {
        Some(old) => RefUpdate::update("HEAD", commit, Some(old)),
        None => RefUpdate::create("HEAD", commit),
    };
    let mut tx = Transaction::new(&store);
    tx.push(update.message(msg::REFLOG_INITIAL.to_owned()))
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| fatal(e.to_string()))?;
    Ok(0)
}
