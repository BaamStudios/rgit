// SPDX-License-Identifier: MIT
//! The worktrees of a repository (`git-worktree(1)`, DETAILS;
//! `gitrepository-layout(5)`, `worktrees/<id>/`): the main worktree and the
//! linked ones registered in `$GIT_COMMON_DIR/worktrees/<id>`, with their
//! `HEAD`, lock and prune state, and the branches they have checked out
//! (which `switch`, `checkout`, `branch` and `worktree add` refuse to touch
//! elsewhere).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::FilesStore;
use rgit_refs::{RefStore, Target};
use rgit_repo::Setup;

/// One worktree.
#[derive(Debug, Clone)]
pub struct Worktree {
    /// The name below `worktrees/`; `None` for the main worktree.
    pub id: Option<String>,
    /// The top of the work tree (the common directory of a bare main
    /// repository), absolute.
    pub path: PathBuf,
    /// The worktree's git directory, absolute.
    pub git_dir: PathBuf,
    /// The main worktree of a bare repository.
    pub bare: bool,
    /// What `HEAD` resolves to (`None`: unborn or unreadable).
    pub head: Option<ObjectId>,
    /// The ref `HEAD` points to, when symbolic.
    pub head_ref: Option<String>,
    /// `HEAD` cannot be read.
    pub head_broken: bool,
    /// The worktree the command runs in.
    pub current: bool,
}

impl Worktree {
    pub fn is_main(&self) -> bool {
        self.id.is_none()
    }

    /// The reason in `locked` (possibly empty) if the worktree is locked.
    pub fn lock_reason(&self) -> Option<String> {
        self.id.as_ref()?;
        lock_reason(&self.git_dir)
    }
}

/// The reason in `<admin>/locked` (possibly empty), if it exists.
pub fn lock_reason(admin: &Path) -> Option<String> {
    let data = std::fs::read(admin.join("locked")).ok()?;
    let mut s = String::from_utf8_lossy(&data).into_owned();
    if s.ends_with('\n') {
        s.pop();
    }
    Some(s)
}

/// The main worktree's top for the common directory `common` (absolute):
/// its parent when it is a `.git` directory, else the directory itself.
pub fn main_path(common: &Path) -> PathBuf {
    if common.file_name().is_some_and(|n| n == ".git") {
        if let Some(p) = common.parent() {
            return p.to_path_buf();
        }
    }
    common.to_path_buf()
}

/// The `.git` file a linked worktree's `gitdir` names (absolute), if it
/// can be read.
pub fn gitdir_target(admin: &Path) -> Option<PathBuf> {
    let data = std::fs::read(admin.join("gitdir")).ok()?;
    let text = String::from_utf8_lossy(&data);
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return None;
    }
    Some(rgit_repo::path::normalize(&admin.join(text)))
}

/// The work tree a linked worktree's `gitdir` names.
fn linked_path(admin: &Path) -> Option<PathBuf> {
    let target = gitdir_target(admin)?;
    Some(main_path(&target))
}

/// The real path of `p` (symbolic links resolved where it exists).
pub fn real(p: &Path) -> PathBuf {
    rgit_repo::path::real_path_lossy(p, Path::new("/"))
}

fn read_head(
    common: &Path,
    admin: &Path,
    kind: HashKind,
) -> (Option<ObjectId>, Option<String>, bool) {
    let store = FilesStore::new(common, kind).with_worktree(admin);
    match store.read_raw("HEAD") {
        Ok(Some(Target::Object(oid))) => (Some(oid), None, false),
        Ok(Some(Target::Symbolic(name))) => {
            let oid = store.resolve(&name).ok().flatten().and_then(|r| r.oid);
            (oid, Some(name), false)
        }
        Ok(None) => (None, None, false),
        Err(_) => (None, None, true),
    }
}

/// All worktrees: the main one first, then the linked ones sorted by path.
/// Linked entries whose `gitdir` cannot be read are left out.
pub fn list(setup: &Setup, bare_main: bool) -> Vec<Worktree> {
    let kind = setup.format.object_format;
    let common = setup.absolute_common_dir();
    let own = real(&setup.absolute_git_dir());
    let mut out = Vec::new();
    let (head, head_ref, head_broken) = read_head(&common, &common, kind);
    out.push(Worktree {
        id: None,
        path: main_path(&common),
        git_dir: common.clone(),
        bare: bare_main,
        head,
        head_ref,
        head_broken,
        current: real(&common) == own,
    });
    let mut linked = Vec::new();
    for id in linked_ids(&common) {
        let admin = common.join("worktrees").join(&id);
        if !admin.is_dir() {
            continue;
        }
        let Some(path) = linked_path(&admin) else {
            continue;
        };
        let (head, head_ref, head_broken) = read_head(&common, &admin, kind);
        linked.push(Worktree {
            id: Some(id),
            path,
            current: real(&admin) == own,
            git_dir: admin,
            bare: false,
            head,
            head_ref,
            head_broken,
        });
    }
    linked.sort_by(|a, b| a.path.cmp(&b.path));
    out.extend(linked);
    out
}

