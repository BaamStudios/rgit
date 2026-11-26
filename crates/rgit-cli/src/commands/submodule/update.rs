// SPDX-License-Identifier: MIT
//! `git submodule init` and `git submodule update`.

use std::path::Path;

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

use super::common::{self, Gitmodules, Module, Super, get_string};

pub fn init_cmd(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_INIT,
        vec![Opt::flag('q', "quiet", &quiet, msg::HELP_INIT_QUIET)],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let mut sup = Super::open(ctx, None)?;
    init(&mut sup, &args, quiet.get())?;
    Ok(0)
}

/// git's `submodule init` over the modules matching `args`. Without
/// pathspecs and with `submodule.active` set, only active submodules are
/// initialised.
pub fn init(sup: &mut Super, args: &[String], quiet: bool) -> Result<(), Die> {
    let list = sup.module_list(args)?;
    let gm = sup.gitmodules();
    let restrict = args.is_empty() && sup.repo.config.get(b"submodule.active").is_some();
    for m in &list {
        if restrict && !common::matches_active(&active_specs(sup), &m.path) {
            continue;
        }
        init_one(sup, &gm, m, quiet)?;
    }
    sup.reload_config()
}

fn active_specs(sup: &Super) -> Vec<String> {
    sup.repo
        .config
        .get_all(b"submodule.active")
        .filter_map(|e| e.value.as_ref().map(|v| String::from_utf8_lossy(v).into_owned()))
        .collect()
}

/// Initialise one submodule: copy (and resolve) its URL and update mode
/// into the repository configuration, mark it active.
fn init_one(sup: &mut Super, gm: &Gitmodules, m: &Module, quiet: bool) -> Result<(), Die> {
    let display = sup.display_path(&m.path);
    let name = gm
        .name_for_path(&m.path)
        .ok_or_else(|| Die::Fatal(msg::no_url_found(&display)))?;
    check_gitmodules_update(gm, &name)?;
    // Mark it active unless `submodule.active` already covers it.
    if !common::is_active(&sup.repo.config, &name, &m.path) {
        sup.set_config(&format!("submodule.{name}.active"), Some("true"))?;
    }
    let url_key = format!("submodule.{name}.url");
    if get_string(&sup.repo.config, &url_key).is_none() {
        let Some(url) = gm.get(&name, "url") else {
            return Err(Die::Fatal(msg::no_url_found(&display)));
        };
        let url = sup.resolve_url(&url, true)?;
        sup.set_config(&url_key, Some(&url))?;
        if !quiet {
            eprintln!("{}", msg::registered(&name, &url, &display));
        }
    }
    let update_key = format!("submodule.{name}.update");
    if get_string(&sup.repo.config, &update_key).is_none() {
        if let Some(mode) = gm.get(&name, "update") {
            sup.set_config(&update_key, Some(&mode))?;
        }
    }
    sup.reload_config()
}

/// The exit code of `update` after a failed merge, rebase or command:
/// the remaining submodules are not updated.
const STOP: i32 = 2;

/// A command as update mode is not accepted from `.gitmodules`.
fn check_gitmodules_update(gm: &Gitmodules, name: &str) -> Result<(), Die> {
    if gm.get(name, "update").is_some_and(|u| u.starts_with('!')) {
        return Err(Die::Fatal(msg::invalid_update_value(name)));
    }
    Ok(())
}

/// How a submodule is updated.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Strategy {
    Checkout,
    Rebase,
    Merge,
    None,
    Command(String),
}

impl Strategy {
    fn parse(s: &str) -> Option<Strategy> {
        Some(match s {
            "checkout" => Strategy::Checkout,
            "rebase" => Strategy::Rebase,
            "merge" => Strategy::Merge,
            "none" => Strategy::None,
            c if c.starts_with('!') => Strategy::Command(c[1..].to_owned()),
            _ => return None,
        })
    }
}

