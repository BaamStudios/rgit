// SPDX-License-Identifier: MIT
//! `push --recurse-submodules=(check|on-demand|only|no)` and
//! `push.recurseSubmodules` (Documentation/git-push.adoc,
//! config/push.adoc); messages and the order of the checks were observed
//! from the `git` binary.
//!
//! The submodule commits recorded by the commits being pushed (reachable
//! from the new values, not from what the remote has) must be on a remote
//! of their submodule: `check` refuses the push otherwise, `on-demand`
//! pushes those submodules first (running `push` in each), `only` pushes
//! them and not the superproject. A submodule without any remote is not
//! checked.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_object::Object;
use rgit_revwalk::walk::{Walk, WalkOptions};

use crate::Die;
use crate::messages::push as msg;
use crate::refs_support::Objects;
use crate::repository::Repo;

/// How to treat submodules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    No,
    Check,
    OnDemand,
    Only,
    /// What a superproject's `on-demand` push passes to a submodule:
    /// recurse further only if the submodule is configured to.
    OnlyIsOnDemand,
}

fn parse(value: &str) -> Option<Mode> {
    Some(match value {
        "check" => Mode::Check,
        "on-demand" => Mode::OnDemand,
        "only" => Mode::Only,
        "only-is-on-demand" => Mode::OnlyIsOnDemand,
        "no" | "false" | "off" | "0" => Mode::No,
        _ => return None,
    })
}

/// `--recurse-submodules=<value>`.
///
/// # Errors
/// A value that is none of the modes.
pub fn parse_cli(value: &str) -> Result<Mode, String> {
    parse(value).ok_or_else(|| msg::bad_recurse_submodules(value))
}

/// `push.recurseSubmodules` (or `submodule.recurse`, which means
/// `on-demand`).
fn configured(config: &ConfigSet) -> Result<Mode, Die> {
    if let Some(e) = config.get("push.recursesubmodules") {
        let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
        return parse(&v)
            .filter(|m| *m != Mode::OnlyIsOnDemand)
            .ok_or_else(|| Die::Fatal(msg::bad_push_recurse_submodules(&v)));
    }
    Ok(if rgit_submodule::settings::recurse(config) {
        Mode::OnDemand
    } else {
        Mode::No
    })
}

/// The mode of a push: the command line, else the configuration. A
/// superproject's `only-is-on-demand` recurses on demand only where the
/// configuration asks for recursion.
///
/// # Errors
/// A bad `push.recurseSubmodules`.
pub fn effective(cli: Option<Mode>, config: &ConfigSet) -> Result<Mode, Die> {
    match cli {
        Some(Mode::OnlyIsOnDemand) => Ok(match configured(config)? {
            Mode::Only => {
                eprintln!("warning: {}", msg::ONLY_IS_ON_DEMAND);
                Mode::OnDemand
            }
            Mode::OnDemand => Mode::OnDemand,
            _ => Mode::No,
        }),
        Some(m) => Ok(m),
        None => configured(config),
    }
}

/// What the push sends.
pub struct Request<'a> {
    pub mode: Mode,
    pub dry_run: bool,
    /// The remote's name when it is a configured remote (passed on to the
    /// submodules with the refspecs).
    pub remote: Option<&'a str>,
    /// The refspecs of the command line.
    pub refspecs: &'a [String],
    /// The superproject's current branch (full name).
    pub head: Option<&'a str>,
    /// `--push-option`s, passed on.
    pub push_options: &'a [String],
}

/// A submodule with commits the remote does not have.
struct Unpushed {
    path: String,
    dir: PathBuf,
}

