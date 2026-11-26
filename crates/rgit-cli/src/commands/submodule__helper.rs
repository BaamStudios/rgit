// SPDX-License-Identifier: MIT
//! `git submodule--helper`: the internal helper of `git submodule`. Only
//! `get-default-remote <path>` and `create-branch` (for `branch
//! --recurse-submodules`) are provided; the subcommands of `git
//! submodule` itself are built in.

use crate::commands::submodule::common::{self, Super};
use crate::messages::submodule_cmd as msg;
use crate::prelude::*;
use crate::tracking::Track;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        msg::USAGE_HELPER,
        vec![
            Opt::subcommand("get-default-remote"),
            Opt::subcommand("create-branch"),
        ],
    );
    let parsed = parser.parse(argv)?;
    drop(parser);
    if parsed.args.first().map(String::as_str) == Some("create-branch") {
        return create_branch(ctx, &parsed.args);
    }
    let mut parser = Parser::new(msg::USAGE_DEFAULT_REMOTE, vec![]);
    let rest = parser.parse(&parsed.args)?.args;
    if rest.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let sup = Super::open(ctx, None)?;
    let path = common::prefixed_path(&sup, &rest[0])?;
    let dir = sup.work_tree.join(&path);
    let git_dir = common::sub_git_dir(&dir)
        .filter(|_| common::is_populated(&dir) && !path.is_empty())
        .ok_or_else(|| Die::Fatal(msg::path_not_submodule(&rest[0])))?;
    println!("{}", common::sub_default_remote(&git_dir));
    Ok(0)
}

