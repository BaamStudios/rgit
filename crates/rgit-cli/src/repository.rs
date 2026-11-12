// SPDX-License-Identifier: MIT
//! A set-up repository for commands that read objects, the index or the
//! configuration: [`crate::repo::setup`] plus the merged configuration and
//! the object database, opened on first use.

use std::cell::OnceCell;
use std::path::PathBuf;

use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{ConfigSet, LoadOptions, StaticRepoFacts, SystemEnv};
use rgit_hash::HashKind;
use rgit_index::{Index, ReadOptions, Time};
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_repo::Setup;
use rgit_worktree::stat::{CheckStat, Checker, StatOptions, index_mtime};

use crate::repo::{self, Mode};
use crate::{Context, Die, objects};

/// The pathspec switches from the environment and the global options.
pub fn pathspec_globals(ctx: &Context) -> rgit_pathspec::GlobalFlags {
    let mut g = rgit_pathspec::GlobalFlags::from_env().unwrap_or_default();
    g.literal |= ctx.literal_pathspecs;
    g.glob |= ctx.glob_pathspecs;
    g.noglob |= ctx.noglob_pathspecs;
    g.icase |= ctx.icase_pathspecs;
    g
}

/// The commit checked out in the submodule (or nested repository) at the
/// directory `dir`: its `.git` directory or `gitdir:` file, then `HEAD`.
pub fn submodule_head(dir: &std::path::Path, hash: HashKind) -> Option<rgit_hash::ObjectId> {
    use rgit_refs::RefStore;
    let dot_git = dir.join(".git");
    let meta = std::fs::metadata(&dot_git).ok()?;
    let git_dir = if meta.is_dir() {
        dot_git
    } else {
        let data = std::fs::read(&dot_git).ok()?;
        let rest = data.strip_prefix(b"gitdir:")?;
        let target = std::str::from_utf8(rest).ok()?.trim();
        dir.join(target)
    };
    rgit_refs::files::FilesStore::new(git_dir, hash)
        .resolve_oid("HEAD")
        .ok()
        .flatten()
}

/// A repository opened by a command.
pub struct Repo {
    /// Discovery result; the process is in [`Setup::cwd`].
    pub setup: Setup,
    /// System, global, local, worktree and command line configuration.
    pub config: ConfigSet,
    db: OnceCell<ObjectDatabase>,
    auto_abbrev: OnceCell<usize>,
}

impl Repo {
    /// Set up the repository and read its configuration.
    pub fn open(ctx: &Context, mode: Mode) -> Result<Repo, Die> {
        let setup = repo::setup(ctx, mode)?;
        let env = SystemEnv::default();
        let facts = StaticRepoFacts {
            git_dir: Some(setup.absolute_git_dir()),
            worktree: setup.work_tree.clone(),
            branch: None,
        };
        let opts = LoadOptions {
            env: &env,
            repo: Some(&facts),
            includes: true,
        };
        let repo_cfg = RepoConfig {
            common_dir: setup.common_dir.clone(),
            git_dir: setup.git_dir.clone(),
            worktree_config: None,
        };
        let mut config = ConfigSet::new();
        if let Err(e) = sources::load_standard(
            opts,
            Some(&repo_cfg),
            &repo::command_line(ctx),
            ScopeFilter::default(),
            &mut config,
        ) {
            return Err(Die::Fatal(e.to_string()));
        }
        Ok(Repo {
            setup,
            config,
            db: OnceCell::new(),
            auto_abbrev: OnceCell::new(),
        })
    }

    /// The repository's hash algorithm.
    pub fn hash(&self) -> HashKind {
        self.setup.format.object_format
    }

    /// The `objects` directory.
    pub fn objects_dir(&self) -> PathBuf {
        rgit_odb::objects_dir(&self.setup.common_dir)
    }

    /// The object database (loose objects and packs).
    pub fn odb(&self) -> &ObjectDatabase {
        self.db.get_or_init(|| {
            let mut warnings = Vec::new();
            let loose = LooseOptions::from_config(&self.config, &mut warnings).unwrap_or_default();
            objects::open(&self.objects_dir(), self.hash(), loose)
        })
    }

    /// The current directory relative to the top of the work tree, with a
    /// trailing `/`, or empty.
    pub fn prefix(&self) -> Vec<u8> {
        self.setup
            .prefix
            .as_ref()
            .map(|p| p.as_encoded_bytes().to_vec())
            .unwrap_or_default()
    }

