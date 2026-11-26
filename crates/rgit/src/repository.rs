// SPDX-License-Identifier: MIT
//! Opening a repository (ADR 0005, "Repository handle" and "I/O").

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use bstr::BString;
use rgit_config::cmdline::Parameter;
use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{CommandLine, ConfigSet, Environment, LoadOptions, MapEnv, StaticRepoFacts};
use rgit_hash::{HashKind, ObjectId};
use rgit_index::{Index, ReadOptions};
use rgit_object::ObjectKind;
use rgit_odb::{LargeObjectOptions, LooseOptions, ObjectDatabase};
use rgit_refs::files::FilesStore;
use rgit_repo::safe::Owner;
use rgit_repo::{Setup, SetupOptions, StandardProtectedConfig, Support};

use crate::error::{Diagnostic, Level};
use crate::{Error, Result, messages};

/// The environment a repository is opened with; see
/// [`OpenOptions::environment`].
pub(crate) type Env = dyn Environment + Send + Sync;

/// How to open a repository.
///
/// Nothing the library does depends on the current directory or on
/// variables of the process that the caller did not let in: the directory
/// is an argument, and the environment is read through the
/// [`Environment`] given here.
///
/// ```no_run
/// # fn main() -> rgit::Result<()> {
/// // Like the `git` command: the user's configuration applies.
/// let repo = rgit::OpenOptions::new().open("/srv/checkout")?;
///
/// // For a server: no environment, no system or user configuration,
/// // settings given explicitly.
/// let repo = rgit::OpenOptions::isolated()
///     .config_override("core.abbrev", Some("12"))
///     .open("/srv/git/project.git")?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct OpenOptions {
    env: Arc<Env>,
    overrides: Vec<(BString, Option<BString>)>,
    ceiling_dirs: Vec<PathBuf>,
}

impl std::fmt::Debug for OpenOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenOptions")
            .field("overrides", &self.overrides)
            .field("ceiling_dirs", &self.ceiling_dirs)
            .finish_non_exhaustive()
    }
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions::new()
    }
}

impl OpenOptions {
    /// Options that read the system and user configuration through the
    /// environment of the process (`HOME`, `XDG_CONFIG_HOME`,
    /// `GIT_CONFIG_*`), as the `git` command does.
    ///
    /// The variables that select a repository for the `git` command
    /// (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, ...) are not read:
    /// in a library the repository is the one the caller names.
    #[must_use]
    pub fn new() -> OpenOptions {
        OpenOptions {
            env: Arc::new(rgit_config::SystemEnv::default()),
            overrides: Vec::new(),
            ceiling_dirs: Vec::new(),
        }
    }

    /// Options that read nothing but the repository: an empty
    /// environment, no system configuration or attributes and no user
    /// configuration.
    /// The choice for servers and for tests.
    ///
    /// Without user configuration there is no `safe.directory` either, so
    /// a repository owned by another user is refused unless it is allowed
    /// with [`OpenOptions::config_override`].
    #[must_use]
    pub fn isolated() -> OpenOptions {
        OpenOptions::new().environment(MapEnv::new([
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_ATTR_NOSYSTEM", "1"),
        ]))
    }

    /// Read environment variables and home directories through `env`
    /// instead of the process ([`MapEnv`] holds a fixed set).
    #[must_use]
    pub fn environment(mut self, env: impl Environment + Send + Sync + 'static) -> OpenOptions {
        self.env = Arc::new(env);
        self
    }

    /// Set a configuration value that wins over every file, like
    /// `git -c <key>=<value>`. `None` is the value-less form (true for a
    /// boolean).
    #[must_use]
    pub fn config_override(
        mut self,
        key: impl AsRef<[u8]>,
        value: Option<impl AsRef<[u8]>>,
    ) -> OpenOptions {
        self.overrides.push((
            key.as_ref().into(),
            value.as_ref().map(|v| v.as_ref().into()),
        ));
        self
    }

    /// A directory [`OpenOptions::discover`] does not search in or above
    /// (git's `GIT_CEILING_DIRECTORIES`). Can be given several times.
    #[must_use]
    pub fn ceiling_dir(mut self, dir: impl Into<PathBuf>) -> OpenOptions {
        self.ceiling_dirs.push(dir.into());
        self
    }

