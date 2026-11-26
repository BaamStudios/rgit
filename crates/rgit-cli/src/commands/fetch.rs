// SPDX-License-Identifier: MIT
//! `git fetch` (Documentation/git-fetch.adoc, fetch-options.adoc,
//! config/fetch.adoc) on top of `rgit-fetch`.
//!
//! One remote or URL is fetched directly. `--all`, `--multiple`, a remote
//! group (`remotes.<group>`) and `fetch.all` fetch several remotes in
//! turn, each like `git fetch --append <name>` after a `Fetching <name>`
//! line, and run the automatic maintenance once at the end.
//!
//! Supported: `[<repository> [<refspec>...]]`, `--stdin`, `-v`, `-q`,
//! `-a`, `-f`, `-t` / `--no-tags` / `-n`, `-p` / `--prune`, `-P` /
//! `--prune-tags`, `--atomic`, `--refmap`, `--refetch`, `--set-upstream`,
//! `--dry-run`, `--[no-]write-fetch-head`, `-u`, `--upload-pack`, `-o`,
//! `-4` / `-6`, `--[no-]progress`, `--porcelain`, `fetch.output`,
//! `--[no-]auto-maintenance` / `--[no-]auto-gc`, `-j` / `fetch.parallel`
//! (the remotes are still fetched one after another),
//! `--[no-]show-forced-updates` / `fetch.showForcedUpdates`. Accepted and
//! ignored: `-k`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use bstr::ByteSlice;
use rgit_config::SystemEnv;
use rgit_fetch::{ConnectSettings, FetchOptions, LocalRepo, display};
use rgit_remote::{BranchConfig, Remotes, TagOpt};
use rgit_submodule::FetchRecurse;
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

/// The parsed command line.
#[derive(Default, Clone)]
struct Args {
    /// `--[no-]write-commit-graph` (-1: `fetch.writeCommitGraph`).
    write_commit_graph: i32,
    verbose: i32,
    quiet: i32,
    all: Option<bool>,
    multiple: bool,
    force: bool,
    tags: i32,
    prune: i32,
    prune_tags: i32,
    dry_run: bool,
    porcelain: bool,
    write_fetch_head: bool,
    /// `--filter` specs (several combine); `--no-filter` empties them and
    /// sets `no_filter`.
    filters: Vec<String>,
    no_filter: bool,
    append: bool,
    atomic: bool,
    keep: bool,
    update_head_ok: bool,
    progress: i32,
    ipv4: bool,
    ipv6: bool,
    auto_maintenance: bool,
    set_upstream: bool,
    refetch: bool,
    /// `--negotiate-only`.
    negotiate_only: bool,
    /// `--prefetch`.
    prefetch: bool,
    stdin: bool,
    jobs: Option<i64>,
    upload_pack: Option<String>,
    server_options: Vec<String>,
    refmap: Option<Vec<String>>,
    negotiation_restrict: Vec<String>,
    negotiation_include: Vec<String>,
    depth: Option<String>,
    shallow_since: Option<String>,
    shallow_exclude: Vec<String>,
    deepen: Option<String>,
    unshallow: bool,
    update_shallow: bool,
    show_forced_updates: i32,
    /// `--[no-]recurse-submodules[=<mode>]`.
    recurse: Option<FetchRecurse>,
    /// `--recurse-submodules-default=<mode>` (internal).
    recurse_default: Option<FetchRecurse>,
    /// `--submodule-prefix=<path>/` (internal).
    submodule_prefix: Option<String>,
}

impl Args {
    /// The options a fetch of one of several remotes gets (git passes them
    /// to its child processes).
    fn child_options(&self) -> Vec<String> {
        let mut o: Vec<String> = ["--append", "--no-auto-gc", "--no-write-commit-graph"]
            .map(String::from)
            .into();
        for _ in 0..self.verbose {
            o.push("-v".into());
        }
        for _ in 0..self.quiet {
            o.push("-q".into());
        }
        let tri = |o: &mut Vec<String>, v: i32, yes: &str, no: &str| match v {
            1 => o.push(yes.into()),
            0 => o.push(no.into()),
            _ => {}
        };
        tri(&mut o, self.tags, "--tags", "--no-tags");
        tri(&mut o, self.prune, "--prune", "--no-prune");
        tri(&mut o, self.prune_tags, "--prune-tags", "--no-prune-tags");
        tri(&mut o, self.progress, "--progress", "--no-progress");
        tri(
            &mut o,
            self.show_forced_updates,
            "--show-forced-updates",
            "--no-show-forced-updates",
        );
        let flags = [
            (self.force, "--force"),
            (self.dry_run, "--dry-run"),
            (!self.write_fetch_head, "--no-write-fetch-head"),
            (self.atomic, "--atomic"),
            (self.keep, "--keep"),
            (self.update_head_ok, "--update-head-ok"),
            (self.ipv4, "--ipv4"),
            (self.ipv6, "--ipv6"),
            (self.refetch, "--refetch"),
            (self.porcelain, "--porcelain"),
        ];
        o.extend(flags.iter().filter(|(on, _)| *on).map(|(_, f)| (*f).to_owned()));
        if let Some(u) = &self.upload_pack {
            o.push(format!("--upload-pack={u}"));
        }
        for s in &self.server_options {
            o.push(format!("--server-option={s}"));
        }
        if self.no_filter {
            o.push("--no-filter".to_owned());
        }
        for f in &self.filters {
            o.push(format!("--filter={f}"));
        }
        for r in self.refmap.iter().flatten() {
            o.push(format!("--refmap={r}"));
        }
        for r in &self.negotiation_restrict {
            o.push(format!("--negotiation-restrict={r}"));
        }
        for r in &self.negotiation_include {
            o.push(format!("--negotiation-include={r}"));
        }
        if let Some(d) = &self.depth {
            o.push(format!("--depth={d}"));
        }
        if let Some(d) = &self.shallow_since {
            o.push(format!("--shallow-since={d}"));
        }
        for r in &self.shallow_exclude {
            o.push(format!("--shallow-exclude={r}"));
        }
        if let Some(d) = &self.deepen {
            o.push(format!("--deepen={d}"));
        }
        if self.unshallow {
            o.push("--unshallow".into());
        }
        if self.update_shallow {
            o.push("--update-shallow".into());
        }
        if let Some(m) = self.recurse {
            o.push(crate::fetch_submodules::option_value(m));
        }
        o
    }

