// SPDX-License-Identifier: MIT
//! `fetch --recurse-submodules`, `fetch.recurseSubmodules`,
//! `submodule.<name>.fetchRecurseSubmodules` and `submodule.fetchJobs`
//! (Documentation/fetch-options.adoc, config/fetch.adoc,
//! config/submodule.adoc, gitsubmodules(7)); the process layout (one
//! `fetch` per submodule, run in its git directory, with
//! `--recurse-submodules-default` and `--submodule-prefix`) was observed
//! with `GIT_TRACE`.
//!
//! After the superproject's own fetch:
//!
//! - the commits it fetched (reachable from the fetched tips, not from the
//!   refs as they were before) are searched for gitlinks that changed;
//! - each submodule of the index (with a work tree) and each changed one
//!   is fetched when its mode says so: `yes` always, `on-demand` when a
//!   commit recorded in the fetched history is missing in the submodule,
//!   `no` never. The command line wins over `fetchRecurseSubmodules` of
//!   the submodule, which wins over `fetch.recurseSubmodules`, then
//!   `submodule.recurse`, then the default (`on-demand`, or what the
//!   superproject's fetch passed down);
//! - recorded commits still missing afterwards (outside the refspecs) are
//!   fetched by id from the submodule's default remote.

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_object::{Object, ObjectKind};
use rgit_revwalk::walk::{Walk, WalkOptions};
use rgit_submodule::{FetchRecurse, Gitmodules, Mode, Origin};

use crate::messages::fetch as msg;
use crate::refs_support::Objects;
use crate::repository::Repo;

/// How the superproject's fetch was asked to recurse.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// `--[no-]recurse-submodules[=<mode>]`.
    pub cli: Option<FetchRecurse>,
    /// `--recurse-submodules-default=<mode>` (a fetch run for a submodule).
    pub default: Option<FetchRecurse>,
    /// `--submodule-prefix=<path>/`.
    pub prefix: String,
    /// `--jobs`.
    pub jobs: Option<i64>,
    /// Options passed on to the submodules' fetches.
    pub child_args: Vec<String>,
    /// `--quiet`: no "Fetching submodule" lines.
    pub quiet: bool,
}

/// Parse a `--recurse-submodules` value (`yes`, `on-demand`, `no`, or a
/// boolean); `None` when it is none of these.
pub fn parse_mode(value: &str) -> Option<FetchRecurse> {
    rgit_submodule::parse_fetch_recurse(Some(value.as_bytes()))
}

/// `--recurse-submodules=<mode>` for a child fetch.
pub fn option_value(m: FetchRecurse) -> String {
    format!("--recurse-submodules={}", mode_name(m))
}

fn mode_name(m: FetchRecurse) -> &'static str {
    match m {
        FetchRecurse::Yes => "yes",
        FetchRecurse::No => "no",
        FetchRecurse::OnDemand => "on-demand",
    }
}

/// `fetch.recurseSubmodules`, else `submodule.recurse`.
fn configured(config: &ConfigSet) -> Option<FetchRecurse> {
    if let Some(e) = config.get("fetch.recursesubmodules") {
        if let Some(m) = rgit_submodule::parse_fetch_recurse(e.value.as_deref().map(|v| v.as_ref()))
        {
            return Some(m);
        }
    }
    rgit_submodule::settings::recurse(config).then_some(FetchRecurse::Yes)
}

/// Every commit the refs point to (peeled).
pub fn ref_tips(repo: &Repo) -> Vec<ObjectId> {
    let objects = Objects::open(&repo.setup);
    let store = crate::refs_support::ref_store(&repo.setup, &repo.config);
    use rgit_refs::RefStore as _;
    let mut out = Vec::new();
    for item in store
        .iter(&rgit_refs::IterOptions::prefix(""))
        .unwrap_or_default()
    {
        if let rgit_refs::IterItem::Ref(r) = item {
            if let Some(c) = r
                .oid
                .and_then(|id| crate::switch_branch::peel_commit(&objects, &id))
            {
                out.push(c);
            }
        }
    }
    out
}

