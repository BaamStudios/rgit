// SPDX-License-Identifier: MIT
//! Locating the repository and work tree for a command.
//!
//! The rules (from `git(1)` ENVIRONMENT, `gitrepository-layout(5)`,
//! `git-config(1)` `core.worktree`/`core.bare`, and the behaviour of the git
//! binary):
//!
//! 1. `GIT_DIR` (or `--git-dir`) names the repository, relative to the
//!    current directory; a `.git` file there is followed. Otherwise the
//!    directories from the current one upwards are searched, first for a
//!    `.git` directory or file, then for the directory itself being a
//!    repository (bare, or the inside of a `.git` directory). The search does
//!    not go up into a `GIT_CEILING_DIRECTORIES` entry nor across file
//!    systems unless `GIT_DISCOVERY_ACROSS_FILESYSTEM` is set.
//! 2. The work tree is `GIT_WORK_TREE` (relative to the current directory),
//!    else `core.worktree` (relative to the git directory), else none when
//!    `core.bare` is true, else the current directory (with `GIT_DIR`) or the
//!    directory holding `.git` (found by the search). A repository found
//!    from inside itself has no work tree.
//! 3. When the current directory is inside the work tree, commands run at
//!    the top of the work tree and the prefix is the path back down to the
//!    original directory; otherwise the directory is unchanged and there is
//!    no prefix.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bstr::ByteSlice;
use rgit_config::{CommandLine, ConfigSet, Environment, LoadOptions, Origin, Scope};

use crate::error::{Error, NotFound};
use crate::format::{RepositoryFormat, Support};
use crate::layout::{self, LayoutOverrides};
use crate::messages;
use crate::path;
use crate::safe::{self, BareRepository, Owner};

/// Inputs of repository setup: the environment and global options.
#[derive(Debug, Clone)]
pub struct SetupOptions {
    /// The physical current directory.
    pub cwd: PathBuf,
    /// `GIT_DIR` / `--git-dir`.
    pub git_dir: Option<PathBuf>,
    /// `GIT_WORK_TREE` / `--work-tree`.
    pub work_tree: Option<PathBuf>,
    /// `GIT_COMMON_DIR`.
    pub common_dir: Option<PathBuf>,
    /// `GIT_OBJECT_DIRECTORY` (only used to recognise repositories).
    pub object_dir: Option<PathBuf>,
    /// `GIT_CEILING_DIRECTORIES`.
    pub ceiling_dirs: Option<OsString>,
    /// `GIT_DISCOVERY_ACROSS_FILESYSTEM`.
    pub across_filesystem: bool,
    /// `GIT_IMPLICIT_WORK_TREE` (false after `--bare`): with `GIT_DIR` and
    /// no other work tree setting, use the current directory.
    pub implicit_work_tree: bool,
    /// Who may own the repository.
    pub owner: Owner,
    /// Extensions this build can handle.
    pub support: Support,
}

impl SetupOptions {
    /// Options from the environment variables in `env`, with `cwd` as the
    /// current directory.
    pub fn from_env(env: &dyn Environment, cwd: PathBuf) -> SetupOptions {
        let path = |name: &str| env.var(name).map(PathBuf::from);
        SetupOptions {
            cwd,
            git_dir: path("GIT_DIR"),
            work_tree: path("GIT_WORK_TREE"),
            common_dir: path("GIT_COMMON_DIR").filter(|p| !p.as_os_str().is_empty()),
            object_dir: path("GIT_OBJECT_DIRECTORY").filter(|p| !p.as_os_str().is_empty()),
            ceiling_dirs: env.var("GIT_CEILING_DIRECTORIES"),
            across_filesystem: safe::env_bool(env, "GIT_DISCOVERY_ACROSS_FILESYSTEM"),
            implicit_work_tree: env
                .var_bytes("GIT_IMPLICIT_WORK_TREE")
                .is_none_or(|v| rgit_config::value::parse_bool(Some(&v)).unwrap_or(true)),
            owner: Owner::current(env),
            support: Support::default(),
        }
    }

    /// Options from the process environment and current directory.
    pub fn from_process() -> Result<SetupOptions, Error> {
        let cwd = std::env::current_dir().map_err(Error::Cwd)?;
        Ok(SetupOptions::from_env(
            &rgit_config::SystemEnv::default(),
            cwd,
        ))
    }

