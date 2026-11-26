// SPDX-License-Identifier: MIT
//! The tasks of `git maintenance run` (Documentation/git-maintenance.adoc,
//! "TASKS"; Documentation/config/maintenance.adoc).
//!
//! Which tasks run comes from `--task`, else from `maintenance.<task>.enabled`
//! and `maintenance.<task>.schedule` falling back to `maintenance.strategy`.
//! Without `--schedule`, an unset strategy means `geometric`; with it, an
//! unset strategy runs nothing. As observed with the reference git
//! (which rejects `none` like any unknown strategy):
//!
//! | strategy | without `--schedule` | scheduled |
//! |---|---|---|
//! | `gc` | `gc` | `gc` daily |
//! | `geometric` | `pack-refs`, `reflog-expire`, `geometric-repack`, `commit-graph`, `worktree-prune`, `rerere-gc` | `commit-graph` hourly; `pack-refs`, `geometric-repack` daily; `reflog-expire`, `worktree-prune`, `rerere-gc` weekly |
//! | `incremental` | `gc` | `prefetch`, `commit-graph` hourly; `loose-objects`, `incremental-repack` daily; `pack-refs` weekly |
//!
//! A run first does the foreground part of the selected tasks (`gc`,
//! `pack-refs`, `reflog-expire`; trace2 region category
//! `maintenance foreground`), then the rest (category `maintenance`), each
//! as child processes of the same binary. With `--auto` only the tasks whose
//! condition holds run, after the `pre-auto-gc` hook agreed.
//!
//! Until rgit has `commit-graph` (#22), `multi-pack-index` (#21; with it
//! `repack --geometric --write-midx`) and `rerere` (#156), a step that
//! needs one of them counts as done when the child reports it is missing,
//! and automatic runs leave those tasks out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use bstr::ByteSlice;
use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId};
use rgit_repo::Setup;

use crate::housekeeping::store;
use crate::messages::maintenance as msg;
use crate::prelude::*;
use crate::repository::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Prefetch,
    LooseObjects,
    IncrementalRepack,
    GeometricRepack,
    Gc,
    CommitGraph,
    PackRefs,
    ReflogExpire,
    WorktreePrune,
    RerereGc,
}

/// All tasks in the order a run without `--task` handles them.
pub const ALL: [Task; 10] = [
    Task::Prefetch,
    Task::LooseObjects,
    Task::IncrementalRepack,
    Task::GeometricRepack,
    Task::Gc,
    Task::CommitGraph,
    Task::PackRefs,
    Task::ReflogExpire,
    Task::WorktreePrune,
    Task::RerereGc,
];

impl Task {
    pub fn name(self) -> &'static str {
        match self {
            Task::Prefetch => "prefetch",
            Task::LooseObjects => "loose-objects",
            Task::IncrementalRepack => "incremental-repack",
            Task::GeometricRepack => "geometric-repack",
            Task::Gc => "gc",
            Task::CommitGraph => "commit-graph",
            Task::PackRefs => "pack-refs",
            Task::ReflogExpire => "reflog-expire",
            Task::WorktreePrune => "worktree-prune",
            Task::RerereGc => "rerere-gc",
        }
    }

    pub fn from_name(name: &str) -> Option<Task> {
        ALL.into_iter().find(|t| t.name() == name)
    }

    fn has_foreground(self) -> bool {
        matches!(self, Task::Gc | Task::PackRefs | Task::ReflogExpire)
    }

    fn has_background(self) -> bool {
        !matches!(self, Task::PackRefs | Task::ReflogExpire)
    }
}

/// `--schedule=<frequency>`; a later frequency includes the earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Schedule {
    Hourly,
    Daily,
    Weekly,
}

impl Schedule {
    pub fn parse(s: &str) -> Option<Schedule> {
        match s {
            "hourly" => Some(Schedule::Hourly),
            "daily" => Some(Schedule::Daily),
            "weekly" => Some(Schedule::Weekly),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Schedule::Hourly => "hourly",
            Schedule::Daily => "daily",
            Schedule::Weekly => "weekly",
        }
    }

    pub const ALL: [Schedule; 3] = [Schedule::Hourly, Schedule::Daily, Schedule::Weekly];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Unset,
    Gc,
    Geometric,
    Incremental,
}