    /// The options a submodule's fetch gets.
    fn submodule_options(&self) -> Vec<String> {
        let mut o: Vec<String> = Vec::new();
        for _ in 0..self.verbose {
            o.push("-v".into());
        }
        for _ in 0..self.quiet {
            o.push("-q".into());
        }
        o.push(if self.prune == 1 { "--prune" } else { "--no-prune" }.into());
        o.push(if self.prune_tags == 1 { "--prune-tags" } else { "--no-prune-tags" }.into());
        match self.progress {
            1 => o.push("--progress".into()),
            0 => o.push("--no-progress".into()),
            _ => {}
        }
        if self.dry_run {
            o.push("--dry-run".into());
        }
        if !self.write_fetch_head {
            o.push("--no-write-fetch-head".into());
        }
        if self.force {
            o.push("--force".into());
        }
        if self.update_head_ok {
            o.push("--update-head-ok".into());
        }
        if let Some(j) = self.jobs {
            o.push(format!("--jobs={j}"));
        }
        o
    }
}

fn parse(argv: &[String]) -> Result<(Args, Vec<String>), Die> {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let all = Cell::new(-1);
    let multiple = Cell::new(false);
    let force = Cell::new(0);
    let tags = Cell::new(-1);
    let prune = Cell::new(-1);
    let prune_tags = Cell::new(-1);
    let dry_run = Cell::new(0);
    let porcelain = Cell::new(false);
    let write_fetch_head = Cell::new(true);
    let append = Cell::new(false);
    let atomic = Cell::new(false);
    let keep = Cell::new(false);
    let update_head_ok = Cell::new(false);
    let progress = Cell::new(-1);
    let ipv4 = Cell::new(false);
    let ipv6 = Cell::new(false);
    let auto_maintenance = Cell::new(true);
    let set_upstream = Cell::new(false);
    let refetch = Cell::new(false);
    let stdin = Cell::new(false);
    let jobs = Cell::new(-1);
    let write_commit_graph = Cell::new(-1);
    let show_forced = Cell::new(-1);
    let upload_pack: RefCell<Option<String>> = RefCell::new(None);
    let recurse_submodules: Cell<Option<FetchRecurse>> = Cell::new(None);
    let recurse_default: RefCell<Option<String>> = RefCell::new(None);
    let submodule_prefix: RefCell<Option<String>> = RefCell::new(None);
    let server_options: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let refmap: RefCell<Option<Vec<String>>> = RefCell::new(None);
    let negotiation_restrict: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let negotiate_only = Cell::new(false);
    let prefetch = Cell::new(false);
    let negotiation_include: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let depth: RefCell<Option<String>> = RefCell::new(None);
    let shallow_since: RefCell<Option<String>> = RefCell::new(None);
    let shallow_exclude: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let deepen: RefCell<Option<String>> = RefCell::new(None);
    let unshallow = Cell::new(false);
    let update_shallow = Cell::new(false);
    let filters: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let no_filter = Cell::new(false);
    // The options in the order of git's `-h` output: what transfers,
    // then what is stored.
    let mut table = vec![
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::bool(None, "all", &all, msg::HELP_ALL),
            Opt::flag(None, "set-upstream", &set_upstream, msg::HELP_SET_UPSTREAM),
            Opt::flag('a', "append", &append, msg::HELP_APPEND),
            Opt::flag(None, "atomic", &atomic, msg::HELP_ATOMIC),
            Opt::string(None, "upload-pack", &upload_pack, "path", msg::HELP_UPLOAD_PACK),
    ];
    table.extend([
            Opt::force(&force, msg::HELP_FORCE),
            Opt::flag('m', "multiple", &multiple, msg::HELP_MULTIPLE),
            Opt::bool('t', "tags", &tags, msg::HELP_TAGS),
            Opt::set_int('n', None, &tags, msg::HELP_NO_TAGS, 0).noneg(),
            Opt::integer('j', "jobs", &jobs, msg::HELP_JOBS),
            Opt::flag(None, "prefetch", &prefetch, msg::HELP_PREFETCH),
            Opt::bool('p', "prune", &prune, msg::HELP_PRUNE),
            Opt::bool('P', "prune-tags", &prune_tags, msg::HELP_PRUNE_TAGS),
            Opt::callback(
                None,
                "recurse-submodules",
                Some("on-demand"),
                msg::HELP_RECURSE_SUBMODULES,
                |v, unset| {
                    let m = if unset {
                        FetchRecurse::No
                    } else {
                        let v = v.unwrap_or("yes");
                        crate::fetch_submodules::parse_mode(v).ok_or_else(|| {
                            CallbackError::Fatal(msg::bad_recurse_submodules(v))
                        })?
                    };
                    recurse_submodules.set(Some(m));
                    Ok(())
                },
            )
            .optional(None),
            Opt::bool(None, "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag(None, "porcelain", &porcelain, msg::HELP_PORCELAIN),
            Opt::flag(None, "write-fetch-head", &write_fetch_head, msg::HELP_WRITE_FETCH_HEAD),
            Opt::flag('k', "keep", &keep, msg::HELP_KEEP),
            Opt::flag('u', "update-head-ok", &update_head_ok, msg::HELP_UPDATE_HEAD_OK),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::string(None, "depth", &depth, "depth", msg::HELP_DEPTH),
            Opt::string(None, "shallow-since", &shallow_since, "time", msg::HELP_SHALLOW_SINCE),
            Opt::string_list(
                None,
                "shallow-exclude",
                &shallow_exclude,
                "ref",
                msg::HELP_SHALLOW_EXCLUDE,
            ),
            Opt::string(None, "deepen", &deepen, "n", msg::HELP_DEEPEN),
            Opt::flag(None, "unshallow", &unshallow, msg::HELP_UNSHALLOW).noneg(),
            Opt::flag(None, "refetch", &refetch, msg::HELP_REFETCH).noneg(),
            Opt::flag(None, "update-shallow", &update_shallow, msg::HELP_UPDATE_SHALLOW),
            Opt::callback(None, "refmap", Some("refmap"), msg::HELP_REFMAP, |v, _| {
                refmap
                    .borrow_mut()
                    .get_or_insert_with(Vec::new)
                    .push(v.unwrap_or_default().to_owned());
                Ok(())
            })
            .noneg(),
            Opt::string_list(
                'o',
                "server-option",
                &server_options,
                "server-specific",
                msg::HELP_SERVER_OPTION,
            ),
            Opt::string_list(
                None,
                "negotiation-restrict",
                &negotiation_restrict,
                "revision",
                msg::HELP_NEGOTIATION_RESTRICT,
            ),
            Opt::string_list(
                None,
                "negotiation-tip",
                &negotiation_restrict,
                "revision",
                msg::HELP_NEGOTIATION_TIP,
            ),
            Opt::string_list(
                None,
                "negotiation-include",
                &negotiation_include,
                "revision",
                msg::HELP_NEGOTIATION_INCLUDE,
            ),
            Opt::flag(None, "negotiate-only", &negotiate_only, msg::HELP_NEGOTIATE_ONLY),
            Opt::flag('4', "ipv4", &ipv4, msg::HELP_IPV4).noneg(),
            Opt::flag('6', "ipv6", &ipv6, msg::HELP_IPV6).noneg(),
            Opt::callback(None, "filter", Some("args"), msg::HELP_FILTER, |v, unset| {
                match v {
                    Some(spec) if !unset => filters.borrow_mut().push(spec.to_owned()),
                    _ => {
                        filters.borrow_mut().clear();
                        no_filter.set(true);
                    }
                }
                Ok(())
            }),
            Opt::flag(None, "auto-maintenance", &auto_maintenance, msg::HELP_AUTO_MAINTENANCE),
            Opt::flag(None, "auto-gc", &auto_maintenance, msg::HELP_AUTO_MAINTENANCE),
            Opt::bool(None, "show-forced-updates", &show_forced, msg::HELP_SHOW_FORCED),
            Opt::bool(None, "write-commit-graph", &write_commit_graph, msg::HELP_WRITE_COMMIT_GRAPH),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::string(None, "recurse-submodules-default", &recurse_default, "on-demand", "")
                .hidden(),
            Opt::string(None, "submodule-prefix", &submodule_prefix, "dir", "").hidden(),
    ]);
    let mut parser = Parser::new(msg::USAGE, table);
    let rest = parser.parse(argv)?.args;
    drop(parser);
    let mut recurse = recurse_submodules.get();
    if recurse.is_some_and(|m| m != FetchRecurse::No) && negotiate_only.get() {
        return Err(Die::Fatal(msg::options_together(
            "--negotiate-only",
            "--recurse-submodules",
        )));
    }
    // The porcelain output has no place for submodule fetches: git refuses
    // an explicit `--recurse-submodules` and otherwise leaves the
    // submodules alone, whatever the configuration says.
    if porcelain.get() {
        if recurse.is_some_and(|m| m != FetchRecurse::No) {
            return Err(Die::Fatal(msg::options_together(
                "--porcelain",
                "--recurse-submodules",
            )));
        }
        recurse = Some(FetchRecurse::No);
    }
    let recurse_default = match recurse_default.borrow().as_deref() {
        None => None,
        Some(v) => Some(
            crate::fetch_submodules::parse_mode(v)
                .ok_or_else(|| Die::Fatal(msg::bad_recurse_submodules(v)))?,
        ),
    };
    let args = Args {
        write_commit_graph: write_commit_graph.get(),
        verbose: verbose.get(),
        quiet: quiet.get(),
        all: (all.get() >= 0).then(|| all.get() > 0),
        multiple: multiple.get(),
        force: force.get() > 0,
        tags: tags.get(),
        prune: prune.get(),
        prune_tags: prune_tags.get(),
        dry_run: dry_run.get() > 0,
        porcelain: porcelain.get(),
        write_fetch_head: write_fetch_head.get(),
        filters: filters.into_inner(),
        no_filter: no_filter.get(),
        append: append.get(),
        atomic: atomic.get(),
        keep: keep.get(),
        update_head_ok: update_head_ok.get(),
        progress: progress.get(),
        ipv4: ipv4.get(),
        ipv6: ipv6.get(),
        auto_maintenance: auto_maintenance.get(),
        set_upstream: set_upstream.get(),
        refetch: refetch.get(),
        negotiate_only: negotiate_only.get(),
        prefetch: prefetch.get(),
        stdin: stdin.get(),
        jobs: (jobs.get() >= 0).then(|| i64::from(jobs.get())),
        upload_pack: upload_pack.into_inner(),
        server_options: server_options.into_inner(),
        refmap: refmap.into_inner(),
        negotiation_restrict: negotiation_restrict.into_inner(),
        negotiation_include: negotiation_include.into_inner(),
        depth: depth.into_inner(),
        shallow_since: shallow_since.into_inner(),
        shallow_exclude: shallow_exclude.into_inner(),
        deepen: deepen.into_inner(),
        unshallow: unshallow.get(),
        update_shallow: update_shallow.get(),
        show_forced_updates: show_forced.get(),
        recurse,
        recurse_default,
        submodule_prefix: submodule_prefix.into_inner(),
    };
    Ok((args, rest))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    run_with_reflog_action(ctx, argv, None)
}

/// The members of the remote group `name` (`remotes.<name>`), if any.
fn group(config: &rgit_config::ConfigSet, name: &str) -> Option<Vec<String>> {
    let key = format!("remotes.{name}");
    let mut found = false;
    let mut out = Vec::new();
    for e in config.get_all(key.as_bytes()) {
        found = true;
        if let Some(v) = &e.value {
            out.extend(v.to_str_lossy().split_ascii_whitespace().map(str::to_owned));
        }
    }
    found.then_some(out)
}

/// The configured remotes in configuration order.
fn remote_names(config: &rgit_config::ConfigSet) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for e in config.entries() {
        let name = e.name.to_str_lossy();
        let Some(rest) = name.strip_prefix("remote.") else {
            continue;
        };
        let Some((remote, _)) = rest.rsplit_once('.') else {
            continue;
        };
        if !names.iter().any(|n| n == remote) {
            names.push(remote.to_owned());
        }
    }
    names
}