    /// Open the repository at `path`: the top of a work tree, its `.git`
    /// directory, or a bare repository. Directories above `path` are not
    /// searched.
    ///
    /// A relative `path` is resolved against the current directory once,
    /// here; nothing depends on the current directory afterwards.
    ///
    /// # Errors
    /// [`Error::NotARepository`] if there is none, [`Error::Open`] if it
    /// cannot be used, [`Error::Config`] for unreadable configuration.
    pub fn open(&self, path: impl AsRef<Path>) -> Result<Repository> {
        self.open_at(path.as_ref(), false)
    }

    /// Open the repository `path` lies in: `path` itself or the nearest
    /// directory above it that is one, as the `git` command finds its
    /// repository from the current directory.
    ///
    /// # Errors
    /// As [`OpenOptions::open`].
    pub fn discover(&self, path: impl AsRef<Path>) -> Result<Repository> {
        self.open_at(path.as_ref(), true)
    }

    fn command_line(&self) -> CommandLine {
        CommandLine {
            parameters: self
                .overrides
                .iter()
                .map(|(key, value)| Parameter {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
        }
    }

    fn open_at(&self, path: &Path, search: bool) -> Result<Repository> {
        let absolute = std::path::absolute(path).map_err(|e| Error::io(path, e))?;
        let mut start = std::fs::canonicalize(&absolute).map_err(|e| Error::io(path, e))?;
        if !search && start.file_name().is_some_and(|n| n == ".git") && start.is_dir() {
            // The `.git` directory of a work tree: open the repository it
            // belongs to (a bare repository of that name is still found,
            // its `core.bare` says so).
            if let Some(parent) = start.parent() {
                start = parent.to_path_buf();
            }
        }
        let ceiling_dirs: Option<OsString> = if search {
            std::env::join_paths(&self.ceiling_dirs)
                .ok()
                .filter(|joined| !joined.is_empty())
        } else {
            start.parent().map(|p| p.as_os_str().to_os_string())
        };
        let env: &dyn Environment = &*self.env;
        let options = SetupOptions {
            cwd: start,
            git_dir: None,
            work_tree: None,
            common_dir: None,
            object_dir: None,
            ceiling_dirs,
            across_filesystem: false,
            implicit_work_tree: true,
            owner: Owner::current(env),
            support: Support::default(),
        };
        let mut protected = StandardProtectedConfig::new(env, self.command_line());
        let setup = match rgit_repo::discover(&options, &mut protected) {
            Ok(Ok(setup)) => setup,
            Ok(Err(not_found)) => {
                return Err(Error::NotARepository {
                    path: path.to_path_buf(),
                    message: rgit_repo::Error::NotFound(not_found).to_string(),
                });
            }
            Err(e) => return Err(open_error(path, &e)),
        };
        Repository::from_setup(setup, Arc::clone(&self.env), &self.command_line())
    }
}

/// The object database of the repository `setup` describes: the objects
/// directory of its common directory with packs and alternates, and the
/// settings of `config`.
///
/// Like `ObjectDatabase::open_for_repo`, but the directory is the one of
/// the repository whatever `GIT_OBJECT_DIRECTORY` says in the process.
fn open_objects(
    setup: &Setup,
    config: &ConfigSet,
    warnings: &mut Vec<String>,
) -> std::result::Result<ObjectDatabase, rgit_odb::Error> {
    let config_error = |e: rgit_config::ValueError| rgit_odb::Error::Config(e.to_string());
    let options = LooseOptions::from_config(config, warnings)?;
    let large = LargeObjectOptions::from_config(config)?;
    let hash = setup.format.object_format;
    let dir = setup.common_dir.join("objects");
    let use_midx = config
        .bool("core.multiPackIndex")
        .map_err(config_error)?
        .unwrap_or(true);
    let mut db = if use_midx {
        ObjectDatabase::open_with_midx(dir, hash, options)?
    } else {
        ObjectDatabase::open_with_packs(dir, hash, options)
    };
    db.set_large_object_options(large);
    let limit = config.int("core.deltaBaseCacheLimit").ok().flatten();
    if let (Some(limit), Some(packs)) = (limit, db.packs()) {
        packs.set_cache_limit(usize::try_from(limit).unwrap_or(0));
    }
    Ok(db)
}

/// A setup error as an [`Error::Open`]: git's last line is the message,
/// the lines before it are the diagnostics.
fn open_error(path: &Path, e: &rgit_repo::Error) -> Error {
    let mut message = e.to_string();
    let mut diagnostics = Vec::new();
    for (level, text) in e.report() {
        match level {
            rgit_config::Level::Fatal => message = text,
            rgit_config::Level::Error => diagnostics.push(Diagnostic {
                level: Level::Error,
                text,
            }),
            rgit_config::Level::Warning => diagnostics.push(Diagnostic::warning(text)),
        }
    }
    Error::Open {
        path: path.to_path_buf(),
        message,
        diagnostics,
    }
}

/// An open repository: the entry point of the library.
///
/// Opening reads the configuration and locates the object database and
/// the references; the index and the work tree are read by the operations
/// that need them, every time they run, so a handle that is kept for long
/// sees what other processes do to the repository.
///
/// A `Repository` can be sent to and shared between threads. Operations
/// take `&self`; two threads can read through the same handle at the same
/// time.
///
/// ```no_run
/// # fn main() -> rgit::Result<()> {
/// let repo = rgit::Repository::open(".")?;
/// println!("{}", repo.git_dir().display());
/// if let Some(id) = repo.head()?.id() {
///     println!("HEAD is at {id}");
/// }
/// # Ok(())
/// # }
/// ```
pub struct Repository {
    pub(crate) setup: Setup,
    pub(crate) config: ConfigSet,
    pub(crate) odb: Arc<ObjectDatabase>,
    pub(crate) refs: FilesStore,
    pub(crate) env: Arc<Env>,
    diagnostics: Arc<Mutex<Vec<Diagnostic>>>,
    shallow: OnceLock<HashSet<ObjectId>>,
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("git_dir", &self.setup.git_dir)
            .field("work_dir", &self.setup.work_tree)
            .finish_non_exhaustive()
    }
}

impl Repository {
    /// [`OpenOptions::open`] with [`OpenOptions::new`].
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let err = rgit::OpenOptions::isolated().open(dir.path()).unwrap_err();
    /// assert!(matches!(err, rgit::Error::NotARepository { .. }));
    /// assert!(err.to_string().starts_with("not a git repository"));
    /// assert_eq!(err.exit_code(), 128);
    /// ```
    ///
    /// # Errors
    /// See [`OpenOptions::open`].
    pub fn open(path: impl AsRef<Path>) -> Result<Repository> {
        OpenOptions::new().open(path)
    }