    /// A command line path relative to the current directory (or absolute)
    /// as a path relative to the top of the work tree: `.` and empty
    /// components dropped, `..` resolved, a trailing `/` kept. The error is
    /// the path when it lies outside the work tree.
    pub fn prefix_path(&self, arg: &[u8]) -> Result<Vec<u8>, ()> {
        let mut full: Vec<u8> = Vec::new();
        if arg.first() == Some(&b'/') {
            let top = self
                .setup
                .work_tree
                .as_ref()
                .map(|w| w.as_os_str().as_encoded_bytes().to_vec())
                .ok_or(())?;
            let rest = arg.strip_prefix(top.as_slice()).ok_or(())?;
            if !(rest.is_empty() || rest.starts_with(b"/")) {
                return Err(());
            }
            full.extend_from_slice(rest);
        } else {
            full.extend_from_slice(&self.prefix());
            full.extend_from_slice(arg);
        }
        let trailing = full.ends_with(b"/") && !full.is_empty();
        let mut parts: Vec<&[u8]> = Vec::new();
        for c in full.split(|&b| b == b'/') {
            match c {
                b"" | b"." => {}
                b".." => {
                    parts.pop().ok_or(())?;
                }
                c => parts.push(c),
            }
        }
        let mut out = parts.join(&b'/');
        if trailing && !out.is_empty() {
            out.push(b'/');
        }
        Ok(out)
    }

    /// `core.quotePath` (default true).
    pub fn quote_path(&self) -> bool {
        self.config
            .bool("core.quotepath")
            .ok()
            .flatten()
            .unwrap_or(true)
    }

    /// The default abbreviation length: `core.abbrev`, else computed from
    /// the number of objects.
    pub fn default_abbrev(&self) -> usize {
        let hexsz = self.hash().hex_len();
        if let Ok(Some(v)) = self.config.string("core.abbrev") {
            let v = v.to_string();
            if v.eq_ignore_ascii_case("no") || v.eq_ignore_ascii_case("false") {
                return hexsz;
            }
            if let Ok(n) = v.parse::<usize>() {
                return n.clamp(objects::MIN_ABBREV, hexsz);
            }
        }
        *self
            .auto_abbrev
            .get_or_init(|| objects::auto_abbrev(self.odb(), &self.objects_dir()))
    }

    fn config_bool(&self, key: &str, default: bool) -> bool {
        self.config.bool(key).ok().flatten().unwrap_or(default)
    }

    /// What stat comparisons trust (`core.trustCtime`, `core.checkStat`,
    /// `core.fileMode`, `core.symlinks`).
    pub fn stat_options(&self) -> StatOptions {
        let check_stat = match self.config.string("core.checkstat") {
            Ok(Some(v)) => CheckStat::from_config(v.as_ref()).unwrap_or_default(),
            _ => CheckStat::default(),
        };
        StatOptions {
            trust_ctime: self.config_bool("core.trustctime", true),
            check_stat,
            file_mode: self.config_bool("core.filemode", true),
            symlinks: self.config_bool("core.symlinks", true),
            ..StatOptions::default()
        }
    }

    /// The index file (`$GIT_INDEX_FILE` or `<git-dir>/index`).
    pub fn index_path(&self) -> PathBuf {
        rgit_index::index_path(&self.setup.git_dir)
    }

    /// Read the index; also returns the index file's mtime for the racy-git
    /// check.
    pub fn read_index(&self) -> Result<(Index, Option<Time>), Die> {
        let path = self.index_path();
        let mtime = index_mtime(&path).ok().flatten();
        let index = Index::read_file(&path, self.hash(), &ReadOptions::default())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok((index, mtime))
    }

    /// The top of the work tree, or git's error for commands that need one.
    pub fn work_tree(&self) -> Result<PathBuf, Die> {
        self.setup
            .require_work_tree()
            .map(|p| p.to_path_buf())
            .map_err(repo::die)
    }

    /// A checker comparing index entries with the work tree.
    pub fn checker(&self, index_mtime: Option<Time>) -> Result<Checker, Die> {
        Ok(Checker::new(
            self.work_tree()?,
            self.hash(),
            self.stat_options(),
            index_mtime,
        ))
    }

    /// `id` abbreviated to at least `len` digits (full when `len` is 0 or
    /// at least the full length), unique in the object database.
    pub fn abbrev(&self, id: &rgit_hash::ObjectId, len: usize) -> String {
        if len == 0 || len >= self.hash().hex_len() {
            return id.to_hex();
        }
        objects::unique_abbrev(self.odb(), id, len)
    }
}
