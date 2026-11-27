// SPDX-License-Identifier: MIT
//! What every `git submodule` subcommand needs: the superproject, the
//! submodules in its index, their `.gitmodules` entries and repository
//! configuration, relative URL resolution, the "active" rules and child
//! processes that run inside a submodule (gitsubmodules.adoc,
//! gitmodules.adoc, config/submodule.adoc).

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use bstr::ByteSlice;
use rgit_config::{ConfigSet, Origin, Scope, SetRequest, ValuePattern};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::Object;
use rgit_pathspec::{ParseOptions, Pathspec};

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::{self, Repo};

/// Environment variables that describe the superproject's repository; a
/// child working in a submodule must not see them. The `-c` parameters are
/// kept, like git does for submodules.
const LOCAL_ENV: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// The superproject a subcommand works on.
pub struct Super {
    pub ctx: Context,
    pub repo: Repo,
    /// Absolute top of the work tree.
    pub work_tree: PathBuf,
    /// Absolute git directory and common directory.
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    /// The current directory relative to the top (`""` or ending in `/`).
    pub prefix: String,
    /// Path of this superproject inside the outermost one when run
    /// recursively (`""` or ending in `/`).
    pub super_prefix: String,
}

/// One submodule of the index.
#[derive(Debug, Clone)]
pub struct Module {
    /// Path relative to the top of the work tree.
    pub path: String,
    /// The recorded commit (all zeroes when unmerged).
    pub oid: ObjectId,
    pub unmerged: bool,
}

/// The `submodule.<name>.*` entries of `.gitmodules`.
pub struct Gitmodules {
    pub config: ConfigSet,
}

impl Gitmodules {
    /// The value of `submodule.<name>.<key>`.
    pub fn get(&self, name: &str, key: &str) -> Option<String> {
        get_string(&self.config, &format!("submodule.{name}.{key}"))
    }

    /// The name of the submodule at `path` (the last entry wins).
    pub fn name_for_path(&self, path: &str) -> Option<String> {
        let mut found = None;
        for name in self.config.subsections("submodule") {
            let name = name.to_str_lossy().into_owned();
            if self.get(&name, "path").as_deref() == Some(path) && check_name(&name) {
                found = Some(name);
            }
        }
        found
    }

    /// The path recorded for `name`.
    pub fn path_for_name(&self, name: &str) -> Option<String> {
        self.get(name, "path")
    }
}

/// git's check of submodule names (no `..` component, not empty).
pub fn check_name(name: &str) -> bool {
    rgit_submodule::check_name(name.as_bytes())
}

/// A string value; the last one wins.
pub fn get_string(config: &ConfigSet, key: &str) -> Option<String> {
    config
        .get_all(key.as_bytes())
        .last()
        .and_then(|e| e.value.as_ref())
        .map(|v| v.to_str_lossy().into_owned())
}

/// A boolean value; `None` when unset or not a boolean.
pub fn get_bool(config: &ConfigSet, key: &str) -> Option<bool> {
    config.bool(key.as_bytes()).ok().flatten()
}

impl Super {
    /// Set up the repository; a work tree is required.
    pub fn open(ctx: &Context, super_prefix: Option<&str>) -> Result<Super, Die> {
        let repo = Repo::open(ctx, Mode::Required)?;
        let work_tree = repo.work_tree()?;
        let cwd = repo.setup.cwd.clone();
        let git_dir = absolute(&cwd.join(&repo.setup.git_dir));
        let common_dir = absolute(&cwd.join(&repo.setup.common_dir));
        let work_tree = absolute(&cwd.join(work_tree));
        let prefix = String::from_utf8_lossy(&repo.prefix()).into_owned();
        Ok(Super {
            ctx: ctx.clone(),
            repo,
            work_tree,
            git_dir,
            common_dir,
            prefix,
            super_prefix: super_prefix.unwrap_or("").to_owned(),
        })
    }

    /// Re-read the configuration after this process changed it.
    pub fn reload_config(&mut self) -> Result<(), Die> {
        self.repo.config = crate::repo_objects::load_config(&self.ctx, Some(&self.repo.setup))?;
        Ok(())
    }

    pub fn hash(&self) -> HashKind {
        self.repo.hash
    }