/// The options of `update`, also passed on when recursing.
struct Opts {
    quiet: bool,
    init: bool,
    remote: bool,
    no_fetch: bool,
    force: bool,
    recursive: bool,
    strategy: Option<Strategy>,
    depth: Option<String>,
    recommend_shallow: Option<bool>,
    single_branch: Option<bool>,
    progress: Option<bool>,
    require_init: bool,
    reference: Vec<String>,
    dissociate: bool,
    /// `--ref-format` for submodules that are cloned.
    ref_format: Option<String>,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let force = Cell::new(false);
    let init = Cell::new(false);
    let remote = Cell::new(false);
    let recursive = Cell::new(false);
    let no_fetch = Cell::new(0);
    let strategy = Cell::new(0);
    let reference = RefCell::new(Vec::<String>::new());
    let ref_format = RefCell::new(None::<String>);
    let dissociate = Cell::new(false);
    let depth = RefCell::new(None::<String>);
    let jobs = Cell::new(-1i32);
    let recommend_shallow = Cell::new(-1);
    let quiet = Cell::new(false);
    let progress = Cell::new(-1);
    let require_init = Cell::new(false);
    let single_branch = Cell::new(-1);
    let filter = RefCell::new(None::<String>);
    let super_prefix = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_UPDATE,
        vec![
            Opt::flag('f', "force", &force, msg::HELP_UPDATE_FORCE),
            Opt::flag('i', "init", &init, msg::HELP_UPDATE_INIT),
            Opt::flag(None, "remote", &remote, msg::HELP_UPDATE_REMOTE),
            Opt::flag(None, "recursive", &recursive, msg::HELP_UPDATE_RECURSIVE),
            Opt::set_int('N', "no-fetch", &no_fetch, msg::HELP_NO_FETCH, 1).noneg(),
            Opt::set_int(None, "fetch", &no_fetch, msg::HELP_FETCH, 0).noneg(),
            Opt::set_int(None, "checkout", &strategy, msg::HELP_CHECKOUT, 1),
            Opt::set_int('m', "merge", &strategy, msg::HELP_MERGE, 2),
            Opt::set_int('r', "rebase", &strategy, msg::HELP_REBASE, 3),
            Opt::string_list(None, "reference", &reference, "repo", msg::HELP_REFERENCE),
            Opt::string(None, "ref-format", &ref_format, "format", msg::HELP_REF_FORMAT),
            Opt::flag(None, "dissociate", &dissociate, msg::HELP_UPDATE_DISSOCIATE),
            Opt::string(None, "depth", &depth, "n", msg::HELP_UPDATE_DEPTH),
            Opt::integer('j', "jobs", &jobs, msg::HELP_JOBS),
            Opt::bool(None, "recommend-shallow", &recommend_shallow, msg::HELP_RECOMMEND_SHALLOW),
            Opt::flag('q', "quiet", &quiet, msg::HELP_UPDATE_QUIET),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::flag(None, "require-init", &require_init, msg::HELP_REQUIRE_INIT),
            Opt::bool(None, "single-branch", &single_branch, msg::HELP_SINGLE_BRANCH),
            Opt::string(None, "filter", &filter, "args", msg::HELP_FILTER),
            Opt::string(None, "super-prefix", &super_prefix, "path", "").hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if filter.borrow().is_some() && !(init.get() || require_init.get()) {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    if filter.borrow().is_some() {
        return Err(Die::Fatal(crate::messages::clone::unsupported("--filter")));
    }
    let tri = |c: &Cell<i32>| (c.get() >= 0).then(|| c.get() > 0);
    let opts = Opts {
        quiet: quiet.get(),
        init: init.get() || require_init.get(),
        remote: remote.get(),
        no_fetch: no_fetch.get() == 1,
        force: force.get(),
        recursive: recursive.get(),
        strategy: match strategy.get() {
            1 => Some(Strategy::Checkout),
            2 => Some(Strategy::Merge),
            3 => Some(Strategy::Rebase),
            _ => None,
        },
        depth: depth.into_inner(),
        recommend_shallow: tri(&recommend_shallow),
        single_branch: tri(&single_branch),
        progress: tri(&progress),
        require_init: require_init.get(),
        ref_format: ref_format.borrow().clone(),
        reference: reference.into_inner(),
        dissociate: dissociate.get(),
    };
    let mut sup = Super::open(ctx, super_prefix.borrow().as_deref())?;
    // The submodules are updated one after the other; the number of
    // parallel jobs is only reported.
    let jobs = match jobs.get() {
        j if j >= 0 => j as i64,
        _ => {
            let gm = sup.gitmodules();
            sup.repo
                .config
                .int(b"submodule.fetchjobs")
                .ok()
                .flatten()
                .or_else(|| gm.config.int(b"submodule.fetchjobs").ok().flatten())
                .unwrap_or(1)
        }
    };
    let jobs = if jobs == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get() as i64)
    } else {
        jobs
    };
    if rgit_trace::TRACE.enabled() {
        rgit_trace::TRACE.write_line(
            rgit_trace::Location::caller(),
            msg::preparing_tasks(jobs).as_bytes(),
        );
    }
    update(&mut sup, &args, &opts)
}