    /// [`OpenOptions::discover`] with [`OpenOptions::new`].
    ///
    /// # Errors
    /// See [`OpenOptions::open`].
    pub fn discover(path: impl AsRef<Path>) -> Result<Repository> {
        OpenOptions::new().discover(path)
    }

    fn from_setup(mut setup: Setup, env: Arc<Env>, command_line: &CommandLine) -> Result<Self> {
        // The library never changes the current directory, so every path
        // it keeps is absolute.
        setup.git_dir = setup.absolute_git_dir();
        setup.common_dir = setup.absolute_common_dir();
        if let Some(top) = &setup.work_tree {
            setup.cwd = top.clone();
        } else {
            setup.cwd = setup.git_dir.clone();
        }
        setup.original_cwd = setup.cwd.clone();
        setup.prefix = None;

        let mut diagnostics: Vec<Diagnostic> = std::mem::take(&mut setup.warnings)
            .into_iter()
            .map(Diagnostic::warning)
            .collect();

        let branch = rgit_refs::read_head_file(&setup.git_dir)
            .ok()
            .and_then(|head| {
                head.strip_prefix(b"ref: refs/heads/")
                    .map(|name| BString::from(name.trim_ascii_end()))
            });
        let facts = StaticRepoFacts {
            git_dir: Some(setup.git_dir.clone()),
            worktree: setup.work_tree.clone(),
            branch,
        };
        let mut config = ConfigSet::new();
        sources::load_standard(
            LoadOptions {
                env: &*env,
                repo: Some(&facts),
                includes: true,
            },
            Some(&RepoConfig {
                common_dir: setup.common_dir.clone(),
                git_dir: setup.git_dir.clone(),
                worktree_config: None,
            }),
            command_line,
            ScopeFilter::default(),
            &mut config,
        )
        .map_err(Error::config)?;

        let mut warnings = Vec::new();
        let mut odb = open_objects(&setup, &config, &mut warnings).map_err(|e| match e {
            rgit_odb::Error::Config(message) => Error::Config { message },
            e => Error::corrupt(e),
        })?;
        diagnostics.extend(warnings.into_iter().map(Diagnostic::warning));
        let diagnostics = Arc::new(Mutex::new(diagnostics));
        let sink = Arc::clone(&diagnostics);
        // Copies of an object that are skipped because they are damaged:
        // git prints them, the library hands them to the caller.
        odb.set_error_sink(move |e| {
            if let Ok(mut list) = sink.lock() {
                list.extend(e.skipped_copy_lines().into_iter().map(|text| Diagnostic {
                    level: Level::Error,
                    text,
                }));
            }
        });

        let hash = setup.format.object_format;
        let refs = FilesStore::new(&setup.common_dir, hash).with_worktree(&setup.git_dir);
        Ok(Repository {
            setup,
            config,
            odb: Arc::new(odb),
            refs,
            env,
            diagnostics,
            shallow: OnceLock::new(),
        })
    }