/// [`run`] for another command fetching in-process (`remote update`):
/// `action` replaces the reflog action derived from `argv` (an explicit
/// `$GIT_REFLOG_ACTION` still wins).
pub fn run_with_reflog_action(
    ctx: &mut Context,
    argv: &[String],
    action: Option<&str>,
) -> CmdResult {
    let code = fetch_main(ctx, argv, action)?;
    if code == 0 {
        write_commit_graph_after(ctx, argv);
    }
    Ok(code)
}

/// `--write-commit-graph` / `fetch.writeCommitGraph`: a new split layer of
/// the commit-graph after the fetch.
fn write_commit_graph_after(ctx: &Context, argv: &[String]) {
    let Ok((args, _)) = parse(argv) else {
        return;
    };
    if args.dry_run || args.negotiate_only {
        return;
    }
    let Ok(setup) = repo::setup(ctx, Mode::Required) else {
        return;
    };
    let config = repo::config(ctx, &setup);
    let want = match args.write_commit_graph {
        -1 => config
            .bool("fetch.writecommitgraph")
            .ok()
            .flatten()
            .unwrap_or(false),
        v => v > 0,
    };
    if want {
        crate::commit_graph::write_reachable(
            &setup,
            &config,
            Some(rgit_commitgraph::write::Split::Merge),
        );
    }
}