/// A submodule to update after the clone phase.
struct Todo {
    module: Module,
    name: String,
    display: String,
    strategy: Strategy,
    just_cloned: bool,
}

fn update(sup: &mut Super, args: &[String], opts: &Opts) -> CmdResult {
    if opts.init {
        init(sup, args, opts.quiet)?;
    }
    let list = sup.module_list(args)?;
    let gm = sup.gitmodules();
    // First every missing submodule is cloned (a failed clone is retried
    // once, after the others), then each one is updated.
    let mut todo: Vec<Todo> = Vec::new();
    let mut retry: Vec<usize> = Vec::new();
    for m in &list {
        let display = sup.display_path(&m.path);
        if m.unmerged {
            eprintln!("{}", msg::skipping_unmerged(&display));
            continue;
        }
        let name = gm.name_for_path(&m.path);
        if let Some(name) = &name {
            check_gitmodules_update(&gm, name)?;
        }
        let Some(name) = name.filter(|n| sup.is_active(n, &m.path)) else {
            // Only submodules asked for by path are reported.
            if !args.is_empty() {
                eprintln!("{}", msg::not_initialized(&display));
                eprintln!("{}", msg::MAYBE_INIT);
            }
            continue;
        };
        let configured = get_string(&sup.repo.config, &format!("submodule.{name}.update"));
        let strategy = match (&opts.strategy, configured) {
            (Some(s), _) => s.clone(),
            (None, Some(c)) => Strategy::parse(&c)
                .ok_or_else(|| Die::Fatal(msg::invalid_update_mode(&c, &display)))?,
            (None, None) => Strategy::Checkout,
        };
        if strategy == Strategy::None {
            eprintln!("{}", msg::skipping(&display));
            continue;
        }
        let mut t = Todo {
            module: m.clone(),
            name,
            display,
            strategy,
            just_cloned: false,
        };
        if !common::is_populated(&sup.work_tree.join(&m.path)) {
            if let Err(e) = clone_module(sup, &gm, &t, opts) {
                report_clone_failure(e);
                eprintln!("{}", msg::retry_scheduled(&t.module.path));
                retry.push(todo.len());
            } else {
                t.just_cloned = true;
            }
        }
        todo.push(t);
    }
    for i in retry {
        if let Err(e) = clone_module(sup, &gm, &todo[i], opts) {
            report_clone_failure(e);
            eprintln!("{}", msg::second_failure(&todo[i].module.path));
            return Ok(1);
        }
        todo[i].just_cloned = true;
    }

    let mut code = 0;
    for t in &todo {
        // A failing submodule does not stop the others.
        let mut result = update_one(sup, &gm, t, opts);
        let populated = common::is_populated(&sup.work_tree.join(&t.module.path));
        if result.is_ok() && opts.recursive && populated {
            result = recurse(sup, &t.module, &t.display, opts);
        }
        match result {
            Ok(()) => {}
            // A failed merge or rebase (here or below) stops at once.
            Err(Die::Exit(STOP)) => return Ok(STOP),
            Err(Die::Error(text, STOP)) => {
                eprintln!("fatal: {text}");
                return Ok(STOP);
            }
            Err(Die::Fatal(text)) => {
                eprintln!("fatal: {text}");
                code = 1;
            }
            Err(e) => {
                e.emit();
                code = 1;
            }
        }
    }
    Ok(code)
}

