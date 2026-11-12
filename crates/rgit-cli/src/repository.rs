// SPDX-License-Identifier: MIT
//! A set-up repository for commands that read objects, the index or the
//! configuration: [`crate::repo::setup`] plus the merged configuration and
//! the object database, opened on first use.

use std::cell::OnceCell;
use std::path::PathBuf;

use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{ConfigSet, LoadOptions, StaticRepoFacts, SystemEnv};
use rgit_hash::HashKind;
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_repo::Setup;

use crate::repo::{self, Mode};
use crate::{Context, Die, objects};

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

    /// `id` abbreviated to at least `len` digits (full when `len` is 0 or
    /// at least the full length), unique in the object database.
    pub fn abbrev(&self, id: &rgit_hash::ObjectId, len: usize) -> String {
        if len == 0 || len >= self.hash().hex_len() {
            return id.to_hex();
        }
        objects::unique_abbrev(self.odb(), id, len)
    }
}
