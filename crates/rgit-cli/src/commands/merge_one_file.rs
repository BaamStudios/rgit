// SPDX-License-Identifier: MIT
//! `git merge-one-file` (Documentation/git-merge-one-file.adoc), the
//! standard helper of `git merge-index`: resolves one unmerged path from
//! its blob ids and modes (`<orig> <ours> <theirs> <path> <orig mode>
//! <our mode> <their mode>`, empty for a missing version).
//!
//! Its behaviour was taken black-box from the `git` binary:
//!
//! - removed on one side and unchanged on the other: the path is removed
//!   (`update-index --remove`), with `Removing <path>` and the work tree
//!   file deleted when our side had it;
//! - added on one side only, or identically on both: the version is
//!   added (`Adding <path>` and checked out unless it is ours alone);
//! - changed on both sides: a content merge of the regular files
//!   (`Auto-merging <path>`, or `Added <path> in both, but differently.`
//!   against the empty blob) into the work tree; the index is only
//!   updated when the merge is clean and the modes agree;
//! - everything else is refused with an `ERROR:` line and exit code 1.
//!
//! Index and work tree are changed through rgit's own plumbing
//! (`update-index`, `checkout-index`, `merge-file`), run in the top of the
//! work tree.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_hash::ObjectId;

use crate::messages::merge as mmsg;
use crate::messages::merge_plumbing as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

const MODE_LINK: &str = "120000";
const MODE_GITLINK: &str = "160000";

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if matches!(args.first().map(String::as_str), Some("-h" | "--help-all")) {
        println!("{}", msg::MERGE_ONE_FILE_USAGE);
        return Ok(0);
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    if args.len() != 7 {
        println!("{}", msg::MERGE_ONE_FILE_USAGE);
        return Ok(1);
    }
    let Some(root) = repo.setup.work_tree.clone() else {
        eprintln!("{}", msg::NEED_WORK_TREE);
        eprintln!("{}", msg::CANNOT_CHDIR_TOPLEVEL);
        return Ok(1);
    };
    let plumbing = Plumbing::new(ctx, &repo, root);
    let a = Args {
        orig: &args[0],
        ours: &args[1],
        theirs: &args[2],
        path: &args[3],
        orig_mode: &args[4],
        our_mode: &args[5],
        their_mode: &args[6],
    };
    merge_one(&repo, &plumbing, &a)
}

struct Args<'a> {
    orig: &'a str,
    ours: &'a str,
    theirs: &'a str,
    path: &'a str,
    orig_mode: &'a str,
    our_mode: &'a str,
    their_mode: &'a str,
}

fn merge_one(repo: &Repo, git: &Plumbing, a: &Args<'_>) -> CmdResult {
    let path = a.path;
    let (o, ours, theirs) = (a.orig, a.ours, a.theirs);
    // Removed on one side (or both), unchanged on the other.
    if !o.is_empty()
        && (ours.is_empty() || theirs.is_empty())
        && (ours.is_empty() || ours == o)
        && (theirs.is_empty() || theirs == o)
    {
        // The surviving side's mode must be the original one (with both
        // sides gone there is none, which counts as a change, observed).
        let survivor = format!("{}{}", a.our_mode, a.their_mode);
        if survivor != a.orig_mode {
            eprintln!("{}", msg::deleted_but_permissions(path));
            return Ok(1);
        }
        if !ours.is_empty() {
            println!("{}", msg::removing(path));
            flush();
            remove_with_dirs(&git.root, path);
        }
        return git.run(["update-index", "--remove", "--", path]);
    }
    // Added on one side only.
    if o.is_empty() && ours.is_empty() != theirs.is_empty() {
        if theirs.is_empty() {
            let info = format!("{},{},{}", a.our_mode, ours, path);
            return git.run(["update-index", "--add", "--cacheinfo", &info]);
        }
        println!("{}", msg::adding(path));
        flush();
        if std::fs::symlink_metadata(git.root.join(path)).is_ok() {
            eprintln!("{}", msg::untracked_overwritten(path));
            return Ok(1);
        }
        let info = format!("{},{},{}", a.their_mode, theirs, path);
        let code = git.run(["update-index", "--add", "--cacheinfo", &info])?;
        if code != 0 {
            return Ok(code);
        }
        return git.run(["checkout-index", "-u", "-f", "--", path]);
    }
    // Added identically on both sides.
    if o.is_empty() && !ours.is_empty() && ours == theirs {
        if a.our_mode != a.their_mode {
            eprintln!(
                "{}",
                msg::added_identically_permissions(path, a.our_mode, a.their_mode)
            );
            return Ok(1);
        }
        println!("{}", msg::adding(path));
        flush();
        let info = format!("{},{},{}", a.our_mode, ours, path);
        let code = git.run(["update-index", "--add", "--cacheinfo", &info])?;
        if code != 0 {
            return Ok(code);
        }
        return git.run(["checkout-index", "-u", "-f", "--", path]);
    }
    if ours.is_empty() || theirs.is_empty() {
        eprintln!("{}", mmsg::resolve_not_handling(path, o, ours, theirs));
        return Ok(1);
    }
    // Changed on both sides.
    let modes = [a.orig_mode, a.our_mode, a.their_mode];
    if modes.contains(&MODE_LINK) {
        eprintln!("{}", mmsg::resolve_symlink(path));
        return Ok(1);
    }
    if modes.contains(&MODE_GITLINK) {
        eprintln!("{}", msg::submodule_conflict(path));
        return Ok(1);
    }
    if o.is_empty() {
        println!("{}", mmsg::added_differently(path));
    } else {
        println!("{}", mmsg::resolve_auto_merging(path));
    }
    flush();
    let code = git.run(["checkout-index", "-f", "--stage=2", "--", path])?;
    if code != 0 {
        return Ok(1);
    }
    let clean = content_merge(repo, git, o, ours, theirs, path)?;
    let modes_differ = a.our_mode != a.their_mode;
    let (o_m, u_m, t_m) = (a.orig_mode, a.our_mode, a.their_mode);
    match (clean, modes_differ) {
        (true, false) => {}
        (false, false) => eprintln!("{}", mmsg::resolve_content_conflict(path)),
        (true, true) => eprintln!("{}", mmsg::resolve_permissions(path, o_m, u_m, t_m)),
        (false, true) => eprintln!(
            "{}",
            mmsg::resolve_content_and_permissions(path, o_m, u_m, t_m)
        ),
    }
    if !clean || modes_differ {
        return Ok(1);
    }
    git.run(["update-index", "--", path])
}