fn strategy(config: &ConfigSet) -> Result<Strategy, Die> {
    let Some(v) = config.get("maintenance.strategy") else {
        return Ok(Strategy::Unset);
    };
    let v = v
        .value
        .as_ref()
        .map(|v| v.to_str_lossy().into_owned())
        .unwrap_or_default();
    match v.as_str() {
        "gc" => Ok(Strategy::Gc),
        "geometric" => Ok(Strategy::Geometric),
        "incremental" => Ok(Strategy::Incremental),
        other => Err(Die::Fatal(msg::unknown_strategy(other))),
    }
}

/// Whether `strategy` runs `task` without `--schedule`.
fn strategy_manual(strategy: Strategy, task: Task) -> bool {
    match strategy {
        Strategy::Gc | Strategy::Incremental => task == Task::Gc,
        Strategy::Unset | Strategy::Geometric => matches!(
            task,
            Task::PackRefs
                | Task::ReflogExpire
                | Task::GeometricRepack
                | Task::CommitGraph
                | Task::WorktreePrune
                | Task::RerereGc
        ),
    }
}

/// The frequency `strategy` gives `task` for scheduled runs.
fn strategy_schedule(strategy: Strategy, task: Task) -> Option<Schedule> {
    use Schedule::*;
    match strategy {
        Strategy::Unset => None,
        Strategy::Gc => (task == Task::Gc).then_some(Daily),
        Strategy::Geometric => match task {
            Task::CommitGraph => Some(Hourly),
            Task::PackRefs | Task::GeometricRepack => Some(Daily),
            Task::ReflogExpire | Task::WorktreePrune | Task::RerereGc => Some(Weekly),
            _ => None,
        },
        Strategy::Incremental => match task {
            Task::Prefetch | Task::CommitGraph => Some(Hourly),
            Task::LooseObjects | Task::IncrementalRepack => Some(Daily),
            Task::PackRefs => Some(Weekly),
            _ => None,
        },
    }
}

fn config_bool(config: &ConfigSet, key: &str) -> Result<Option<bool>, Die> {
    config.bool(key).map_err(|e| Die::Fatal(e.to_string()))
}

fn config_int(config: &ConfigSet, key: &str) -> Result<Option<i64>, Die> {
    config.int(key).map_err(|e| Die::Fatal(e.to_string()))
}

fn config_string(config: &ConfigSet, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned())
}

/// The tasks a run considers: `--task` in the given order, else the
/// enabled ones (for the given frequency).
pub fn select(
    config: &ConfigSet,
    explicit: &[Task],
    schedule: Option<Schedule>,
) -> Result<Vec<Task>, Die> {
    let strategy = strategy(config)?;
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    let mut out = Vec::new();
    for task in ALL {
        let enabled = config_bool(config, &format!("maintenance.{}.enabled", task.name()))?;
        let include = match schedule {
            None => enabled.unwrap_or_else(|| strategy_manual(strategy, task)),
            Some(freq) => {
                let default = strategy_schedule(strategy, task);
                let configured =
                    config_string(config, &format!("maintenance.{}.schedule", task.name()));
                let sched = match configured {
                    Some(v) => Schedule::parse(&v),
                    None => default,
                };
                enabled.unwrap_or(default.is_some()) && sched.is_some_and(|s| s <= freq)
            }
        };
        if include {
            out.push(task);
        }
    }
    Ok(out)
}

/// Options of one run.
pub struct RunOptions {
    pub auto: bool,
    pub quiet: bool,
    pub detach: bool,
}

/// Runs the child processes of the tasks.
struct Children {
    exe: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    dir: PathBuf,
}

impl Children {
    fn new(ctx: &Context, setup: &Setup) -> Result<Children, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let mut env = ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        env.push(("GIT_DIR".into(), setup.absolute_git_dir().into_os_string()));
        if let Some(wt) = &setup.work_tree {
            env.push(("GIT_WORK_TREE".into(), setup.cwd.join(wt).into_os_string()));
        }
        Ok(Children {
            exe,
            env,
            dir: setup.cwd.clone(),
        })
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).current_dir(&self.dir);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }

    /// Runs `git <args>`; whether it succeeded.
    fn run(&self, args: &[&str]) -> bool {
        self.run_with(args, self.command(args).stdin(Stdio::null()))
    }

    fn run_with(&self, args: &[&str], cmd: &mut Command) -> bool {
        rgit_trace::trace2::status(cmd, Default::default(), &rgit_trace::trace2::git_argv(args))
            .is_ok_and(|s| s.success())
    }

    /// Runs a step rgit may not have yet (`commit-graph`,
    /// `multi-pack-index`, `rerere`, `repack --geometric`): when the child
    /// reports that, the step counts as done.
    fn run_optional(&self, args: &[&str]) -> bool {
        use std::io::Write;
        let out = rgit_trace::trace2::output(
            self.command(args).stdin(Stdio::null()),
            Default::default(),
            &rgit_trace::trace2::git_argv(args),
        );
        let Ok(out) = out else {
            return false;
        };
        let _ = std::io::stdout().write_all(&out.stdout);
        if out.status.success() {
            let _ = std::io::stderr().write_all(&out.stderr);
            return true;
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("is not a git command") || err.contains("is not supported by rgit yet") {
            return true;
        }
        let _ = std::io::stderr().write_all(&out.stderr);
        false
    }
}