/// A submodule whose gitlink changed in fetched commits.
#[derive(Debug, Default)]
struct Changed {
    path: Vec<u8>,
    /// The superproject commit it was first seen in.
    commit: Option<ObjectId>,
    /// The recorded commits.
    ids: Vec<ObjectId>,
    /// `.gitmodules` of that commit says this about it.
    gitmodules: Option<rgit_submodule::Submodule>,
}

/// The submodules (by name) whose gitlinks changed in the commits
/// reachable from `new` but not from `old`.
fn changed_submodules(
    repo: &Repo,
    objects: &Objects,
    new: &[ObjectId],
    old: &[ObjectId],
) -> Vec<(Vec<u8>, Changed)> {
    let mut walk = Walk::new(
        crate::switch_branch::CommitReader(objects),
        WalkOptions::default(),
    );
    let mut any = false;
    for id in new {
        if let Some(c) = crate::switch_branch::peel_commit(objects, id) {
            any |= walk.push(&c, false).is_ok();
        }
    }
    if !any {
        return Vec::new();
    }
    for id in old {
        let _ = walk.push(id, true);
    }
    let commits = walk.collect_all().unwrap_or_default();
    let mut out: Vec<(Vec<u8>, Changed)> = Vec::new();
    let mut by_name: HashMap<Vec<u8>, usize> = HashMap::new();
    for c in commits {
        let Some(Object::Commit(commit)) = repo.read_object(&c) else {
            continue;
        };
        let Ok(tree) = commit.tree() else { continue };
        let parents = commit.parents().unwrap_or_default();
        let mut found = Vec::new();
        if parents.is_empty() {
            gitlink_changes(repo, None, Some(&tree), b"", &mut found);
        }
        for p in &parents {
            let ptree = match repo.read_object(p) {
                Some(Object::Commit(pc)) => pc.tree().ok(),
                _ => None,
            };
            gitlink_changes(repo, ptree.as_ref(), Some(&tree), b"", &mut found);
        }
        if found.is_empty() {
            continue;
        }
        let gm = commit_gitmodules(repo, &tree);
        for (path, id) in found {
            let sub = gm.as_ref().and_then(|g| g.by_path(&path)).cloned();
            let name = sub
                .as_ref()
                .map_or_else(|| path.clone(), |s| s.name.to_vec());
            let slot = *by_name.entry(name.clone()).or_insert_with(|| {
                out.push((
                    name,
                    Changed {
                        path: path.clone(),
                        commit: Some(c),
                        ids: Vec::new(),
                        gitmodules: sub.clone(),
                    },
                ));
                out.len() - 1
            });
            if !out[slot].1.ids.contains(&id) {
                out[slot].1.ids.push(id);
            }
        }
    }
    out
}

/// Whether a commit reachable from `head` but not from `base` changes a
/// gitlink (`pull --rebase --recurse-submodules` refuses then).
pub fn touches_submodules(repo: &Repo, base: &ObjectId, head: &ObjectId) -> bool {
    let objects = Objects::open(&repo.setup);
    let mut walk = Walk::new(
        crate::switch_branch::CommitReader(&objects),
        WalkOptions::default(),
    );
    if walk.push(head, false).is_err() {
        return false;
    }
    let _ = walk.push(base, true);
    for c in walk.collect_all().unwrap_or_default() {
        let Some(Object::Commit(commit)) = repo.read_object(&c) else {
            continue;
        };
        let Ok(tree) = commit.tree() else { continue };
        let mut found = Vec::new();
        for p in commit.parents().unwrap_or_default() {
            let ptree = match repo.read_object(&p) {
                Some(Object::Commit(pc)) => pc.tree().ok(),
                _ => None,
            };
            gitlink_changes(repo, ptree.as_ref(), Some(&tree), b"", &mut found);
        }
        if !found.is_empty() {
            return true;
        }
    }
    false
}

fn tree_entries(repo: &Repo, id: Option<&ObjectId>) -> Vec<rgit_object::TreeEntry> {
    match id.and_then(|i| repo.read_object(i)) {
        Some(Object::Tree(t)) => t.entries,
        _ => Vec::new(),
    }
}