    /// The git directory (`.git`, or the repository itself when it is
    /// bare; the private directory of a linked work tree). Absolute.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.setup.git_dir
    }

    /// The directory that holds what all work trees of the repository
    /// share (objects, references, `config`). The same as
    /// [`Repository::git_dir`] outside linked work trees. Absolute.
    #[must_use]
    pub fn common_dir(&self) -> &Path {
        &self.setup.common_dir
    }

    /// The top of the work tree; `None` for a bare repository. Absolute.
    #[must_use]
    pub fn work_dir(&self) -> Option<&Path> {
        self.setup.work_tree.as_deref()
    }

    /// Whether the repository has no work tree.
    #[must_use]
    pub fn is_bare(&self) -> bool {
        self.setup.is_bare()
    }

    /// The hash algorithm of the repository's object names.
    #[must_use]
    pub fn object_format(&self) -> HashKind {
        self.setup.format.object_format
    }

    /// Take the warnings and non-fatal errors collected so far: what the
    /// `git` command would have printed to standard error while opening
    /// the repository and reading objects.
    pub fn take_diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .lock()
            .map(|mut list| std::mem::take(&mut *list))
            .unwrap_or_default()
    }

    pub(crate) fn push_diagnostics(&self, more: impl IntoIterator<Item = Diagnostic>) {
        if let Ok(mut list) = self.diagnostics.lock() {
            list.extend(more);
        }
    }

    /// The work tree, for operations that need one.
    pub(crate) fn require_work_dir(&self) -> Result<&Path> {
        self.work_dir().ok_or(Error::BareRepository)
    }

    /// The index as it is on disk now (an empty index if there is no
    /// file).
    pub(crate) fn read_index(&self) -> Result<Index> {
        let path = self.setup.git_dir.join("index");
        Index::read_file_in(
            &path,
            self.object_format(),
            &ReadOptions::default(),
            Some(&self.setup.git_dir),
        )
        .map_err(Error::corrupt)
    }

    /// The content of object `id`, which must be of type `kind`.
    pub(crate) fn read_kind(&self, id: &ObjectId, kind: ObjectKind) -> Result<Vec<u8>> {
        match self.odb.read(id) {
            Ok(Some(raw)) if raw.kind == kind => Ok(raw.data),
            Ok(Some(_)) => Err(Error::corrupt_message(messages::not_a(id, kind.as_str()))),
            Ok(None) => Err(Error::ObjectNotFound { id: *id }),
            Err(e) => Err(Error::corrupt(e)),
        }
    }

    /// The commits a shallow clone ends at (`$GIT_DIR/shallow`): their
    /// parents are not in the repository.
    pub(crate) fn shallow(&self) -> &HashSet<ObjectId> {
        self.shallow.get_or_init(|| {
            let Ok(data) = std::fs::read(self.setup.common_dir.join("shallow")) else {
                return HashSet::new();
            };
            let hash = self.object_format();
            data.split(|&b| b == b'\n')
                .filter_map(|line| ObjectId::from_hex(hash, line.trim_ascii()).ok())
                .collect()
        })
    }
}