    fn overrides(&self) -> LayoutOverrides {
        LayoutOverrides {
            common_dir: self.common_dir.clone(),
            object_dir: self.object_dir.clone(),
        }
    }
}

/// Where protected configuration (`safe.*`) comes from.
pub trait ProtectedConfig {
    /// The system, global and command-line configuration.
    fn load(&mut self) -> Result<ConfigSet, Error>;
    /// The environment for path interpolation.
    fn env(&self) -> &dyn Environment;
}

/// The standard protected configuration: system and global files and the
/// command line, read with `env`.
pub struct StandardProtectedConfig<'a> {
    env: &'a dyn Environment,
    command_line: CommandLine,
    cached: Option<ConfigSet>,
}

impl<'a> StandardProtectedConfig<'a> {
    /// Read with `env`; `command_line` holds `-c`/`--config-env` (the
    /// `GIT_CONFIG_*` variables are read from `env`).
    pub fn new(env: &'a dyn Environment, command_line: CommandLine) -> Self {
        StandardProtectedConfig {
            env,
            command_line,
            cached: None,
        }
    }
}

impl ProtectedConfig for StandardProtectedConfig<'_> {
    fn load(&mut self) -> Result<ConfigSet, Error> {
        if let Some(c) = &self.cached {
            return Ok(c.clone());
        }
        let mut set = ConfigSet::new();
        let filter = rgit_config::sources::ScopeFilter {
            local: false,
            worktree: false,
            ..Default::default()
        };
        rgit_config::sources::load_standard(
            LoadOptions {
                env: self.env,
                repo: None,
                includes: true,
            },
            None,
            &self.command_line,
            filter,
            &mut set,
        )?;
        self.cached = Some(set.clone());
        Ok(set)
    }

    fn env(&self) -> &dyn Environment {
        self.env
    }
}

/// The result of repository setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// The git directory as git reports it: relative to [`Setup::cwd`] or
    /// absolute.
    pub git_dir: PathBuf,
    /// The common directory (holds objects, refs and `config`).
    pub common_dir: PathBuf,
    /// The top of the work tree (absolute), if any.
    pub work_tree: Option<PathBuf>,
    /// The directory commands run in (the top of the work tree when the
    /// original directory is inside it).
    pub cwd: PathBuf,
    /// The original directory relative to the work tree, with a trailing
    /// `/`; `None` at the top or outside the work tree.
    pub prefix: Option<OsString>,
    /// The directory the command was started in.
    pub original_cwd: PathBuf,
    /// The repository format and its `core.bare` / `core.worktree`.
    pub format: RepositoryFormat,
    /// `GIT_DIR` was given.
    pub explicit: bool,
    /// `core.bare` and `core.worktree` are both set: commands that need a
    /// work tree fail.
    pub invalid_work_tree_config: bool,
    /// Warnings to print (without `warning: `).
    pub warnings: Vec<String>,
}

impl Setup {
    /// `git rev-parse --is-bare-repository`: no work tree, and `core.bare`
    /// is not false.
    pub fn is_bare(&self) -> bool {
        self.work_tree.is_none() && self.format.bare != Some(false)
    }

    /// The git directory as an absolute path.
    pub fn absolute_git_dir(&self) -> PathBuf {
        path::real_path_lossy(&self.git_dir, &self.cwd)
    }

    /// The common directory as an absolute path.
    pub fn absolute_common_dir(&self) -> PathBuf {
        path::real_path_lossy(&self.common_dir, &self.cwd)
    }

    /// The work tree, for commands that need one.
    pub fn require_work_tree(&self) -> Result<&Path, Error> {
        if self.invalid_work_tree_config {
            return Err(Error::InvalidWorkTreeConfig);
        }
        self.work_tree.as_deref().ok_or(Error::NoWorkTree)
    }

    /// Whether the original directory is inside the work tree.
    pub fn is_inside_work_tree(&self) -> bool {
        match &self.work_tree {
            Some(wt) => self.original_cwd.starts_with(wt),
            None => false,
        }
    }

    /// Whether the original directory is inside the git directory.
    pub fn is_inside_git_dir(&self) -> bool {
        self.original_cwd.starts_with(self.absolute_git_dir())
    }