fn fetch_main(ctx: &mut Context, argv: &[String], action: Option<&str>) -> CmdResult {
    let (args, mut rest) = parse(argv)?;

    // Shallow options (fetch-options.adoc).
    let mut deepen_n: Option<u32> = None;
    let mut deepen_relative = false;
    if let Some(d) = args.depth.as_deref() {
        match d.parse::<i64>() {
            Ok(n) if n > 0 => deepen_n = Some(n.min(i64::from(i32::MAX)) as u32),
            _ => return Err(Die::Fatal(msg::depth_not_positive(d))),
        }
    }
    if let Some(d) = args.deepen.as_deref() {
        if args.depth.is_some() {
            return Err(Die::Fatal(msg::options_together("--deepen", "--depth")));
        }
        let n: i64 = d
            .parse()
            .map_err(|_| Die::Fatal(msg::depth_not_positive(d)))?;
        if n < 0 {
            return Err(Die::Fatal(msg::NEGATIVE_DEEPEN.into()));
        }
        if n > 0 {
            deepen_n = Some(n.min(i64::from(i32::MAX)) as u32);
            deepen_relative = true;
        }
    }
    if args.unshallow && deepen_n.is_some() {
        return Err(Die::Fatal(msg::options_together("--depth", "--unshallow")));
    }
    let deepen_since = match args.shallow_since.as_deref() {
        Some(t) => {
            let dctx = rgit_date::DateContext::from_env();
            Some(rgit_date::approxidate(t, &dctx) as i64)
        }
        None => None,
    };

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::config(ctx, &setup);
    if args.stdin {
        // Refspecs one per line after those on the command line.
        let mut lines = Vec::new();
        for line in std::io::stdin().lock().lines() {
            let line = line.map_err(|e| Die::Fatal(e.to_string()))?;
            if !line.is_empty() {
                lines.push(line);
            }
        }
        if rest.is_empty() && !lines.is_empty() {
            rest.push("origin".into());
        }
        rest.extend(lines);
    }
    let output = match config.get("fetch.output") {
        None => None,
        Some(e) => match e.value.as_ref().map(|v| v.to_str_lossy().into_owned()) {
            None => {
                eprintln!("error: {}", msg::missing_value("fetch.output"));
                return Err(Die::Fatal(crate::revisions::unable_to_parse(
                    "fetch.output",
                    e,
                )));
            }
            Some(v) if v == "full" || v == "compact" => Some(v),
            Some(v) => return Err(Die::Fatal(msg::invalid_output(&v))),
        },
    };
    let compact = output.as_deref() == Some("compact");

    // Several remotes?
    let fetch_all = args.all.unwrap_or_else(|| {
        rest.is_empty()
            && !args.multiple
            && config.bool("fetch.all").ok().flatten().unwrap_or(false)
    });
    let remotes = Remotes::from_config(&config).map_err(super::ls_remote::remote_config_die)?;
    for w in remotes.warnings() {
        eprintln!("warning: {w}");
    }
    let configured = remote_names(&config);
    let mut names: Option<Vec<String>> = None;
    if fetch_all {
        if !rest.is_empty() {
            return Err(Die::Fatal(msg::ALL_NO_ARGUMENTS.into()));
        }
        let mut list = Vec::new();
        for name in &configured {
            let r = remotes
                .get(name.as_bytes(), None)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if !r.skip_fetch_all {
                list.push(name.clone());
            }
        }
        names = Some(list);
    } else if args.multiple {
        let mut list = Vec::new();
        for arg in &rest {
            if let Some(members) = group(&config, arg) {
                list.extend(members);
            } else if configured.iter().any(|n| n == arg) {
                list.push(arg.clone());
            } else {
                return Err(Die::Fatal(msg::no_such_remote_or_group(arg)));
            }
        }
        names = Some(list);
    } else if let Some(first) = rest.first() {
        if !configured.iter().any(|n| n == first) {
            if let Some(members) = group(&config, first) {
                if rest.len() > 1 {
                    return Err(Die::Fatal(msg::GROUP_WITH_REFSPECS.into()));
                }
                names = Some(members);
            }
        }
    }
    let quiet_mode = args.quiet > 0;
    if let Some(mut list) = names {
        list.dedup();
        if list.len() == 1 {
            rest = list;
        } else {
            return fetch_multiple(ctx, &args, &setup, &list, quiet_mode);
        }
    }

    let mut repo = LocalRepo::open(&setup, config).map_err(|e| Die::Fatal(e.to_string()))?;
    let hooks = crate::hooks::for_repo(ctx, &setup, &repo.config)?;
    repo.transaction_hook = Some(Box::new(move |state, updates| {
        let mut input = Vec::new();
        for (old, new, name) in updates {
            input.extend_from_slice(format!("{old} {new} {name}\n").as_bytes());
        }
        let opts = rgit_hook::RunOptions::with_args([state.as_str()])
            .stdin(rgit_hook::Input::Data(&input));
        let outcome = hooks.run("reference-transaction", &opts)?;
        let can_abort = matches!(
            state,
            rgit_refs::HookState::Preparing | rgit_refs::HookState::Prepared
        );
        if can_abort && !outcome.ok() {
            return Err(crate::messages::hook::transaction_aborted_by_hook(
                state.as_str(),
            ));
        }
        Ok(())
    }));
    if args.unshallow {
        if repo.shallow().is_empty() {
            return Err(Die::Fatal(msg::UNSHALLOW_COMPLETE.into()));
        }
        deepen_n = Some(i32::MAX as u32);
    }
    for w in &repo.warnings {
        eprintln!("warning: {w}");
    }

    let mut defaulted = false;
    let (name, refspecs): (Vec<u8>, Vec<String>) = match rest.split_first() {
        Some((r, more)) => (r.as_bytes().to_vec(), more.to_vec()),
        None => {
            let branch_remote = repo
                .current_branch()
                .and_then(|b| {
                    BranchConfig::from_config(&repo.config, &b.as_bytes()["refs/heads/".len()..])
                        .ok()
                })
                .and_then(|b| b.remote);
            defaulted = branch_remote.is_none();
            let name = remotes.default_remote_name(branch_remote.as_ref().map(|r| r.as_bstr()));
            (name.to_vec(), Vec::new())
        }
    };
    let remote = remotes
        .get(&name, Some(&repo.git_dir))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if defaulted && !remote.is_configured() {
        // No remote to fall back to: like git, there is nothing to fetch
        // (`FETCH_HEAD` ends up empty, which `pull` relies on).
        if args.write_fetch_head && !args.append && !args.dry_run {
            let _ = std::fs::write(repo.git_dir.join("FETCH_HEAD"), b"");
        }
        return Ok(0);
    }
    for w in &remote.warnings {
        eprintln!("warning: {w}");
    }
    if args.negotiate_only {
        return negotiate_only(ctx, &setup, &repo, &remote, &args);
    }
    if args.refmap.is_some() && refspecs.is_empty() {
        return Err(Die::Fatal(msg::REFMAP_WITHOUT_REFSPECS.into()));
    }

    let reflog_action = match std::env::var("GIT_REFLOG_ACTION") {
        Ok(a) if !a.is_empty() => a,
        _ if action.is_some() => action.unwrap_or_default().to_owned(),
        _ => {
            let mut a = String::from("fetch");
            for arg in &argv[1..] {
                a.push(' ');
                // No credentials from URLs in reflog messages.
                a.push_str(&rgit_fetch::display::anonymize(arg));
            }
            a
        }
    };
    let show_progress = match args.progress {
        -1 => !quiet_mode && std::io::stderr().is_terminal(),
        p => p > 0,
    };
    // `fetch.showForcedUpdates` / `--[no-]show-forced-updates`.
    let show_forced_updates = match args.show_forced_updates {
        -1 => repo
            .config
            .bool("fetch.showforcedupdates")
            .ok()
            .flatten()
            .unwrap_or(true),
        v => v > 0,
    };
    let (filter, promisor) = crate::promisor::fetch_filter(
        &setup,
        &repo.config,
        &remote.name.to_str_lossy(),
        &args.filters,
        args.no_filter,
    )?;
    let opts = FetchOptions {
        filter,
        promisor,
        refspecs,
        tags: match args.tags {
            1 => Some(TagOpt::All),
            0 => Some(TagOpt::NoTags),
            _ => None,
        },
        prune: match args.prune {
            -1 => None,
            p => Some(p > 0),
        },
        prune_tags: match args.prune_tags {
            -1 => None,
            p => Some(p > 0),
        },
        force: args.force,
        dry_run: args.dry_run,
        update_head_ok: args.update_head_ok,
        write_fetch_head: args.write_fetch_head,
        append: args.append,
        reflog_action,
        no_progress: !show_progress,
        server_options: args.server_options.clone(),
        keep: args.keep,
        refmap: args.refmap.clone(),
        checked_out: checked_out_branches(&setup, &repo.config),
        refetch: args.refetch,
        atomic: args.atomic,
        negotiation_tips: negotiation_tips(ctx, &setup, &repo, &remote.name.to_str_lossy(), &args)?,
        deepen: deepen_n,
        deepen_relative,
        deepen_since,
        deepen_not: args.shallow_exclude.clone(),
        update_shallow: args.update_shallow,
        no_show_forced_updates: !show_forced_updates,
        prefetch: args.prefetch,
    };
    let env = SystemEnv::default();
    let connect = ConnectSettings {
        env: &env,
        config: &repo.config,
        base_env: ctx.child_env(),
        upload_pack: args
            .upload_pack
            .clone()
            .or_else(|| remote.upload_pack.as_ref().map(|u| u.to_str_lossy().into_owned())),
        ip: if args.ipv4 {
            IpVersion::V4
        } else if args.ipv6 {
            IpVersion::V6
        } else {
            IpVersion::Any
        },
        verbose: args.verbose > 0 && !quiet_mode,
    };
    rgit_protocol::sideband::set_remote_colors(crate::color::remote_colors(&repo.config));
    crate::color::set_sideband_controls(&repo.config, None);
    if !show_forced_updates
        && ctx
            .advisor(&repo.config)?
            .enabled(Advice::FetchShowForcedUpdates)
            .map_err(Die::Fatal)?
    {
        eprintln!("warning: {}", msg::FORCED_UPDATES_CHECK_DISABLED);
    }
    let sub_opts = crate::fetch_submodules::Options {
        cli: args.recurse,
        default: args.recurse_default,
        prefix: args.submodule_prefix.clone().unwrap_or_default(),
        jobs: args.jobs,
        child_args: args.submodule_options(),
        quiet: quiet_mode,
    };
    let recursing = args.recurse != Some(FetchRecurse::No);
    let old_tips = if recursing {
        let before = crate::repository::Repo::open(ctx, Mode::RequiredQuiet)?;
        crate::fetch_submodules::ref_tips(&before)
    } else {
        Vec::new()
    };
    if !args.dry_run {
        fetch_bundle_uri_once(&repo, &connect)?;
    }
    let mut result =
        rgit_fetch::fetch(&repo, &remote, &opts, &connect).map_err(|e| Die::Fatal(e.to_string()))?;
    let new_tips: Vec<rgit_hash::ObjectId> = result
        .lines
        .iter()
        .filter(|l| l.remote.is_some() && !l.status.is_error())
        .map(|l| l.new)
        .collect();
    // Without FETCH_HEAD, refs stored nowhere are not listed.
    if !args.write_fetch_head && !args.dry_run {
        result.lines.retain(|l| l.local.is_some());
    }

    if let (Some(w), false) = (&result.head_warning, quiet_mode) {
        let advisor = ctx.advisor(&repo.config)?;
        advisor
            .advise(Advice::FetchRemoteHeadWarn, &msg::remote_head_hint(&w.remote, &w.branch))
            .map_err(Die::Fatal)?;
    }
    let mut err = std::io::stderr().lock();
    for w in &result.warnings {
        let _ = writeln!(err, "warning: {w}");
    }
    for e in &result.errors {
        let _ = writeln!(err, "error: {e}");
    }
    let verbosity = if quiet_mode { -1 } else { args.verbose };
    let cli_repo = crate::repository::Repo::open(ctx, Mode::RequiredQuiet)?;
    let abbrev_len = cli_repo.default_abbrev();
    let abbrev = |id: &rgit_hash::ObjectId| cli_repo.abbrev(id, abbrev_len);
    if args.porcelain {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&rgit_bstr::to_bytes(&display::porcelain(&result.lines, repo.hash)));
    } else {
        let text = display::render(
            &result.url,
            &result.lines,
            &display::DisplayOptions {
                verbosity,
                columns: columns(),
                abbrev: &abbrev,
                abbrev_len,
                compact,
            },
        );
        // Reference names that are not UTF-8 are printed as their bytes.
        let _ = err.write_all(&rgit_bstr::to_bytes(&text));
    }
    // A ref that could not be written (e.g. a directory/file conflict
    // with a stale remote-tracking branch).
    let failed = result
        .lines
        .iter()
        .any(|l| matches!(l.status, rgit_fetch::update::Status::Failed(_)));
    if failed && remote.is_configured() {
        let _ = writeln!(err, "error: {}", msg::prune_advice(&remote.name.to_str_lossy()));
    }
    let mut code = result.exit_code();
    drop(err);
    if let (Some(w), false) = (&result.head_warning, quiet_mode) {
        println!("{}", w.text);
    }
    // Also when a ref could not be updated (t5510 "even with conflicts").
    if args.set_upstream {
        code = code.max(set_upstream(&repo, &remote, &result)?);
    }
    if recursing && !args.negotiate_only {
        // The objects just fetched must be visible.
        let fresh = crate::repository::Repo::open(ctx, Mode::RequiredQuiet)?;
        code = code.max(crate::fetch_submodules::fetch(&fresh, &sub_opts, &old_tips, &new_tips));
    }
    drop(cli_repo);
    if code == 0 && args.auto_maintenance {
        maintenance(ctx, quiet_mode);
    }
    Ok(code)
}