/// Merges the three blobs with `merge-file` on temporary files (whose
/// names label the conflict markers) and writes the result over the work
/// tree file; whether the merge was clean.
fn content_merge(
    repo: &Repo,
    git: &Plumbing,
    orig: &str,
    ours: &str,
    theirs: &str,
    path: &str,
) -> Result<bool, Die> {
    let blob = |hex: &str| -> Result<Option<Vec<u8>>, Die> {
        let id = ObjectId::from_hex(repo.hash, hex.as_bytes())
            .map_err(|_| Die::Fatal(msg::not_a_valid_object(hex)))?;
        Ok(match repo.odb().read(&id) {
            Ok(Some(o)) => Some(o.data),
            _ => None,
        })
    };
    let read = |hex: &str| -> Result<Vec<u8>, Die> {
        blob(hex)?.ok_or_else(|| Die::Fatal(msg::unable_to_read_blob(hex)))
    };
    let our_data = read(ours)?;
    let their_data = read(theirs)?;
    let base_data = if orig.is_empty() {
        // Merged against the empty blob, which must be in the repository.
        let empty = repo.hash.empty_blob().to_hex();
        match blob(&empty)? {
            Some(d) => d,
            None => {
                eprintln!("fatal: {}", mmsg::resolve_unreadable_blob(&empty));
                eprintln!("error: {}", mmsg::RESOLVE_COULD_NOT_STAT);
                return Ok(false);
            }
        }
    } else {
        read(orig)?
    };
    let io = |e: std::io::Error| Die::Fatal(e.to_string());
    let t_ours = crate::commands::unpack_file::temp_file(&git.root, &our_data).map_err(io)?;
    let t_base = crate::commands::unpack_file::temp_file(&git.root, &base_data).map_err(io)?;
    let t_theirs = crate::commands::unpack_file::temp_file(&git.root, &their_data).map_err(io)?;
    let code = git.run(["merge-file", t_ours.as_str(), t_base.as_str(), t_theirs.as_str()]);
    let merged = std::fs::read(git.root.join(&t_ours));
    for t in [&t_ours, &t_base, &t_theirs] {
        let _ = std::fs::remove_file(git.root.join(t));
    }
    let code = code?;
    let merged = merged.map_err(io)?;
    // Written into the file checked out from stage 2 (keeping its mode).
    std::fs::write(git.root.join(path), merged).map_err(io)?;
    Ok(code == 0)
}

/// Removes `path` and the directories it leaves empty.
fn remove_with_dirs(root: &Path, path: &str) {
    let _ = std::fs::remove_file(root.join(path));
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        if d.as_os_str().is_empty() || std::fs::remove_dir(root.join(d)).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn flush() {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
}

/// Runs rgit's own plumbing in the top of the work tree.
pub(crate) struct Plumbing {
    exe: PathBuf,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub root: PathBuf,
}

impl Plumbing {
    pub fn new(ctx: &Context, repo: &Repo, root: PathBuf) -> Plumbing {
        let mut env = ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        env.push(("GIT_DIR".into(), repo.setup.absolute_git_dir().into_os_string()));
        if repo.setup.work_tree.is_some() {
            env.push(("GIT_WORK_TREE".into(), root.clone().into_os_string()));
        }
        Plumbing {
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git")),
            env,
            root,
        }
    }

    /// Runs in the top of the work tree (the current directory without
    /// one).
    pub fn at_top(ctx: &Context, repo: &Repo) -> Plumbing {
        let root = repo
            .setup
            .work_tree
            .clone()
            .unwrap_or_else(|| repo.setup.cwd.clone());
        Plumbing::new(ctx, repo, root)
    }

    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&self.root);
        cmd
    }

    /// Runs `git <args>` and returns its exit code.
    pub fn run<I, S>(&self, args: I) -> CmdResult
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let status = self
            .command(args)
            .status()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(status.code().unwrap_or(128))
    }

    /// Runs `git <args>` with standard output captured (standard error
    /// passes through); its exit code and output.
    pub fn output<I, S>(&self, args: I) -> Result<(i32, Vec<u8>), Die>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self
            .command(args)
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok((out.status.code().unwrap_or(128), out.stdout))
    }

    /// Like [`Plumbing::output`] with standard error discarded.
    pub fn output_quiet<I, S>(&self, args: I) -> Result<(i32, Vec<u8>), Die>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self
            .command(args)
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok((out.status.code().unwrap_or(128), out.stdout))
    }
}