/// Checks or pushes the submodules for a push of `new` to a remote that
/// has `remote_has`. Returns whether the superproject's own push goes on
/// (false for `only`). Refusals are fatal.
///
/// # Errors
/// `check` with unpushed submodule commits, a submodule that could not
/// be pushed, a gitlink naming a tag.
pub fn run(
    repo: &Repo,
    req: &Request<'_>,
    new: &[ObjectId],
    remote_has: &[ObjectId],
) -> Result<bool, Die> {
    if req.mode == Mode::No {
        return Ok(true);
    }
    let unpushed = find_unpushed(repo, new, remote_has)?;
    match req.mode {
        Mode::Check => {
            if !unpushed.is_empty() {
                let mut text = String::from(msg::UNPUSHED_HEAD);
                for u in &unpushed {
                    text.push_str(&format!("  {}\n", u.path));
                }
                text.push_str(msg::UNPUSHED_TAIL);
                eprint!("{text}");
                return Err(Die::Fatal(msg::ABORTING.into()));
            }
            Ok(true)
        }
        Mode::OnDemand | Mode::Only | Mode::OnlyIsOnDemand => {
            if let Some(remote) = req.remote {
                for u in &unpushed {
                    push_check(u, remote, req)?;
                }
            }
            let mut ok = true;
            for u in &unpushed {
                eprintln!("{}", msg::pushing_submodule(&u.path));
                if !push_one(u, req) {
                    eprintln!("{}", msg::unable_to_push_submodule(&u.path));
                    ok = false;
                }
            }
            // What could not be pushed after all (e.g. a detached HEAD
            // whose commits no ref pushes) fails the push too.
            if ok && !req.dry_run && !unpushed.is_empty() {
                ok = find_unpushed(repo, new, remote_has)?.is_empty();
            }
            if !ok {
                return Err(Die::Fatal(msg::FAILED_SUBMODULES.into()));
            }
            Ok(req.mode != Mode::Only)
        }
        Mode::No => Ok(true),
    }
}