/// The automatic maintenance after fetching (a `maintenance run --auto`
/// child process).
fn maintenance(ctx: &Context, quiet: bool) {
    crate::maintenance::run_auto_quiet(ctx, quiet);
}

/// `--all`, `--multiple`, a group: every remote in turn, `FETCH_HEAD`
/// collecting them all.
fn fetch_multiple(
    ctx: &mut Context,
    args: &Args,
    setup: &rgit_repo::Setup,
    names: &[String],
    quiet: bool,
) -> CmdResult {
    let jobs = match args.jobs {
        Some(0) => std::thread::available_parallelism().map_or(1, |n| n.get() as i64),
        Some(n) => n,
        None => {
            let config = repo::config(ctx, setup);
            config.int("fetch.parallel").ok().flatten().unwrap_or(1)
        }
    }
    .max(1);
    if args.write_fetch_head && !args.dry_run && !args.append {
        let _ = std::fs::write(setup.cwd.join(&setup.git_dir).join("FETCH_HEAD"), b"");
    }
    let parallel = jobs > 1;
    if parallel {
        trace_parallel(jobs.min(names.len() as i64));
    }
    let mut code = 0;
    for name in names {
        if !quiet && !args.porcelain {
            println!("{}", msg::fetching(name));
        }
        let mut argv = vec!["fetch".to_owned()];
        argv.extend(args.child_options());
        if parallel {
            argv.push("--end-of-options".into());
        }
        argv.push(name.clone());
        let child = match run_with_reflog_action(ctx, &argv, None) {
            Ok(c) => c,
            Err(die) => die.emit(),
        };
        if child != 0 {
            if parallel {
                eprintln!("{}", msg::could_not_fetch_code(name, child));
            } else {
                eprintln!("error: {}", msg::could_not_fetch(name));
            }
            code = 1;
        }
    }
    if parallel {
        trace_parallel_done();
    }
    if args.auto_maintenance {
        maintenance(ctx, quiet);
    }
    Ok(code)
}