/// Whether rgit has what `task` needs. Automatic runs leave the other
/// tasks out.
fn available(task: Task) -> bool {
    let has = |name: &str| crate::commands::find(name).is_some();
    match task {
        Task::CommitGraph => has("commit-graph"),
        Task::IncrementalRepack | Task::GeometricRepack => has("multi-pack-index"),
        Task::RerereGc => has("rerere"),
        _ => true,
    }
}

/// `git maintenance run`: returns the exit code.
pub fn run(
    ctx: &Context,
    repo: &Repo,
    explicit: &[Task],
    schedule: Option<Schedule>,
    opts: &RunOptions,
) -> Result<i32, Die> {
    let config = &repo.config;
    let mut tasks = select(config, explicit, schedule)?;
    let objects = crate::repo_objects::objects_dir(&repo.setup);
    let pid_file = config_bool(config, "core.lockfilepid")?.unwrap_or(false);
    let lock = match rgit_fs::LockFile::acquire(
        objects.join("maintenance"),
        &rgit_fs::LockOptions::new().pid_file(pid_file),
    ) {
        Ok(l) => l,
        Err(_) => {
            if !opts.auto && !opts.quiet {
                let shown = objects.join("maintenance");
                let shown = shown.strip_prefix(&repo.setup.cwd).unwrap_or(&shown);
                eprintln!(
                    "warning: {}",
                    msg::lock_skipping(&shown.display().to_string())
                );
            }
            return Ok(0);
        }
    };
    if opts.auto {
        let mut needed = Vec::new();
        for t in tasks {
            if available(t) && auto_condition(repo, t)? {
                needed.push(t);
            }
        }
        tasks = needed;
        if !tasks.is_empty() {
            let hooks = crate::hooks::for_repo(ctx, &repo.setup, config)?;
            let outcome = crate::hooks::run(&hooks, "pre-auto-gc", &Default::default())?;
            if outcome.code != 0 {
                return Ok(0);
            }
        }
    }
    let children = Children::new(ctx, &repo.setup)?;
    let mut code = 0;
    for &t in tasks.iter().filter(|t| t.has_foreground()) {
        rgit_trace::trace2::region_enter("maintenance foreground", t.name(), None, true);
        let ok = foreground(repo, &children, t, opts);
        rgit_trace::trace2::region_leave();
        if !ok {
            eprintln!("error: {}", msg::task_failed(t.name()));
            code = 1;
        }
    }
    let background: Vec<Task> = tasks.into_iter().filter(|t| t.has_background()).collect();
    if opts.detach && !background.is_empty() {
        rgit_trace::trace2::region_enter("maintenance", "detach", None, true);
        rgit_trace::trace2::region_leave();
        daemonize();
    }
    for t in background {
        rgit_trace::trace2::region_enter("maintenance", t.name(), None, true);
        let ok = background_task(repo, &children, t, opts);
        rgit_trace::trace2::region_leave();
        if !ok {
            eprintln!("error: {}", msg::task_failed(t.name()));
            code = 1;
        }
    }
    drop(lock);
    Ok(code)
}

/// `git maintenance is-needed`: whether any of the tasks would run.
pub fn is_needed(repo: &Repo, explicit: &[Task], auto: bool) -> Result<bool, Die> {
    let tasks = select(&repo.config, explicit, None)?;
    if !auto {
        return Ok(!tasks.is_empty());
    }
    for t in tasks {
        if auto_condition(repo, t)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Continue in a background process: the parent returns success at once
/// and leaves the maintenance lock to the child, which removes it.
#[cfg(unix)]
fn daemonize() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: rgit is single-threaded here; the child only continues
    // running ordinary Rust code of this thread.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return; // run in the foreground
    }
    if pid > 0 {
        // The parent: leave without running exit handlers, which would
        // remove the lock the child keeps holding.
        // SAFETY: plain process exit.
        unsafe { libc::_exit(0) };
    }
    // SAFETY: setsid/open/dup2 on the child's own descriptors.
    unsafe {
        libc::setsid();
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            libc::dup2(null, 0);
            libc::dup2(null, 1);
            libc::dup2(null, 2);
            if null > 2 {
                libc::close(null);
            }
        }
    }
}

