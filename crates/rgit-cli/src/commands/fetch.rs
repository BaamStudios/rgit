// SPDX-License-Identifier: MIT
//! `git fetch` (Documentation/git-fetch.adoc) from one remote or URL, on
//! top of `rgit-fetch`.
//!
//! Supported: `[<repository> [<refspec>...]]`, `-v`, `-q`, `-a`, `-f`, `-t` /
//! `--no-tags` / `-n`, `-p` / `--prune`, `--dry-run`,
//! `--[no-]write-fetch-head`, `-u`, `--upload-pack`, `-o`, `-4` / `-6`,
//! `--[no-]progress`. Accepted and ignored: `-k`, `--auto-maintenance`,
//! `--auto-gc`, `--show-forced-updates`, `--write-commit-graph`.
//! Only protocol v2 servers are supported.

use std::io::{IsTerminal, Write};

use bstr::ByteSlice;
use rgit_config::SystemEnv;
use rgit_fetch::{ConnectSettings, FetchOptions, LocalRepo, display};
use rgit_remote::{BranchConfig, Remotes, TagOpt};
use rgit_transport::IpVersion;

use crate::messages::fetch as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

/// Terminal width for the summary: `$COLUMNS`, else 80.
fn columns() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    run_with_reflog_action(ctx, argv, None)
}

