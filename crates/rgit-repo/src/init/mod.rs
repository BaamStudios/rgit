// SPDX-License-Identifier: MIT
//! Creating and reinitialising repositories (`git init`).
//!
//! Behaviour follows `git-init(1)`, `gitrepository-layout(5)` and
//! `git-config(1)` (`init.*`, `core.*`) and was checked against the git
//! binary: which files and directories are created with which modes, which
//! configuration keys are written in which order, and what a second run
//! changes. The caller handles the `<directory>` argument and the
//! environment (`GIT_DIR`, `GIT_WORK_TREE`, ...) and passes absolute paths;
//! the library never changes the process working directory.

mod refname;
mod template;
mod worktrees;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use bstr::{BString, ByteSlice};
use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{
    CommandLine, ConfigSet, Environment, LoadOptions, Origin, RepoFacts, Scope, SetRequest,
    StaticRepoFacts, ValuePattern,
};
use rgit_fs::{LockOptions, SharedRepository, SharedRepositoryError};
use rgit_hash::HashKind;

pub use refname::{is_valid_branch_name, is_valid_ref_name};
pub use template::Template;

use crate::error::{Error, Level, io_text};
use crate::format::{RefStorage, RepositoryFormat, Support};
use crate::layout::{self, LayoutOverrides};
use crate::{messages, path};

/// The branch name used when nothing else names one.
pub const DEFAULT_BRANCH: &str = "master";

/// What [`init`] should do. Paths are absolute.
#[derive(Debug, Clone)]
pub struct InitOptions {
    /// Where the repository goes: the git directory, or with
    /// [`InitOptions::separate_git_dir`] the `.git` file pointing to it.
    /// An existing `.git` file is followed.
    pub git_dir: PathBuf,
    /// `--separate-git-dir`: the real repository location (symbolic links
    /// resolved). An existing repository at [`InitOptions::git_dir`] is
    /// moved there.
    pub separate_git_dir: Option<PathBuf>,
    /// Create a bare repository.
    pub bare: bool,
    /// An explicitly given work tree (`GIT_WORK_TREE`), symbolic links
    /// resolved; recorded as `core.worktree` unless it is the directory
    /// containing [`InitOptions::git_dir`] named `.git`.
    pub work_tree: Option<PathBuf>,
    /// `GIT_OBJECT_DIRECTORY`.
    pub object_dir: Option<PathBuf>,
    /// The template; `None` reads `init.templateDir` and falls back to the
    /// built-in template.
    pub template: Option<Template>,
    /// `--shared`; `None` reads `core.sharedRepository`.
    pub shared: Option<SharedRepository>,
    /// `--object-format`.
    pub object_format: Option<HashKind>,
    /// `--ref-format`.
    pub ref_format: Option<RefStorage>,
    /// `-b`/`--initial-branch`.
    pub initial_branch: Option<String>,
}

/// Something to tell the user while [`init`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `warning: <text>`.
    Warning(String),
    /// The initial branch got the built-in default name because neither
    /// `-b` nor `init.defaultBranch` named one (git's `defaultBranchName`
    /// advice).
    DefaultBranchName,
}

/// The result of [`init`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// A repository existed already (its `HEAD` was there).
    pub reinitialized: bool,
    /// `core.sharedRepository` is something other than `umask`.
    pub shared: bool,
    /// The git directory, absolute with symbolic links resolved.
    pub git_dir: PathBuf,
}