    /// Write the `GIT_TRACE_SETUP` lines.
    #[track_caller]
    pub fn trace(&self) {
        rgit_trace::trace_setup(
            &self.git_dir,
            &self.common_dir,
            self.work_tree.as_deref(),
            &self.cwd,
            self.prefix.as_deref(),
        );
    }
}

/// Locate the repository. `Ok(None)`: there is none (for commands that can
/// run outside a repository); the reason is in the returned [`NotFound`].
pub fn discover(
    opts: &SetupOptions,
    protected: &mut dyn ProtectedConfig,
) -> Result<Result<Setup, NotFound>, Error> {
    match &opts.git_dir {
        Some(gd) => explicit(opts, gd).map(Ok),
        None => search(opts, protected),
    }
}

/// Locate the repository; not finding one is an error.
pub fn setup(opts: &SetupOptions, protected: &mut dyn ProtectedConfig) -> Result<Setup, Error> {
    discover(opts, protected)?.map_err(Error::NotFound)
}

/// A repository directory before the work tree is decided.
struct Candidate {
    /// As reported when the current directory does not change.
    display: PathBuf,
    /// Absolute path.
    abs: PathBuf,
    /// The repository configuration file, as messages show it.
    config_display: PathBuf,
}

fn explicit(opts: &SetupOptions, given: &Path) -> Result<Setup, Error> {
    let cwd = &opts.cwd;
    let ov = opts.overrides();
    let abs = path::absolute(given, cwd);
    let cand = if abs.is_file() || (abs.exists() && !abs.is_dir()) {
        let target = layout::read_gitfile(given, cwd, &ov).map_err(|kind| Error::Gitfile {
            kind,
            path: given.to_path_buf(),
        })?;
        Candidate {
            display: target.clone(),
            config_display: target.join("config"),
            abs: target,
        }
    } else {
        if given.as_os_str().is_empty() || !layout::is_git_directory(&abs, cwd, &ov) {
            return Err(Error::NotAGitDir(given.to_path_buf()));
        }
        Candidate {
            display: given.to_path_buf(),
            config_display: given.join("config"),
            abs,
        }
    };
    let format = read_format(opts, &cand)?;
    let mut warnings = Vec::new();
    let (work_tree, invalid) = match configured_work_tree(opts, &cand, &format, &mut warnings)? {
        WorkTree::Some(wt) => (Some(wt), false),
        WorkTree::Conflict => (None, true),
        WorkTree::Unset if format.bare == Some(true) || !opts.implicit_work_tree => (None, false),
        WorkTree::Unset => (Some(cwd.clone()), false),
    };
    Ok(finish(
        opts, cand, format, work_tree, true, invalid, warnings,
    ))
}

enum WorkTree {
    Some(PathBuf),
    Unset,
    Conflict,
}

/// `GIT_WORK_TREE`, else `core.worktree` (unless `core.bare` is set too).
fn configured_work_tree(
    opts: &SetupOptions,
    cand: &Candidate,
    format: &RepositoryFormat,
    warnings: &mut Vec<String>,
) -> Result<WorkTree, Error> {
    if let Some(wt) = &opts.work_tree {
        if wt.as_os_str().is_empty() {
            return Err(Error::EmptyWorkTree);
        }
        return Ok(WorkTree::Some(path::real_path(wt, &opts.cwd)?));
    }
    let Some(cfg) = &format.worktree else {
        return Ok(WorkTree::Unset);
    };
    if format.bare == Some(true) {
        warnings.push(messages::BARE_AND_WORKTREE.into());
        return Ok(WorkTree::Conflict);
    }
    let value = layout::bytes_to_path(cfg.as_bytes());
    let base = path::real_path_lossy(&cand.abs, &opts.cwd);
    // git enters the directory, so it must exist.
    let resolved = std::fs::canonicalize(base.join(&value)).and_then(|p| {
        if p.is_dir() {
            Ok(p)
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::NotADirectory))
        }
    });
    match resolved {
        Ok(p) => Ok(WorkTree::Some(p)),
        Err(source) => Err(Error::CannotChdir {
            path: value,
            source,
        }),
    }
}