#[cfg(not(unix))]
fn daemonize() {}

fn progress_flag(opts: &RunOptions) -> &'static str {
    if opts.quiet {
        "--no-progress"
    } else {
        "--progress"
    }
}

fn foreground(repo: &Repo, children: &Children, task: Task, opts: &RunOptions) -> bool {
    match task {
        Task::PackRefs => pack_refs(children, opts),
        Task::ReflogExpire => children.run(&["reflog", "expire", "--all"]),
        Task::Gc => {
            // What `gc` itself does in the foreground.
            let config = &repo.config;
            let pack = match config.string("gc.packrefs").ok().flatten() {
                Some(v) if v == "notbare" => !repo.setup.is_bare(),
                _ => config.bool("gc.packrefs").ok().flatten().unwrap_or(true),
            };
            if pack && !pack_refs(children, opts) {
                return false;
            }
            let never = |key: &str| {
                config
                    .string(key)
                    .ok()
                    .flatten()
                    .is_some_and(|v| v == "never")
            };
            if !(never("gc.reflogexpire") && never("gc.reflogexpireunreachable")) {
                return children.run(&["reflog", "expire", "--all"]);
            }
            true
        }
        _ => true,
    }
}

fn pack_refs(children: &Children, opts: &RunOptions) -> bool {
    let mut args = vec!["pack-refs", "--all", "--prune"];
    if opts.auto {
        args.push("--auto");
    }
    children.run(&args)
}

fn background_task(repo: &Repo, children: &Children, task: Task, opts: &RunOptions) -> bool {
    let config = &repo.config;
    match task {
        Task::Prefetch => prefetch(repo, children, opts),
        Task::LooseObjects => loose_objects(repo, children, opts),
        Task::IncrementalRepack => incremental_repack(repo, children, opts),
        Task::GeometricRepack => geometric_repack(repo, children, opts),
        Task::Gc => {
            let mut args = vec!["gc"];
            if opts.auto {
                args.push("--auto");
            }
            args.push(if opts.quiet { "--quiet" } else { "--no-quiet" });
            args.extend(["--no-detach", "--skip-foreground-tasks"]);
            children.run(&args)
        }
        Task::CommitGraph => {
            if config_bool(config, "core.commitgraph").ok().flatten() == Some(false) {
                return true;
            }
            children.run_optional(&[
                "commit-graph",
                "write",
                "--split",
                "--reachable",
                progress_flag(opts),
            ])
        }
        Task::WorktreePrune => {
            let expire = config_string(config, "gc.worktreepruneexpire")
                .unwrap_or_else(|| "3.months.ago".into());
            children.run(&["worktree", "prune", "--expire", &expire])
        }
        Task::RerereGc => children.run_optional(&["rerere", "gc"]),
        Task::PackRefs | Task::ReflogExpire => true,
    }
}

fn prefetch(repo: &Repo, children: &Children, opts: &RunOptions) -> bool {
    let Ok(remotes) = rgit_remote::Remotes::from_config(&repo.config) else {
        return false;
    };
    let names: Vec<String> = remotes
        .names()
        .map(|n| n.to_str_lossy().into_owned())
        .collect();
    let mut ok = true;
    for name in names {
        let skip = remotes
            .get(name.as_bytes(), None)
            .is_ok_and(|r| r.skip_fetch_all);
        if skip {
            continue;
        }
        let mut args = vec![
            "fetch",
            &name,
            "--prefetch",
            "--prune",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
        ];
        if opts.quiet {
            args.push("--quiet");
        }
        if !children.run(&args) {
            ok = false;
        }
    }
    if !ok {
        eprintln!("error: {}", msg::PREFETCH_FAILED);
    }
    ok
}