/// Gitlinks of `new` that differ from `old` (added or changed), with their
/// full paths.
pub(crate) fn gitlink_changes(
    repo: &Repo,
    old: Option<&ObjectId>,
    new: Option<&ObjectId>,
    base: &[u8],
    out: &mut Vec<(Vec<u8>, ObjectId)>,
) {
    if old == new {
        return;
    }
    let olds = tree_entries(repo, old);
    for e in tree_entries(repo, new) {
        let before = olds.iter().find(|o| o.name == e.name);
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        match e.mode.object_kind() {
            ObjectKind::Commit => {
                let same = before
                    .is_some_and(|b| b.mode.object_kind() == ObjectKind::Commit && b.oid == e.oid);
                if !same {
                    out.push((path, e.oid));
                }
            }
            ObjectKind::Tree => {
                let prev = before
                    .filter(|b| b.mode.object_kind() == ObjectKind::Tree)
                    .map(|b| b.oid);
                if prev != Some(e.oid) {
                    path.push(b'/');
                    gitlink_changes(repo, prev.as_ref(), Some(&e.oid), &path, out);
                }
            }
            _ => {}
        }
    }
}

/// The `.gitmodules` of the tree `tree`.
pub(crate) fn commit_gitmodules(repo: &Repo, tree: &ObjectId) -> Option<Gitmodules> {
    let e = tree_entries(repo, Some(tree))
        .into_iter()
        .find(|e| e.name == rgit_submodule::GITMODULES.as_bytes())?;
    let Some(Object::Blob(b)) = repo.read_object(&e.oid) else {
        return None;
    };
    let origin = Origin::Blob(format!("{}:.gitmodules", tree.to_hex()));
    Gitmodules::parse(&b.data, &origin, Mode::History)
        .ok()
        .map(|(g, _)| g)
}

/// One submodule to fetch.
struct Task {
    /// Path as shown (`--submodule-prefix` included).
    shown: String,
    /// Path in the superproject (for `submodule--helper`).
    path: String,
    git_dir: PathBuf,
    /// " at commit <abbrev>" when found in fetched history only.
    at_commit: Option<String>,
    mode: FetchRecurse,
    /// Recorded commits that must end up in the submodule.
    wanted: Vec<ObjectId>,
}