fn read_format(opts: &SetupOptions, cand: &Candidate) -> Result<RepositoryFormat, Error> {
    let (file, display) = match &opts.common_dir {
        Some(c) => (
            path::absolute(c, &opts.cwd).join("config"),
            c.join("config"),
        ),
        None => match layout::common_dir_of(&cand.abs) {
            Some(c) => {
                let c = path::real_path_lossy(&c, &opts.cwd);
                (c.join("config"), c.join("config"))
            }
            None => (cand.abs.join("config"), cand.config_display.clone()),
        },
    };
    let set = match std::fs::read(&file) {
        Ok(data) => rgit_config::parse_bytes(&data, Origin::File(display), Scope::Local)?,
        Err(_) => ConfigSet::new(),
    };
    let mut format = RepositoryFormat::from_config(&set)?;
    format.check()?;
    format.check_support(&opts.support)?;
    // A linked work tree (its git directory has a separate common
    // directory) does not take core.worktree / core.bare from the common
    // configuration (t1501 "$GIT_DIR/common overrides core.worktree").
    let own_config = path::real_path_lossy(&cand.abs, &opts.cwd).join("config");
    if path::real_path_lossy(&file, &opts.cwd) != own_config {
        format.worktree = None;
        format.bare = None;
    }
    Ok(format)
}

fn finish(
    opts: &SetupOptions,
    cand: Candidate,
    format: RepositoryFormat,
    work_tree: Option<PathBuf>,
    explicit: bool,
    invalid_work_tree_config: bool,
    warnings: Vec<String>,
) -> Setup {
    let cwd = &opts.cwd;
    let (git_dir, new_cwd, prefix) = match &work_tree {
        Some(wt) => match path::prefix_of(cwd, wt) {
            Some(prefix) => (
                path::real_path_lossy(&cand.abs, cwd),
                wt.clone(),
                Some(prefix),
            ),
            None => (cand.display, cwd.clone(), None),
        },
        None => (cand.display, cwd.clone(), None),
    };
    let common_dir = common_dir(opts, &git_dir, &new_cwd);
    Setup {
        git_dir,
        common_dir,
        work_tree,
        cwd: new_cwd,
        prefix,
        original_cwd: cwd.clone(),
        format,
        explicit,
        invalid_work_tree_config,
        warnings,
    }
}

fn common_dir(opts: &SetupOptions, git_dir: &Path, cwd: &Path) -> PathBuf {
    if let Some(c) = &opts.common_dir {
        return c.clone();
    }
    let abs = path::absolute(git_dir, cwd);
    match layout::common_dir_of(&abs) {
        Some(c) => path::real_path_lossy(&c, cwd),
        None => git_dir.to_path_buf(),
    }
}

enum Found {
    /// `<dir>/.git` is a directory or a `.git` file.
    DotGit {
        dir: PathBuf,
        gitdir: PathBuf,
        gitfile: Option<PathBuf>,
    },
    /// `<dir>` itself is a repository.
    Bare { dir: PathBuf },
}