    /// `.gitmodules` from the work tree, else the index, else `HEAD`.
    pub fn gitmodules(&mut self) -> Gitmodules {
        let file = self.work_tree.join(".gitmodules");
        let data = match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() => std::fs::read(&file).ok(),
            _ => self
                .blob(":.gitmodules")
                .or_else(|| self.blob("HEAD:.gitmodules")),
        };
        let config = data
            .and_then(|d| rgit_config::parse_bytes(&d, Origin::File(file), Scope::Submodule).ok())
            .unwrap_or_default();
        Gitmodules { config }
    }

    fn blob(&mut self, name: &str) -> Option<Vec<u8>> {
        let id = self.repo.resolve(name)?;
        match self.repo.read_object(&id)? {
            Object::Blob(b) => Some(b.data),
            _ => None,
        }
    }

    /// The configuration file of the repository.
    pub fn config_path(&self) -> PathBuf {
        self.common_dir.join("config")
    }

    /// `git config <key> <value>` (`None` unsets all values).
    pub fn set_config(&self, key: &str, value: Option<&str>) -> Result<(), Die> {
        set_in_file(&self.config_path(), key, value)
    }

    /// Remove the section `submodule.<name>`.
    pub fn remove_section(&self, section: &str) -> Result<(), Die> {
        match rgit_config::write::rename_section_in_file(
            &self.config_path(),
            &Default::default(),
            section.as_bytes(),
            None,
        ) {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.edit_error(),
                    Some(rgit_config::EditError::NoSuchSection(_))
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(Die::Fatal(e.to_string())),
        }
    }

    /// A path relative to the top as shown to the user: below the outer
    /// superproject's path and relative to the current directory.
    pub fn display_path(&self, path: &str) -> String {
        let full = format!("{}{path}", self.super_prefix);
        if self.super_prefix.is_empty() {
            String::from_utf8_lossy(&crate::quote::relative_to(
                full.as_bytes(),
                self.prefix.as_bytes(),
            ))
            .into_owned()
        } else {
            full
        }
    }

    /// The submodules in the index matching `args` (pathspecs relative to
    /// the current directory). An unmatched pathspec is git's error
    /// (exit 1).
    pub fn module_list(&self, args: &[String]) -> Result<Vec<Module>, Die> {
        let pathspec = self.pathspec(args)?;
        let (index, _) = self.repo.read_index()?;
        let mut seen = vec![false; pathspec.items().len()];
        let mut out: Vec<Module> = Vec::new();
        for e in index.entries() {
            let mut one = vec![false; seen.len()];
            if !pathspec.is_empty() && pathspec.match_path_seen(e.path(), &mut one).is_none() {
                continue;
            }
            for (s, o) in seen.iter_mut().zip(one) {
                *s |= o;
            }
            if !e.mode.is_gitlink() {
                continue;
            }
            let path = e.path().to_str_lossy().into_owned();
            let unmerged = e.stage() != rgit_index::Stage::MERGED;
            if let Some(last) = out.last_mut() {
                if last.path == path {
                    last.unmerged = true;
                    last.oid = ObjectId::null(self.hash());
                    continue;
                }
            }
            out.push(Module {
                path,
                oid: if unmerged {
                    ObjectId::null(self.hash())
                } else {
                    e.id
                },
                unmerged,
            });
        }
        if let Some(item) = pathspec.unmatched(&seen).next() {
            return Err(Die::Error(
                rgit_pathspec::messages::did_not_match_known_to_git(item.original()),
                1,
            ));
        }
        Ok(out)
    }

    /// `args` as pathspecs relative to the current directory.
    pub fn pathspec(&self, args: &[String]) -> Result<Pathspec, Die> {
        let opts = ParseOptions {
            prefix: self.prefix.as_bytes().to_vec(),
            root: Some(self.work_tree.as_os_str().as_encoded_bytes().to_vec()),
            globals: repository::pathspec_globals(&self.ctx),
            ..Default::default()
        };
        Pathspec::parse(args, &opts).map_err(|e| Die::Fatal(e.to_string()))
    }

    /// The paths of all index entries (any mode) matching `args`.
    pub fn module_list_all(&self, args: &[String]) -> Result<Vec<String>, Die> {
        let pathspec = self.pathspec(args)?;
        let (index, _) = self.repo.read_index()?;
        let mut out: Vec<String> = Vec::new();
        for e in index.entries() {
            if !pathspec.is_empty() && pathspec.match_path(e.path()).is_none() {
                continue;
            }
            let p = e.path().to_str_lossy().into_owned();
            if out.last() != Some(&p) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// The name of the submodule at `path`, or git's error.
    pub fn name_of(&self, gm: &Gitmodules, path: &str) -> Result<String, Die> {
        gm.name_for_path(path)
            .ok_or_else(|| Die::Fatal(msg::no_mapping(path)))
    }

    /// git's "is this submodule active": `submodule.<name>.active`, else
    /// the `submodule.active` pathspecs, else a configured URL.
    pub fn is_active(&self, name: &str, path: &str) -> bool {
        is_active(&self.repo.config, name, path)
    }

    /// The remote of the current branch (`origin` when there is none or
    /// `HEAD` is detached).
    pub fn default_remote(&self) -> String {
        default_remote(&self.repo.config, self.repo.refs.head_target().as_deref())
    }

    /// The URL of the default remote, if configured.
    pub fn default_remote_url(&self) -> (String, Option<String>) {
        let remote = self.default_remote();
        let url = get_string(&self.repo.config, &format!("remote.{remote}.url"));
        (remote, url)
    }

    /// Resolve a `.gitmodules` URL: relative ones (`./`, `../`) against
    /// the default remote's URL, or the superproject itself when it has
    /// none (with git's warning when `warn`).
    pub fn resolve_url(&self, url: &str, warn: bool) -> Result<String, Die> {
        if !is_relative_url(url) {
            return Ok(url.to_owned());
        }
        let (remote, remote_url) = self.default_remote_url();
        let base = match remote_url {
            Some(u) => u,
            None => {
                if warn {
                    eprintln!("warning: {}", msg::no_remote_url(&remote));
                }
                self.work_tree.to_string_lossy().into_owned()
            }
        };
        resolve_relative_url(url, &base, None)
    }

    /// The git directory of submodule `name` below `modules/`.
    pub fn module_git_dir(&self, name: &str) -> PathBuf {
        self.git_dir.join("modules").join(name)
    }

    /// Whether `.gitmodules` may be changed: it is not in the index, or its
    /// work tree copy matches the index.
    pub fn gitmodules_staged_ok(&self) -> Result<bool, Die> {
        let (index, _) = self.repo.read_index()?;
        let Some(entry) = index.get(b".gitmodules", rgit_index::Stage::MERGED) else {
            return Ok(true);
        };
        let Ok(data) = std::fs::read(self.work_tree.join(".gitmodules")) else {
            return Ok(false);
        };
        let id = rgit_hash::hash_object(self.hash(), "blob", &data).ok();
        Ok(id == Some(entry.id))
    }

    /// Set (or unset) a key in `.gitmodules` of the work tree.
    pub fn set_gitmodules(&self, key: &str, value: Option<&str>) -> Result<(), Die> {
        set_in_file(&self.work_tree.join(".gitmodules"), key, value)
    }

    /// A child process of this binary running in the superproject.
    pub fn git(&self, args: &[&str]) -> Result<Command, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let mut cmd = Command::new(exe);
        let mut env = self.ctx.child_env();
        env.retain(|(k, _)| k != "GIT_DIR" && k != "GIT_WORK_TREE");
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        cmd.env("GIT_DIR", &self.git_dir);
        cmd.env("GIT_WORK_TREE", &self.work_tree);
        cmd.args(args).current_dir(&self.work_tree);
        Ok(cmd)
    }

    /// A child process of this binary running in the submodule work tree
    /// `dir` (absolute, or relative to the top of the superproject).
    pub fn sub_git(&self, dir: &Path, args: &[&str]) -> Result<Command, Die> {
        let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
        let mut cmd = Command::new(exe);
        self.sub_env(&mut cmd);
        cmd.args(args).current_dir(self.work_tree.join(dir));
        Ok(cmd)
    }

    /// The environment of a child working in a submodule.
    pub fn sub_env(&self, cmd: &mut Command) {
        let env: Vec<(OsString, OsString)> = self.ctx.child_env();
        for (k, v) in env {
            if !LOCAL_ENV.iter().any(|l| k == *l) {
                cmd.env(k, v);
            }
        }
        for k in LOCAL_ENV {
            cmd.env_remove(k);
        }
    }
}