#[track_caller]
fn trace_parallel(tasks: i64) {
    if rgit_trace::TRACE.enabled() {
        rgit_trace::TRACE.write_line(
            rgit_trace::Location::caller(),
            msg::trace_parallel(tasks).as_bytes(),
        );
    }
}

#[track_caller]
fn trace_parallel_done() {
    if rgit_trace::TRACE.enabled() {
        rgit_trace::TRACE.write_line(rgit_trace::Location::caller(), msg::TRACE_PARALLEL_DONE);
    }
}

/// The values of a multi-valued `remote.<name>.<key>`; an empty value
/// clears the list.
fn config_list(config: &rgit_config::ConfigSet, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for e in config.get_all(key.as_bytes()) {
        match e.value.as_ref().map(|v| v.to_str_lossy().into_owned()) {
            Some(v) if !v.is_empty() => out.push(v),
            _ => out.clear(),
        }
    }
    out
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// `--negotiate-only`: print the commits the server has in common with
/// the negotiation tips (protocol v2 servers with `wait-for-done`).
fn negotiate_only(
    ctx: &Context,
    setup: &rgit_repo::Setup,
    repo: &LocalRepo,
    remote: &rgit_remote::Remote,
    args: &Args,
) -> CmdResult {
    let name = remote.name.to_str_lossy().into_owned();
    let Some(tips) = negotiation_tips(ctx, setup, repo, &name, args)? else {
        return Err(Die::Fatal(msg::NEGOTIATE_ONLY_NEEDS_TIPS.into()));
    };
    let Some(url) = remote.fetch_url() else {
        return Err(Die::Fatal(rgit_fetch::messages::NO_URL.into()));
    };
    let url = url.to_str_lossy().into_owned();
    let env = SystemEnv::default();
    let connect = ConnectSettings {
        env: &env,
        config: &repo.config,
        base_env: ctx.child_env(),
        upload_pack: args
            .upload_pack
            .clone()
            .or_else(|| remote.upload_pack.as_ref().map(|u| u.to_str_lossy().into_owned())),
        ip: IpVersion::Any,
        verbose: false,
    };
    let fatal = |e: rgit_fetch::Error| Die::Fatal(e.to_string());
    let negotiation = rgit_fetch::Negotiation::from_config(&repo.config).map_err(fatal)?;
    let mut session = rgit_fetch::Session::connect(&url, &connect, "fetch").map_err(fatal)?;
    let server_options: Vec<String> = if args.server_options.is_empty() {
        remote
            .server_options
            .iter()
            .map(|o| o.to_str_lossy().into_owned())
            .collect()
    } else {
        args.server_options.clone()
    };
    let _region = rgit_trace::trace2::region("fetch", "negotiate-only", true);
    let common = session
        .negotiate_only(repo, &tips, negotiation, &server_options)
        .map_err(fatal)?;
    let Some(common) = common else {
        // The server is left waiting for wants and reports the hang-up.
        eprintln!("warning: {}", msg::NEGOTIATE_ONLY_NEEDS_V2);
        drop(session);
        return Ok(255);
    };
    // Like git: no final flush, the connection is just closed.
    drop(session);
    let mut out = std::io::stdout().lock();
    for id in common {
        let _ = writeln!(out, "{id}");
    }
    Ok(0)
}

/// The tips of the negotiation (fetch-options.adoc): with
/// `--negotiation-restrict` (or `remote.<name>.negotiationRestrict`) only
/// those revisions, plus the refs of `--negotiation-include` (or
/// `remote.<name>.negotiationInclude`). Unknown names are ignored, a
/// missing object is fatal.
fn negotiation_tips(
    ctx: &Context,
    setup: &rgit_repo::Setup,
    repo: &LocalRepo,
    remote: &str,
    args: &Args,
) -> Result<Option<Vec<rgit_hash::ObjectId>>, Die> {
    use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
    let pick = |cli: &[String], key: &str| {
        if cli.is_empty() {
            config_list(&repo.config, &format!("remote.{remote}.{key}"))
        } else {
            cli.to_vec()
        }
    };
    let restrict = pick(&args.negotiation_restrict, "negotiationrestrict");
    if restrict.is_empty() {
        return Ok(None);
    }
    let include = pick(&args.negotiation_include, "negotiationinclude");
    let refs = repo.list_refs("");
    let revs = crate::refs_support::Revs::new(ctx, setup)?;
    let mut tips = Vec::new();
    let mut add = |id: rgit_hash::ObjectId| {
        if !tips.contains(&id) {
            tips.push(id);
        }
    };
    for r in &restrict {
        if is_glob(r) {
            for (name, id) in &refs {
                if wildmatch(r.as_bytes(), name.as_bytes(), WildmatchFlags::NONE) {
                    add(*id);
                }
            }
        } else if let Some(id) = (r.len() == repo.hash.hex_len())
            .then(|| rgit_hash::ObjectId::from_hex(repo.hash, r.as_bytes()).ok())
            .flatten()
            .or_else(|| revs.resolve(r))
        {
            if !repo.has_object(&id) {
                return Err(Die::Fatal(msg::object_does_not_exist(&id.to_hex())));
            }
            add(id);
        }
    }
    for pattern in &include {
        for (name, id) in &refs {
            let hit = if is_glob(pattern) {
                wildmatch(pattern.as_bytes(), name.as_bytes(), WildmatchFlags::NONE)
            } else {
                name == pattern
            };
            if hit {
                add(*id);
            }
        }
    }
    Ok(Some(tips))
}

/// Branches checked out in a worktree, or being rebased or bisected
/// there, with the worktree's path.
fn checked_out_branches(
    setup: &rgit_repo::Setup,
    config: &rgit_config::ConfigSet,
) -> Vec<(String, PathBuf)> {
    let bare_main = crate::worktrees::main_is_bare(setup, config);
    let mut out = Vec::new();
    for wt in crate::worktrees::list(setup, bare_main) {
        if wt.bare {
            continue;
        }
        if let Some(r) = &wt.head_ref {
            out.push((r.clone(), wt.path.clone()));
        }
        let read = |p: PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim_end().to_owned());
        for dir in ["rebase-merge", "rebase-apply"] {
            if let Some(r) = read(wt.git_dir.join(dir).join("head-name")) {
                if r.starts_with("refs/") {
                    out.push((r, wt.path.clone()));
                }
            }
        }
        if let Some(b) = read(wt.git_dir.join("BISECT_START")) {
            out.push((format!("refs/heads/{b}"), wt.path.clone()));
        }
    }
    out
}