fn search(
    opts: &SetupOptions,
    protected: &mut dyn ProtectedConfig,
) -> Result<Result<Setup, NotFound>, Error> {
    let cwd = &opts.cwd;
    let ov = opts.overrides();
    let ceiling = opts
        .ceiling_dirs
        .as_ref()
        .and_then(|c| path::ceiling_for(cwd, c));
    let start_dev = device(cwd);
    let mut dir = cwd.clone();
    let found = loop {
        if let Some(f) = probe(&dir, cwd, &ov)? {
            break f;
        }
        let Some(parent) = dir.parent().map(Path::to_path_buf) else {
            return Ok(Err(NotFound::Ceiling));
        };
        if let Some(c) = &ceiling {
            // Never go up into a ceiling directory.
            if path::strictly_inside(&parent, c).is_none() {
                return Ok(Err(NotFound::Ceiling));
            }
        }
        if !opts.across_filesystem && device(&parent) != start_dev {
            return Ok(Err(NotFound::FilesystemBoundary(parent)));
        }
        dir = parent;
    };

    match found {
        Found::DotGit {
            dir,
            gitdir,
            gitfile,
        } => {
            let mut paths: Vec<&Path> = vec![&dir];
            if let Some(f) = &gitfile {
                paths.push(f);
            }
            paths.push(&gitdir);
            let own_warnings = check_ownership(opts, protected, &paths, &dir)?;
            let cand = match &gitfile {
                Some(_) => Candidate {
                    display: gitdir.clone(),
                    config_display: gitdir.join("config"),
                    abs: gitdir.clone(),
                },
                None => Candidate {
                    display: if dir == *cwd {
                        PathBuf::from(".git")
                    } else {
                        gitdir.clone()
                    },
                    config_display: PathBuf::from(".git/config"),
                    abs: gitdir.clone(),
                },
            };
            let format = read_format(opts, &cand)?;
            let mut warnings = own_warnings;
            let setup = match configured_work_tree(opts, &cand, &format, &mut warnings)? {
                WorkTree::Some(wt) => finish(opts, cand, format, Some(wt), false, false, warnings),
                WorkTree::Conflict => finish(opts, cand, format, None, false, true, warnings),
                WorkTree::Unset if format.bare == Some(true) => {
                    finish(opts, cand, format, None, false, false, warnings)
                }
                WorkTree::Unset => {
                    // The directory holding `.git` is the work tree; run there.
                    let git_dir = match &gitfile {
                        Some(_) => gitdir.clone(),
                        None => PathBuf::from(".git"),
                    };
                    let common_dir = common_dir(opts, &git_dir, &dir);
                    Setup {
                        git_dir,
                        common_dir,
                        prefix: path::prefix_of(cwd, &dir),
                        cwd: dir.clone(),
                        work_tree: Some(dir),
                        original_cwd: cwd.clone(),
                        format,
                        explicit: false,
                        invalid_work_tree_config: false,
                        warnings,
                    }
                }
            };
            Ok(Ok(setup))
        }
        Found::Bare { dir } => {
            let setting = safe::bare_repository_setting(&protected.load()?)?;
            if setting == BareRepository::Explicit && !safe::is_embedded_git_dir(&dir) {
                return Err(Error::BareNotAllowed(dir));
            }
            let own_warnings = check_ownership(opts, protected, &[&dir], &dir)?;
            let cand = Candidate {
                display: if dir == *cwd {
                    PathBuf::from(".")
                } else {
                    dir.clone()
                },
                config_display: PathBuf::from("./config"),
                abs: dir.clone(),
            };
            let format = read_format(opts, &cand)?;
            let mut warnings = own_warnings;
            let (wt, invalid) = match configured_work_tree(opts, &cand, &format, &mut warnings)? {
                WorkTree::Some(wt) => (Some(wt), false),
                WorkTree::Conflict => (None, true),
                WorkTree::Unset => (None, false),
            };
            Ok(Ok(finish(opts, cand, format, wt, false, invalid, warnings)))
        }
    }
}

fn check_ownership(
    opts: &SetupOptions,
    protected: &mut dyn ProtectedConfig,
    paths: &[&Path],
    report: &Path,
) -> Result<Vec<String>, Error> {
    if paths.iter().all(|p| opts.owner.owns(p)) {
        return Ok(Vec::new());
    }
    let report = path::real_path_lossy(report, &opts.cwd);
    let set = protected.load()?;
    let (safe, warnings) = safe::is_safe_directory(&set, protected.env(), &report, &opts.cwd);
    if safe {
        Ok(warnings)
    } else {
        Err(Error::DubiousOwnership {
            path: report,
            warnings,
        })
    }
}

/// Look for a repository at `dir`: `dir/.git` first, then `dir` itself.
fn probe(dir: &Path, cwd: &Path, ov: &LayoutOverrides) -> Result<Option<Found>, Error> {
    let dotgit = dir.join(".git");
    match std::fs::metadata(&dotgit) {
        Ok(m) if m.is_dir() => {
            if layout::is_git_directory(&dotgit, cwd, ov) {
                return Ok(Some(Found::DotGit {
                    dir: dir.to_path_buf(),
                    gitdir: dotgit,
                    gitfile: None,
                }));
            }
        }
        Ok(_) => {
            let gitdir = layout::read_gitfile(&dotgit, cwd, ov).map_err(|kind| Error::Gitfile {
                kind,
                path: dotgit.clone(),
            })?;
            return Ok(Some(Found::DotGit {
                dir: dir.to_path_buf(),
                gitdir,
                gitfile: Some(dotgit),
            }));
        }
        Err(_) => {}
    }
    if layout::is_git_directory(dir, cwd, ov) {
        return Ok(Some(Found::Bare {
            dir: dir.to_path_buf(),
        }));
    }
    Ok(None)
}

#[cfg(unix)]
fn device(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device(_p: &Path) -> Option<u64> {
    None
}