/// Fetch the submodules after the superproject fetched `new` (the refs
/// were `old` before). Returns the exit code contribution (1 when a
/// submodule fetch failed).
pub fn fetch(repo: &Repo, opts: &Options, old: &[ObjectId], new: &[ObjectId]) -> i32 {
    if opts.cli == Some(FetchRecurse::No) {
        return 0;
    }
    let config = &repo.config;
    let fallback = configured(config)
        .or(opts.default)
        .unwrap_or(FetchRecurse::OnDemand);
    let objects = Objects::open(&repo.setup);
    let changed = changed_submodules(repo, &objects, new, old);
    let abbrev_len = repo.default_abbrev();
    let jobs = match opts.jobs {
        Some(n) => n,
        None => config
            .int("submodule.fetchjobs")
            .ok()
            .flatten()
            .unwrap_or(1),
    };
    let jobs = if jobs <= 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get() as i64)
    } else {
        jobs
    };

    let mut tasks: Vec<Task> = Vec::new();
    let mut broken = false;
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    // The submodules of the index, when there is a work tree.
    if let Some(wt) = repo.setup.work_tree.as_ref() {
        let root = repo.setup.cwd.join(wt);
        if let Ok((index, _)) = repo.read_index() {
            let gm = crate::submodules::load_gitmodules(repo, Some(&index)).unwrap_or_default();
            for e in index.entries() {
                if !e.mode.is_gitlink() || e.stage().is_conflict() {
                    continue;
                }
                let path = e.path().to_vec();
                let sub = gm.by_path(&path);
                let name = sub.map_or_else(|| path.clone(), |s| s.name.to_vec());
                if !seen.insert(name.clone()) {
                    continue;
                }
                let dir = crate::diff::collect::os_path(&root, &path);
                let shown = format!("{}{}", opts.prefix, String::from_utf8_lossy(&path));
                if let Some(g) = rgit_submodule::gitdir::git_dir_of(&dir) {
                    if g.join("HEAD").exists() && !crate::submodules::is_git_dir(&g) {
                        eprintln!("{}", crate::messages::submodule::could_not_access(&shown));
                        broken = true;
                        continue;
                    }
                }
                let git_dir = match rgit_submodule::gitdir::git_dir_of(&dir)
                    .filter(|g| g.join("HEAD").exists())
                {
                    Some(g) => g,
                    None => {
                        let m = rgit_submodule::gitdir::modules_dir(
                            &repo.setup.absolute_git_dir(),
                            &name,
                        );
                        if !m.join("HEAD").exists() {
                            continue;
                        }
                        m
                    }
                };
                let mode = opts
                    .cli
                    .or_else(|| rgit_submodule::settings::fetch_recurse(config, &name, sub))
                    .unwrap_or(fallback);
                let wanted = changed
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, c)| c.ids.clone())
                    .unwrap_or_default();
                tasks.push(Task {
                    shown: format!("{}{}", opts.prefix, String::from_utf8_lossy(&path)),
                    path: String::from_utf8_lossy(&path).into_owned(),
                    git_dir,
                    at_commit: None,
                    mode,
                    wanted,
                });
            }
        }
    }
    // Submodules only found in the fetched history.
    for (name, c) in &changed {
        if !seen.insert(name.clone()) {
            continue;
        }
        if !rgit_submodule::is_active(config, name, &c.path) {
            continue;
        }
        let git_dir = rgit_submodule::gitdir::modules_dir(&repo.setup.absolute_git_dir(), name);
        if !git_dir.join("HEAD").exists() {
            continue;
        }
        let mode = opts
            .cli
            .or_else(|| {
                rgit_submodule::settings::fetch_recurse(config, name, c.gitmodules.as_ref())
            })
            .unwrap_or(fallback);
        tasks.push(Task {
            shown: format!("{}{}", opts.prefix, String::from_utf8_lossy(&c.path)),
            path: String::from_utf8_lossy(&c.path).into_owned(),
            git_dir,
            at_commit: c.commit.map(|id| repo.abbrev(&id, abbrev_len)),
            mode,
            wanted: c.ids.clone(),
        });
    }

    trace_parallel(jobs);
    let mut runnable = Vec::new();
    for t in tasks {
        let run = match t.mode {
            FetchRecurse::No => false,
            FetchRecurse::Yes => true,
            FetchRecurse::OnDemand => !t.wanted.is_empty() && missing(&t.git_dir, &t.wanted),
        };
        if run {
            runnable.push(t);
        }
    }
    let mut failed: Vec<String> = Vec::new();
    let results = run_all(&runnable, opts, jobs as usize);
    for (t, ok) in runnable.iter().zip(results) {
        let mut ok = ok;
        // Recorded commits outside the refspecs: fetched by id.
        if ok && !t.wanted.is_empty() && missing(&t.git_dir, &t.wanted) {
            ok = fetch_by_id(t, opts);
        }
        if !ok {
            failed.push(t.shown.clone());
        }
    }
    trace_parallel_done();
    if failed.is_empty() {
        return i32::from(broken);
    }
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}", msg::SUBMODULE_ERRORS);
    for f in &failed {
        let _ = writeln!(err, "\t{f}");
    }
    1
}

/// Whether one of `ids` is not a commit reachable from a ref of the
/// repository `git_dir`.
fn missing(git_dir: &Path, ids: &[ObjectId]) -> bool {
    let mut args: Vec<String> = vec!["rev-list".into(), "-n".into(), "1".into()];
    args.extend(ids.iter().map(ObjectId::to_hex));
    args.extend(["--not".into(), "--all".into()]);
    let Some(mut cmd) = git_in(git_dir) else {
        return true;
    };
    cmd.args(&args).stdin(Stdio::null()).stderr(Stdio::null());
    match cmd.output() {
        Ok(out) => !out.status.success() || !out.stdout.is_empty(),
        Err(_) => true,
    }
}

/// This binary, to be run in the submodule's git directory.
fn git_in(git_dir: &Path) -> Option<Command> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    crate::submodules::prepare_env(&mut cmd);
    cmd.env_remove("GIT_PREFIX");
    cmd.current_dir(git_dir).env("GIT_DIR", ".");
    Some(cmd)
}