/// `--set-upstream`: the current branch tracks the one ref fetched for
/// merging (a command-line refspec without destination, or the remote's
/// `HEAD`).
fn set_upstream(
    repo: &LocalRepo,
    remote: &rgit_remote::Remote,
    result: &rgit_fetch::FetchResult,
) -> Result<i32, Die> {
    let candidates: Vec<&rgit_fetch::refmap::Entry> = result
        .entries
        .iter()
        .filter(|e| e.for_merge && e.local.is_none())
        .collect();
    let [entry] = candidates.as_slice() else {
        if candidates.len() > 1 {
            eprintln!("error: {}", msg::MULTIPLE_BRANCHES_UPSTREAM);
        } else {
            eprintln!("warning: {}", msg::NO_SOURCE_BRANCH_UPSTREAM);
        }
        return Ok(0);
    };
    let remote_name = if remote.is_configured() {
        remote.name.to_str_lossy().into_owned()
    } else {
        remote
            .fetch_url()
            .map(|u| u.to_str_lossy().into_owned())
            .unwrap_or_default()
    };
    let merge = entry.remote.to_str_lossy().into_owned();
    if merge.starts_with("refs/tags/") {
        eprintln!("warning: {}", msg::NO_UPSTREAM_FOR_TAG);
        return Ok(0);
    }
    let Some(branch) = repo.current_branch() else {
        let short = display::pretty_ref(entry.remote.as_bstr());
        eprintln!("warning: {}", msg::upstream_detached(&short, &remote_name));
        return Ok(0);
    };
    let short = &branch["refs/heads/".len()..];
    let path = repo.common_dir.join("config");
    for (key, value) in [("remote", remote_name.as_str()), ("merge", merge.as_str())] {
        let key = format!("branch.{short}.{key}");
        config_set(&path, &key, value)?;
    }
    Ok(0)
}