/// Why [`init`] failed. Every error ends the command with exit code 128;
/// [`InitError::report`] gives the lines to print.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// A repository or setup problem.
    #[error(transparent)]
    Repo(#[from] Error),
    /// Reading configuration failed.
    #[error(transparent)]
    Config(#[from] rgit_config::Error),
    /// Writing the configuration file failed.
    #[error(transparent)]
    Write(#[from] rgit_config::WriteError),
    /// `core.sharedRepository` is invalid.
    #[error(transparent)]
    Shared(#[from] SharedRepositoryError),
    /// Anything else: `fatal: <message>`.
    #[error("{0}")]
    Fatal(String),
    /// A message printed without a prefix, ending the command with `code`.
    #[error("{message}")]
    Plain {
        /// The line.
        message: String,
        /// The exit code.
        code: i32,
    },
}

impl InitError {
    /// The exit code: 128, except for [`InitError::Plain`].
    pub fn exit_code(&self) -> i32 {
        match self {
            InitError::Plain { code, .. } => *code,
            _ => 128,
        }
    }

    /// The lines to print, in order (the last one is usually `fatal:`);
    /// `None` as level means no prefix.
    pub fn report(&self) -> Vec<(Option<Level>, String)> {
        if let InitError::Plain { message, .. } = self {
            return vec![(None, message.clone())];
        }
        self.prefixed_report()
            .into_iter()
            .map(|(l, m)| (Some(l), m))
            .collect()
    }

    fn prefixed_report(&self) -> Vec<(Level, String)> {
        match self {
            InitError::Repo(e) => e.report(),
            InitError::Config(e) => e.report(),
            InitError::Write(e) => e.report(),
            _ => vec![(Level::Fatal, self.to_string())],
        }
    }
}

impl From<rgit_config::ValueError> for InitError {
    fn from(e: rgit_config::ValueError) -> Self {
        InitError::Repo(Error::Value(e))
    }
}

/// Read the configuration `init` uses: system, global and command line,
/// plus the repository's own files when `repo` names one (`git_dir`,
/// `common_dir`). `includeIf.onbranch` sees the branch in `git_dir/HEAD`.
pub fn load_config(
    env: &dyn Environment,
    command_line: &CommandLine,
    repo: Option<(&Path, &Path)>,
) -> Result<ConfigSet, rgit_config::Error> {
    let facts = repo.map(|(git_dir, _)| StaticRepoFacts {
        git_dir: Some(git_dir.to_path_buf()),
        worktree: None,
        branch: head_branch(git_dir),
    });
    let repo_config = repo.map(|(git_dir, common_dir)| RepoConfig {
        common_dir: common_dir.to_path_buf(),
        git_dir: git_dir.to_path_buf(),
        worktree_config: None,
    });
    let mut set = ConfigSet::new();
    sources::load_standard(
        LoadOptions {
            env,
            repo: facts.as_ref().map(|f| f as &dyn RepoFacts),
            includes: true,
        },
        repo_config.as_ref(),
        command_line,
        ScopeFilter::default(),
        &mut set,
    )?;
    Ok(set)
}

/// The branch `HEAD` in `git_dir` points to (without `refs/heads/`).
fn head_branch(git_dir: &Path) -> Option<BString> {
    let data = fs::read(git_dir.join("HEAD")).ok()?;
    let rest = data.strip_prefix(b"ref:")?.trim_ascii();
    rest.strip_prefix(b"refs/heads/").map(BString::from)
}

/// Parse an `--object-format` / `GIT_DEFAULT_HASH` value.
pub fn parse_hash(name: &str) -> Result<HashKind, InitError> {
    HashKind::from_name(name).ok_or_else(|| InitError::Fatal(messages::unknown_hash(name)))
}

/// Parse a `--ref-format` / `GIT_DEFAULT_REF_FORMAT` value.
pub fn parse_ref_format(name: &str) -> Result<RefStorage, InitError> {
    match name {
        "files" => Ok(RefStorage::Files),
        "reftable" => Ok(RefStorage::Reftable),
        _ => Err(InitError::Fatal(messages::unknown_ref_format(name))),
    }
}

/// Where an existing repository is, seen from the `.git` location.
#[derive(Debug, Clone)]
struct Located {
    /// The git directory (a `.git` file followed).
    git_dir: PathBuf,
    /// Its common directory.
    common_dir: PathBuf,
    /// `git_dir` belongs to a linked work tree (it has a `commondir` file).
    linked: bool,
    /// The location was a `.git` file.
    gitfile: bool,
}

fn locate(link: &Path) -> Result<Located, InitError> {
    let mut git_dir = link.to_path_buf();
    let mut gitfile = false;
    if fs::metadata(link).is_ok_and(|m| m.is_file()) {
        let base = link.parent().unwrap_or(Path::new("/"));
        git_dir =
            layout::read_gitfile(link, base, &LayoutOverrides::default()).map_err(|kind| {
                Error::Gitfile {
                    kind,
                    path: link.to_path_buf(),
                }
            })?;
        gitfile = true;
    }
    let common = layout::common_dir_of(&git_dir);
    Ok(Located {
        linked: common.is_some(),
        common_dir: common.map_or_else(|| git_dir.clone(), |c| path::normalize(&c)),
        git_dir,
        gitfile,
    })
}

/// Create a repository or reinitialise an existing one.
///
/// `early` is the configuration read before the repository was looked at
/// ([`load_config`] without a repository); it is reloaded with the
/// repository's own file when there is one. `notify` receives warnings and
/// advice as they happen.
pub fn init(
    opts: &InitOptions,
    env: &dyn Environment,
    command_line: &CommandLine,
    early: ConfigSet,
    notify: &mut dyn FnMut(Notice),
) -> Result<Outcome, InitError> {
    let link = path::normalize(&opts.git_dir);
    let mut located = locate(&link)?;
    let existing_config = located.common_dir.join("config");
    let has_config = existing_config.is_file();
    let mut config = if has_config {
        load_config(
            env,
            command_line,
            Some((&located.git_dir, &located.common_dir)),
        )?
    } else {
        early
    };

    // The format of an existing repository.
    let existing = if has_config {
        let data = fs::read(&existing_config).unwrap_or_default();
        let set = rgit_config::parse_bytes(&data, Origin::File(existing_config), Scope::Local)?;
        let format = RepositoryFormat::from_config(&set)?;
        format.check()?;
        Some(format)
    } else {
        None
    };

    if opts.separate_git_dir.is_some() {
        let bare_common =
            located.linked && config.bool("core.bare").ok().flatten().unwrap_or(false);
        if opts.bare || bare_common {
            return Err(InitError::Fatal(messages::SEPARATE_GIT_DIR_BARE.into()));
        }
    }

    let mut shared = shared_setting(opts, &config)?;

    // Object and reference formats.
    let hash = choose_hash(opts, env, &config, notify)?;
    let refs = choose_ref_format(opts, env, &config, notify)?;
    match &existing {
        Some(f) => {
            if opts.object_format.is_some_and(|h| h != f.object_format) {
                return Err(InitError::Fatal(messages::REINIT_DIFFERENT_HASH.into()));
            }
            if opts.ref_format.is_some_and(|r| r != f.ref_storage) {
                return Err(InitError::Fatal(
                    messages::REINIT_DIFFERENT_REF_FORMAT.into(),
                ));
            }
            f.check_support(&Support::default())?;
        }
        None => {
            if hash == HashKind::Sha256 {
                return Err(InitError::Fatal(messages::init_unsupported(
                    "extensions.objectFormat=sha256",
                )));
            }
            if refs == RefStorage::Reftable {
                return Err(InitError::Fatal(messages::init_unsupported(
                    "extensions.refStorage=reftable",
                )));
            }
        }
    }

    // --separate-git-dir: move an existing repository, write the link later.
    let mut gitfile_at = None;
    if let Some(sep) = &opts.separate_git_dir {
        let exists = located.git_dir.is_dir();
        if exists {
            let (from, shown, link_path) = if located.linked {
                (
                    located.common_dir.clone(),
                    located.common_dir.clone(),
                    located.common_dir.clone(),
                )
            } else if located.gitfile {
                (
                    located.git_dir.clone(),
                    located.git_dir.clone(),
                    link.clone(),
                )
            } else {
                let real = path::real_path_lossy(&link, Path::new("/"));
                (real, link.clone(), link.clone())
            };
            let from_real = path::real_path_lossy(&from, Path::new("/"));
            if &from_real != sep {
                fs::rename(&from_real, sep).map_err(|e| {
                    InitError::Fatal(messages::unable_to_move(
                        shown.display(),
                        sep.display(),
                        io_text(&e),
                    ))
                })?;
                let relative = config
                    .bool("worktree.userelativepaths")
                    .ok()
                    .flatten()
                    .unwrap_or(false);
                worktrees::repair_after_move(&from_real, sep, relative)?;
            }
            gitfile_at = Some(link_path);
        } else {
            gitfile_at = Some(link.clone());
        }
        located = Located {
            git_dir: sep.clone(),
            common_dir: sep.clone(),
            linked: false,
            gitfile: false,
        };
    }
    let git_dir = located.git_dir.clone();
    let common_dir = located.common_dir.clone();

    // The git directory itself always gets the shared permissions.
    if !git_dir.is_dir() {
        create_dir_all(&git_dir)?;
    }
    shared.apply(&git_dir).map_err(|e| InitError::Plain {
        message: format!("{}: {}", git_dir.display(), io_text(&e)),
        code: 1,
    })?;

    // Templates go to the common directory.
    let template = match &opts.template {
        Some(t) => t.clone(),
        None => match config.path("init.templatedir", env)? {
            Some(p) => Template::Dir(p),
            None => Template::Builtin,
        },
    };
    template::copy(&template, &common_dir, shared, notify)?;

    let reinitialized = fs::symlink_metadata(git_dir.join("HEAD")).is_ok();

    // A template may have brought a configuration file.
    let config_path = common_dir.join("config");
    if !has_config && config_path.is_file() {
        config = load_config(env, command_line, Some((&git_dir, &common_dir)))?;
        shared = shared_setting(opts, &config)?;
    }

    write_config(
        &config_path,
        &config,
        &ConfigValues {
            existing_version: existing.as_ref().map(|f| f.version),
            git_dir: &git_dir,
            link: &link,
            bare: opts.bare,
            work_tree: opts.work_tree.as_deref(),
            fresh: !reinitialized,
            shared,
        },
    )?;

    let refs_dir = common_dir.join("refs");
    template::make_dir(&refs_dir, shared)?;
    template::make_dir(&refs_dir.join("heads"), shared)?;
    template::make_dir(&refs_dir.join("tags"), shared)?;

    if reinitialized {
        if let Some(b) = &opts.initial_branch {
            notify(Notice::Warning(messages::reinit_ignored_branch(b)));
        }
    } else {
        let branch = initial_branch(opts, env, &config, notify)?;
        let head = format!("ref: refs/heads/{branch}\n");
        template::write_new(&git_dir.join("HEAD"), head.as_bytes(), false, shared)?;
    }

    let objects = opts
        .object_dir
        .clone()
        .unwrap_or_else(|| common_dir.join("objects"));
    template::make_dir(&objects, shared)?;
    template::make_dir(&objects.join("pack"), shared)?;
    template::make_dir(&objects.join("info"), shared)?;

    if let Some(at) = gitfile_at {
        let text = format!("gitdir: {}\n", git_dir.display());
        fs::write(&at, text)
            .map_err(|e| InitError::Fatal(messages::cannot_write(at.display(), io_text(&e))))?;
    }

    Ok(Outcome {
        reinitialized,
        shared: shared != SharedRepository::Umask,
        git_dir: path::real_path_lossy(&git_dir, Path::new("/")),
    })
}

/// Create the git directory and missing leading directories. Like git,
/// a failure is reported as `<path>: <reason>` with exit code 1.
/// `--shared`, else `core.sharedRepository`.
fn shared_setting(opts: &InitOptions, config: &ConfigSet) -> Result<SharedRepository, InitError> {
    if let Some(s) = opts.shared {
        return Ok(s);
    }
    Ok(match config.string("core.sharedrepository")? {
        Some(v) => SharedRepository::parse(&v.to_str_lossy())?,
        None => SharedRepository::Umask,
    })
}

fn create_dir_all(dir: &Path) -> Result<(), InitError> {
    fs::create_dir_all(dir).map_err(|e| InitError::Plain {
        message: format!("{}: {}", dir.display(), io_text(&e)),
        code: 1,
    })
}

/// The object format of a new repository: `--object-format`, else
/// `GIT_DEFAULT_HASH`, else `init.defaultObjectFormat`, else SHA-1. An
/// invalid `init.defaultObjectFormat` is reported even when not used.
fn choose_hash(
    opts: &InitOptions,
    env: &dyn Environment,
    config: &ConfigSet,
    notify: &mut dyn FnMut(Notice),
) -> Result<HashKind, InitError> {
    let mut from_config = None;
    if let Some(v) = config.string("init.defaultobjectformat")? {
        let name = v.to_str_lossy();
        from_config = HashKind::from_name(&name);
        if from_config.is_none() {
            notify(Notice::Warning(messages::unknown_hash(&name)));
        }
    }
    if let Some(h) = opts.object_format {
        return Ok(h);
    }
    if let Some(v) = env.var("GIT_DEFAULT_HASH") {
        return parse_hash(&v.to_string_lossy());
    }
    Ok(from_config.unwrap_or(HashKind::Sha1))
}

/// The ref storage format of a new repository: `--ref-format`, else
/// `GIT_DEFAULT_REF_FORMAT`, else `init.defaultRefFormat`, else reftable
/// with `feature.experimental`, else files. An invalid
/// `init.defaultRefFormat` is reported even when not used.
fn choose_ref_format(
    opts: &InitOptions,
    env: &dyn Environment,
    config: &ConfigSet,
    notify: &mut dyn FnMut(Notice),
) -> Result<RefStorage, InitError> {
    let mut from_config = None;
    if let Some(v) = config.string("init.defaultrefformat")? {
        let name = v.to_str_lossy();
        from_config = parse_ref_format(&name).ok();
        if from_config.is_none() {
            notify(Notice::Warning(messages::unknown_ref_format(&name)));
        }
    }
    if let Some(r) = opts.ref_format {
        return Ok(r);
    }
    if let Some(v) = env.var("GIT_DEFAULT_REF_FORMAT") {
        return parse_ref_format(&v.to_string_lossy());
    }
    if let Some(r) = from_config {
        return Ok(r);
    }
    if config.bool("feature.experimental")? == Some(true) {
        return Ok(RefStorage::Reftable);
    }
    Ok(RefStorage::Files)
}

/// The name of the initial branch of a new repository.
fn initial_branch(
    opts: &InitOptions,
    env: &dyn Environment,
    config: &ConfigSet,
    notify: &mut dyn FnMut(Notice),
) -> Result<String, InitError> {
    if let Some(b) = &opts.initial_branch {
        if !is_valid_branch_name(b) {
            return Err(InitError::Fatal(messages::invalid_initial_branch(b)));
        }
        return Ok(b.clone());
    }
    // The test suite pins the default with this variable.
    let from_env = env
        .var("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME")
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|v| !v.is_empty());
    let name = match from_env {
        Some(v) => Some(v),
        None => config
            .string("init.defaultbranch")?
            .map(|v| v.to_str_lossy().into_owned()),
    };
    match name {
        Some(n) => {
            if !is_valid_branch_name(&n) {
                return Err(InitError::Fatal(messages::invalid_default_branch(&n)));
            }
            Ok(n)
        }
        None => {
            notify(Notice::DefaultBranchName);
            Ok(DEFAULT_BRANCH.to_string())
        }
    }
}

struct ConfigValues<'a> {
    existing_version: Option<i64>,
    git_dir: &'a Path,
    link: &'a Path,
    bare: bool,
    work_tree: Option<&'a Path>,
    fresh: bool,
    shared: SharedRepository,
}

/// Write the `core.*` settings git records, one key at a time and in git's
/// order, keeping everything else in the file.
fn write_config(path: &Path, config: &ConfigSet, v: &ConfigValues<'_>) -> Result<(), InitError> {
    let lock = LockOptions::new().shared(v.shared);
    let set = |key: &str, value: &str| -> Result<(), InitError> {
        let req = SetRequest {
            key: key.as_bytes(),
            value: Some(value.as_bytes()),
            pattern: &ValuePattern::Any,
            all: false,
            comment: None,
        };
        rgit_config::write::set_in_file(path, &lock, &req)?;
        Ok(())
    };
    // The file has to exist for the file mode probe.
    if !path.exists() {
        template::write_new(path, b"", false, v.shared)?;
    }
    let version = v.existing_version.unwrap_or(0).max(0);
    set("core.repositoryformatversion", &version.to_string())?;
    set("core.filemode", bool_str(probe_filemode(path)))?;
    set("core.bare", bool_str(v.bare))?;
    if !v.bare {
        match config.get("core.logallrefupdates") {
            None => set("core.logallrefupdates", "true")?,
            Some(e) => {
                let always = e
                    .value
                    .as_ref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(b"always"));
                if !always {
                    e.bool()?;
                }
            }
        }
    }
    if let Some(wt) = v.work_tree {
        let default_git_dir = wt.join(".git");
        if path::normalize(&default_git_dir) != path::normalize(v.link) {
            set("core.worktree", &wt.to_string_lossy())?;
        }
    }
    if v.fresh {
        if !probe_symlinks(v.git_dir) {
            set("core.symlinks", "false")?;
        }
        if probe_ignorecase(path) {
            set("core.ignorecase", "true")?;
        }
    }
    if let Some(value) = v.shared.config_value() {
        set("core.sharedrepository", &value)?;
        set("receive.denyNonFastforwards", "true")?;
    }
    Ok(())
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// Whether the executable bit of `file` can be changed (and read back).
#[cfg(unix)]
fn probe_filemode(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::metadata(file) else {
        return false;
    };
    let mode = meta.permissions().mode();
    let flipped = mode ^ 0o100;
    if fs::set_permissions(file, fs::Permissions::from_mode(flipped)).is_err() {
        return false;
    }
    let works = fs::metadata(file).is_ok_and(|m| m.permissions().mode() == flipped);
    let _ = fs::set_permissions(file, fs::Permissions::from_mode(mode));
    works
}

#[cfg(not(unix))]
fn probe_filemode(_: &Path) -> bool {
    false
}

/// Whether symbolic links can be created in `dir`.
fn probe_symlinks(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let probe = dir.join(format!("tmp-symlink-probe-{}", std::process::id()));
        let ok = std::os::unix::fs::symlink("testing", &probe).is_ok();
        let _ = fs::remove_file(&probe);
        ok
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// Whether the file system ignores case: `config` is also found under a
/// differently cased name.
fn probe_ignorecase(config: &Path) -> bool {
    let Some(dir) = config.parent() else {
        return false;
    };
    let other = dir.join("CoNfIg");
    match (fs::metadata(config), fs::metadata(&other)) {
        (Ok(_), Ok(_)) => {
            // Both names exist: case-insensitive unless both really exist.
            !fs::read_dir(dir).is_ok_and(|entries| {
                entries
                    .filter_map(Result::ok)
                    .any(|e| e.file_name() == "CoNfIg")
            })
        }
        _ => false,
    }
}

/// Create the directory given to `git init <directory>` and any missing
/// leading directories (mode from the umask). An existing directory is
/// fine. On failure the error says which `errno` git would report.
pub fn create_init_directory(dir: &Path) -> io::Result<()> {
    match fs::create_dir(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return if dir.is_dir() { Ok(()) } else { Err(e) };
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) => {}
        Err(e) => return Err(e),
    }
    // Create the leading directories one by one.
    let mut prefix = PathBuf::new();
    let comps: Vec<_> = dir.components().collect();
    for c in &comps[..comps.len().saturating_sub(1)] {
        prefix.push(c);
        match fs::symlink_metadata(&prefix) {
            Ok(m) if m.is_dir() => continue,
            Ok(m) if m.file_type().is_symlink() && prefix.is_dir() => continue,
            Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
            Err(_) => match fs::create_dir(&prefix) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && prefix.is_dir() => {}
                Err(e) => return Err(e),
            },
        }
    }
    fs::create_dir(dir)
}

/// git's text for an I/O error (`strerror`).
pub fn strerror(e: &io::Error) -> String {
    io_text(e)
}

#[cfg(test)]
mod tests;
