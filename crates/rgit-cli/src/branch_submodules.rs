// SPDX-License-Identifier: MIT
//! `branch --recurse-submodules <name> [<start-point>]` (with
//! `submodule.propagateBranches`, Documentation/git-branch.adoc,
//! config/submodule.adoc) and its helper `submodule--helper
//! create-branch`; the process layout and messages were observed with
//! `GIT_TRACE`.
//!
//! The branch is created in the superproject and in every active submodule
//! recorded in the start point's tree, each at the commit recorded there.
//! Every repository is checked first (`create-branch --dry-run`), so that
//! nothing is created when one of them refuses. A submodule's tracking is
//! derived from the superproject's start point (full ref name).

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use rgit_hash::ObjectId;
use rgit_object::Object;

use crate::Die;
use crate::messages::branch as msg;
use crate::repository::Repo;
use crate::tracking::Track;

/// A submodule to create the branch in.
pub struct Sub {
    path: String,
    git_dir: PathBuf,
    oid: ObjectId,
}

/// The active submodules recorded in `commit`'s tree (gitlinks at any
/// depth), with their git directories below `$GIT_DIR/modules`.
///
/// # Errors
/// An active submodule whose git directory is missing.
pub fn submodules(repo: &Repo, commit: &ObjectId) -> Result<Vec<Sub>, Die> {
    let tree = match repo.read_object(commit) {
        Some(Object::Commit(c)) => c.tree().ok(),
        _ => None,
    };
    let Some(tree) = tree else {
        return Ok(Vec::new());
    };
    let mut links = Vec::new();
    crate::fetch_submodules::gitlink_changes(repo, None, Some(&tree), b"", &mut links);
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let gm = crate::fetch_submodules::commit_gitmodules(repo, &tree);
    let mut out = Vec::new();
    for (path, oid) in links {
        let name = gm
            .as_ref()
            .and_then(|g| g.by_path(&path))
            .map_or_else(|| path.clone(), |s| s.name.to_vec());
        if !rgit_submodule::is_active(&repo.config, &name, &path) {
            continue;
        }
        let shown = String::from_utf8_lossy(&path).into_owned();
        let git_dir = rgit_submodule::gitdir::modules_dir(&repo.setup.absolute_git_dir(), &name);
        if !git_dir.join("HEAD").exists() {
            return Err(Die::Fatal(msg::unable_to_find_submodule(&shown)));
        }
        out.push(Sub {
            path: shown,
            git_dir,
            oid,
        });
    }
    Ok(out)
}

/// What to create.
pub struct Create<'a> {
    pub name: &'a str,
    /// The superproject's start point as a full ref name when it is one.
    pub start_name: &'a str,
    pub track: Track,
    pub force: bool,
    pub quiet: bool,
}

/// Runs `submodule--helper create-branch` in each submodule (`dry_run`:
/// only the checks). The children's standard error is shown with
/// `submodule '<path>': ` in front of each line.
///
/// # Errors
/// The first submodule that refuses.
pub fn run(subs: &[Sub], c: &Create<'_>, dry_run: bool) -> Result<(), Die> {
    for s in subs {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let mut cmd = Command::new(exe);
        crate::submodules::prepare_env(&mut cmd);
        cmd.env_remove("GIT_PREFIX");
        cmd.env("GIT_DIR", &s.git_dir).current_dir(&s.git_dir);
        cmd.args(["submodule--helper", "create-branch"]);
        if dry_run {
            cmd.arg("--dry-run");
        }
        if c.force {
            cmd.arg("--force");
        }
        if c.quiet {
            cmd.arg("--quiet");
        }
        match c.track {
            Track::Direct => {
                cmd.arg("--track=direct");
            }
            Track::Inherit => {
                cmd.arg("--track=inherit");
            }
            Track::Never => {
                cmd.arg("--no-track");
            }
            Track::Unspecified => {}
        }
        cmd.args([c.name, &s.oid.to_hex(), c.start_name]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let _ = std::io::stdout().flush();
        let mut child = cmd.spawn().map_err(|e| Die::Fatal(e.to_string()))?;
        let prefix = msg::submodule_prefix(&s.path);
        let out = child.stdout.take().map(|o| {
            let prefix = prefix.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(o)
                    .split(b'\n')
                    .map_while(Result::ok)
                {
                    let mut w = std::io::stdout().lock();
                    let _ = w.write_all(prefix.as_bytes());
                    let _ = w.write_all(&line);
                    let _ = w.write_all(b"\n");
                    let _ = w.flush();
                }
            })
        });
        if let Some(err) = child.stderr.take() {
            for line in std::io::BufReader::new(err)
                .split(b'\n')
                .map_while(Result::ok)
            {
                let mut w = std::io::stderr().lock();
                let _ = w.write_all(prefix.as_bytes());
                let _ = w.write_all(&line);
                let _ = w.write_all(b"\n");
            }
        }
        if let Some(t) = out {
            let _ = t.join();
        }
        let ok = child.wait().is_ok_and(|st| st.success());
        if !ok {
            return Err(Die::Fatal(msg::cannot_create_in_submodule(&s.path, c.name)));
        }
    }
    Ok(())
}

/// The full name of `start` when it names a branch or remote-tracking
/// branch (`main` -> `refs/heads/main`), else `start`.
pub fn full_start_name(store: &rgit_refs::files::FilesStore, start: &str) -> String {
    use rgit_refs::RefStore as _;
    if start.starts_with("refs/") {
        return start.to_owned();
    }
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        let full = format!("{prefix}{start}");
        if store.read_raw(&full).ok().flatten().is_some() {
            return full;
        }
    }
    start.to_owned()
}
