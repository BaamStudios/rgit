// SPDX-License-Identifier: MIT
//! `--recurse-submodules` for the commands that move the work tree
//! (`checkout`, `switch`, `reset`, `read-tree`, `restore`): the work trees
//! of active submodules follow their gitlinks (gitsubmodules(7),
//! git-read-tree(1) `--recurse-submodules`).
//!
//! The merge in `rgit-unpack` decides which submodule moves where; this
//! module carries the moves out the way git does, observed with
//! `GIT_TRACE`: by running this binary inside the submodule.
//!
//! - check (while planning): the submodule's index must match its `HEAD`
//!   (`diff-index --cached --quiet HEAD`) and `read-tree -n` must accept
//!   the move;
//! - an appearing submodule whose git directory exists in
//!   `$GIT_COMMON_DIR/modules/<name>` is connected to it (`.git` file,
//!   `core.worktree`) and checked out;
//! - a changed one is reset to the new commit (`read-tree -u --reset`,
//!   recursing further), its `HEAD` detached there;
//! - a removed one is emptied, an embedded `.git` directory absorbed into
//!   the superproject first, and disconnected from its git directory.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};

use rgit_hash::ObjectId;
use rgit_options::Opt;
use rgit_submodule::{Gitmodules, Mode, Origin};

use crate::commands::submodule::common;
use crate::messages::recurse_submodules as msg;
use crate::messages::submodule_cmd as sub_msg;
use crate::repository::Repo;

/// What the running command said about recursing: 0 it has no
/// `--recurse-submodules` option, 1 not given, 2 `--no-recurse-submodules`,
/// 3 `--recurse-submodules`.
static GIVEN: AtomicU8 = AtomicU8::new(0);

/// A command with `--[no-]recurse-submodules` records what it was given;
/// the shared work tree move ([`crate::switch_branch`]) then recurses as
/// [`enabled`] says.
pub fn declare(given: Option<bool>) {
    let v = match given {
        None => 1,
        Some(false) => 2,
        Some(true) => 3,
    };
    GIVEN.store(v, Ordering::Relaxed);
}

/// Whether work tree moves recurse into submodules: as the command was told,
/// else `submodule.recurse` (only for commands that have the option).
pub fn enabled(repo: &Repo) -> bool {
    match GIVEN.load(Ordering::Relaxed) {
        0 | 2 => false,
        3 => true,
        _ => rgit_submodule::settings::recurse(&repo.config),
    }
}

/// [`declare`] and [`enabled`] in one.
pub fn set(repo: &Repo, given: Option<bool>) -> bool {
    declare(given);
    enabled(repo)
}

/// The `--[no-]recurse-submodules[=<checkout>]` option of the work tree
/// commands, recorded in `cell`.
pub fn option<'a>(cell: &'a Cell<Option<bool>>, help: &'a str) -> Opt<'a> {
    option_named(cell, "checkout", help)
}

/// [`option`] with another argument name in the help.
pub fn option_named<'a>(cell: &'a Cell<Option<bool>>, argh: &'a str, help: &'a str) -> Opt<'a> {
    Opt::callback(None, "recurse-submodules", Some(argh), help, |v, unset| {
        cell.set(Some(!unset && parse_value(v)?));
        Ok(())
    })
    .optional(None)
}

/// The value of `--recurse-submodules=<value>` for the work tree commands:
/// a boolean (no value: true).
///
/// # Errors
/// A value that is not a boolean.
pub fn parse_value(v: Option<&str>) -> Result<bool, rgit_options::CallbackError> {
    match v {
        None => Ok(true),
        Some(v) => rgit_config::value::parse_bool(Some(v.as_bytes()))
            .ok_or_else(|| rgit_options::CallbackError::Fatal(msg::bad_recurse_value(v))),
    }
}

/// The submodule hooks of a merge in `repo`'s work tree.
pub struct Updater<'r> {
    repo: &'r Repo,
    /// Top of the work tree (absolute).
    root: PathBuf,
    /// The path of this repository as the outermost superproject sees it
    /// (`--super-prefix`, empty or ending in `/`).
    prefix: String,
    /// Submodules whose index was found to differ from their `HEAD`.
    dirty: RefCell<HashMap<Vec<u8>, bool>>,
}