fn loose_objects(repo: &Repo, children: &Children, opts: &RunOptions) -> bool {
    let mut args = vec!["prune-packed"];
    if opts.quiet {
        args.push("--quiet");
    }
    if !children.run(&args) {
        eprintln!("error: {}", msg::LOOSE_OBJECTS_FAILED);
        return false;
    }
    let batch = config_int(&repo.config, "maintenance.loose-objects.batchsize")
        .ok()
        .flatten()
        .unwrap_or(50_000);
    let objects = crate::repo_objects::objects_dir(&repo.setup);
    let objects = repo.setup.cwd.join(objects);
    let mut ids: Vec<ObjectId> = Vec::new();
    'fans: for fan in 0..=255u8 {
        for e in store::fan_entries(&objects, fan, repo.hash) {
            if let Some(id) = e.id {
                if batch > 0 && ids.len() as i64 >= batch {
                    break 'fans;
                }
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return true;
    }
    let base = objects.join("pack").join("loose");
    let base = base.to_string_lossy().into_owned();
    let mut args = vec!["pack-objects"];
    if opts.quiet {
        args.push("--quiet");
    }
    args.push(&base);
    let mut cmd = children.command(&args);
    cmd.stdin(Stdio::piped()).stdout(Stdio::null());
    let traced =
        rgit_trace::trace2::child_start(Default::default(), &rgit_trace::trace2::git_argv(&args));
    spawn_pack(cmd, &ids, traced)
}

fn spawn_pack(
    mut cmd: Command,
    ids: &[ObjectId],
    traced: Option<rgit_trace::trace2::ChildTrace>,
) -> bool {
    use std::io::Write;
    let Ok(mut child) = cmd.spawn() else {
        if let Some(t) = traced {
            t.exit(-1, -1);
        }
        eprintln!("error: {}", msg::LOOSE_PACK_FAILED);
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let mut buf = String::with_capacity(ids.len() * 41);
        for id in ids {
            buf.push_str(&id.to_hex());
            buf.push('\n');
        }
        let _ = stdin.write_all(buf.as_bytes());
    }
    let status = child.wait();
    if let Some(t) = traced {
        match &status {
            Ok(s) => t.exit_status(child.id(), *s),
            Err(_) => t.exit(i64::from(child.id()), -1),
        }
    }
    let ok = status.is_ok_and(|s| s.success());
    if !ok {
        eprintln!("error: {}", msg::LOOSE_PACK_FAILED);
    }
    ok
}

fn multi_pack_index_enabled(config: &ConfigSet) -> bool {
    config_bool(config, "core.multipackindex")
        .ok()
        .flatten()
        .unwrap_or(true)
}

fn incremental_repack(repo: &Repo, children: &Children, opts: &RunOptions) -> bool {
    if !multi_pack_index_enabled(&repo.config) {
        return true;
    }
    let progress = progress_flag(opts);
    if !children.run_optional(&["multi-pack-index", "write", progress]) {
        return false;
    }
    if !children.run_optional(&["multi-pack-index", "expire", progress]) {
        return false;
    }
    let batch = format!("--batch-size={}", repack_batch_size(repo));
    children.run_optional(&["multi-pack-index", "repack", progress, &batch])
}

/// The `--batch-size` for `multi-pack-index repack`: one more than the
/// size of the second-largest pack, at most 2 GiB - 1, so that the
/// largest pack stays as it is.
fn repack_batch_size(repo: &Repo) -> u64 {
    let objects = repo
        .setup
        .cwd
        .join(crate::repo_objects::objects_dir(&repo.setup));
    let mut sizes: Vec<u64> = std::fs::read_dir(objects.join("pack"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".pack"))
                .filter_map(|e| e.metadata().ok().map(|m| m.len()))
                .collect()
        })
        .unwrap_or_default();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let second = sizes.get(1).copied().unwrap_or(0);
    (second + 1).min((1u64 << 31) - 1)
}

/// What the `geometric-repack` task finds: whether packs need merging
/// and whether everything would end up in one pack.
struct Geometry {
    needs_merge: bool,
    all_into_one: bool,
}

fn split_factor(config: &ConfigSet) -> i64 {
    config_int(config, "maintenance.geometric-repack.splitfactor")
        .ok()
        .flatten()
        .filter(|f| *f > 0)
        .unwrap_or(2)
}