/// `create-branch [--dry-run] [--force] [--quiet] [--track[=...]|--no-track]
/// <name> <start-oid> <start-name>`: create branch `<name>` at
/// `<start-oid>` in this (submodule) repository and its submodules, with
/// tracking derived from the superproject's `<start-name>`.
fn create_branch(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(0);
    let force = Cell::new(false);
    let quiet = Cell::new(0);
    let create_reflog = Cell::new(false);
    let track = Cell::new(Track::Unspecified);
    let mut parser = Parser::new(
        msg::USAGE_CREATE_BRANCH,
        vec![
            Opt::quiet(&quiet, msg::HELP_CB_QUIET),
            Opt::flag('f', "force", &force, msg::HELP_CB_FORCE),
            Opt::flag(None, "create-reflog", &create_reflog, msg::HELP_CB_CREATE_REFLOG),
            Track::option_with_help(&track, msg::HELP_CB_TRACK),
            Opt::dry_run(&dry_run, msg::HELP_CB_DRY_RUN),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 3 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let (name, start_hex, start_name) = (&args[0], &args[1], &args[2]);
    let repo = crate::repository::Repo::open(ctx, crate::repo::Mode::Required)?;
    if name == "HEAD" || rgit_refs::name::check_branch_name(name.as_bytes()).is_err() {
        return Err(Die::Fatal(crate::messages::branch::not_valid_branch_name(name)));
    }
    let store = crate::refs_support::ref_store(&repo.setup, &repo.config);
    {
        use rgit_refs::RefStore as _;
        if !force.get() && store.read_raw(&format!("refs/heads/{name}")).ok().flatten().is_some() {
            return Err(Die::Fatal(crate::messages::branch::already_exists(name)));
        }
    }
    let oid = rgit_hash::ObjectId::from_hex(repo.hash, start_hex.as_bytes())
        .ok()
        .filter(|id| matches!(repo.read_object(id), Some(rgit_object::Object::Commit(_))))
        .ok_or_else(|| Die::Fatal(crate::messages::branch::not_valid_object_name(start_hex)))?;
    let subs = crate::branch_submodules::submodules(&repo, &oid)?;
    let c = crate::branch_submodules::Create {
        name,
        start_name,
        track: track.get(),
        force: force.get(),
        quiet: quiet.get() > 0,
    };
    if dry_run.get() > 0 {
        return crate::branch_submodules::run(&subs, &c, true).map(|()| 0);
    }
    // The branch itself (`branch` does the checks once more).
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["branch", "--no-recurse-submodules", "--no-track", "--quiet"]);
    if force.get() {
        cmd.arg("--force");
    }
    if create_reflog.get() {
        cmd.arg("--create-reflog");
    }
    cmd.args([name.as_str(), start_hex.as_str()]);
    if !cmd.status().is_ok_and(|s| s.success()) {
        return Ok(128);
    }
    let quiet = quiet.get() > 0;
    if !quiet {
        crate::argv::outln(&crate::messages::branch::creating_branch(name));
    }
    set_tracking(&repo, name, start_name, track.get(), quiet)?;
    crate::branch_submodules::run(&subs, &c, false)?;
    Ok(0)
}

/// The tracking configuration of the new branch `name` from the
/// superproject's start point `start` (a full ref name): a local branch is
/// tracked with `--track` or `branch.autoSetupMerge=always`, a
/// remote-tracking ref (even one this repository does not have) through
/// the remote whose fetch refspec maps it, unless `--no-track` or
/// `branch.autoSetupMerge=false`; `--track=inherit` copies the tracking of
/// the branch `start` names.
fn set_tracking(
    repo: &crate::repository::Repo,
    name: &str,
    start: &str,
    track: Track,
    quiet: bool,
) -> Result<(), Die> {
    let report = |upstream: &str| {
        if !quiet {
            crate::argv::outln(&crate::messages::tracking::set_up_to_track(name, upstream, false));
        }
    };
    let auto = repo
        .config
        .get("branch.autosetupmerge")
        .and_then(|e| e.value.as_ref().map(|v| v.to_string()))
        .unwrap_or_else(|| "true".to_owned());
    let auto_on = rgit_config::value::parse_bool(Some(auto.as_bytes())).unwrap_or(true);
    let config = repo.setup.common_dir.join("config");
    let set = |key: &str, value: &str| common::set_in_file(&config, key, Some(value));
    match track {
        Track::Never => Ok(()),
        Track::Inherit => {
            let from = start.strip_prefix("refs/heads/").unwrap_or(start);
            let remote = repo
                .config
                .get(crate::tracking::branch_key(from, "remote"))
                .and_then(|e| e.value.as_ref().map(|v| rgit_bstr::from_bytes(v).into_owned()));
            let merge = repo
                .config
                .get(crate::tracking::branch_key(from, "merge"))
                .and_then(|e| e.value.as_ref().map(|v| rgit_bstr::from_bytes(v).into_owned()));
            if let (Some(r), Some(m)) = (remote, merge) {
                set(&format!("branch.{name}.remote"), &r)?;
                set(&format!("branch.{name}.merge"), &m)?;
                let short = m.strip_prefix("refs/heads/").unwrap_or(&m);
                report(&if r == "." { short.to_owned() } else { format!("{r}/{short}") });
            }
            Ok(())
        }
        Track::Direct | Track::Unspecified => {
            if start.starts_with("refs/heads/") {
                if track == Track::Direct || auto == "always" {
                    set(&format!("branch.{name}.remote"), ".")?;
                    set(&format!("branch.{name}.merge"), start)?;
                    report(start.strip_prefix("refs/heads/").unwrap_or(start));
                }
                return Ok(());
            }
            if !start.starts_with("refs/remotes/") || (track == Track::Unspecified && !auto_on) {
                return Ok(());
            }
            let Ok(remotes) = rgit_remote::Remotes::from_config(&repo.config) else {
                return Ok(());
            };
            let names: Vec<String> = remotes
                .names()
                .map(|n| rgit_bstr::from_bytes(n).into_owned())
                .collect();
            for n in names {
                let Ok(r) = remotes.get(&rgit_bstr::to_bytes(&n), Some(&repo.setup.git_dir)) else {
                    continue;
                };
                if let Some(src) = r.fetch.map_dst(&rgit_bstr::to_bytes(start)) {
                    let src = rgit_bstr::from_vec(src.into());
                    set(&format!("branch.{name}.remote"), &n)?;
                    set(&format!("branch.{name}.merge"), &src)?;
                    report(&format!("{n}/{}", src.strip_prefix("refs/heads/").unwrap_or(&src)));
                    break;
                }
            }
            Ok(())
        }
    }
}