fn report_clone_failure(e: Die) {
    match e {
        Die::Error(text, _) => eprintln!("error: {text}"),
        Die::Fatal(text) => eprintln!("fatal: {text}"),
        e => {
            e.emit();
        }
    }
}

/// Clone a missing submodule.
fn clone_module(sup: &Super, gm: &Gitmodules, t: &Todo, opts: &Opts) -> Result<(), Die> {
    let name = t.name.as_str();
    check_no_symlink(sup, &t.module.path)?;
    let url = match get_string(&sup.repo.config, &format!("submodule.{name}.url")) {
        Some(u) => u,
        None => match gm.get(name, "url") {
            Some(u) => sup.resolve_url(&u, false)?,
            None => return Err(Die::Fatal(msg::cannot_clone_without_url(name))),
        },
    };
    let shallow = opts
        .recommend_shallow
        .unwrap_or_else(|| gm.get(name, "shallow").as_deref() == Some("true"));
    let depth = opts
        .depth
        .clone()
        .or_else(|| shallow.then(|| "1".to_owned()));
    let branch = gm.get(name, "branch");
    clone_into(
        sup,
        CloneArgs {
            path: &t.module.path,
            name,
            url: &url,
            depth: depth.as_deref(),
            quiet: opts.quiet,
            progress: opts.progress,
            single_branch: opts.single_branch,
            branch: if opts.single_branch == Some(true) {
                branch.as_deref()
            } else {
                None
            },
            require_init: opts.require_init,
            reference: &opts.reference,
            dissociate: opts.dissociate,
            ref_format: opts.ref_format.as_deref(),
        },
    )
}

/// A submodule is never created behind a symbolic link: neither a leading
/// directory of its path nor the path itself may be one.
fn check_no_symlink(sup: &Super, path: &str) -> Result<(), Die> {
    let parts: Vec<&str> = path.split('/').collect();
    for i in 1..=parts.len() {
        let p = parts[..i].join("/");
        if std::fs::symlink_metadata(sup.work_tree.join(&p)).is_ok_and(|m| m.file_type().is_symlink()) {
            let text = if i == parts.len() {
                msg::path_is_symlink(path)
            } else {
                msg::leading_symlink(&p, path)
            };
            return Err(Die::Error(text, 1));
        }
    }
    Ok(())
}