/// A path argument relative to the top of the work tree, without a
/// trailing slash.
pub fn prefixed_path(sup: &Super, arg: &str) -> Result<String, Die> {
    let p = sup
        .repo
        .prefix_path(arg.as_bytes())
        .map_err(|()| {
            Die::Fatal(msg::outside_repository(arg, &sup.work_tree.to_string_lossy()))
        })?;
    let s = String::from_utf8_lossy(&p).into_owned();
    Ok(s.trim_end_matches('/').to_owned())
}

/// Write `key` into the configuration file `path`.
pub fn set_in_file(path: &Path, key: &str, value: Option<&str>) -> Result<(), Die> {
    let req = SetRequest {
        key: key.as_bytes(),
        value: value.map(str::as_bytes),
        pattern: &ValuePattern::Any,
        all: value.is_none(),
        comment: None,
    };
    match rgit_config::write::set_in_file(path, &Default::default(), &req) {
        Ok(()) => Ok(()),
        Err(e) if value.is_none() => {
            // Nothing to unset is fine.
            let _ = e;
            Ok(())
        }
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

pub fn is_active(config: &ConfigSet, name: &str, path: &str) -> bool {
    rgit_submodule::is_active(config, name.as_bytes(), path.as_bytes())
}

/// Whether `path` matches the `submodule.active` pathspecs.
pub fn matches_active(specs: &[String], path: &str) -> bool {
    let opts = ParseOptions::default();
    match Pathspec::parse(specs, &opts) {
        Ok(ps) => ps.match_path(path.as_bytes()).is_some(),
        Err(_) => false,
    }
}

/// The remote of the current branch, else the only remote, else `origin`.
pub fn default_remote(config: &ConfigSet, head_target: Option<&str>) -> String {
    if let Some(branch) = head_target.and_then(|t| t.strip_prefix("refs/heads/")) {
        if let Some(r) = get_string(config, &format!("branch.{branch}.remote")) {
            return r;
        }
    }
    let remotes = config.subsections("remote");
    if remotes.len() == 1 {
        return remotes[0].to_str_lossy().into_owned();
    }
    "origin".to_owned()
}

/// Whether `url` is relative to the superproject's remote.
pub fn is_relative_url(url: &str) -> bool {
    rgit_submodule::is_relative_url(url.as_bytes())
}

/// Resolve the relative URL `rel` against `base`; a relative local `base`
/// keeps the result relative, with `up_path` in front.
pub fn resolve_relative_url(rel: &str, base: &str, up_path: Option<&str>) -> Result<String, Die> {
    rgit_submodule::resolve_relative_url(up_path.map(str::as_bytes), base.as_bytes(), rel.as_bytes())
        .map(|u| u.to_str_lossy().into_owned())
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// Make `p` absolute and drop `.` and `..` components lexically.
pub fn absolute(p: &Path) -> PathBuf {
    let p = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// `target` relative to the directory `base` (both absolute).
pub fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<Component> = target.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// The real path of `p` when it exists, else `p` made absolute.
pub fn real(p: &Path) -> PathBuf {
    rgit_fs::path::canonicalize(p).unwrap_or_else(|_| absolute(p))
}

/// The git directory of the submodule work tree `dir`: its `.git`
/// directory or the target of its `gitdir:` file, if that exists.
pub fn sub_git_dir(dir: &Path) -> Option<PathBuf> {
    rgit_submodule::gitdir::git_dir_of(dir).filter(|g| g.is_dir())
}

/// Whether the work tree of a submodule is populated (has a `.git`).
pub fn is_populated(dir: &Path) -> bool {
    rgit_submodule::gitdir::is_populated(dir)
}

/// The commit checked out in the submodule at `dir`.
pub fn sub_head(dir: &Path, hash: HashKind) -> Option<ObjectId> {
    repository::submodule_head(dir, hash)
}

/// Connect a work tree and a git directory: a `.git` file in the work tree
/// pointing at the git directory and `core.worktree` pointing back, both
/// relative.
pub fn connect(work_tree: &Path, git_dir: &Path) -> Result<(), Die> {
    let wt = real(work_tree);
    let gd = real(git_dir);
    let to_git = relative_path(&gd, &wt);
    let to_wt = relative_path(&wt, &gd);
    std::fs::write(
        work_tree.join(".git"),
        format!("gitdir: {}\n", to_git.to_string_lossy()),
    )
    .map_err(|e| Die::Fatal(e.to_string()))?;
    set_in_file(
        &git_dir.join("config"),
        "core.worktree",
        Some(&to_wt.to_string_lossy()),
    )
}

/// Move the `.git` directory of the work tree `dir` to `target` and
/// connect them.
pub fn relocate_git_dir(dir: &Path, target: &Path) -> Result<(), Die> {
    move_git_dir(&dir.join(".git"), target)?;
    connect(dir, target)
}

/// Move the git directory `from` to `target`. The git directories of
/// nested submodules below its `modules/` move along; their work trees are
/// connected again.
pub fn move_git_dir(from: &Path, target: &Path) -> Result<(), Die> {
    let mut nested = Vec::new();
    nested_git_dirs(&from.join("modules"), &mut nested);
    let nested: Vec<(PathBuf, PathBuf)> = nested
        .into_iter()
        .filter_map(|g| {
            let wt = get_string(&sub_config(&g), "core.worktree")?;
            let wt = absolute(&g.join(wt));
            let rel = g.strip_prefix(from).ok()?.to_path_buf();
            Some((rel, wt))
        })
        .collect();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Die::Fatal(e.to_string()))?;
    }
    std::fs::rename(from, target).map_err(|e| Die::Fatal(e.to_string()))?;
    for (rel, wt) in nested {
        if wt.join(".git").is_file() {
            connect(&wt, &target.join(rel))?;
        }
    }
    Ok(())
}

/// Every git directory below `dir` (submodules of submodules included).
fn nested_git_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if p.join("HEAD").is_file() && p.join("config").is_file() {
            out.push(p.clone());
            nested_git_dirs(&p.join("modules"), out);
        } else {
            // Names with slashes are nested directories.
            nested_git_dirs(&p, out);
        }
    }
}