/// [`run`] for another command fetching in-process (`remote update`):
/// `action` replaces the reflog action derived from `argv` (an explicit
/// `$GIT_REFLOG_ACTION` still wins).
pub fn run_with_reflog_action(
    ctx: &mut Context,
    argv: &[String],
    action: Option<&str>,
) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let force = Cell::new(0);
    let tags = Cell::new(-1);
    let prune = Cell::new(-1);
    let dry_run = Cell::new(0);
    let write_fetch_head = Cell::new(true);
    let append = Cell::new(false);
    let keep = Cell::new(false);
    let update_head_ok = Cell::new(false);
    let progress = Cell::new(-1);
    let ipv4 = Cell::new(false);
    let ipv6 = Cell::new(false);
    let ignored = Cell::new(false);
    let ignored_i = Cell::new(0);
    let upload_pack: RefCell<Option<String>> = RefCell::new(None);
    let server_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::string(None, "upload-pack", &upload_pack, "path", msg::HELP_UPLOAD_PACK),
            Opt::force(&force, msg::HELP_FORCE),
            Opt::bool('t', "tags", &tags, msg::HELP_TAGS),
            Opt::set_int('n', None, &tags, msg::HELP_NO_TAGS, 0).noneg(),
            Opt::bool('p', "prune", &prune, msg::HELP_PRUNE),
            Opt::bool(None, "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag(None, "write-fetch-head", &write_fetch_head, msg::HELP_WRITE_FETCH_HEAD),
            Opt::flag('a', "append", &append, msg::HELP_APPEND),
            Opt::flag('k', "keep", &keep, msg::HELP_KEEP),
            Opt::flag('u', "update-head-ok", &update_head_ok, msg::HELP_UPDATE_HEAD_OK),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::string_list(
                'o',
                "server-option",
                &server_options,
                "server-specific",
                msg::HELP_SERVER_OPTION,
            ),
            Opt::flag('4', "ipv4", &ipv4, msg::HELP_IPV4).noneg(),
            Opt::flag('6', "ipv6", &ipv6, msg::HELP_IPV6).noneg(),
            Opt::flag(None, "auto-maintenance", &ignored, msg::HELP_AUTO_MAINTENANCE),
            Opt::flag(None, "auto-gc", &ignored, msg::HELP_AUTO_MAINTENANCE),
            Opt::bool(None, "show-forced-updates", &ignored_i, msg::HELP_SHOW_FORCED),
            Opt::bool(None, "write-commit-graph", &ignored_i, msg::HELP_WRITE_COMMIT_GRAPH),
        ],
    );
    let args = parser.parse(argv)?.args;

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::config(ctx, &setup);
    let repo = LocalRepo::open(&setup, config).map_err(|e| Die::Fatal(e.to_string()))?;
    for w in &repo.warnings {
        eprintln!("warning: {w}");
    }
    let remotes = Remotes::from_config(&repo.config).map_err(|e| Die::Fatal(e.to_string()))?;

    let (name, refspecs): (Vec<u8>, Vec<String>) = match args.split_first() {
        Some((r, rest)) => (r.as_bytes().to_vec(), rest.to_vec()),
        None => {
            let branch_remote = repo
                .current_branch()
                .and_then(|b| {
                    BranchConfig::from_config(&repo.config, &b.as_bytes()["refs/heads/".len()..])
                        .ok()
                })
                .and_then(|b| b.remote);
            let name = remotes.default_remote_name(branch_remote.as_ref().map(|r| r.as_bstr()));
            (name.to_vec(), Vec::new())
        }
    };
    let remote = remotes
        .get(&name, Some(&repo.git_dir))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if args.is_empty() && !remote.is_configured() {
        return Err(Die::Fatal(rgit_fetch::messages::NO_URL.into()));
    }
    for w in &remote.warnings {
        eprintln!("warning: {w}");
    }

    let reflog_action = match std::env::var("GIT_REFLOG_ACTION") {
        Ok(a) if !a.is_empty() => a,
        _ if action.is_some() => action.unwrap_or_default().to_owned(),
        _ => {
            let mut a = String::from("fetch");
            for arg in &argv[1..] {
                a.push(' ');
                a.push_str(arg);
            }
            a
        }
    };
    let quiet_mode = quiet.get() > 0;
    let show_progress = match progress.get() {
        -1 => !quiet_mode && std::io::stderr().is_terminal(),
        p => p > 0,
    };
    let opts = FetchOptions {
        refspecs,
        tags: match tags.get() {
            1 => Some(TagOpt::All),
            0 => Some(TagOpt::NoTags),
            _ => None,
        },
        prune: match prune.get() {
            -1 => None,
            p => Some(p > 0),
        },
        force: force.get() > 0,
        dry_run: dry_run.get() > 0,
        update_head_ok: update_head_ok.get(),
        write_fetch_head: write_fetch_head.get(),
        append: append.get(),
        reflog_action,
        no_progress: !show_progress,
        server_options: server_options.borrow().clone(),
    };
    let env = SystemEnv::default();
    let connect = ConnectSettings {
        env: &env,
        config: &repo.config,
        base_env: ctx.child_env(),
        upload_pack: upload_pack
            .borrow()
            .clone()
            .or_else(|| remote.upload_pack.as_ref().map(|u| u.to_str_lossy().into_owned())),
        ip: if ipv4.get() {
            IpVersion::V4
        } else if ipv6.get() {
            IpVersion::V6
        } else {
            IpVersion::Any
        },
    };
    let result =
        rgit_fetch::fetch(&repo, &remote, &opts, &connect).map_err(|e| Die::Fatal(e.to_string()))?;

    let mut err = std::io::stderr().lock();
    for e in &result.errors {
        let _ = writeln!(err, "error: {e}");
    }
    let verbosity = if quiet_mode {
        -1
    } else {
        verbose.get()
    };
    let cli_repo = crate::repository::Repo::open(ctx, Mode::RequiredQuiet)?;
    let abbrev_len = cli_repo.default_abbrev();
    let abbrev = |id: &rgit_hash::ObjectId| cli_repo.abbrev(id, abbrev_len);
    let text = display::render(
        &result.url,
        &result.lines,
        &display::DisplayOptions {
            verbosity,
            columns: columns(),
            abbrev: &abbrev,
            abbrev_len,
        },
    );
    let _ = err.write_all(text.as_bytes());
    Ok(result.exit_code())
}