/// Update one (cloned) submodule.
fn update_one(sup: &mut Super, gm: &Gitmodules, t: &Todo, opts: &Opts) -> Result<(), Die> {
    let (m, name, display, strategy) = (&t.module, t.name.as_str(), t.display.as_str(), &t.strategy);
    let just_cloned = t.just_cloned;
    let dir = sup.work_tree.join(&m.path);
    let mut target = m.oid;
    let git_dir = common::sub_git_dir(&dir).ok_or_else(|| Die::Fatal(msg::path_not_submodule(display)))?;
    if opts.remote {
        let remote = common::sub_default_remote(&git_dir);
        if !opts.no_fetch {
            let mut a = vec!["fetch"];
            if opts.quiet {
                a.push("--quiet");
            }
            a.push(&remote);
            let mut cmd = sup.sub_git(&dir, &a)?;
            if common::run_status(&mut cmd)? != 0 {
                return Err(Die::Fatal(msg::unable_to_fetch(display)));
            }
        }
        let branch = remote_branch(sup, gm, name);
        let rev = match branch {
            Some(b) => format!("refs/remotes/{remote}/{b}"),
            None => format!("refs/remotes/{remote}/HEAD"),
        };
        let mut cmd = sup.sub_git(&dir, &["rev-parse", "--verify", &format!("{rev}^{{commit}}")])?;
        let out = common::capture(&mut cmd)
            .ok_or_else(|| Die::Fatal(msg::no_current_revision(&remote, display)))?;
        target = rgit_hash::ObjectId::from_hex(sup.hash(), out.trim().as_bytes())
            .map_err(|_| Die::Fatal(msg::no_current_revision(&remote, display)))?;
    }
    let head = if just_cloned {
        None
    } else {
        common::sub_head(&dir, sup.hash())
    };
    if head == Some(target) && !opts.force {
        return Ok(());
    }
    let hex = target.to_hex();
    // Also after a clone: a reused git directory may lack the commit.
    if !opts.no_fetch && !has_commit(sup, &dir, &hex)? {
        let mut cmd = sup.sub_git(&dir, if opts.quiet { &["fetch", "--quiet"][..] } else { &["fetch"] })?;
        if common::run_status(&mut cmd)? != 0 {
            return Err(Die::Fatal(msg::unable_to_fetch(display)));
        }
        if !has_commit(sup, &dir, &hex)? {
            let remote = common::sub_default_remote(&git_dir);
            let mut cmd = sup.sub_git(&dir, &["fetch", "--quiet", &remote, &hex])?;
            if common::run_status(&mut cmd)? != 0 || !has_commit(sup, &dir, &hex)? {
                return Err(Die::Fatal(msg::fetched_but_missing(display, &hex)));
            }
        }
    }
    let quiet = opts.quiet;
    // A new clone is always checked out.
    let strategy = if just_cloned && matches!(strategy, Strategy::Merge | Strategy::Rebase) {
        &Strategy::Checkout
    } else {
        strategy
    };
    let (ok, done, fail) = match strategy {
        Strategy::Checkout => {
            let mut a = vec!["checkout", "-q"];
            if opts.force || just_cloned {
                a.push("-f");
            }
            a.push(&hex);
            let mut cmd = sup.sub_git(&dir, &a)?;
            (
                common::run_status(&mut cmd)? == 0,
                msg::checked_out(display, &hex),
                msg::unable_to_checkout(&hex, display),
            )
        }
        Strategy::Rebase => {
            let mut a = vec!["rebase"];
            if quiet {
                a.push("--quiet");
            }
            a.push(&hex);
            let mut cmd = sup.sub_git(&dir, &a)?;
            (
                common::run_status(&mut cmd)? == 0,
                msg::rebased(display, &hex),
                msg::unable_to_rebase(&hex, display),
            )
        }
        Strategy::Merge => {
            let mut a = vec!["merge"];
            if quiet {
                a.push("--quiet");
            }
            a.push(&hex);
            let mut cmd = sup.sub_git(&dir, &a)?;
            (
                common::run_status(&mut cmd)? == 0,
                msg::merged(display, &hex),
                msg::unable_to_merge(&hex, display),
            )
        }
        Strategy::Command(c) => {
            let mut cmd = std::process::Command::new(rgit_transport::command::SHELL_PATH);
            sup.sub_env(&mut cmd);
            cmd.arg("-c")
                .arg(format!("{c} \"$@\""))
                .arg(c)
                .arg(&hex)
                .current_dir(&dir);
            (
                common::run_status(&mut cmd)? == 0,
                msg::custom_done(display, c, &hex),
                msg::custom_failed(c, &hex, display),
            )
        }
        Strategy::None => return Ok(()),
    };
    if !ok {
        return Err(match strategy {
            Strategy::Checkout => Die::Fatal(fail),
            _ => Die::Error(fail, STOP),
        });
    }
    if !quiet {
        println!("{done}");
    }
    Ok(())
}

/// `submodule.<name>.branch` from the configuration or `.gitmodules`; `.`
/// is the superproject's current branch.
fn remote_branch(sup: &Super, gm: &Gitmodules, name: &str) -> Option<String> {
    let key = format!("submodule.{name}.branch");
    let b = get_string(&sup.repo.config, &key).or_else(|| gm.get(name, "branch"))?;
    if b == "." {
        return sup
            .repo
            .refs
            .head_target()
            .and_then(|t| t.strip_prefix("refs/heads/").map(str::to_owned));
    }
    Some(b)
}