/// Run a child, its output going to ours; the exit code.
pub fn run_status(cmd: &mut Command) -> Result<i32, Die> {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let status = cmd.status().map_err(|e| Die::Fatal(e.to_string()))?;
    // A child killed by SIGPIPE: our output goes to the same closed pipe;
    // end quietly with the same status.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if status.signal() == Some(libc::SIGPIPE) {
            std::process::exit(128 + libc::SIGPIPE);
        }
    }
    Ok(status.code().unwrap_or(128))
}

/// Run a child quietly; its standard output when it succeeds.
pub fn capture(cmd: &mut Command) -> Option<String> {
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The configuration of the submodule repository with git directory
/// `git_dir`.
pub fn sub_config(git_dir: &Path) -> ConfigSet {
    std::fs::read(git_dir.join("config"))
        .ok()
        .and_then(|d| {
            rgit_config::parse_bytes(&d, Origin::File(git_dir.join("config")), Scope::Local).ok()
        })
        .unwrap_or_default()
}

/// The remote a submodule fetches from: the remote of its current branch,
/// else `origin`.
pub fn sub_default_remote(git_dir: &Path) -> String {
    let config = sub_config(git_dir);
    let head = rgit_refs::read_head_text(git_dir).unwrap_or_default();
    let target = head.trim().strip_prefix("ref: ").map(str::to_owned);
    default_remote(&config, target.as_deref())
}

/// The path to the top of a directory `path` (`a/b` gives `../../`).
pub fn up_path(path: &str) -> String {
    "../".repeat(path.split('/').filter(|c| !c.is_empty()).count())
}

/// Ask git's question for a pathless command: the option list ends at
/// `--`; is `-h` among the options?
pub fn wants_help(args: &[String]) -> bool {
    for a in args {
        if a == "--" || !a.starts_with('-') {
            return false;
        }
        if a == "-h" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_urls() {
        let r = |rel, base| resolve_relative_url(rel, base, None).unwrap();
        assert_eq!(r("../subrepo", "user@host:repo"), "user@host:subrepo");
        assert_eq!(r("../subrepo", "foo"), "subrepo");
        assert_eq!(r("../bar/a/b/c", "../foo/bar.git"), "../foo/bar/a/b/c");
        assert_eq!(r("./sub", "/abs/super"), "/abs/super/sub");
    }

    #[test]
    fn paths() {
        assert_eq!(up_path("a/b"), "../../");
        assert_eq!(
            relative_path(Path::new("/a/b/c"), Path::new("/a/d")),
            PathBuf::from("../b/c")
        );
    }
}