impl std::fmt::Debug for Updater<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updater")
            .field("root", &self.root)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl<'r> Updater<'r> {
    /// Hooks for the work tree of `repo`; `None` for a bare repository.
    pub fn new(repo: &'r Repo, prefix: Option<&str>) -> Option<Updater<'r>> {
        let wt = repo.setup.work_tree.as_ref()?;
        Some(Updater {
            repo,
            root: common::absolute(&repo.setup.cwd.join(wt)),
            prefix: prefix.unwrap_or("").to_owned(),
            dirty: RefCell::new(HashMap::new()),
        })
    }

    /// `path` as the outermost superproject shows it.
    fn shown(&self, path: &[u8]) -> String {
        format!("{}{}", self.prefix, String::from_utf8_lossy(path))
    }

    fn dir(&self, path: &[u8]) -> PathBuf {
        crate::diff::collect::os_path(&self.root, path)
    }

    /// git does not work in a submodule whose path is, or leads through, a
    /// symbolic link (the link may point into another repository): it
    /// prints the error and ends, as observed with exit code 128.
    pub fn refuse_symlinks(&self, path: &[u8]) {
        let parts: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
        for i in 1..=parts.len() {
            let leading = parts[..i].join(&b'/');
            let is_link = std::fs::symlink_metadata(self.dir(&leading))
                .is_ok_and(|m| m.file_type().is_symlink());
            if is_link {
                let shown = self.shown(path);
                let text = if i == parts.len() {
                    sub_msg::path_is_symlink(&shown)
                } else {
                    sub_msg::leading_symlink(&self.shown(&leading), &shown)
                };
                crate::convert::exit_after_error(&text);
            }
        }
    }

    /// The name of the submodule at `path`: from the work tree's
    /// `.gitmodules` as it is now, else the path.
    fn name(&self, path: &[u8]) -> Vec<u8> {
        let file = self.root.join(rgit_submodule::GITMODULES);
        let parsed = std::fs::read(&file).ok().and_then(|data| {
            let origin = Origin::File(file.to_string_lossy().into_owned());
            Gitmodules::parse(&data, &origin, Mode::Current)
                .ok()
                .map(|(gm, _)| gm)
        });
        parsed
            .as_ref()
            .and_then(|gm| gm.by_path(path))
            .map_or_else(|| path.to_vec(), |s| s.name.to_vec())
    }

    /// `$GIT_COMMON_DIR/modules/<name>` of the submodule at `path`.
    fn modules_dir(&self, path: &[u8]) -> PathBuf {
        rgit_submodule::gitdir::modules_dir(&self.repo.setup.absolute_git_dir(), &self.name(path))
    }

    /// git does not use a submodule git directory that lies within the git
    /// directory of another submodule (the name `a/b` next to a submodule
    /// `a`): it reports both and ends.
    fn refuse_nested_git_dir(&self, path: &[u8]) {
        let name = self.name(path);
        let git_dir = self.repo.setup.absolute_git_dir();
        let shown = |dir: &Path| {
            dir.strip_prefix(&self.root)
                .unwrap_or(dir)
                .to_string_lossy()
                .into_owned()
        };
        for (i, _) in name.iter().enumerate().filter(|(_, b)| **b == b'/') {
            let outer = rgit_submodule::gitdir::modules_dir(&git_dir, &name[..i]);
            if crate::submodules::is_git_dir(&outer) {
                let sub = shown(&rgit_submodule::gitdir::modules_dir(&git_dir, &name));
                eprintln!("error: {}", msg::git_dir_inside(&sub, &shown(&outer)));
                eprintln!("hint: {}", msg::HINT_PATH_CONFIG);
                crate::convert::die_now(&msg::refusing_git_dir(&sub));
            }
        }
    }

    fn empty_tree(&self) -> String {
        ObjectId::empty_tree(self.repo.hash).to_hex()
    }

    /// Runs this binary in the submodule work tree `dir` with the
    /// superproject's repository environment removed; whether it succeeded.
    fn run(&self, dir: &Path, args: &[&str]) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let mut cmd = Command::new(exe);
        cmd.current_dir(dir).args(args).stdin(Stdio::null());
        crate::submodules::prepare_env(&mut cmd);
        cmd.env_remove("GIT_PREFIX");
        cmd.status().is_ok_and(|s| s.success())
    }

    /// `read-tree --recurse-submodules --super-prefix=<path>/ <args>`.
    fn read_tree(&self, dir: &Path, path: &[u8], recurse: bool, args: &[&str]) -> bool {
        let prefix = format!("--super-prefix={}/", self.shown(path));
        let mut all = vec!["read-tree"];
        if recurse {
            all.push("--recurse-submodules");
        }
        all.push(&prefix);
        all.extend_from_slice(args);
        self.run(dir, &all)
    }

    /// Detaches the submodule's `HEAD` at `id`.
    fn detach(&self, dir: &Path, id: &str) -> bool {
        self.run(dir, &["update-ref", "--no-deref", "HEAD", id])
    }

    /// Absorbs an embedded `.git` directory of the submodule at `path` (and
    /// of submodules nested in it) into the superproject.
    fn absorb(&self, dir: &Path, path: &[u8]) -> Result<(), String> {
        if !std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.is_dir()) {
            return Ok(());
        }
        let shown = self.shown(path);
        crate::submodules::absorb_git_dir(self.repo, dir, &self.name(path), Some(shown.as_bytes()))
            .map_err(|e| match e {
                crate::Die::Fatal(m) | crate::Die::Error(m, _) => m,
                _ => msg::could_not_update(&shown),
            })
    }

    fn remove(&self, path: &[u8]) -> Result<(), String> {
        let dir = self.dir(path);
        if !rgit_submodule::gitdir::is_populated(&dir) {
            return Ok(());
        }
        let shown = self.shown(path);
        self.absorb(&dir, path)?;
        if !self.read_tree(&dir, path, true, &["-u", "--reset", &self.empty_tree()]) {
            return Err(rgit_unpack::messages::submodule_new_head(&shown));
        }
        // Disconnect the work tree from the git directory.
        if let Some(git_dir) = rgit_submodule::gitdir::git_dir_of(&dir) {
            if rgit_submodule::gitdir::is_absorbed(&dir) {
                let _ = std::fs::remove_file(dir.join(".git"));
                let _ = common::set_in_file(&git_dir.join("config"), "core.worktree", None);
            }
        }
        Ok(())
    }

    fn checkout(&self, path: &[u8], id: &ObjectId) -> Result<(), String> {
        let dir = self.dir(path);
        let shown = self.shown(path);
        let fail = || rgit_unpack::messages::submodule_new_head(&shown);
        let hex = id.to_hex();
        let dot_git = dir.join(".git");
        let is_file = std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_file());
        let modules = self.modules_dir(path);
        let populated = rgit_submodule::gitdir::is_populated(&dir);
        if populated && !is_file {
            // An embedded git directory: used as it is.
        } else if modules.join("HEAD").is_file() {
            // (Re)connect: an appearing submodule, or a `.git` file that
            // leads elsewhere.
            self.refuse_nested_git_dir(path);
            if !dir.is_dir() {
                std::fs::create_dir_all(&dir).map_err(|_| fail())?;
            }
            common::connect(&dir, &modules).map_err(|_| fail())?;
            if !populated {
                if !self.read_tree(&dir, path, false, &["-u", "--reset", &self.empty_tree()])
                    || !self.read_tree(&dir, path, true, &["-u", "-m", &self.empty_tree(), &hex])
                    || !self.detach(&dir, &hex)
                {
                    return Err(fail());
                }
                return Ok(());
            }
        } else if !populated {
            // Not initialized: the empty directory stays.
            return Ok(());
        }
        if !self.read_tree(&dir, path, true, &["-u", "--reset", &hex]) || !self.detach(&dir, &hex) {
            return Err(fail());
        }
        Ok(())
    }
}