/// The submodules (in order of path) whose recorded commits in the pushed
/// history are not on any of their remotes.
fn find_unpushed(
    repo: &Repo,
    new: &[ObjectId],
    remote_has: &[ObjectId],
) -> Result<Vec<Unpushed>, Die> {
    let objects = Objects::open(&repo.setup);
    let mut walk = Walk::new(
        crate::switch_branch::CommitReader(&objects),
        WalkOptions::default(),
    );
    let mut any = false;
    for id in new {
        if let Some(c) = crate::switch_branch::peel_commit(&objects, id) {
            any |= walk.push(&c, false).is_ok();
        }
    }
    if !any {
        return Ok(Vec::new());
    }
    for id in remote_has {
        if let Some(c) = crate::switch_branch::peel_commit(&objects, id) {
            let _ = walk.push(&c, true);
        }
    }
    // path -> (name, recorded commits)
    let mut subs: Vec<(Vec<u8>, Vec<u8>, Vec<ObjectId>)> = Vec::new();
    for c in walk.collect_all().unwrap_or_default() {
        let Some(Object::Commit(commit)) = repo.read_object(&c) else {
            continue;
        };
        let Ok(tree) = commit.tree() else { continue };
        let parents = commit.parents().unwrap_or_default();
        let mut found = Vec::new();
        if parents.is_empty() {
            crate::fetch_submodules::gitlink_changes(repo, None, Some(&tree), b"", &mut found);
        }
        for p in &parents {
            let ptree = match repo.read_object(p) {
                Some(Object::Commit(pc)) => pc.tree().ok(),
                _ => None,
            };
            crate::fetch_submodules::gitlink_changes(
                repo,
                ptree.as_ref(),
                Some(&tree),
                b"",
                &mut found,
            );
        }
        if found.is_empty() {
            continue;
        }
        let gm = crate::fetch_submodules::commit_gitmodules(repo, &tree);
        for (path, id) in found {
            let name = gm
                .as_ref()
                .and_then(|g| g.by_path(&path))
                .map_or_else(|| path.clone(), |s| s.name.to_vec());
            match subs.iter_mut().find(|(p, _, _)| *p == path) {
                Some((_, _, ids)) => {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                None => subs.push((path, name, vec![id])),
            }
        }
    }
    subs.sort_by(|a, b| a.0.cmp(&b.0));
    let Some(wt) = repo.setup.work_tree.as_ref() else {
        return Ok(Vec::new());
    };
    let root = repo.setup.cwd.join(wt);
    let mut out = Vec::new();
    for (path, name, ids) in subs {
        let dir = crate::diff::collect::os_path(&root, &path);
        let git_dir = match rgit_submodule::gitdir::git_dir_of(&dir)
            .filter(|g| g.join("HEAD").exists())
        {
            Some(g) => g,
            None => {
                let m = rgit_submodule::gitdir::modules_dir(&repo.setup.absolute_git_dir(), &name);
                if !m.join("HEAD").exists() {
                    continue;
                }
                m
            }
        };
        let shown = String::from_utf8_lossy(&path).into_owned();
        for id in &ids {
            if run_in(&git_dir, &["cat-file", "-t", &id.to_hex()])
                .is_some_and(|t| t.trim() == "tag")
            {
                return Err(Die::Fatal(msg::submodule_entry_is_tag(
                    &shown,
                    &id.to_hex(),
                )));
            }
        }
        if !has_remote(&git_dir) {
            continue;
        }
        let hex: Vec<String> = ids.iter().map(ObjectId::to_hex).collect();
        // Commits the submodule does not have at all cannot be pushed.
        let mut args: Vec<&str> = vec!["rev-list", "-n", "1"];
        args.extend(hex.iter().map(String::as_str));
        args.extend(["--not", "--all"]);
        if run_in(&git_dir, &args).is_none() {
            continue;
        }
        let mut args: Vec<&str> = vec!["rev-list"];
        args.extend(hex.iter().map(String::as_str));
        args.extend(["--not", "--remotes", "-n", "1"]);
        if run_in(&git_dir, &args).is_some_and(|o| !o.trim().is_empty()) {
            out.push(Unpushed { path: shown, dir });
        }
    }
    Ok(out)
}

/// Whether the repository `git_dir` has a remote with a URL.
fn has_remote(git_dir: &Path) -> bool {
    run_in(git_dir, &["config", "--get-regexp", r"^remote\..*\.url$"])
        .is_some_and(|o| !o.trim().is_empty())
}

/// Runs this binary on the repository `git_dir`; its output on success.
fn run_in(git_dir: &Path, args: &[&str]) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    crate::submodules::prepare_env(&mut cmd);
    cmd.env_remove("GIT_PREFIX");
    cmd.env("GIT_DIR", git_dir)
        .current_dir(git_dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// With a configured remote: the submodule must have that remote, every
/// refspec source must name one of its refs, and `HEAD` must be on the
/// superproject's branch.
fn push_check(u: &Unpushed, remote: &str, req: &Request<'_>) -> Result<(), Die> {
    let fail = |m: String| {
        eprintln!("fatal: {m}");
        Die::Fatal(msg::process_failed(&u.path))
    };
    let git = |args: &[&str]| {
        let exe = std::env::current_exe().ok()?;
        let mut cmd = Command::new(exe);
        crate::submodules::prepare_env(&mut cmd);
        cmd.env_remove("GIT_PREFIX");
        cmd.current_dir(&u.dir)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let out = cmd.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let url = git(&["config", "--get", &format!("remote.{remote}.url")]);
    let push_url = git(&["config", "--get", &format!("remote.{remote}.pushurl")]);
    if url.is_none() && push_url.is_none() {
        return Err(fail(msg::remote_not_configured(remote)));
    }
    for spec in req.refspecs {
        let spec = spec.strip_prefix('+').unwrap_or(spec);
        let src = spec.split_once(':').map_or(spec, |(s, _)| s);
        if src.is_empty() || src.contains('*') {
            continue;
        }
        if src == "HEAD" {
            let sub_head = git(&["symbolic-ref", "-q", "HEAD"]);
            if sub_head.is_none() || sub_head.as_deref() != req.head {
                return Err(fail(msg::HEAD_MISMATCH.into()));
            }
            continue;
        }
        let full = git(&["rev-parse", "--symbolic-full-name", src]).unwrap_or_default();
        if !full.starts_with("refs/") {
            return Err(fail(msg::src_must_name_ref(src)));
        }
    }
    Ok(())
}

/// `push --recurse-submodules=only-is-on-demand [<remote> <refspec>...]`
/// in the submodule.
fn push_one(u: &Unpushed, req: &Request<'_>) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut cmd = Command::new(exe);
    crate::submodules::prepare_env(&mut cmd);
    cmd.env_remove("GIT_PREFIX");
    cmd.current_dir(&u.dir).arg("push");
    if req.dry_run {
        cmd.arg("--dry-run");
    }
    for o in req.push_options {
        cmd.arg(format!("--push-option={o}"));
    }
    cmd.arg("--recurse-submodules=only-is-on-demand");
    if let Some(r) = req.remote {
        cmd.arg(r).args(req.refspecs);
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    cmd.stdin(Stdio::null()).status().is_ok_and(|s| s.success())
}