/// The names below `worktrees/`, sorted.
pub fn linked_ids(common: &Path) -> Vec<String> {
    let mut ids = linked_ids_unsorted(common);
    ids.sort();
    ids
}

/// The names below `worktrees/` in directory order.
pub fn linked_ids_unsorted(common: &Path) -> Vec<String> {
    std::fs::read_dir(common.join("worktrees"))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `main_config` says the main worktree is bare.
pub fn main_is_bare(setup: &Setup, config: &rgit_config::ConfigSet) -> bool {
    if setup.git_dir == setup.common_dir {
        return setup.is_bare();
    }
    config.bool("core.bare").ok().flatten().unwrap_or(false)
}

/// Why the linked worktree `admin` can be pruned, if it can: the messages
/// `git worktree prune -v` and `list -v` show. A missing work tree only
/// counts when the worktree's `index` is older than `expire`.
pub fn prune_reason(admin: &Path, expire: Option<SystemTime>) -> Option<&'static str> {
    use crate::messages::worktree as msg;
    if !admin.is_dir() {
        return Some(msg::PRUNE_NOT_DIR);
    }
    if admin.join("locked").exists() {
        return None;
    }
    let file = admin.join("gitdir");
    if !file.exists() {
        return Some(msg::PRUNE_NO_GITDIR);
    }
    let data = match std::fs::read(&file) {
        Ok(d) => d,
        Err(_) => return Some(msg::PRUNE_UNREADABLE_GITDIR),
    };
    let text = String::from_utf8_lossy(&data);
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return Some(msg::PRUNE_INVALID_GITDIR);
    }
    let target = admin.join(text);
    if target.exists() {
        return None;
    }
    if let Some(expire) = expire {
        let fresh = std::fs::metadata(admin.join("index"))
            .and_then(|m| m.modified())
            .is_ok_and(|t| t > expire);
        if fresh {
            return None;
        }
    }
    Some(msg::PRUNE_NONEXISTENT)
}

/// The worktree (other than the current one unless `include_current`)
/// that has `refname` checked out, or is rebasing or bisecting it.
pub fn checked_out<'a>(
    worktrees: &'a [Worktree],
    refname: &str,
    include_current: bool,
) -> Option<&'a Worktree> {
    worktrees.iter().find(|wt| {
        if wt.bare || (wt.current && !include_current) {
            return false;
        }
        if wt.head_ref.as_deref() == Some(refname) {
            return true;
        }
        busy_with(&wt.git_dir, refname)
    })
}

/// What works on `refname` in the current worktree of `setup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Rebase,
    Bisect,
}

/// Whether a rebase or bisect in the current worktree of `setup` works
/// on `refname`.
pub fn busy_here(setup: &Setup, refname: &str) -> Option<Busy> {
    let git_dir = setup.cwd.join(&setup.git_dir);
    if !busy_with(&git_dir, refname) {
        return None;
    }
    let rebase = ["rebase-merge", "rebase-apply"].iter().any(|d| {
        std::fs::read_to_string(git_dir.join(d).join("head-name"))
            .is_ok_and(|s| s.trim_end() == refname)
    }) || updated_by_rebase(&git_dir, refname);
    Some(if rebase { Busy::Rebase } else { Busy::Bisect })
}

/// Whether the rebase in `git_dir` will update `refname` (`--update-refs`:
/// the first of every three lines of `rebase-merge/update-refs`).
fn updated_by_rebase(git_dir: &Path, refname: &str) -> bool {
    std::fs::read_to_string(git_dir.join("rebase-merge/update-refs"))
        .is_ok_and(|s| s.lines().step_by(3).any(|l| l.trim_end() == refname))
}

/// Whether a rebase or bisect in `git_dir` works on `refname`.
fn busy_with(git_dir: &Path, refname: &str) -> bool {
    let read = |p: PathBuf| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim_end().to_string())
    };
    for dir in ["rebase-merge", "rebase-apply"] {
        if read(git_dir.join(dir).join("head-name")).as_deref() == Some(refname) {
            return true;
        }
    }
    if updated_by_rebase(git_dir, refname) {
        return true;
    }
    match (
        read(git_dir.join("BISECT_START")),
        refname.strip_prefix("refs/heads/"),
    ) {
        (Some(b), Some(short)) => b == short,
        _ => false,
    }
}

/// The worktree other than the current one that has `refname` checked
/// out, for the commands of `setup`.
pub fn checked_out_elsewhere(
    setup: &Setup,
    config: &rgit_config::ConfigSet,
    refname: &str,
) -> Option<PathBuf> {
    let all = list(setup, main_is_bare(setup, config));
    checked_out(&all, refname, false).map(|w| w.path.clone())
}

/// The worktree in which a rebase works on `refname`, if any.
pub fn rebasing(setup: &Setup, refname: &str) -> Option<PathBuf> {
    list(setup, false).into_iter().find_map(|w| {
        let rebasing = ["rebase-merge", "rebase-apply"].iter().any(|d| {
            std::fs::read_to_string(w.git_dir.join(d).join("head-name"))
                .is_ok_and(|s| s.trim_end() == refname)
        });
        rebasing.then_some(w.path)
    })
}