/// The command for a task's fetch. A submodule found only in history may
/// have lost its work tree (its `core.worktree` names a removed
/// directory), so its git directory stands in for one.
fn task_cmd(t: &Task) -> Option<Command> {
    let mut cmd = git_in(&t.git_dir)?;
    if t.at_commit.is_some() {
        cmd.env("GIT_WORK_TREE", ".");
    }
    Some(cmd)
}

fn fetch_args(t: &Task, opts: &Options) -> Vec<String> {
    let mut args: Vec<String> = vec!["fetch".into()];
    args.extend(opts.child_args.iter().cloned());
    if let Some(m) = opts.cli {
        args.push(format!("--recurse-submodules={}", mode_name(m)));
    }
    args.push("--recurse-submodules-default".into());
    args.push(mode_name(t.mode).into());
    args.push(format!("--submodule-prefix={}/", t.shown));
    args
}

fn header(t: &Task) -> String {
    msg::fetching_submodule(&t.shown, t.at_commit.as_deref())
}

/// Runs the fetch of every task, at most `jobs` at a time; whether each
/// succeeded. Output appears in task order: directly when one at a time,
/// collected per task otherwise.
fn run_all(tasks: &[Task], opts: &Options, jobs: usize) -> Vec<bool> {
    if jobs <= 1 || tasks.len() <= 1 {
        return tasks
            .iter()
            .map(|t| {
                if !opts.quiet {
                    eprintln!("{}", header(t));
                }
                let Some(mut cmd) = task_cmd(t) else {
                    return false;
                };
                cmd.args(fetch_args(t, opts)).stdin(Stdio::null());
                // The submodule's standard output goes to standard error.
                cmd.stdout(Stdio::from(std::io::stderr()));
                cmd.status().is_ok_and(|s| s.success())
            })
            .collect();
    }
    let mut results = vec![false; tasks.len()];
    let mut outputs: Vec<Option<Vec<u8>>> = vec![None; tasks.len()];
    let mut next_print = 0;
    let mut running: Vec<(usize, Child, std::thread::JoinHandle<Vec<u8>>)> = Vec::new();
    let mut next = 0;
    while next < tasks.len() || !running.is_empty() {
        while running.len() < jobs && next < tasks.len() {
            let t = &tasks[next];
            let spawned = task_cmd(t).and_then(|mut cmd| {
                let (reader, writer) = std::io::pipe().ok()?;
                let w2 = writer.try_clone().ok()?;
                cmd.args(fetch_args(t, opts))
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(writer))
                    .stderr(Stdio::from(w2));
                let child = cmd.spawn().ok()?;
                drop(cmd);
                let reader_thread = std::thread::spawn(move || {
                    let mut r = reader;
                    let mut buf = Vec::new();
                    let _ = r.read_to_end(&mut buf);
                    buf
                });
                Some((child, reader_thread))
            });
            match spawned {
                Some((child, th)) => running.push((next, child, th)),
                None => outputs[next] = Some(Vec::new()),
            }
            next += 1;
        }
        // Wait for the oldest one (output is kept in order anyway).
        if let Some((i, mut child, th)) = (!running.is_empty()).then(|| running.remove(0)) {
            results[i] = child.wait().is_ok_and(|s| s.success());
            outputs[i] = Some(th.join().unwrap_or_default());
        }
        while next_print < tasks.len() {
            let Some(out) = outputs[next_print].take() else {
                break;
            };
            let mut err = std::io::stderr().lock();
            if !opts.quiet {
                let _ = writeln!(err, "{}", header(&tasks[next_print]));
            }
            let _ = err.write_all(&out);
            next_print += 1;
        }
    }
    results
}

/// `fetch <remote> <id>...` for recorded commits the regular fetch did not
/// bring; the remote is the submodule's default remote
/// (`submodule--helper get-default-remote`).
fn fetch_by_id(t: &Task, opts: &Options) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let remote = Command::new(&exe)
        .args(["submodule--helper", "get-default-remote", &t.path])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "origin".to_owned());
    let Some(mut cmd) = task_cmd(t) else {
        return false;
    };
    let mut args = fetch_args(t, opts);
    args.push(remote);
    args.extend(t.wanted.iter().map(ObjectId::to_hex));
    cmd.args(args).stdin(Stdio::null());
    cmd.stdout(Stdio::from(std::io::stderr()));
    cmd.status().is_ok_and(|s| s.success())
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