/// Which packs a geometric repack with `factor` would roll up
/// (git-repack(1), `--geometric`): packs without `.keep` and not cruft,
/// by object count.
fn geometry(repo: &Repo, factor: i64) -> Geometry {
    // `Repo::odb` reads through its stores and has no list of the local
    // packs: open the object database the way `repack` does.
    let packs = crate::repo::odb_with_config(&repo.setup, &repo.config)
        .map(|odb| store::local_packs(&odb, &[]))
        .unwrap_or_default();
    let mut counts: Vec<u64> = packs
        .into_iter()
        .filter(|p| !p.keep && !p.base().with_extension("mtimes").exists())
        .map(|p| u64::from(p.pack.index().len()))
        .collect();
    counts.sort_unstable();
    let factor = factor as u64;
    let mut split = 0;
    for i in (1..counts.len()).rev() {
        if counts[i - 1].saturating_mul(factor) > counts[i] {
            split = i;
            break;
        }
    }
    let mut total: u64 = counts[..split].iter().sum();
    let mut rolled = split;
    for &c in &counts[split..] {
        if c < total.saturating_mul(factor) {
            total += c;
            rolled += 1;
        } else {
            break;
        }
    }
    Geometry {
        needs_merge: rolled >= 2,
        all_into_one: rolled == counts.len(),
    }
}

fn geometric_repack(repo: &Repo, children: &Children, opts: &RunOptions) -> bool {
    let factor = split_factor(&repo.config);
    let g = geometry(repo, factor);
    let mut args: Vec<String> = vec!["repack".into(), "-d".into(), "-l".into()];
    if opts.quiet {
        args.push("-q".into());
    }
    if g.all_into_one {
        let expire =
            config_string(&repo.config, "gc.pruneexpire").unwrap_or_else(|| "2.weeks.ago".into());
        args.push("--cruft".into());
        args.push(format!("--cruft-expiration={expire}"));
    } else {
        args.push(format!("--geometric={factor}"));
    }
    args.push("--write-midx".into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    children.run_optional(&args)
}

/// `maintenance.<task>.auto`: `None` when the task must not run, `Some(n)`
/// with `n < 0` when it always runs.
fn auto_limit(config: &ConfigSet, task: Task, default: i64) -> Result<Option<i64>, Die> {
    let n = config_int(config, &format!("maintenance.{}.auto", task.name()))?.unwrap_or(default);
    Ok((n != 0).then_some(n))
}

/// Whether `task` should run with `--auto`.
pub fn auto_condition(repo: &Repo, task: Task) -> Result<bool, Die> {
    let config = &repo.config;
    let objects = repo
        .setup
        .cwd
        .join(crate::repo_objects::objects_dir(&repo.setup));
    let at_least = |limit: Option<i64>, count: &dyn Fn(u64) -> u64| match limit {
        None => false,
        Some(n) if n < 0 => true,
        Some(n) => count(n as u64) >= n as u64,
    };
    Ok(match task {
        // No condition makes prefetching worthwhile.
        Task::Prefetch => false,
        Task::LooseObjects => {
            let limit = auto_limit(config, task, 100)?;
            at_least(limit, &|n| count_loose(&objects, repo.hash, n))
        }
        Task::IncrementalRepack => {
            if !multi_pack_index_enabled(config) {
                return Ok(false);
            }
            let limit = auto_limit(config, task, 10)?;
            at_least(limit, &|_| packs_outside_midx(&objects))
        }
        Task::GeometricRepack => match auto_limit(config, task, 6700)? {
            None => false,
            Some(n) if n < 0 => true,
            Some(n) => {
                geometry(repo, split_factor(config)).needs_merge
                    || loose_in_17(&objects, repo.hash) > (n + 255) / 256
            }
        },
        Task::Gc => {
            let gc_auto = config_int(config, "gc.auto")?.unwrap_or(6700);
            if gc_auto <= 0 {
                return Ok(false);
            }
            let limit = config_int(config, "gc.autopacklimit")?.unwrap_or(50);
            (limit > 0 && crate::commands::gc::count_packs(&objects) > limit)
                || crate::commands::gc::too_many_loose(&objects, gc_auto, repo.hash)
        }
        Task::CommitGraph => {
            if config_bool(config, "core.commitgraph")? == Some(false) {
                return Ok(false);
            }
            let limit = auto_limit(config, task, 100)?;
            at_least(limit, &|n| commits_outside_graph(repo, &objects, n))
        }
        Task::PackRefs => match repo.setup.format.ref_storage {
            rgit_repo::RefStorage::Reftable => true,
            _ => crate::commands::pack_refs::needs_packing(&repo.setup),
        },
        Task::ReflogExpire => {
            let limit = auto_limit(config, task, 100)?;
            match limit {
                None => false,
                Some(n) if n < 0 => true,
                Some(n) => {
                    let date = rgit_date::DateContext::from_env();
                    let settings = crate::reflog_expire::Config::load(
                        config,
                        crate::reflog_expire::Explicit::default(),
                        &date,
                    )?;
                    crate::reflog_expire::expired_head_entries(repo, &settings) as i64 >= n
                }
            }
        }
        Task::WorktreePrune => {
            let limit = auto_limit(config, task, 1)?;
            at_least(limit, &|_| prunable_worktrees(repo))
        }
        Task::RerereGc => {
            let limit = auto_limit(config, task, 512)?;
            at_least(limit, &|_| stale_rerere_estimate(repo))
        }
    })
}

/// Loose objects, counting stops at `limit`.
fn count_loose(objects: &Path, hash: HashKind, limit: u64) -> u64 {
    let mut n = 0;
    for fan in 0..=255u8 {
        n += store::fan_entries(objects, fan, hash)
            .iter()
            .filter(|e| e.id.is_some())
            .count() as u64;
        if n >= limit {
            break;
        }
    }
    n
}

fn loose_in_17(objects: &Path, hash: HashKind) -> i64 {
    store::fan_entries(objects, 0x17, hash)
        .iter()
        .filter(|e| e.id.is_some())
        .count() as i64
}

/// Local packs the multi-pack-index does not list.
fn packs_outside_midx(objects: &Path) -> u64 {
    let pack_dir = objects.join("pack");
    let listed = midx_pack_names(&pack_dir);
    std::fs::read_dir(&pack_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".idx") && n.starts_with("pack-"))
                .filter(|n| !listed.contains(n))
                .count() as u64
        })
        .unwrap_or(0)
}