impl rgit_unpack::Submodules for Updater<'_> {
    fn active(&self, path: &[u8]) -> bool {
        rgit_submodule::is_active(&self.repo.config, &self.name(path), path)
    }

    fn index_clean(&self, path: &[u8]) -> bool {
        self.refuse_symlinks(path);
        if let Some(clean) = self.dirty.borrow().get(path) {
            return *clean;
        }
        let dir = self.dir(path);
        let clean = !rgit_submodule::gitdir::is_populated(&dir)
            || self.run(&dir, &["diff-index", "--quiet", "--cached", "HEAD"]);
        if !clean {
            eprintln!("error: {}", msg::dirty_index(&self.shown(path)));
        }
        self.dirty.borrow_mut().insert(path.to_vec(), clean);
        clean
    }

    fn check(
        &self,
        path: &[u8],
        _old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        force: bool,
    ) -> bool {
        self.refuse_symlinks(path);
        let dir = self.dir(path);
        if !rgit_submodule::gitdir::is_populated(&dir) {
            return true;
        }
        if !force && !self.index_clean(path) {
            return false;
        }
        let new = new.map_or_else(|| self.empty_tree(), ObjectId::to_hex);
        let ok = if force {
            self.read_tree(&dir, path, true, &["-n", "--reset", &new])
        } else {
            self.read_tree(&dir, path, true, &["-n", "-m", "HEAD", &new])
        };
        if !ok {
            eprintln!("error: {}", msg::could_not_update(&self.shown(path)));
        }
        ok
    }

    fn update(
        &self,
        path: &[u8],
        _old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        _force: bool,
    ) -> Result<(), String> {
        self.refuse_symlinks(path);
        match new {
            None => self.remove(path),
            Some(id) => self.checkout(path, id),
        }
    }
}