fn has_commit(sup: &Super, dir: &Path, hex: &str) -> Result<bool, Die> {
    let mut cmd = sup.sub_git(dir, &["rev-parse", "-q", "--verify", &format!("{hex}^{{commit}}")])?;
    Ok(common::capture(&mut cmd).is_some())
}

fn recurse(sup: &Super, m: &Module, display: &str, opts: &Opts) -> Result<(), Die> {
    let mut a: Vec<String> = vec!["submodule".into(), "update".into(), "--recursive".into()];
    let sp = format!("{display}/");
    a.push(format!("--super-prefix={sp}"));
    if opts.quiet {
        a.push("--quiet".into());
    }
    if opts.init {
        a.push("--init".into());
    }
    if opts.remote {
        a.push("--remote".into());
    }
    if opts.no_fetch {
        a.push("--no-fetch".into());
    }
    if opts.force {
        a.push("--force".into());
    }
    match &opts.strategy {
        Some(Strategy::Checkout) => a.push("--checkout".into()),
        Some(Strategy::Merge) => a.push("--merge".into()),
        Some(Strategy::Rebase) => a.push("--rebase".into()),
        _ => {}
    }
    if let Some(d) = &opts.depth {
        a.push(format!("--depth={d}"));
    }
    for r in &opts.reference {
        a.push(format!("--reference={r}"));
    }
    if opts.dissociate {
        a.push("--dissociate".into());
    }
    if let Some(r) = opts.recommend_shallow {
        a.push(if r { "--recommend-shallow" } else { "--no-recommend-shallow" }.into());
    }
    if let Some(p) = opts.progress {
        a.push(if p { "--progress" } else { "--no-progress" }.into());
    }
    if let Some(s) = opts.single_branch {
        a.push(if s { "--single-branch" } else { "--no-single-branch" }.into());
    }
    let refs: Vec<&str> = a.iter().map(String::as_str).collect();
    let mut cmd = sup.sub_git(Path::new(&m.path), &refs)?;
    match common::run_status(&mut cmd)? {
        0 => Ok(()),
        STOP => Err(Die::Error(msg::failed_recurse(display), STOP)),
        _ => Err(Die::Fatal(msg::failed_recurse(display))),
    }
}

/// `submodule.alternateLocation=superproject`: for each alternate of the
/// superproject, the repository of this submodule below its `modules/`.
/// One that does not exist is handled by `submodule.alternateErrorStrategy`
/// (`die`, the default; `info`; `ignore`).
/// Returns the strategy too when `superproject` is in effect.
fn superproject_alternates(sup: &Super, name: &str) -> Result<(Vec<String>, Option<String>), Die> {
    let location = common::get_string(&sup.repo.config, "submodule.alternatelocation");
    match location.as_deref() {
        None | Some("no") => return Ok((Vec::new(), None)),
        Some("superproject") => {}
        Some(v) => {
            return Err(Die::Fatal(msg::value_not_recognized(v, "submodule.alternateLocation")));
        }
    }
    let strategy = common::get_string(&sup.repo.config, "submodule.alternateerrorstrategy")
        .unwrap_or_else(|| "die".to_owned());
    if !matches!(strategy.as_str(), "die" | "info" | "ignore") {
        return Err(Die::Fatal(msg::value_not_recognized(
            &strategy,
            "submodule.alternateErrorStrategy",
        )));
    }
    let objects = sup.common_dir.join("objects");
    let list = std::fs::read_to_string(objects.join("info/alternates")).unwrap_or_default();
    let mut out = Vec::new();
    for line in list.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let alt = objects.join(line);
        let alt = alt.to_string_lossy();
        let base = alt.strip_suffix("objects").unwrap_or(&alt);
        let base = if base.ends_with('/') {
            base.to_owned()
        } else {
            format!("{base}/")
        };
        let candidate = format!("{base}modules/{name}/");
        if Path::new(&candidate).is_dir() {
            out.push(candidate);
            continue;
        }
        let err = msg::cannot_add_alternate(name, &candidate);
        match strategy.as_str() {
            "die" => {
                sup.ctx
                    .advisor(&sup.repo.config)?
                    .advise_without_footer(
                        rgit_messages::Advice::SubmoduleAlternateErrorStrategyDie,
                        msg::ALTERNATE_ERROR_STRATEGY_HINT,
                    )
                    .map_err(Die::Fatal)?;
                return Err(Die::Fatal(err));
            }
            "info" => eprintln!("{err}"),
            _ => {}
        }
    }
    Ok((out, Some(strategy)))
}