/// Bundles of `fetch.bundleURI` were looked at already (`--all` fetches
/// several remotes in one process; the bundles are fetched once).
static BUNDLE_URI_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `fetch.bundleURI` (set by a clone from a bundle list with the
/// `creationToken` heuristic): fetch the bundles newer than
/// `fetch.bundleCreationToken` first, and remember the newest one.
fn fetch_bundle_uri_once(repo: &LocalRepo, connect: &ConnectSettings<'_>) -> Result<(), Die> {
    let Some(uri) = repo
        .config
        .string("fetch.bundleuri")
        .ok()
        .flatten()
        .map(|v| v.to_str_lossy().into_owned())
    else {
        return Ok(());
    };
    if BUNDLE_URI_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    let stored = repo
        .config
        .int64("fetch.bundlecreationtoken")
        .ok()
        .flatten()
        .map_or(0, |v| v.max(0) as u64);
    let o = rgit_fetch::bundle_uri::fetch_bundle_uri(repo, connect, &uri, Some(stored));
    if let Some(t) = o.token.filter(|t| *t > stored) {
        config_set(
            &repo.common_dir.join("config"),
            "fetch.bundleCreationToken",
            &t.to_string(),
        )?;
    }
    Ok(())
}

fn config_set(path: &std::path::Path, key: &str, value: &str) -> Result<(), Die> {
    use rgit_config::edit::{SetRequest, ValuePattern};
    let req = SetRequest {
        key: key.as_bytes(),
        value: Some(value.as_bytes()),
        pattern: &ValuePattern::Any,
        all: false,
        comment: None,
    };
    rgit_config::write::set_in_file(path, &rgit_fs::LockOptions::default(), &req)
        .map(|_| ())
        .map_err(|e| Die::Fatal(e.to_string()))
}