/// The pack index names (`pack-<hash>.idx`) listed by `multi-pack-index`
/// and by the layers of `multi-pack-index.d` (gitformat-pack(5), chunk
/// `PNAM`).
fn midx_pack_names(pack_dir: &Path) -> HashSet<String> {
    let mut files = vec![pack_dir.join("multi-pack-index")];
    let chain_dir = pack_dir.join("multi-pack-index.d");
    if let Ok(chain) = std::fs::read_to_string(chain_dir.join("multi-pack-index-chain")) {
        for line in chain.lines().filter(|l| !l.is_empty()) {
            files.push(chain_dir.join(format!("multi-pack-index-{line}.midx")));
        }
    }
    let mut out = HashSet::new();
    for f in files {
        let Ok(data) = std::fs::read(&f) else {
            continue;
        };
        if let Some(pnam) = chunk(&data, b"MIDX", b"PNAM") {
            for name in pnam.split(|&b| b == 0).filter(|n| !n.is_empty()) {
                let name = String::from_utf8_lossy(name).into_owned();
                let name = match name.strip_suffix(".pack") {
                    Some(base) => format!("{base}.idx"),
                    None => name,
                };
                out.insert(name);
            }
        }
    }
    out
}

/// The contents of chunk `id` of a chunked file whose header starts with
/// `magic` and has the chunk count at byte 6: 8 bytes for a commit-graph,
/// 12 for a multi-pack-index (gitformat-commit-graph(5), gitformat-pack(5)).
fn chunk<'a>(data: &'a [u8], magic: &[u8; 4], id: &[u8; 4]) -> Option<&'a [u8]> {
    let table = if magic == b"MIDX" { 12 } else { 8 };
    if data.len() < table || &data[..4] != magic {
        return None;
    }
    let count = data[6] as usize;
    let entry = |i: usize| -> Option<(&[u8], u64)> {
        let at = table + i * 12;
        let e = data.get(at..at + 12)?;
        Some((&e[..4], u64::from_be_bytes(e[4..12].try_into().ok()?)))
    };
    for i in 0..count {
        let (cid, start) = entry(i)?;
        let (_, end) = entry(i + 1)?;
        if cid == id {
            return data.get(start as usize..end as usize);
        }
    }
    None
}