/// What [`clone_into`] needs.
pub struct CloneArgs<'a> {
    pub path: &'a str,
    pub name: &'a str,
    pub url: &'a str,
    pub depth: Option<&'a str>,
    pub quiet: bool,
    pub progress: Option<bool>,
    pub single_branch: Option<bool>,
    pub branch: Option<&'a str>,
    pub require_init: bool,
    pub reference: &'a [String],
    pub dissociate: bool,
    /// The reference storage format of the new repository.
    pub ref_format: Option<&'a str>,
}

/// Clone a submodule into its path, its git directory below
/// `modules/<name>`; an existing git directory there is reused.
pub fn clone_into(sup: &Super, a: CloneArgs<'_>) -> Result<(), Die> {
    let dir = sup.work_tree.join(a.path);
    let git_dir = sup.module_git_dir(a.name);
    if git_dir.join("HEAD").exists() {
        std::fs::create_dir_all(&dir).map_err(|e| Die::Fatal(e.to_string()))?;
        return common::connect(&dir, &git_dir);
    }
    let dir_s = dir.to_string_lossy().into_owned();
    if a.require_init
        && std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some())
    {
        return Err(Die::Fatal(crate::messages::clone::destination_exists(&dir_s)));
    }
    let mut args: Vec<String> = vec!["clone".into(), "--no-checkout".into()];
    if a.quiet {
        args.push("--quiet".into());
    }
    match a.progress {
        Some(true) => args.push("--progress".into()),
        Some(false) => args.push("--no-progress".into()),
        None => {}
    }
    if let Some(d) = a.depth {
        args.push(format!("--depth={d}"));
    }
    match a.single_branch {
        Some(true) => args.push("--single-branch".into()),
        Some(false) => args.push("--no-single-branch".into()),
        None => {}
    }
    if let Some(b) = a.branch {
        args.push(format!("--branch={b}"));
    }
    for r in a.reference {
        args.push(format!("--reference={r}"));
    }
    let (borrowed, strategy) = superproject_alternates(sup, a.name)?;
    for r in borrowed {
        args.push(format!("--reference={r}"));
    }
    if a.dissociate {
        args.push("--dissociate".into());
    }
    if let Some(f) = a.ref_format {
        args.push(format!("--ref-format={f}"));
    }
    args.push("--".into());
    args.push(a.url.to_owned());
    args.push(dir_s.clone());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut cmd = sup.sub_git(Path::new(""), &refs)?;
    if common::run_status(&mut cmd)? != 0 {
        return Err(Die::Fatal(msg::clone_failed(a.url, &dir_s)));
    }
    common::relocate_git_dir(&dir, &git_dir)?;
    // The submodule passes the settings on to its own submodules.
    if let Some(strategy) = strategy {
        let file = git_dir.join("config");
        let file = file.to_string_lossy();
        for (key, value) in [
            ("submodule.alternateLocation", "superproject"),
            ("submodule.alternateErrorStrategy", strategy.as_str()),
        ] {
            let mut cmd = sup.sub_git(Path::new(""), &["config", "--file", &file, key, value])?;
            common::run_status(&mut cmd)?;
        }
    }
    Ok(())
}