/// The commits listed by the commit-graph (single file and split chain).
fn graph_commits(objects: &Path, hash: HashKind) -> HashSet<ObjectId> {
    let info = objects.join("info");
    let mut files = vec![info.join("commit-graph")];
    let dir = info.join("commit-graphs");
    if let Ok(chain) = std::fs::read_to_string(dir.join("commit-graph-chain")) {
        for line in chain.lines().filter(|l| !l.is_empty()) {
            files.push(dir.join(format!("graph-{line}.graph")));
        }
    }
    let len = hash.byte_len();
    let mut out = HashSet::new();
    for f in files {
        let Ok(data) = std::fs::read(&f) else {
            continue;
        };
        if let Some(oids) = chunk(&data, b"CGPH", b"OIDL") {
            for raw in oids.chunks_exact(len) {
                if let Ok(id) = ObjectId::from_bytes(hash, raw) {
                    out.insert(id);
                }
            }
        }
    }
    out
}

/// Commits reachable from the references (and `HEAD`) that the
/// commit-graph does not contain; counting stops at `limit`.
fn commits_outside_graph(repo: &Repo, objects: &Path, limit: u64) -> u64 {
    let graph = graph_commits(objects, repo.hash);
    let Ok(roots) = crate::housekeeping::reach::roots(&repo.setup, repo.odb()) else {
        return 0;
    };
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();
    for (_, id) in roots.refs {
        let mut id = id;
        // Peel tags.
        loop {
            match repo.odb().read(&id) {
                Ok(Some(o)) if o.kind == rgit_object::ObjectKind::Tag => {
                    match rgit_object::Tag::parse(repo.hash, &o.data).and_then(|t| t.object()) {
                        Ok(target) => id = target,
                        Err(_) => break,
                    }
                }
                Ok(Some(o)) if o.kind == rgit_object::ObjectKind::Commit => {
                    stack.push(id);
                    break;
                }
                _ => break,
            }
        }
    }
    let mut n = 0;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) || graph.contains(&id) {
            continue;
        }
        let Ok(Some(o)) = repo.odb().read(&id) else {
            continue;
        };
        if o.kind != rgit_object::ObjectKind::Commit {
            continue;
        }
        n += 1;
        if n >= limit {
            break;
        }
        if let Ok(c) = rgit_object::Commit::parse(repo.hash, &o.data) {
            if let Ok(parents) = c.parents() {
                stack.extend(parents);
            }
        }
    }
    n
}

/// Linked worktrees `worktree prune` would remove.
fn prunable_worktrees(repo: &Repo) -> u64 {
    let expire = config_string(&repo.config, "gc.worktreepruneexpire")
        .unwrap_or_else(|| "3.months.ago".into());
    if expire == "never" {
        return 0;
    }
    let date = rgit_date::DateContext::from_env();
    let Ok(secs) = rgit_date::parse_expiry_date(&expire, &date) else {
        return 0;
    };
    // `now` and the like are beyond any timestamp: everything is old enough.
    let cutoff = SystemTime::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(secs))
        .unwrap_or_else(|| SystemTime::now() + std::time::Duration::from_secs(86400));
    let common = repo.setup.absolute_common_dir();
    crate::worktrees::linked_ids(&common)
        .into_iter()
        .filter(|id| {
            crate::worktrees::prune_reason(&common.join("worktrees").join(id), Some(cutoff))
                .is_some()
        })
        .count() as u64
}

/// git's estimate of the stale `rr-cache` entries: the stale ones whose
/// name starts with `17`, times 256. Resolved entries (with a
/// `postimage`) go stale after `gc.rerereResolved` (60 days), unresolved
/// ones after `gc.rerereUnresolved` (15 days).
fn stale_rerere_estimate(repo: &Repo) -> u64 {
    let dir = repo.setup.absolute_common_dir().join("rr-cache");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let cutoff = |key: &str, days: u64| -> u64 {
        let date = rgit_date::DateContext::from_env();
        match config_string(&repo.config, key) {
            Some(v) => match v.parse::<u64>() {
                Ok(d) => now.saturating_sub(d * 86400),
                Err(_) => rgit_date::parse_expiry_date(&v, &date).unwrap_or(0),
            },
            None => now.saturating_sub(days * 86400),
        }
    };
    let resolved = cutoff("gc.rerereresolved", 60);
    let unresolved = cutoff("gc.rerereunresolved", 15);
    let mtime = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    };
    let mut stale = 0;
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with("17") {
            continue;
        }
        let entry = e.path();
        let post = mtime(&entry.join("postimage"));
        let is_stale = match post {
            Some(t) => t < resolved,
            None => mtime(&entry.join("preimage")).is_some_and(|t| t < unresolved),
        };
        if is_stale {
            stale += 1;
        }
    }
    stale * 256
}

#[cfg(test)]
#[path = "tasks_tests.rs"]
mod tests;
